const ext = globalThis.browser ?? globalThis.chrome;
const $ = (id) => document.getElementById(id);
const statusBox = $("status");
const statusTitle = $("status-title");
const statusDetail = $("status-detail");
const mediaList = $("media-list");
const mediaEmpty = $("media-empty");
const mediaSummary = $("media-summary");
const feedback = $("media-feedback");
const capture = $("capture");
const subtitles = $("subtitles");

let currentTab = null;
let connection = { connected: false };
let subtitlesOn = false;

function ask(message) {
  return ext.runtime
    .sendMessage(message)
    .catch((error) => ({ ok: false, error: String(error?.message || error) }));
}

function kindLabel(item) {
  if (item.kind === "hls") return "HLS";
  if (item.kind === "dash") return "DASH";
  return item.ext || (item.kind === "audio" ? "AUDIO" : "VIDEO");
}

function kindDetail(item) {
  const parts = [];
  if (item.mime) parts.push(item.mime);
  if (item.kind === "hls" || item.kind === "dash") parts.push("分片流");
  return parts.join(" · ") || "当前页面媒体";
}

function say(text, bad = false) {
  feedback.hidden = !text;
  feedback.textContent = text;
  feedback.dataset.bad = bad ? "1" : "0";
}

async function current() {
  const [tab] = await ext.tabs.query({ active: true, currentWindow: true }).catch(() => []);
  return tab || null;
}

function renderStatus(state) {
  connection = state || { connected: false };
  statusBox.dataset.kind = connection.connected ? "ok" : "bad";
  statusTitle.textContent = connection.connected ? "已连接 Volant" : "未连接 Volant";
  statusDetail.textContent = connection.connected
    ? connection.videoLookup === false
      ? "桌面端可用，当前版本不支持视频画质菜单"
      : `桌面端在线${connection.app ? ` · v${connection.app}` : ""}`
    : connection.error || "请启动 Volant，或重新安装浏览器集成";
  statusDetail.title = statusDetail.textContent;
  $("version").textContent = `v${ext.runtime.getManifest().version}`;
  $("open-app").disabled = !connection.connected;
  $("refresh").disabled = !currentTab;
}

function renderMedia(items) {
  mediaList.replaceChildren();
  mediaEmpty.hidden = items.length > 0;
  mediaSummary.textContent = items.length ? `${items.length} 个可下载媒体` : "当前页面尚未发现媒体";
  for (const item of items) {
    const row = document.createElement("li");
    row.className = "media-item";

    const tag = document.createElement("span");
    tag.className = "tag";
    tag.textContent = kindLabel(item);

    const copy = document.createElement("span");
    copy.className = "media-copy";
    const name = document.createElement("span");
    name.className = "media-name";
    name.textContent = item.name || item.url;
    name.title = item.url;
    const meta = document.createElement("span");
    meta.className = "media-meta";
    meta.textContent = kindDetail(item);
    copy.append(name, meta);

    const download = document.createElement("button");
    download.className = "download";
    download.type = "button";
    download.textContent = "下载";
    download.disabled = !connection.connected;
    download.title = "用 Volant 下载";
    download.addEventListener("click", async () => {
      download.disabled = true;
      say("正在解析并加入队列…");
      const reply = await ask({
        type: "media-download",
        tabId: currentTab?.id,
        url: item.url,
        title: currentTab?.title || item.name,
        subtitles: subtitlesOn,
      });
      say(
        reply.ok ? `已加入队列${reply.title ? `：${reply.title}` : ""}` : reply.error || "无法加入队列",
        !reply.ok,
      );
      download.disabled = !connection.connected;
    });
    row.append(tag, copy, download);
    mediaList.append(row);
  }
}

async function refreshMedia() {
  if (!currentTab) {
    mediaEmpty.hidden = false;
    mediaEmpty.textContent = "当前不是网页，或浏览器不允许读取这个页面。";
    mediaSummary.textContent = "当前标签页不可读取";
    return;
  }
  say("");
  const reply = await ask({ type: "media", tab: currentTab.id });
  renderMedia(reply?.media || []);
}

async function boot() {
  const stored = await ext.storage.local
    .get({ paused: false, videoSubtitles: false })
    .catch(() => ({ paused: false, videoSubtitles: false }));
  capture.checked = !stored.paused;
  subtitles.checked = Boolean(stored.videoSubtitles);
  subtitlesOn = subtitles.checked;
  currentTab = await current();
  renderStatus(await ask({ type: "state" }));
  await refreshMedia();
}

capture.addEventListener("change", async () => {
  await ext.storage.local.set({ paused: !capture.checked }).catch(() => {});
  say(capture.checked ? "已开启浏览器下载捕获" : "普通下载将继续由浏览器处理");
});

subtitles.addEventListener("change", () => {
  subtitlesOn = subtitles.checked;
  ext.storage.local.set({ videoSubtitles: subtitlesOn }).catch(() => {});
});

$("refresh").addEventListener("click", refreshMedia);
$("open-app").addEventListener("click", async () => {
  const reply = await ask({ type: "open" });
  if (!reply.ok) say(reply.error || "无法打开 Volant", true);
});

boot();
