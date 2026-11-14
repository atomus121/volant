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
  const IDLE_MS = 1500;
  const controls = new Set();
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
      --paper: #fff;
      --subtle: #f7f8fa;
      --hover: #f2f4f6;
      --sunken: #f3f5f7;
      --ink: #111417;
      --muted: #59626b;
      --faint: #657079;
      --rule: #e1e4e7;
      --strong: #cdd3d8;
      --current: #2f6bff;
      --current-soft: #edf2ff;
      --danger: #b3423d;
      position: fixed;
      z-index: 2147483647;
      display: flex;
      align-items: flex-start;
      gap: 5px;
      color: var(--ink);
      font: 400 11.5px/1.3 "Segoe UI Variable Text", "Segoe UI", "Microsoft YaHei UI", sans-serif;
      pointer-events: none;
      opacity: 0;
      transform: translateY(4px);
      transition: opacity 140ms ease-out, transform 140ms ease-out;
    }
    @media (prefers-color-scheme: dark) {
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
      }
    }
    .surface[data-show="1"] {
      opacity: 1;
      transform: translateY(0);
      pointer-events: auto;
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
    .go:focus-visible, .more:focus-visible, .choice:focus-visible,
    .retry:focus-visible, .check input:focus-visible {
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
      width: min(310px, calc(100vw - 24px));
      border: 1px solid var(--strong);
      border-radius: 10px;
      background: var(--paper);
      box-shadow: 0 18px 52px rgba(14, 19, 24, .18), 0 2px 8px rgba(14, 19, 24, .08);
      overflow: hidden;
    }
    .menu[hidden] { display: none; }
    .head {
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
    </div>
    <section class="menu" role="dialog" aria-label="视频下载选项" hidden>
      <header class="head">
        <h2 class="title">视频画质</h2>
        <div class="caption">正在读取可用格式…</div>
      </header>
      <div class="list"><div class="skeleton"></div><div class="skeleton"></div></div>
      <footer class="foot">
        <label class="check"><input type="checkbox"> 同时保存字幕</label>
        <button class="retry" type="button" hidden>重试</button>
      </footer>
    </section>
  `;
  shadow.append(surface);
  (document.fullscreenElement || document.documentElement).append(host);

  const go = shadow.querySelector(".go");
  const more = shadow.querySelector(".more");
  const label = shadow.querySelector(".label");
  const menu = shadow.querySelector(".menu");
  const title = shadow.querySelector(".title");
  const caption = shadow.querySelector(".caption");
  const list = shadow.querySelector(".list");
  const subtitles = shadow.querySelector(".check input");
  const retry = shadow.querySelector(".retry");
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
    place();
    surface.dataset.show = "1";
    hideTimer = setTimeout(() => {
      if (!menuOpen && shadow.activeElement !== go && shadow.activeElement !== more) hide();
    }, IDLE_MS);
  }

  function hide() {
    if (menuOpen) return;
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
    const bottom = Math.max(8, Math.min(innerHeight - 8, innerHeight - box.bottom + 10));
    surface.style.right = `${right}px`;
    surface.style.bottom = `${bottom}px`;
  }

  function setBusy(busy, text = "下载视频") {
    go.disabled = busy;
    more.disabled = busy;
    label.textContent = busy ? "正在解析…" : text;
  }

  function setError(error) {
    setBusy(false);
    caption.textContent = error || "无法解析这个视频";
    list.replaceChildren();
    const box = document.createElement("div");
    box.className = "error";
    box.textContent = error || "无法解析这个视频";
    list.append(box);
    retry.hidden = false;
  }

  function choiceRow(choice, group) {
    const button = document.createElement("button");
    button.className = "choice";
    button.type = "button";
    button.dataset.index = String(choice.index);
    button.dataset.group = group;
    button.setAttribute("role", "menuitem");
    const what = document.createElement("span");
    const name = document.createElement("span");
    name.className = "choice-name";
    name.textContent = choice.label;
    const meta = document.createElement("span");
    meta.className = "choice-meta";
    meta.textContent = [choice.ext, choice.fps ? `${Math.round(choice.fps)} fps` : ""]
      .filter(Boolean)
      .join(" · ");
    what.append(name, meta);
    const size = document.createElement("span");
    size.className = "choice-size";
    size.textContent = sizeText(choice.size);
    button.append(what, size);
    button.addEventListener("click", () => choose(choice));
    return button;
  }

  function renderMenu(result) {
    const data = result?.lookup;
    if (!data?.choices?.length) {
      setError(result?.error || "没有可下载的画质");
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
    label.textContent = videos[0]?.label || "最佳画质";
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

  async function resolve(force = false) {
    if (!active) return null;
    const key = `${pageFor(active)}\n${sourceFor(active)}`;
    if (!force && lookup && lookupKey === key) return lookup;
    setBusy(true);
    retry.hidden = true;
    caption.textContent = "正在读取可用格式…";
    list.replaceChildren();
    for (let i = 0; i < 3; i++) {
      const row = document.createElement("div");
      row.className = "skeleton";
      list.append(row);
    }
    const reply = await send({
      type: "video-lookup",
      url: pageFor(active),
      src: sourceFor(active),
      title: document.title,
      referrer: location.href,
    });
    setBusy(false);
    if (reply?.ok && reply.lookup) {
      lookup = reply.lookup;
      lookupKey = key;
      renderMenu(reply);
      return reply;
    }
    setError(reply?.error || "无法解析这个视频");
    return null;
  }

  async function choose(choice) {
    if (!lookup?.token || !choice) return;
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
    if (reply?.ok) {
      caption.textContent = "已加入 Volant 下载队列";
      label.textContent = choice.label;
      setTimeout(() => {
        menuOpen = false;
        menu.hidden = true;
        more.setAttribute("aria-expanded", "false");
        hide();
      }, 900);
    } else {
      setError(reply?.error || "无法加入下载队列");
    }
  }

  async function downloadBest() {
    const result = await resolve();
    const data = result?.lookup;
    if (!data?.choices?.length) return;
    const best = data.choices.find((choice) => !choice.audio) || data.choices[0];
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

  function closeMenu() {
    menuOpen = false;
    menu.hidden = true;
    more.setAttribute("aria-expanded", "false");
  }

  go.addEventListener("click", downloadBest);
  more.addEventListener("click", () => {
    if (menuOpen) closeMenu();
    else openMenu();
  });
  retry.addEventListener("click", () => resolve(true));
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

  const observer = new MutationObserver(mountAll);
  observer.observe(document.documentElement, {
    childList: true,
    subtree: true,
    attributes: true,
    attributeFilter: ["src"],
  });

  function mount(video) {
    if (controls.has(video)) return;
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
