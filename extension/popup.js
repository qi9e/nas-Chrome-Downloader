// popup.js — fetches tasks from the NAS server and renders them.

const DEFAULT_SETTINGS = { serverUrl: "", apiKey: "", enabled: true, minSizeBytes: 0 };
const $ = (id) => document.getElementById(id);

function fmtBytes(n) {
  if (n == null) return "?";
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return `${n.toFixed(i ? 1 : 0)} ${u[i]}`;
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
  );
}

async function getSettings() {
  return await chrome.storage.sync.get(DEFAULT_SETTINGS);
}

function setStatus(text, cls) {
  $("status").textContent = text;
  $("dot").className = "dot " + (cls || "");
}

async function loadList() {
  const s = await getSettings();
  if (!s.serverUrl || !s.apiKey) {
    setStatus("Not configured", "err");
    $("list").innerHTML =
      '<div class="item">Open Settings to configure the server URL and API key.</div>';
    return;
  }
  try {
    const r = await fetch(s.serverUrl.replace(/\/+$/, "") + "/api/downloads", {
      headers: { Authorization: `Bearer ${s.apiKey}` },
    });
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    const tasks = await r.json();
    setStatus(`Connected · ${tasks.length} task${tasks.length === 1 ? "" : "s"}`, "ok");
    $("list").innerHTML = tasks.length
      ? tasks.slice(0, 50).map(renderTask).join("")
      : '<div class="item">No downloads yet.</div>';
  } catch (e) {
    setStatus(`Error: ${e.message}`, "err");
    $("list").innerHTML = '<div class="item">Failed to reach server.</div>';
  }
}

function renderTask(t) {
  const pct = t.total_bytes ? Math.min(100, (t.downloaded_bytes / t.total_bytes) * 100) : 0;
  const sizeText = t.total_bytes
    ? `${fmtBytes(t.downloaded_bytes)} / ${fmtBytes(t.total_bytes)} · ${pct.toFixed(0)}%`
    : `${fmtBytes(t.downloaded_bytes)}`;
  const showBar = t.status === "downloading" || (t.status === "completed" && t.total_bytes);
  const bar = showBar
    ? `<div class="progress"><div style="width: ${pct}%"></div></div>`
    : "";
  const err = t.error
    ? `<div class="err-text">${escapeHtml(t.error)}</div>`
    : "";
  return `
    <div class="item">
      <div class="name" title="${escapeHtml(t.url)}">${escapeHtml(t.filename)}</div>
      ${bar}
      <div class="meta">
        <span class="badge ${t.status}">${t.status}</span>
        <span>${sizeText}</span>
      </div>
      ${err}
    </div>
  `;
}

async function init() {
  const s = await getSettings();
  $("enabled").checked = s.enabled !== false;
  $("enabled").addEventListener("change", async (e) => {
    await chrome.storage.sync.set({ enabled: e.target.checked });
  });
  $("open-options").addEventListener("click", () => chrome.runtime.openOptionsPage());
  $("refresh").addEventListener("click", loadList);
  await loadList();
  // Poll while popup is open. Stops automatically when popup closes.
  setInterval(loadList, 2500);
}

init();
