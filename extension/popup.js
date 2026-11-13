const HOST = "com.volant.host";

const on = document.getElementById("on");
const state = document.getElementById("state");

chrome.storage.local.get({ on: true }).then((v) => (on.checked = v.on));
on.addEventListener("change", () =>
  chrome.storage.local.set({ on: on.checked })
);

// 探一下 host 装没装：连一次就断，能连上说明注册表跟清单都对。
function probeHost() {
  return new Promise((resolve) => {
    let port;
    try {
      port = chrome.runtime.connectNative(HOST);
    } catch (e) {
      resolve(e.message || String(e));
      return;
    }
    port.onDisconnect.addListener(() =>
      resolve(chrome.runtime.lastError?.message || "")
    );
    setTimeout(() => {
      try {
        port.disconnect();
      } catch {}
      resolve("");
    }, 300);
  });
}

probeHost().then((why) => {
  if (why) {
    state.className = "bad";
    state.textContent = `桌面端没接上：${why}`;
    state.title = "先跑 volant.exe --install-host";
  } else {
    state.textContent = "桌面端已连接";
  }
});
