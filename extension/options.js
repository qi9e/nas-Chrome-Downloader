// options.js — load/save settings, test connection.

const DEFAULT_SETTINGS = { serverUrl: "", apiKey: "", enabled: true, minSizeBytes: 0 };
const $ = (id) => document.getElementById(id);

async function load() {
  const s = await chrome.storage.sync.get(DEFAULT_SETTINGS);
  $("serverUrl").value = s.serverUrl || "";
  $("apiKey").value = s.apiKey || "";
  $("minSizeBytes").value = s.minSizeBytes ?? 0;
  $("enabled").checked = s.enabled !== false;
}

async function save() {
  const settings = {
    serverUrl: $("serverUrl").value.trim().replace(/\/+$/, ""),
    apiKey: $("apiKey").value.trim(),
    minSizeBytes: parseInt($("minSizeBytes").value, 10) || 0,
    enabled: $("enabled").checked,
  };
  await chrome.storage.sync.set(settings);
  setStatus("Saved.", "ok");
}

async function test() {
  const serverUrl = $("serverUrl").value.trim().replace(/\/+$/, "");
  const apiKey = $("apiKey").value.trim();
  if (!serverUrl) { setStatus("Set the server URL first.", "err"); return; }
  setStatus("Testing…", "");
  try {
    const r = await fetch(`${serverUrl}/api/downloads`, {
      headers: { Authorization: `Bearer ${apiKey}` },
    });
    if (r.ok) {
      const tasks = await r.json();
      setStatus(`✓ Connected. ${tasks.length} task(s) on server.`, "ok");
    } else {
      setStatus(`✗ HTTP ${r.status}: ${(await r.text()).slice(0, 200)}`, "err");
    }
  } catch (e) {
    setStatus(`✗ ${e.message}`, "err");
  }
}

function setStatus(text, cls) {
  $("status").textContent = text;
  $("status").className = "status " + (cls || "");
}

$("save").addEventListener("click", save);
$("test").addEventListener("click", test);
load();
