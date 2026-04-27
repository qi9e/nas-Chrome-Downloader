# NAS Downloader

一个跑在 NAS 上的 Rust 下载服务器，配合一个 Manifest V3 的 Chrome 扩展。
扩展拦截浏览器内的下载请求，把 URL（连带 cookies、referer、user-agent）转发给服务器，由 NAS 完成下载。

## 目录结构

```
nas-downloader/
├── backend/          # Rust 服务器（部署到 NAS）
│   ├── Cargo.toml
│   └── src/main.rs
├── extension/        # Chrome 扩展（开发者模式加载）
│   ├── manifest.json
│   ├── background.js
│   ├── popup.html / popup.js
│   └── options.html / options.js
└── README.md
```

---

## Backend（NAS 端）

### 编译

需要 Rust 1.75+。

```bash
cd backend
cargo build --release
```

产物在 `backend/target/release/nas-downloader`。

### 运行

```bash
# 必填：与扩展中的 API Key 完全一致
export NAS_DOWNLOADER_API_KEY="$(openssl rand -hex 32)"
# 选填：下载目录，默认 ./downloads
export NAS_DOWNLOADER_DIR="/mnt/nas/downloads"
# 选填：监听地址，默认 0.0.0.0:8787
export NAS_DOWNLOADER_LISTEN="0.0.0.0:8787"

./target/release/nas-downloader
```

启动时会打印监听地址和下载目录。**保存好 `NAS_DOWNLOADER_API_KEY`**，下面要填到扩展里。

### 部署成 systemd 服务（可选）

`/etc/systemd/system/nas-downloader.service`：

```ini
[Unit]
Description=NAS Downloader
After=network-online.target

[Service]
Type=simple
User=nasdownloader
Environment=NAS_DOWNLOADER_API_KEY=换成你自己生成的key
Environment=NAS_DOWNLOADER_DIR=/mnt/nas/downloads
Environment=NAS_DOWNLOADER_LISTEN=0.0.0.0:8787
ExecStart=/opt/nas-downloader/nas-downloader
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

然后 `systemctl enable --now nas-downloader`。

### Docker（可选）

`backend/Dockerfile`（自己加一个，简单示例）：

```dockerfile
FROM rust:1-bookworm as build
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/nas-downloader /usr/local/bin/
EXPOSE 8787
CMD ["nas-downloader"]
```

### API 参考

`/api/downloads*` 全部要 `Authorization: Bearer <API_KEY>`。

| 方法   | 路径                    | 说明                    |
| ------ | ----------------------- | ----------------------- |
| GET    | `/api/health`           | 健康检查（不鉴权）      |
| POST   | `/api/downloads`        | 提交下载任务            |
| GET    | `/api/downloads`        | 列出所有任务            |
| GET    | `/api/downloads/{id}`   | 查询单个任务            |
| DELETE | `/api/downloads/{id}`   | 取消并从列表中移除      |

POST body：

```json
{
  "url": "https://example.com/file.zip",
  "filename": "optional-override.zip",
  "referer": "https://example.com/page",
  "user_agent": "Mozilla/5.0 …",
  "cookie": "sessionid=abc; foo=bar",
  "headers": { "X-Custom": "value" }
}
```

curl 测试：

```bash
curl -X POST http://nas.local:8787/api/downloads \
  -H "Authorization: Bearer $NAS_DOWNLOADER_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"url":"https://speed.hetzner.de/100MB.bin"}'
```

### 已知限制

- 任务列表存在内存里，重启后清空（已下载完成的文件还在磁盘上）。
- 不支持断点续传。
- 不带 HTTPS — 暴露到 LAN 之外的话，前面挂一个 Caddy / nginx / Traefik 反向代理。
- API key 用普通字符串比较。LAN 环境够用，要上公网建议加 mTLS 或 OAuth。

---

## Extension（Chrome 端）

### 安装

1. 打开 `chrome://extensions/`
2. 打开右上角"开发者模式"
3. 点"加载已解压的扩展程序"，选择 `extension/` 文件夹
4. 选项页会自动打开，填入：
   - **Server URL**: `http://<NAS地址>:8787`
   - **API Key**: 与 `NAS_DOWNLOADER_API_KEY` 完全一致
5. 点 *Test connection*，看到绿色就 OK。

### 使用

随便点一个下载链接，工具栏图标会闪一下绿色 ✓ — 就被转走了。
点击工具栏图标可以看任务列表（每 2.5 秒自动刷新）。

### 拦截原理

扩展监听 `chrome.downloads.onCreated`。每次 Chrome 开启下载：

1. 立刻 `chrome.downloads.cancel` + `erase`，本地下载停掉。
2. 通过 `chrome.cookies.getAll` 读这个 URL 的 cookies（让需要登录的下载也能在 NAS 上跑）。
3. POST `{ url, filename, referer, user_agent, cookie }` 到 `/api/downloads`。
4. 工具栏图标闪绿色 ✓，状态写到 `chrome.storage.local`，popup 显示出来。

### 提示

- 在 Chrome *设置 → 下载内容* 里**关闭**"下载前询问每个文件的保存位置"，否则会先弹保存框、再被扩展取消，体验不好。
- `blob:` 和 `data:` URL 不会转发（NAS 拿不到这种 URL，只有浏览器内存里有）。
- popup 里的 *Intercept downloads* 开关可以临时关闭转发（不卸载扩展）。
- 如果想只转发大文件，把 *Minimum size* 设个值（比如 `1048576` 表示只转发 ≥ 1 MB 的文件）。

---

## 安全说明

- 扩展会把当前页面的 cookies 发给 NAS，**只在你信任的、自己控制的服务器上用**。
- API key 视同密码处理。
- 不上 TLS 之前请用防火墙限制只在 LAN 访问。
- 服务器只校验 API key，不会校验 URL 是否合法 — 谁拿到 API key，谁就能让你的 NAS 去下载任意 URL。
