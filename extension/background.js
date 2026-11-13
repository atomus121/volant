// Volant 的浏览器侧：Chrome 一决定「要下载了」，就把这件事交给桌面端。
//
// 只做三件事：拦住、拼上浏览器的身份（Cookie / Referer / UA）、通过 native
// messaging 递过去。真正的下载在 volant.exe 里跑，这里不碰网络也不碰磁盘。

const HOST = "com.volant.host";

// 交还浏览器的链接：失败时重新发起下载会被自己再拦一次，用这个集合放行一轮。
const passThrough = new Set();

async function enabled() {
  const { on } = await chrome.storage.local.get({ on: true });
  return on;
}

/** 递给 native host，成功返回空串，失败返回原因（给角标看）。 */
function send(payload) {
  return new Promise((resolve) => {
    let port;
    try {
      port = chrome.runtime.connectNative(HOST);
    } catch (e) {
      resolve(e.message || String(e));
      return;
    }
    let done = false;
    const finish = (why) => {
      if (done) return;
      done = true;
      try {
        port.disconnect();
      } catch {}
      resolve(why);
    };
    // host 不回话；断开就是它退出了（多半是没装清单）。
    port.onDisconnect.addListener(() =>
      finish(chrome.runtime.lastError?.message || "连接已断开")
    );
    try {
      port.postMessage(payload);
    } catch (e) {
      finish(e.message || String(e));
      return;
    }
    // 给 host 一点时间把这一帧转进管道，再主动收线（不让 service worker 挂着）。
    setTimeout(() => finish(""), 400);
  });
}

async function cookieHeader(url) {
  try {
    const list = await chrome.cookies.getAll({ url });
    return list.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch {
    return "";
  }
}

async function badged(message) {
  const text = message ? "!" : "";
  await chrome.action.setBadgeText({ text });
  await chrome.action.setTitle({
    title: message ? `Volant：${message}` : "Volant 下载捕获",
  });
}

chrome.downloads.onCreated.addListener(async (item) => {
  const url = item.finalUrl || item.url || "";
  if (!/^https?:/i.test(url)) return;
  if (passThrough.delete(url)) return; // 自己交还给浏览器的那一条
  if (!(await enabled())) return;

  // 先掐掉浏览器自己的下载，否则两个下载器会抢同一个文件。
  // cancel/erase 对已完成或已失效的下载会抛错，忽略即可。
  try {
    await chrome.downloads.cancel(item.id);
  } catch {}
  try {
    await chrome.downloads.erase({ id: item.id });
  } catch {}

  const why = await send({
    url,
    referer: item.referrer || "",
    cookie: await cookieHeader(url),
    user_agent: navigator.userAgent,
    t: Date.now(),
  });

  if (why) {
    // 桌面端没接住（没装 / 装错位置）：交还给浏览器，别让用户丢了这个文件。
    await badged(why);
    passThrough.add(url);
    chrome.downloads.download({ url });
  } else {
    await badged("");
  }
});
