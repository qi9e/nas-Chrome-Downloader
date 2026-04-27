// background.js — service worker.
// Listens for new browser downloads, cancels them, and POSTs the URL plus
// the page's cookies/referer/UA to the NAS server.

const DEFAULT_SETTINGS = {
  serverUrl: "",
  apiKey: "",
  enabled: true,
  minSizeBytes: 0,
};

async function getSettings() {
  return await chrome.storage.sync.get(DEFAULT_SETTINGS);
}

chrome.runtime.onInstalled.addListener(async () => {
  const s = await getSettings();
  if (!s.serverUrl || !s.apiKey) {
    chrome.runtime.openOptionsPage();
  }
});

// Don't await inside the listener — Chrome can dispose the SW between awaits.
// Kick off a background promise instead.
chrome.downloads.onCreated.addListener((item) => {
  handleDownload(item).catch((err) => {
    console.error("[NAS Downloader] handler error:", err);
    setLastStatus({ ok: false, message: String(err) });
    flashBadge("ERR", "#dc2626");
  });
});

async function handleDownload(item) {
  const settings = await getSettings();
  if (!settings.enabled) return;
  if (!settings.serverUrl || !settings.apiKey) {
    flashBadge("CFG", "#f59e0b");
    return;
  }
  if (!item.url || !/^https?:/i.test(item.url)) {
    // blob: / data: / file: — server can't fetch these.
    return;
  }
  if (
    settings.minSizeBytes > 0 &&
    item.totalBytes > 0 &&
    item.totalBytes < settings.minSizeBytes
  ) {
    return;
  }

  // Stop the local download immediately and remove it from history.
  await safeCancel(item.id);

  const cookie = await getCookieHeader(item.url);
  const filename = item.filename ? lastPathComponent(item.filename) : undefined;

  const body = {
    url: item.url,
    filename,
    referer: item.referrer || undefined,
    user_agent: navigator.userAgent,
    cookie: cookie || undefined,
  };

  const url = settings.serverUrl.replace(/\/+$/, "") + "/api/downloads";
  const resp = await fetch(url, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "Authorization": `Bearer ${settings.apiKey}`,
    },
    body: JSON.stringify(body),
  });

  if (!resp.ok) {
    const text = await resp.text();
    throw new Error(`server ${resp.status}: ${text.slice(0, 200)}`);
  }

  const task = await resp.json();
  setLastStatus({ ok: true, message: `Sent: ${task.filename}`, task });
  flashBadge("✓", "#16a34a");
  console.log("[NAS Downloader] forwarded:", task);
}

async function safeCancel(id) {
  try {
    await chrome.downloads.cancel(id);
  } catch (_) {}
  try {
    await chrome.downloads.erase({ id });
  } catch (_) {}
}

async function getCookieHeader(url) {
  try {
    const cookies = await chrome.cookies.getAll({ url });
    if (!cookies.length) return "";
    return cookies.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch (_) {
    return "";
  }
}

function flashBadge(text, color) {
  chrome.action.setBadgeText({ text });
  chrome.action.setBadgeBackgroundColor({ color });
  setTimeout(() => chrome.action.setBadgeText({ text: "" }), 4000);
}

async function setLastStatus(status) {
  await chrome.storage.local.set({
    lastStatus: { ...status, time: Date.now() },
  });
}

function lastPathComponent(p) {
  return p.split(/[/\\]/).filter(Boolean).pop() || "";
}
