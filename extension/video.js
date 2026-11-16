// The video affordance on web pages.
//
// It follows the player, offers the best quality in one click, and opens a
// compact quality menu for other video/sound choices. All visual values mirror
// Volant's Current design tokens; the host page cannot style the closed shadow UI.

(() => {
  const previous = globalThis.__volantVideoBridge;
  if (previous && typeof previous.alive === "function" && previous.alive()) return;
  previous?.stop?.();

  const ext = globalThis.browser ?? globalThis.chrome;
  const MIN_WIDTH = 200;
  const MIN_HEIGHT = 112;
  const IDLE_MS = 2000;
  const LOOKUP_TIMEOUT = 60_000;
  const POLL_MS = 1200;
  const controls = new Set();
  const dismissed = new WeakSet();
  let menuOpen = false;
  let savedSubtitles = false;

  ext.storage.local
    .get({ videoSubtitles: false })
    .then((value) => {
      savedSubtitles = Boolean(value.videoSubtitles);
    })
    .catch(() => {});

  const host = document.createElement("volant-video-affordance");
  host.setAttribute("data-volant-ui", "video");
  const shadow = host.attachShadow({ mode: "closed" });
  const sheet = new CSSStyleSheet();
  sheet.replaceSync(`
    :host { all: initial; }
    *, *::before, *::after { box-sizing: border-box; }
    .surface {
      --paper: #15191f;
      --subtle: #12161b;
      --hover: #1b2128;
      --sunken: #171c22;
      --ink: #edf1f5;
      --muted: #a2acb6;
      --faint: #87929c;
      --rule: #222830;
      --strong: #34404c;
      --current: #7299ff;
      --current-soft: #202832;
      --danger: #ff8078;
      position: fixed;
      z-index: 2147483647;
      display: flex;
      align-items: flex-start;
      gap: 5px;
      color: var(--ink);
      font: 400 11.5px/1.3 "Segoe UI Variable Text", "Segoe UI", "Microsoft YaHei UI", sans-serif;
      pointer-events: none;
      visibility: hidden;
      opacity: 0;
      transform: translateY(4px);
      transition:
        opacity 140ms ease-out,
        transform 140ms ease-out,
        visibility 0s linear 140ms;
    }
    .surface[data-show="1"] {
      visibility: visible;
      opacity: 1;
      transform: translateY(0);
      pointer-events: auto;
      transition-delay: 0s;
    }
    button {
      appearance: none;
      margin: 0;
      font: inherit;
      color: inherit;
      cursor: pointer;
    }
    .bar {
      display: flex;
      align-items: stretch;
      height: 32px;
      border: 1px solid var(--strong);
      border-radius: 7px;
      background: var(--paper);
      box-shadow: 0 3px 12px rgba(14, 19, 24, .14);
      overflow: hidden;
    }
    .go {
      display: flex;
      align-items: center;
      gap: 7px;
      min-width: 102px;
      padding: 0 10px;
      border: 0;
      border-right: 1px solid var(--rule);
      background: transparent;
      color: var(--ink);
      font-weight: 580;
      white-space: nowrap;
    }
    .go:hover { background: var(--hover); }
    .go:active { background: var(--sunken); }
    .more {
      display: grid;
      place-items: center;
      width: 31px;
      border: 0;
      background: transparent;
      color: var(--muted);
    }
    .more:hover { background: var(--hover); color: var(--ink); }
    .close {
      display: grid;
      place-items: center;
      width: 29px;
      border: 0;
      border-left: 1px solid var(--rule);
      background: transparent;
      color: var(--faint);
    }
    .close:hover { background: var(--hover); color: var(--ink); }
    .go:focus-visible, .more:focus-visible, .close:focus-visible, .choice:focus-visible,
    .retry:focus-visible, .open-app:focus-visible, .check input:focus-visible {
      outline: 2px solid var(--current);
      outline-offset: -2px;
    }
    .go[disabled], .more[disabled], .choice[disabled] {
      cursor: wait;
      opacity: .55;
    }
    .icon { width: 13px; height: 13px; flex: none; }
    .menu {
      position: absolute;
      top: 37px;
      right: 0;
      display: flex;
      flex-direction: column;
      width: min(310px, calc(100vw - 24px));
      max-height: calc(100vh - 48px);
      border: 1px solid var(--strong);
      border-radius: 10px;
      background: var(--paper);
      box-shadow: 0 18px 52px rgba(14, 19, 24, .18), 0 2px 8px rgba(14, 19, 24, .08);
      overflow: hidden;
    }
    .surface[data-up="1"] .menu { top: auto; bottom: 37px; }
    .menu[hidden] { display: none; }
    .head {
      flex: none;
      padding: 13px 14px 11px;
      border-bottom: 1px solid var(--rule);
    }
    .title {
      margin: 0;
      overflow: hidden;
      color: var(--ink);
      font-size: 12.5px;
      font-weight: 610;
      line-height: 1.35;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .caption {
      margin-top: 3px;
      color: var(--muted);
      font-size: 11px;
    }
    .list {
      min-height: 0;
      max-height: 330px;
      overflow: auto;
      overscroll-behavior: contain;
      scrollbar-color: var(--strong) transparent;
      scrollbar-width: thin;
    }
    .group {
      padding: 7px 14px 5px;
      color: var(--faint);
      font-size: 10.5px;
    }
    .choice {
      display: grid;
      grid-template-columns: 1fr auto;
      gap: 12px;
      width: 100%;
      min-height: 42px;
      padding: 8px 14px;
      border: 0;
      border-top: 1px solid transparent;
      background: transparent;
      text-align: left;
    }
    .choice + .choice { border-top-color: var(--rule); }
    .choice:hover { background: var(--hover); }
    .choice:active { background: var(--current-soft); }
    .choice-name {
      overflow: hidden;
      color: var(--ink);
      font-weight: 540;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .choice-meta {
      margin-top: 2px;
      color: var(--muted);
      font-size: 10.5px;
    }
    .choice-size {
      align-self: center;
      color: var(--muted);
      font: 400 10.5px/1 "Cascadia Mono", Consolas, monospace;
      font-variant-numeric: tabular-nums;
      white-space: nowrap;
    }
    .skeleton {
      height: 42px;
      border-top: 1px solid var(--rule);
      background: var(--sunken);
      animation: breathe 1.1s ease-in-out infinite alternate;
    }
    @keyframes breathe { to { opacity: .48; } }
    .foot {
      flex: none;
      display: flex;
      align-items: center;
      justify-content: space-between;
      min-height: 42px;
      padding: 7px 12px 7px 14px;
      border-top: 1px solid var(--rule);
      background: var(--subtle);
    }
    .check {
      display: flex;
      align-items: center;
      gap: 7px;
      color: var(--muted);
      cursor: pointer;
      user-select: none;
    }
    .check input { width: 14px; height: 14px; margin: 0; accent-color: var(--current); }
    .foot-actions { display: flex; align-items: center; gap: 5px; }
    .retry {
      min-height: 28px;
      padding: 0 8px;
      border: 0;
      border-radius: 5px;
      background: transparent;
      color: var(--current);
      font-weight: 560;
    }
    .retry:hover { background: var(--current-soft); }
    .open-app {
      min-height: 28px;
      padding: 0 9px;
      border: 1px solid var(--strong);
      border-radius: 5px;
      background: var(--paper);
      color: var(--ink);
      font-weight: 560;
    }
    .open-app:hover { background: var(--hover); }
    .error {
      padding: 14px;
      color: var(--danger);
      line-height: 1.45;
    }
    @media (prefers-reduced-motion: reduce) {
      .surface, .skeleton { transition: none; animation: none; }
    }
  `);
  shadow.adoptedStyleSheets = [sheet];

  const surface = document.createElement("div");
  surface.className = "surface";
  surface.dataset.show = "0";
  surface.innerHTML = `
    <div class="bar">
      <button class="go" type="button" aria-label="用 Volant 下载最佳画质">
        <svg class="icon" viewBox="0 0 16 16" aria-hidden="true">
          <path d="M8 1.75v7.5m0 0 3-3m-3 3-3-3M2.25 11.25v1.5c0 .83.67 1.5 1.5 1.5h8.5c.83 0 1.5-.67 1.5-1.5v-1.5"
            fill="none" stroke="currentColor" stroke-width="1.45" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
        <span class="label">下载视频</span>
      </button>
      <button class="more" type="button" aria-label="选择画质" aria-expanded="false">
        <svg class="icon" viewBox="0 0 16 16" aria-hidden="true">
          <path d="m4 6 4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.5"
            stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
      </button>
      <button class="close" type="button" aria-label="在此视频上隐藏下载按钮" title="在此视频上隐藏">
        <svg class="icon" viewBox="0 0 16 16" aria-hidden="true">
          <path d="m4.5 4.5 7 7m0-7-7 7" fill="none" stroke="currentColor" stroke-width="1.35"
            stroke-linecap="round"/>
        </svg>
      </button>
    </div>
    <section class="menu" role="dialog" aria-label="视频下载选项" hidden>
      <header class="head">
        <h2 class="title">视频画质</h2>
        <div class="caption">正在读取可用格式…</div>
      </header>
      <div class="list"><div class="skeleton"></div><div class="skeleton"></div></div>
      <footer class="foot">
        <label class="check"><input type="checkbox"> 同时保存字幕</label>
        <span class="foot-actions">
          <button class="retry" type="button" hidden>重试</button>
          <button class="open-app" type="button">打开 Volant</button>
        </span>
      </footer>
    </section>
  `;
  shadow.append(surface);
  (document.fullscreenElement || document.documentElement).append(host);

  const go = shadow.querySelector(".go");
  const more = shadow.querySelector(".more");
  const close = shadow.querySelector(".close");
  const label = shadow.querySelector(".label");
  const menu = shadow.querySelector(".menu");
  const title = shadow.querySelector(".title");
  const caption = shadow.querySelector(".caption");
  const list = shadow.querySelector(".list");
  const subtitles = shadow.querySelector(".check input");
  const retry = shadow.querySelector(".retry");
  const openApp = shadow.querySelector(".open-app");
  subtitles.checked = savedSubtitles;
  subtitles.addEventListener("change", () => {
    savedSubtitles = subtitles.checked;
    ext.storage.local.set({ videoSubtitles: savedSubtitles }).catch(() => {});
  });

  let active = null;
  let hideTimer = 0;
  let lookup = null;
  let lookupKey = "";

  function pageFor(video) {
    try {
      const url = new URL(location.href);
      const hostname = url.hostname.replace(/^(www|m|music)\./, "");
      if (hostname === "youtu.be") {
        const id = url.pathname.split("/")[1];
        if (id) return `https://www.youtube.com/watch?v=${id}`;
      }
      if (hostname === "youtube.com" || hostname === "youtube-nocookie.com") {
        const id =
          url.searchParams.get("v") ||
          document.querySelector("ytd-watch-flexy")?.getAttribute("video-id") ||
          document.querySelector("#movie_player")?.getAttribute("video-id");
        if (/^[\w-]{6,32}$/.test(id || "")) return `https://www.youtube.com/watch?v=${id}`;
      }
      const article = video.closest("article, [role='article'], .post, .feed-item");
      const own = article?.querySelector(
        "a[data-testid='timestamp'], a[href*='/status/'], a[href*='/posts/'], time a[href]",
      );
      if (own && /^https?:/i.test(own.href)) return own.href;
      return location.href;
    } catch {
      return location.href;
    }
  }

  function sourceFor(video) {
    const source = video.currentSrc || video.src || "";
    return /^(https?:|blob:|mediastream:)/i.test(source) ? source : "";
  }

  function send(message) {
    try {
      return ext.runtime.sendMessage(message).catch((error) => ({
        ok: false,
        error: String(error?.message || error),
      }));
    } catch (error) {
      return Promise.resolve({ ok: false, error: String(error?.message || error) });
    }
  }

  function sizeText(bytes) {
    if (!bytes || bytes <= 0) return "";
    const units = [
      [1 << 30, "GB"],
      [1 << 20, "MB"],
      [1 << 10, "KB"],
    ];
    const [unit, name] = units.find(([size]) => bytes >= size) || [1, "B"];
    return `${(bytes / unit).toFixed(unit === 1 ? 0 : 1)} ${name}`;
  }

  function show() {
    if (!active) return;
    clearTimeout(hideTimer);
    const protectedVideo = Boolean(active.mediaKeys);
    go.disabled = protectedVideo;
    more.disabled = protectedVideo;
    if (protectedVideo) label.textContent = "受保护视频";
    else if (lookup?.state === "ready") {
      label.textContent =
        lookup.choices?.find((choice) => !choice.audio && choice.height)?.label || "下载视频";
    } else if (lookup?.state === "pending") label.textContent = "查找画质…";
    else label.textContent = "下载视频";
    place();
    surface.dataset.show = "1";
    hideTimer = setTimeout(() => {
      if (!menuOpen && shadow.activeElement !== go && shadow.activeElement !== more) hide();
    }, IDLE_MS);
  }

  function hide() {
    if (menuOpen) return;
    if (shadow.activeElement && active?.isConnected) {
      active.focus({ preventScroll: true });
    }
    surface.dataset.show = "0";
  }

  function place() {
    if (!active?.isConnected) return;
    const box = active.getBoundingClientRect();
    if (box.width < 80 || box.height < 40) {
      surface.dataset.show = "0";
      return;
    }
    const right = Math.max(8, Math.min(innerWidth - 8, innerWidth - box.right + 10));
    const top = Math.max(8, Math.min(innerHeight - 40, box.top + 10));
    surface.style.right = `${right}px`;
    surface.style.top = `${top}px`;
    surface.style.bottom = "auto";
    menu.style.removeProperty("max-height");
    const opening = menuOpen && !menu.hidden;
    const above = opening && top + 37 + menu.offsetHeight > innerHeight - 8;
    surface.dataset.up = above ? "1" : "0";
    if (opening) {
      const available = Math.max(120, (above ? top : innerHeight - top) - 45);
      menu.style.maxHeight = `${available}px`;
    }
  }

  function setError(error) {
    go.disabled = false;
    more.disabled = false;
    label.textContent = "下载视频";
    caption.textContent = error || "无法解析这个视频";
    list.replaceChildren();
    const box = document.createElement("div");
    box.className = "error";
    box.textContent = error || "无法解析这个视频";
    list.append(box);
    retry.hidden = false;
    place();
  }

  function setPending() {
    go.disabled = false;
    more.disabled = false;
    label.textContent = "查找画质…";
    caption.textContent = "正在读取可用格式…";
    retry.hidden = true;
    list.replaceChildren();
    for (let i = 0; i < 3; i++) {
      const row = document.createElement("div");
      row.className = "skeleton";
      list.append(row);
    }
    place();
  }

  function choiceRow(choice, group) {
    const button = document.createElement("button");
    button.className = "choice";
    button.type = "button";
    button.dataset.index = String(choice.index);
    button.dataset.group = group;
    const what = document.createElement("span");
    const name = document.createElement("span");
    name.className = "choice-name";
    name.textContent = choice.label;
    const meta = document.createElement("span");
    meta.className = "choice-meta";
    const ext = choice.ext && choice.ext !== "自动" ? choice.ext : "";
    meta.textContent = [
      ext,
      choice.codec,
      choice.range,
      choice.fps ? `${Math.round(choice.fps)} fps` : "",
      choice.kbps ? `${choice.kbps} kbps` : "",
    ]
      .filter(Boolean)
      .join(" · ");
    what.append(name, meta);
    const size = document.createElement("span");
    size.className = "choice-size";
    size.textContent = `${choice.approx && choice.size ? "~" : ""}${sizeText(choice.size)}`;
    button.append(what, size);
    button.addEventListener("click", () => choose(choice));
    return button;
  }

  function renderMenu(data) {
    if (!data?.choices?.length) {
      setError(data?.error || "没有可下载的画质");
      return;
    }
    title.textContent = data.title || document.title || "视频";
    caption.textContent = data.duration
      ? `${clock(data.duration)} · ${data.choices.length} 个选项`
      : `${data.choices.length} 个选项`;
    list.replaceChildren();
    const videos = data.choices.filter((choice) => !choice.audio);
    const audio = data.choices.filter((choice) => choice.audio);
    for (const [name, choices] of [
      ["视频", videos],
      ["音频", audio],
    ]) {
      if (!choices.length) continue;
      const group = document.createElement("div");
      group.className = "group";
      group.textContent = name;
      list.append(group);
      for (const choice of choices) list.append(choiceRow(choice, name));
    }
    retry.hidden = true;
    label.textContent = videos.find((choice) => choice.height)?.label || "最佳画质";
    place();
  }

  function clock(seconds) {
    const value = Math.round(seconds);
    const hours = Math.floor(value / 3600);
    const minutes = Math.floor(value / 60) % 60;
    const secs = value % 60;
    return hours
      ? `${hours}:${String(minutes).padStart(2, "0")}:${String(secs).padStart(2, "0")}`
      : `${minutes}:${String(secs).padStart(2, "0")}`;
  }

  let lookupPromise = null;

  async function followLookup(first, key, video) {
    let data = first;
    const token = data?.token;
    const deadline = Date.now() + LOOKUP_TIMEOUT;
    while (data?.state === "pending" && token && Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, POLL_MS));
      if (key !== lookupKey || !video.isConnected) return null;
      const reply = await send({ type: "video-poll", token });
      if (!reply?.ok) {
        data = { state: "failed", error: reply?.error || "无法继续读取画质" };
        break;
      }
      data = reply.lookup || { state: "gone" };
    }
    if (data?.state === "pending") {
      data = { state: "failed", error: "读取画质超时，请重试。" };
    }
    lookup = data;
    lookupKey = key;
    if (active && `${pageFor(active)}\n${sourceFor(active)}` === key) {
      if (data?.state === "ready" && data.choices?.length) renderMenu(data);
      else setError(data?.error || "没有可下载的画质");
    }
    return data;
  }

  async function resolve(force = false) {
    if (!active) return null;
    const video = active;
    const key = `${pageFor(video)}\n${sourceFor(video)}`;
    if (!force && lookup && lookupKey === key) {
      if (lookup.state === "ready") return lookup;
      if (lookup.state !== "pending") return null;
    }
    if (!force && lookupPromise && lookupKey === key) return lookupPromise;
    lookup = { state: "pending" };
    lookupKey = key;
    setPending();
    lookupPromise = (async () => {
      const reply = await send({
        type: "video-lookup",
        url: pageFor(video),
        src: sourceFor(video),
        title: document.title,
        referrer: location.href,
      });
      if (!reply?.ok || !reply.lookup) {
        lookup = { state: "failed", error: reply?.error || "无法解析这个视频" };
        lookupKey = key;
        if (active && `${pageFor(active)}\n${sourceFor(active)}` === key) {
          setError(lookup.error);
        }
        return null;
      }
      return followLookup(reply.lookup, key, video);
    })().finally(() => {
      lookupPromise = null;
    });
    return lookupPromise;
  }

  async function choose(choice) {
    if (!lookup?.token || lookup.state !== "ready" || !choice) return;
    more.disabled = true;
    go.disabled = true;
    caption.textContent = "正在加入下载队列…";
    const reply = await send({
      type: "video-pick",
      token: lookup.token,
      choice: choice.index,
      subtitles: savedSubtitles,
    });
    more.disabled = false;
    go.disabled = false;
    if (reply?.ok && reply.lookup?.state !== "failed") {
      caption.textContent = "已加入 Volant 下载队列";
      label.textContent = choice.label;
      setTimeout(() => {
        closeMenu(true);
      }, 900);
    } else {
      setError(reply?.error || "无法加入下载队列");
    }
  }

  async function downloadBest() {
    const data = await resolve();
    if (!data?.choices?.length) return;
    const best =
      data.choices.find((choice) => !choice.audio && choice.height) ||
      data.choices.find((choice) => !choice.audio) ||
      data.choices[0];
    await choose(best);
  }

  async function openMenu() {
    menuOpen = true;
    menu.hidden = false;
    more.setAttribute("aria-expanded", "true");
    show();
    await resolve();
    const first = list.querySelector(".choice");
    first?.focus({ preventScroll: true });
  }

  function closeMenu(restoreFocus = false) {
    const hadFocus = menu.contains(shadow.activeElement);
    menuOpen = false;
    menu.hidden = true;
    more.setAttribute("aria-expanded", "false");
    if (restoreFocus && hadFocus) more.focus({ preventScroll: true });
  }

  go.addEventListener("click", downloadBest);
  more.addEventListener("click", () => {
    if (menuOpen) closeMenu();
    else openMenu();
  });
  retry.addEventListener("click", () => resolve(true));
  openApp.addEventListener("click", () => send({ type: "open" }));
  close.addEventListener("click", () => {
    if (!active) return;
    dismissed.add(active);
    closeMenu();
    hide();
  });
  surface.addEventListener("pointerenter", show);
  surface.addEventListener("pointerleave", () => {
    if (!menuOpen) hide();
  });

  document.addEventListener(
    "pointerdown",
    (event) => {
      if (menuOpen && !event.composedPath().includes(host)) closeMenu();
    },
    true,
  );
  shadow.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      if (menuOpen) closeMenu(true);
      else hide();
      event.preventDefault();
      return;
    }
    if (!menuOpen || !["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    const items = [...list.querySelectorAll(".choice")];
    if (!items.length) return;
    const at = items.indexOf(shadow.activeElement);
    const next =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? items.length - 1
          : event.key === "ArrowDown"
            ? Math.min(items.length - 1, at + 1)
            : Math.max(0, at <= 0 ? items.length - 1 : at - 1);
    items[next].focus({ preventScroll: true });
    event.preventDefault();
    event.stopPropagation();
  });
  shadow.addEventListener("focusout", (event) => {
    if (menuOpen && !shadow.contains(event.relatedTarget)) closeMenu();
  });

  const observer = new MutationObserver(mountAll);
  observer.observe(document.documentElement, {
    childList: true,
    subtree: true,
    attributes: true,
    attributeFilter: ["src"],
  });

  function mount(video) {
    if (controls.has(video) || dismissed.has(video)) return;
    const rect = video.getBoundingClientRect();
    if (video.tagName === "VIDEO") {
      if (rect.width < MIN_WIDTH || rect.height < MIN_HEIGHT) return;
      if (video.muted && video.loop) return;
    } else if (!video.controls || rect.height < 24) {
      return;
    }
    controls.add(video);
    video.addEventListener("pointermove", () => {
      active = video;
      show();
    });
    video.addEventListener("pointerenter", () => {
      active = video;
      show();
      const key = `${pageFor(video)}\n${sourceFor(video)}`;
      setTimeout(() => {
        if (
          active === video &&
          video.isConnected &&
          !video.mediaKeys &&
          surface.dataset.show === "1" &&
          (!lookup || lookupKey !== key)
        ) {
          resolve();
        }
      }, 650);
    });
    video.addEventListener("pointerleave", () => {
      if (!menuOpen) hide();
    });
    video.addEventListener("focusin", () => {
      active = video;
      show();
    });
    video.addEventListener("loadedmetadata", () => {
      if (active === video) {
        lookup = null;
        lookupKey = "";
      }
    });
  }

  function mountAll() {
    for (const video of document.querySelectorAll("video, audio[controls]")) mount(video);
  }

  addEventListener("scroll", () => {
    if (surface.dataset.show === "1") place();
  }, { capture: true, passive: true });
  addEventListener("resize", () => {
    if (surface.dataset.show === "1") place();
  }, { passive: true });
  document.addEventListener("fullscreenchange", () => {
    (document.fullscreenElement || document.documentElement).append(host);
  });

  mountAll();
  globalThis.__volantVideoBridge = {
    alive: () => Boolean(ext.runtime?.id),
    stop() {
      observer.disconnect();
      host.remove();
      controls.clear();
    },
  };
})();
