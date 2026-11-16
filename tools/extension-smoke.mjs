import { spawn } from "node:child_process";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import http from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const extension = path.join(root, "extension");
const profile = path.join(root, "target", "extension-smoke-profile");
const chrome = "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe";
const port = 9327;
const testPage = `<!doctype html>
<html lang="zh-CN"><meta charset="utf-8">
<style>
  html,body{margin:0;background:#f3f4f6;font:14px "Segoe UI"}
  main{max-width:820px;margin:40px auto}
  video{display:block;width:720px;height:405px;background:#12161b}
</style>
<main><h1>Volant extension smoke test</h1><video controls></video></main>
</html>`;

const mime = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
};

const server = http.createServer(async (request, response) => {
  try {
    const url = new URL(request.url, "http://127.0.0.1");
    if (url.pathname === "/") {
      response.writeHead(200, { "content-type": mime[".html"] });
      response.end(testPage);
      return;
    }
    const relative = decodeURIComponent(url.pathname).replace(/^\/+/, "");
    const file = path.resolve(extension, relative);
    if (!file.startsWith(extension)) throw new Error("invalid path");
    const body = await readFile(file);
    response.writeHead(200, {
      "content-type": mime[path.extname(file)] || "application/octet-stream",
    });
    response.end(body);
  } catch {
    response.writeHead(404);
    response.end("not found");
  }
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const pageUrl = `http://127.0.0.1:${server.address().port}/`;

await rm(profile, { recursive: true, force: true });
await mkdir(profile, { recursive: true });
const browser = spawn(
  chrome,
  [
    "--no-first-run",
    "--no-default-browser-check",
    "--window-size=1100,820",
    `--remote-debugging-port=${port}`,
    `--user-data-dir=${profile}`,
    pageUrl,
  ],
  { stdio: "ignore", windowsHide: true },
);

let socket;
try {
  let target;
  for (let attempt = 0; attempt < 80; attempt++) {
    try {
      const targets = await fetch(`http://127.0.0.1:${port}/json/list`).then((r) => r.json());
      target = targets.find((item) => item.type === "page" && item.url.startsWith("http://"));
      if (target) break;
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  if (!target) throw new Error("Chrome page target did not start");

  socket = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    socket.addEventListener("open", resolve, { once: true });
    socket.addEventListener("error", reject, { once: true });
  });
  let sequence = 0;
  const waiting = new Map();
  socket.addEventListener("message", (event) => {
    const message = JSON.parse(event.data);
    if (!message.id) return;
    const pending = waiting.get(message.id);
    if (!pending) return;
    waiting.delete(message.id);
    if (message.error) pending.reject(new Error(message.error.message));
    else pending.resolve(message.result);
  });
  const call = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = ++sequence;
      waiting.set(id, { resolve, reject });
      socket.send(JSON.stringify({ id, method, params }));
    });

  const apiStub = `(() => {
    const deps = {
      ytdlp: { ready: true, managed: true },
      ffmpeg: { ready: true, managed: true },
      jsRuntime: { ready: true, managed: false },
      ready: true,
      canInstall: true
    };
    globalThis.chrome = globalThis.chrome || {};
    globalThis.chrome.runtime = {
      id: "volant-smoke",
      getManifest: () => ({ version: "0.6.21" }),
      sendMessage: async (message) => {
        if (message?.type === "state") {
          return { connected: true, app: "0.6.21", videoLookup: true, videoDependencies: deps };
        }
        if (message?.type === "media") return { ok: true, media: [] };
        if (message?.type === "video-lookup") return {
          ok: true,
          lookup: {
            state: "ready",
            token: "smoke",
            page: location.href,
            title: "Smoke video",
            duration: 125,
            choices: [
              { index: 0, label: "1080p · H.264", ext: "MP4", size: 12582912, height: 1080, fps: 30, codec: "H.264", audio: false },
              { index: 1, label: "720p · H.264", ext: "MP4", size: 7340032, height: 720, fps: 30, codec: "H.264", audio: false },
              { index: 2, label: "仅音频 · M4A", ext: "M4A", size: 1048576, audio: true }
            ]
          }
        };
        return { ok: true };
      }
    };
    globalThis.chrome.storage = {
      local: {
        get: async (defaults) => defaults || {},
        set: async () => {}
      }
    };
    globalThis.chrome.tabs = { query: async () => [] };
    const attachShadow = Element.prototype.attachShadow;
    Element.prototype.attachShadow = function (options) {
      return attachShadow.call(this, { ...options, mode: "open" });
    };
  })();`;

  await call("Page.enable");
  await call("Runtime.enable");
  const exceptions = [];
  socket.addEventListener("message", (event) => {
    const message = JSON.parse(event.data);
    if (message.method === "Runtime.exceptionThrown") {
      const details = message.params.exceptionDetails;
      exceptions.push(details?.exception?.description || details?.text || "runtime exception");
    }
  });
  await call("Page.addScriptToEvaluateOnNewDocument", { source: apiStub });
  await call("Runtime.evaluate", { expression: apiStub });
  let pageReady;
  for (let attempt = 0; attempt < 80; attempt++) {
    pageReady = await call("Runtime.evaluate", {
      expression: "document.readyState !== 'loading' && Boolean(document.querySelector('video'))",
      returnByValue: true,
    });
    if (pageReady.result?.value) break;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  if (!pageReady.result?.value) {
    throw new Error(`test page did not finish loading: ${JSON.stringify(pageReady.result)}`);
  }
  const content = await readFile(path.join(extension, "video.js"), "utf8");
  const injected = await call("Runtime.evaluate", { expression: content });
  if (injected.exceptionDetails) {
    throw new Error(
      `video script threw: ${injected.exceptionDetails.exception?.description || injected.exceptionDetails.text}`,
    );
  }
  let mounted;
  for (let attempt = 0; attempt < 40; attempt++) {
    mounted = await call("Runtime.evaluate", {
      expression:
        "({host:document.querySelectorAll('volant-video-affordance').length,video:document.querySelectorAll('video').length,bridge:Boolean(globalThis.__volantVideoBridge)})",
      returnByValue: true,
    });
    if (mounted.result?.value?.host && mounted.result.value.video) break;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  if (!mounted.result?.value?.host || !mounted.result.value.video) {
    throw new Error(`video affordance did not mount: ${JSON.stringify(mounted.result)} ${exceptions}`);
  }
  const rect = await call("Runtime.evaluate", {
    expression:
      "(()=>{const r=document.querySelector('video').getBoundingClientRect();return {x:r.x,y:r.y,w:r.width,h:r.height}})()",
    returnByValue: true,
  });
  const box = rect.result.value;
  if (!box || !box.w || !box.h) {
    throw new Error(`video bounds unavailable: ${JSON.stringify(rect.result)} ${exceptions}`);
  }
  await call("Emulation.setDeviceMetricsOverride", {
    width: 1000,
    height: 620,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await call("Input.dispatchMouseEvent", { type: "mouseMoved", x: 1, y: 1 });
  await call("Input.dispatchMouseEvent", {
    type: "mouseMoved",
    x: box.x + box.w - 30,
    y: box.y + 20,
  });
  await call("Runtime.evaluate", {
    expression: `(()=>{const v=document.querySelector('video');v.dispatchEvent(new PointerEvent('pointermove',{bubbles:true,clientX:${box.x + box.w - 30},clientY:${box.y + 20}}));})()`,
  });
  await new Promise((resolve) => setTimeout(resolve, 350));
  let shot = await call("Page.captureScreenshot", { format: "png" });
  await writeFile(
    path.join(root, "target", "extension-video-overlay.png"),
    Buffer.from(shot.data, "base64"),
  );
  await call("Runtime.evaluate", {
    expression:
      "document.querySelector('volant-video-affordance').shadowRoot.querySelector('.more').click()",
  });
  await new Promise((resolve) => setTimeout(resolve, 350));
  const menuState = await call("Runtime.evaluate", {
    expression: `(()=>{
      const root=document.querySelector('volant-video-affordance').shadowRoot;
      const surface=root.querySelector('.surface');
      const menu=root.querySelector('.menu');
      const rect=menu.getBoundingClientRect();
      return {shown:surface.dataset.show,up:surface.dataset.up,hidden:menu.hidden,
        top:rect.top,bottom:rect.bottom,height:rect.height,viewport:innerHeight};
    })()`,
    returnByValue: true,
  });
  const opened = menuState.result?.value;
  if (
    !opened ||
    opened.shown !== "1" ||
    opened.hidden ||
    opened.top < 0 ||
    opened.bottom > opened.viewport
  ) {
    throw new Error(`menu did not fit the viewport: ${JSON.stringify(menuState.result)}`);
  }
  shot = await call("Page.captureScreenshot", { format: "png" });
  await writeFile(
    path.join(root, "target", "extension-video-menu.png"),
    Buffer.from(shot.data, "base64"),
  );
  await call("Input.dispatchMouseEvent", {
    type: "mousePressed",
    x: 10,
    y: 10,
    button: "left",
    clickCount: 1,
  });
  await call("Input.dispatchMouseEvent", {
    type: "mouseReleased",
    x: 10,
    y: 10,
    button: "left",
    clickCount: 1,
  });
  await new Promise((resolve) => setTimeout(resolve, 2200));
  shot = await call("Page.captureScreenshot", { format: "png" });
  await writeFile(
    path.join(root, "target", "extension-video-overlay-hidden.png"),
    Buffer.from(shot.data, "base64"),
  );
  await call("Input.dispatchMouseEvent", {
    type: "mouseMoved",
    x: box.x + box.w / 2,
    y: box.y + box.h / 2,
  });
  await new Promise((resolve) => setTimeout(resolve, 350));
  shot = await call("Page.captureScreenshot", { format: "png" });
  await writeFile(
    path.join(root, "target", "extension-video-overlay-restored.png"),
    Buffer.from(shot.data, "base64"),
  );

  await call("Emulation.setDeviceMetricsOverride", {
    width: 304,
    height: 620,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await call("Page.navigate", {
    url: `http://127.0.0.1:${server.address().port}/popup.html`,
  });
  await new Promise((resolve) => setTimeout(resolve, 900));
  shot = await call("Page.captureScreenshot", { format: "png" });
  await writeFile(
    path.join(root, "target", "extension-popup-dark.png"),
    Buffer.from(shot.data, "base64"),
  );
  console.log("extension render smoke screenshots written");
} finally {
  socket?.close();
  browser.kill();
  server.close();
}
