// Volant browser bridge.
//
// It hands ordinary browser downloads to the desktop app, and forwards media
// pages/stream manifests for yt-dlp to resolve. The browser never merges media.

const ext = globalThis.browser ?? globalThis.chrome;
const HOST = "com.volant.host";
const VERSION = ext.runtime.getManifest().version;
const REQUEST_TIMEOUT = 120_000;
const MANIFEST = /\.(m3u8|mpd)(?:[?#]|$)/i;
const MEDIA_FILE = /\.(mp4|m4v|webm|mov|mkv|m4a|mp3|ogg|opus|flac|wav|aac)(?:[?#]|$)/i;

let nextRequest = 1;
let statusTitle = "正在连接 Volant";
const passThrough = new Map();
const mediaByTab = new Map();
const sentWith = new Map();

function showBadge(text) {
  ext.action
    ?.setBadgeBackgroundColor?.({ color: text ? "#B3423D" : "#59626B" })
    .catch?.(() => {});
  ext.action?.setBadgeText?.({ text }).catch?.(() => {});
  ext.action?.setTitle?.({ title: statusTitle }).catch?.(() => {});
}

function connected() {
  statusTitle = "已连接 Volant";
  showBadge("");
}

function failed(error) {
  statusTitle = "未连接 Volant";
  showBadge("!");
  return {
    ok: false,
    why: "host-failed",
    error: String(error?.message || error || "原生消息连接失败"),
  };
}

/** One native request. The native host exits after its one reply. */
function native(message, timeout = REQUEST_TIMEOUT) {
  return new Promise((resolve) => {
    const req = nextRequest++;
    let port;
    let done = false;
    const finish = (reply) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      try {
        port?.disconnect();
      } catch {}
      resolve(reply);
    };
    const timer = setTimeout(
      () => finish(failed("Volant 长时间没有应答")),
      timeout,
    );
    try {
      port = ext.runtime.connectNative(HOST);
    } catch (error) {
      finish(failed(error));
      return;
    }
    port.onMessage.addListener((reply) => {
      if (reply && reply.req && reply.req !== req) return;
      if (reply?.ok) connected();
      finish(reply || failed("Volant 没有返回结果"));
    });
    port.onDisconnect.addListener(() => {
      const error = ext.runtime.lastError?.message;
      if (!done) finish(failed(error || "Volant 已断开连接"));
    });
    try {
      port.postMessage({
        ...message,
        req,
        userAgent: navigator.userAgent,
        browserVersion: VERSION,
      });
    } catch (error) {
      finish(failed(error));
    }
  });
}

function web(url) {
  return typeof url === "string" && /^https?:\/\//i.test(url);
}

async function cookieJar(...urls) {
  const targets = [...new Set(urls.filter(web))];
  const seen = new Set();
  const out = [];
  for (const url of targets) {
    let cookies = [];
    try {
      cookies = await ext.cookies.getAll({ url });
    } catch {}
    for (const cookie of cookies) {
      const key = `${cookie.domain}\t${cookie.path}\t${cookie.name}`;
      if (seen.has(key)) continue;
      seen.add(key);
      out.push({
        name: cookie.name,
        value: cookie.value,
        domain: cookie.domain,
        path: cookie.path,
        secure: Boolean(cookie.secure),
        httpOnly: Boolean(cookie.httpOnly),
        hostOnly: Boolean(cookie.hostOnly),
        expirationDate: cookie.expirationDate,
      });
    }
  }
  return out;
}

async function cookieHeader(url) {
  try {
    return (await ext.cookies.getAll({ url }))
      .map((cookie) => `${cookie.name}=${cookie.value}`)
      .join("; ");
  } catch {
    return "";
  }
}

function pageOf(tab) {
  return web(tab?.url) ? tab.url : "";
}

function fallbackTitle(url) {
  try {
    return new URL(url).hostname;
  } catch {
    return "视频";
  }
}

async function videoContext(message, sender) {
  const tab = sender?.tab || {};
  const page = web(message.url) ? message.url : pageOf(tab);
  const src = web(message.src) ? message.src : bestTrackedSource(tab.id);
  const sent = (mediaByTab.get(tab.id) || []).find((item) => item.url === src)?.sent || {};
  return {
    page,
    src,
    title: String(message.title || tab.title || fallbackTitle(page)).slice(0, 300),
    referrer: web(sent.referer)
      ? sent.referer
      : web(message.referrer)
        ? message.referrer
        : pageOf(tab),
    origin: web(sent.origin) ? sent.origin : "",
    cookies: await cookieJar(page, src),
  };
}

async function lookup(message, sender) {
  const context = await videoContext(message, sender);
  if (!web(context.page) && !web(context.src)) {
    return { ok: false, error: "没有找到可下载的视频页面" };
  }
  return native({
    type: "videoLookup",
    url: context.page,
    page: context.page,
    src: context.src,
    title: context.title,
    referer: context.referrer,
    origin: context.origin,
    cookies: context.cookies,
  });
}

async function pick(message, sender) {
  if (!message.token || !Number.isInteger(message.choice) || message.choice < 0) {
    return { ok: false, error: "画质信息无效" };
  }
  return native({
    type: "videoPick",
    token: message.token,
    choice: message.choice,
    subtitles: Boolean(message.subtitles),
  });
}

async function poll(message) {
  if (!message.token || typeof message.token !== "string") {
    return { ok: true, lookup: { state: "gone" } };
  }
  return native({ type: "videoPoll", token: message.token });
}

async function awaitLookup(reply) {
  let result = reply;
  const token = result?.lookup?.token;
  const deadline = Date.now() + REQUEST_TIMEOUT;
  while (result?.lookup?.state === "pending" && token && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 1200));
    result = await poll({ token });
  }
  return result;
}

function forgetMedia(tabId) {
  if (Number.isInteger(tabId)) mediaByTab.delete(tabId);
}

function mediaKind(url, type = "") {
  if (MANIFEST.test(url)) {
    return /\.m3u8(?:[?#]|$)/i.test(url) ? "hls" : "dash";
  }
  if (/^video\//i.test(type)) return "video";
  if (/^audio\//i.test(type)) return "audio";
  if (/\.(mp4|m4v|webm|mov|mkv)(?:[?#]|$)/i.test(url)) return "video";
  if (/\.(m4a|mp3|ogg|opus|flac|wav|aac)(?:[?#]|$)/i.test(url)) return "audio";
  return "";
}

function mediaName(url) {
  try {
    const parsed = new URL(url);
    const name = decodeURIComponent(parsed.pathname.split("/").filter(Boolean).pop() || "");
    return name || parsed.hostname;
  } catch {
    return url;
  }
}

function remember(tabId, url, kind, mime = "", extra = {}) {
  if (!Number.isInteger(tabId) || tabId < 0 || !kind) return;
  const list = mediaByTab.get(tabId) || [];
  const existing = list.find((item) => item.url === url);
  if (existing) {
    Object.assign(
      existing,
      { kind: existing.kind || kind, mime: mime || existing.mime },
      extra,
    );
  } else {
    list.push({
      url,
      kind,
      mime,
      name: mediaName(url),
      at: Date.now(),
      ...extra,
    });
  }
  mediaByTab.set(tabId, list.slice(-40));
}

const requestFilter = { urls: ["<all_urls>"] };
const sentFilter = {
  urls: ["<all_urls>"],
  types: ["xmlhttprequest", "media", "other"],
};
function noteSent(details) {
  if (details.tabId < 0) return;
  const sent = {};
  for (const header of details.requestHeaders || []) {
    const name = String(header.name).toLowerCase();
    if (
      ["referer", "origin", "cookie"].includes(name) &&
      typeof header.value === "string" &&
      header.value
    ) {
      sent[name] = header.value;
    }
  }
  sentWith.delete(details.requestId);
  if (Object.keys(sent).length) sentWith.set(details.requestId, sent);
  while (sentWith.size > 512) sentWith.delete(sentWith.keys().next().value);
}
try {
  ext.webRequest.onBeforeSendHeaders.addListener(
    noteSent,
    sentFilter,
    ["requestHeaders", "extraHeaders"],
  );
} catch {
  try {
    ext.webRequest.onBeforeSendHeaders.addListener(
      noteSent,
      sentFilter,
      ["requestHeaders"],
    );
  } catch {}
}
ext.webRequest.onBeforeRequest.addListener(
  (details) => {
    const kind = mediaKind(details.url);
    if (kind) remember(details.tabId, details.url, kind);
  },
  requestFilter,
);

ext.webRequest.onHeadersReceived.addListener(
  (details) => {
    const kind = mediaKind(details.url);
    if (!kind) return;
    const header = (details.responseHeaders || []).find(
      (item) => item.name.toLowerCase() === "content-type",
    );
    const mime = String(header?.value || "").split(";")[0].trim().toLowerCase();
    const sent = sentWith.get(details.requestId) || {};
    sentWith.delete(details.requestId);
    remember(details.tabId, details.url, kind, mime, { sent });
  },
  requestFilter,
  ["responseHeaders"],
);

ext.tabs.onRemoved.addListener(forgetMedia);
ext.tabs.onUpdated.addListener((tabId, change) => {
  if (change.status === "loading" || change.url) forgetMedia(tabId);
});

async function listMedia(tabId) {
  return (mediaByTab.get(tabId) || []).map((item) => ({
    ...item,
    name: item.name || mediaName(item.url),
    ext: /\.([a-z0-9]{2,5})(?:[?#]|$)/i.exec(item.url)?.[1]?.toUpperCase() || "",
  }));
}

function bestTrackedSource(tabId) {
  const items = mediaByTab.get(tabId) || [];
  return (
    [...items]
      .reverse()
      .find((item) => item.kind === "hls" || item.kind === "dash")?.url ||
    [...items].reverse().find((item) => item.kind === "video")?.url ||
    ""
  );
}

async function captureDownload(item) {
  const url = item.finalUrl || item.url || "";
  if (!web(url)) return false;
  const { paused } = await ext.storage.local.get({ paused: false });
  if (paused) return false;
  const stamp = passThrough.get(url);
  if (stamp && Date.now() - stamp < 15_000) {
    passThrough.delete(url);
    return false;
  }
  const reply = await native({
    type: "download",
    url,
    referer: item.referrer || "",
    cookie: await cookieHeader(url),
    user_agent: navigator.userAgent,
    t: Date.now(),
  });
  if (!reply.ok) return false;
  try {
    await ext.downloads.cancel(item.id);
  } catch {}
  try {
    const [download] = await ext.downloads.search({ id: item.id });
    if (download?.state === "complete" && download.exists !== false) {
      await ext.downloads.removeFile(item.id);
    }
  } catch {}
  ext.downloads.erase({ id: item.id }).catch?.(() => {});
  return true;
}

ext.downloads.onCreated.addListener((item) => {
  captureDownload(item).catch(() => {});
});

async function state() {
  const reply = await native({ type: "rules" }, 5000);
  if (!reply.ok) {
    return { connected: false, why: reply.why || "host-failed", error: reply.error };
  }
  return {
    connected: true,
    app: reply.version || "",
    videoLookup: reply.videoLookup !== false,
    videoDependencies: reply.videoDependencies || null,
  };
}

async function mediaDownload(message, sender) {
  const tab = sender?.tab || {};
  const item = (mediaByTab.get(tab.id) || []).find((entry) => entry.url === message.url);
  if (!item) return { ok: false, error: "这个媒体已经离开当前页面" };
  if (item.kind === "hls" || item.kind === "dash") {
    const found = await awaitLookup(
      await lookup(
        {
          url: pageOf(tab),
          src: item.url,
          title: message.title || tab.title,
          referrer: pageOf(tab),
        },
        sender,
      ),
    );
    if (!found.ok || !found.lookup?.choices?.length) {
      return { ok: false, error: found.error || "无法解析视频画质" };
    }
    const picked = await pick(
      {
        token: found.lookup.token,
        choice: 0,
        subtitles: Boolean(message.subtitles),
      },
      sender,
    );
    return picked.ok
      ? { ok: true, title: found.lookup.title || item.name }
      : { ok: false, error: picked.error || "无法加入下载队列" };
  }
  const reply = await native({
    type: "video",
    url: pageOf(tab),
    page: pageOf(tab),
    src: item.url,
    title: message.title || tab.title,
    referer: pageOf(tab),
    origin: item.sent?.origin || "",
    cookie: await cookieHeader(item.url),
    user_agent: navigator.userAgent,
    mime: item.mime || "",
    t: Date.now(),
  });
  return reply.ok ? { ok: true, title: item.name } : { ok: false, error: reply.error };
}

ext.runtime.onMessage.addListener((message, sender, respond) => {
  if (!message || sender.id !== ext.runtime.id) return false;
  (async () => {
    switch (message.type) {
      case "state":
      case "rules":
        return state();
      case "video-lookup":
        return lookup(message, sender);
      case "video-poll":
        return poll(message);
      case "video-pick":
        return pick(message, sender);
      case "video":
        return native({
          type: "video",
          url: pageOf(sender.tab),
          page: message.url || pageOf(sender.tab),
          src: message.src || "",
          title: message.title || sender.tab?.title,
          referer: pageOf(sender.tab),
          origin: message.origin || "",
          cookie: await cookieHeader(message.src || pageOf(sender.tab)),
          user_agent: navigator.userAgent,
          t: Date.now(),
        });
      case "media":
        return { ok: true, media: await listMedia(message.tab) };
      case "media-download":
        return mediaDownload(
          message,
          Number.isInteger(message.tabId)
            ? { tab: await ext.tabs.get(message.tabId) }
            : sender,
        );
      case "open":
        return native({ type: "open" });
      case "paused":
        await ext.storage.local.set({ paused: Boolean(message.paused) });
        return { ok: true };
      default:
        return { ok: false, error: "未知请求" };
    }
  })().then(respond, (error) => respond({ ok: false, error: String(error) }));
  return true;
});

ext.runtime.onInstalled.addListener(() => {
  try {
    ext.contextMenus?.create({
      id: "volant-video",
      title: "用 Volant 下载视频",
      contexts: ["video", "audio", "link"],
    });
  } catch {}
  state().catch(() => {});
});

ext.contextMenus?.onClicked.addListener(async (info, tab) => {
  if (info.menuItemId !== "volant-video" || !tab) return;
  const url = info.srcUrl || info.linkUrl || "";
  const sender = { tab };
  const src = MANIFEST.test(url) || MEDIA_FILE.test(url) ? url : bestTrackedSource(tab.id);
  const found = await awaitLookup(
    await lookup(
      { url: info.pageUrl || tab.url, src, title: tab.title, referrer: info.pageUrl },
      sender,
    ),
  );
  if (!found.ok || !found.lookup?.choices?.length) {
    statusTitle = found.error || "无法解析视频";
    showBadge("!");
    return;
  }
  const reply = await pick({ token: found.lookup.token, choice: 0 }, sender);
  statusTitle = reply.ok ? "视频已加入 Volant" : reply.error || "加入队列失败";
  showBadge(reply.ok ? "" : "!");
});

ext.runtime.onStartup.addListener(() => state().catch(() => {}));
showBadge("");
