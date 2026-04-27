//! NAS download server.
//!
//! Receives download requests from the Chrome extension and downloads the
//! files to a directory on the NAS. Holds task state in memory; restart loses
//! the queue (but completed files stay on disk).

use anyhow::Result;
use axum::{
    extract::{Path, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path as StdPath, PathBuf},
    sync::Arc,
};
use tokio::{fs, io::AsyncWriteExt, sync::RwLock};
use tokio_util::sync::CancellationToken;
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info, warn};
use uuid::Uuid;

// ---------- types ----------

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    tasks: Arc<DashMap<Uuid, Arc<TaskHandle>>>,
    http: reqwest::Client,
}

struct Config {
    api_key: String,
    download_dir: PathBuf,
    listen: SocketAddr,
}

struct TaskHandle {
    task: RwLock<DownloadTask>,
    cancel: CancellationToken,
}

#[derive(Serialize, Clone, Debug)]
struct DownloadTask {
    id: Uuid,
    url: String,
    filename: String,
    save_path: String,
    status: Status,
    total_bytes: Option<u64>,
    downloaded_bytes: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    error: Option<String>,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Status {
    Pending,
    Downloading,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Deserialize, Debug)]
struct CreateDownload {
    url: String,
    filename: Option<String>,
    referer: Option<String>,
    user_agent: Option<String>,
    cookie: Option<String>,
    headers: Option<HashMap<String, String>>,
}

#[derive(thiserror::Error, Debug)]
enum AppError {
    #[error("not found")]
    NotFound,
    #[error("unauthorized")]
    Unauthorized,
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
            AppError::BadRequest(s) => (StatusCode::BAD_REQUEST, s),
            AppError::Other(e) => {
                error!("internal error: {:?}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        };
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

// ---------- main ----------

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,nas_downloader=debug")),
        )
        .init();

    let api_key = std::env::var("NAS_DOWNLOADER_API_KEY")
        .map_err(|_| anyhow::anyhow!("NAS_DOWNLOADER_API_KEY env var must be set"))?;
    if api_key.len() < 16 {
        anyhow::bail!("NAS_DOWNLOADER_API_KEY should be at least 16 chars");
    }

    let download_dir = std::env::var("NAS_DOWNLOADER_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./downloads"));
    let listen: SocketAddr = std::env::var("NAS_DOWNLOADER_LISTEN")
        .unwrap_or_else(|_| "0.0.0.0:8787".to_string())
        .parse()?;

    fs::create_dir_all(&download_dir).await?;

    let config = Arc::new(Config {
        api_key,
        download_dir: download_dir.clone(),
        listen,
    });
    let state = AppState {
        config: config.clone(),
        tasks: Arc::new(DashMap::new()),
        http: reqwest::Client::builder()
            .user_agent(concat!("nas-downloader/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::limited(20))
            .build()?,
    };

    // /api/downloads* requires auth; /api/health is public.
    let api = Router::new()
        .route("/downloads", post(create_download).get(list_downloads))
        .route("/downloads/:id", get(get_download).delete(cancel_download))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth_middleware));

    let app = Router::new()
        .route("/api/health", get(health))
        .nest("/api", api)
        .with_state(state)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        );

    info!("nas-downloader listening on http://{}", listen);
    info!("download directory: {}", download_dir.display());
    let listener = tokio::net::TcpListener::bind(listen).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

// ---------- handlers ----------

async fn health() -> &'static str {
    "ok"
}

async fn auth_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match token {
        Some(t) if t == state.config.api_key => Ok(next.run(req).await),
        _ => Err(AppError::Unauthorized),
    }
}

async fn create_download(
    State(state): State<AppState>,
    Json(req): Json<CreateDownload>,
) -> Result<(StatusCode, Json<DownloadTask>), AppError> {
    if !req.url.starts_with("http://") && !req.url.starts_with("https://") {
        return Err(AppError::BadRequest("url must be http:// or https://".into()));
    }

    let id = Uuid::new_v4();
    let raw_filename = req
        .filename
        .clone()
        .or_else(|| url_to_filename(&req.url))
        .unwrap_or_else(|| format!("download-{}", id));
    let mut safe_name = sanitize_filename::sanitize(&raw_filename);
    if safe_name.is_empty() {
        safe_name = format!("download-{}", id);
    }
    let save_path = unique_path(&state.config.download_dir, &safe_name);
    let final_filename = save_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or(safe_name);

    let now = Utc::now();
    let task = DownloadTask {
        id,
        url: req.url.clone(),
        filename: final_filename,
        save_path: save_path.to_string_lossy().into_owned(),
        status: Status::Pending,
        total_bytes: None,
        downloaded_bytes: 0,
        created_at: now,
        updated_at: now,
        error: None,
    };

    let cancel = CancellationToken::new();
    let handle = Arc::new(TaskHandle {
        task: RwLock::new(task.clone()),
        cancel,
    });
    state.tasks.insert(id, handle.clone());

    tokio::spawn(run_download(state.clone(), handle, req, save_path));

    Ok((StatusCode::CREATED, Json(task)))
}

async fn list_downloads(State(state): State<AppState>) -> Json<Vec<DownloadTask>> {
    let handles: Vec<Arc<TaskHandle>> =
        state.tasks.iter().map(|e| e.value().clone()).collect();
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        out.push(h.task.read().await.clone());
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Json(out)
}

async fn get_download(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<DownloadTask>, AppError> {
    let handle = state.tasks.get(&id).ok_or(AppError::NotFound)?.clone();
    Ok(Json(handle.task.read().await.clone()))
}

async fn cancel_download(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let handle = state.tasks.get(&id).ok_or(AppError::NotFound)?.clone();
    handle.cancel.cancel();
    {
        let mut t = handle.task.write().await;
        if matches!(t.status, Status::Pending | Status::Downloading) {
            t.status = Status::Cancelled;
            t.updated_at = Utc::now();
        }
    }
    state.tasks.remove(&id);
    Ok(StatusCode::NO_CONTENT)
}

// ---------- worker ----------

async fn run_download(
    state: AppState,
    handle: Arc<TaskHandle>,
    req: CreateDownload,
    save_path: PathBuf,
) {
    let cancel = handle.cancel.clone();
    let result = tokio::select! {
        _ = cancel.cancelled() => {
            let _ = fs::remove_file(&save_path).await;
            update_task(&handle, |t| {
                t.status = Status::Cancelled;
                t.updated_at = Utc::now();
            }).await;
            return;
        }
        r = do_download(&state, &handle, &req, &save_path) => r,
    };

    match result {
        Ok(()) => {
            update_task(&handle, |t| {
                t.status = Status::Completed;
                t.updated_at = Utc::now();
            })
            .await;
            info!("completed: {}", save_path.display());
        }
        Err(e) => {
            warn!("failed: {} -> {}", req.url, e);
            let _ = fs::remove_file(&save_path).await;
            update_task(&handle, |t| {
                t.status = Status::Failed;
                t.error = Some(e.to_string());
                t.updated_at = Utc::now();
            })
            .await;
        }
    }
}

async fn do_download(
    state: &AppState,
    handle: &Arc<TaskHandle>,
    req: &CreateDownload,
    save_path: &StdPath,
) -> Result<()> {
    if let Some(parent) = save_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    // Build request headers from the browser context.
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(ua) = &req.user_agent {
        if let Ok(v) = ua.parse() {
            headers.insert(reqwest::header::USER_AGENT, v);
        }
    }
    if let Some(referer) = &req.referer {
        if let Ok(v) = referer.parse() {
            headers.insert(reqwest::header::REFERER, v);
        }
    }
    if let Some(cookie) = &req.cookie {
        if let Ok(v) = cookie.parse() {
            headers.insert(reqwest::header::COOKIE, v);
        }
    }
    if let Some(extra) = &req.headers {
        for (k, v) in extra {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                v.parse::<reqwest::header::HeaderValue>(),
            ) {
                headers.insert(name, value);
            }
        }
    }

    update_task(handle, |t| {
        t.status = Status::Downloading;
        t.updated_at = Utc::now();
    })
    .await;

    let resp = state
        .http
        .get(&req.url)
        .headers(headers)
        .send()
        .await?
        .error_for_status()?;
    let total = resp.content_length();

    update_task(handle, |t| t.total_bytes = total).await;

    let mut file = fs::File::create(save_path).await?;
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = 0;
    let mut last_update = std::time::Instant::now();

    while let Some(chunk) = stream.next().await {
        if handle.cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;

        if last_update.elapsed().as_millis() >= 500 {
            update_task(handle, |t| {
                t.downloaded_bytes = downloaded;
                t.updated_at = Utc::now();
            })
            .await;
            last_update = std::time::Instant::now();
        }
    }

    file.flush().await?;
    file.sync_all().await?;
    update_task(handle, |t| {
        t.downloaded_bytes = downloaded;
        if t.total_bytes.is_none() {
            t.total_bytes = Some(downloaded);
        }
    })
    .await;

    Ok(())
}

async fn update_task<F: FnOnce(&mut DownloadTask)>(handle: &Arc<TaskHandle>, f: F) {
    let mut t = handle.task.write().await;
    f(&mut t);
}

// ---------- helpers ----------

fn url_to_filename(url: &str) -> Option<String> {
    let no_query = url.split('?').next().unwrap_or(url);
    let no_frag = no_query.split('#').next().unwrap_or(no_query);
    let last = no_frag.rsplit('/').find(|s| !s.is_empty())?;
    Some(last.to_string())
}

fn unique_path(dir: &StdPath, filename: &str) -> PathBuf {
    let candidate = dir.join(filename);
    if !candidate.exists() {
        return candidate;
    }
    let stem = StdPath::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("download")
        .to_string();
    let ext = StdPath::new(filename)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{}", s))
        .unwrap_or_default();
    for i in 1..10_000 {
        let candidate = dir.join(format!("{}-{}{}", stem, i, ext));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{}-{}{}", stem, Uuid::new_v4(), ext))
}
