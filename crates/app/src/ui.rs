//! 窗口与界面。单窗口、单列表、行内 hover 操作、底部详情、右侧设置。
//!
//! 这一层只做两件事：把 `UiEvent` 变成列表里的数字，把点击变成 `Cmd`。
//! 不碰网络，不碰引擎（PRD 的模块边界）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Key, Margin, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, TextEdit, pos2, vec2,
};
#[cfg(windows)]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::capture::{Capture, Listener};
use crate::clipboard::Watch;
use crate::control::{Cmd, Control, TorSource, UiEvent};
use crate::model::{
    Kind, MAX_CONNECTIONS_PER_TASK, MAX_PARALLEL_DOWNLOADS, Paths, Persisted, Row, Settings,
    Status, Store, ThemeMode, TorrentFile, disk_space, friendly_error, is_magnet, remove_leftovers,
    writable_download_dir,
};
use crate::theme::{self, Icon};

const DETAIL_SCROLL_MIN_H: f32 = 660.0;
const DETAIL_FOOTER_H: f32 = 140.0;
const CARD_H: f32 = 88.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HeadDensity {
    Full,
    Compact,
    Minimal,
}

#[derive(Clone, Copy)]
struct HeadLayout {
    density: HeadDensity,
    height: f32,
    title_y: f32,
    subtitle_y: Option<f32>,
    band_y: f32,
    band_h: f32,
}

pub struct Launch {
    /// 截一张图存到这个路径，然后退出。
    pub shot: Option<PathBuf>,
    /// 采样资源指标，写到这个路径，然后退出。
    pub metrics: Option<PathBuf>,
    pub seconds: f64,
    pub wait: Duration,
    /// 启动时直接加进队列的链接（测试/截图用）。
    pub adds: Vec<String>,
    /// 覆盖下载目录（测试/截图用，不写进配置）。
    pub dir: Option<PathBuf>,
    /// 启动就打开设置面板（截图用）。
    pub open_settings: bool,
    /// 启动就打开新建下载弹窗（截图/冒烟测试用）。
    pub open_add: bool,
    /// 启动就打开删除确认弹窗（截图/冒烟测试用）。
    pub open_delete: bool,
    /// 跑无窗口自检（添加 → 暂停 → 续传 → 完成），结果写到 `selftest_out`。
    pub selftest: Option<String>,
    pub selftest_out: Option<PathBuf>,
    pub selftest_dir: PathBuf,
    /// 覆盖窗口逻辑尺寸（截图验证「最小尺寸下不破版」用）。
    pub size: Option<[f32; 2]>,
    /// 覆盖设置里的主题（截图/指标要固定浅色或深色时用；设了就不再跟随系统）。
    pub theme: Option<ThemeMode>,
}

impl Launch {
    pub fn parse(args: impl Iterator<Item = String>) -> Launch {
        let mut l = Launch {
            shot: None,
            metrics: None,
            seconds: 12.0,
            wait: Duration::from_millis(1500),
            adds: Vec::new(),
            dir: None,
            open_settings: false,
            open_add: false,
            open_delete: false,
            selftest: None,
            selftest_out: None,
            selftest_dir: PathBuf::from("target/selftest"),
            size: None,
            theme: None,
        };
        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--shot" => l.shot = it.next().map(PathBuf::from),
                "--metrics-out" => l.metrics = it.next().map(PathBuf::from),
                "--seconds" => {
                    if let Some(v) = it.next() {
                        l.seconds = v.parse().unwrap_or(12.0);
                    }
                }
                "--wait" => {
                    if let Some(v) = it.next() {
                        l.wait = Duration::from_millis(v.parse().unwrap_or(1500));
                    }
                }
                "--add" => {
                    if let Some(v) = it.next() {
                        l.adds.push(v);
                    }
                }
                "--dir" => l.dir = it.next().map(PathBuf::from),
                "--open-settings" => l.open_settings = true,
                "--open-add" => l.open_add = true,
                "--open-delete" => l.open_delete = true,
                "--theme" => {
                    l.theme = match it.next().as_deref() {
                        Some("light") => Some(ThemeMode::Light),
                        Some("dark") => Some(ThemeMode::Dark),
                        _ => Some(ThemeMode::System),
                    }
                }
                "--selftest" => l.selftest = it.next(),
                "--selftest-out" => l.selftest_out = it.next().map(PathBuf::from),
                "--selftest-dir" => {
                    if let Some(v) = it.next() {
                        l.selftest_dir = PathBuf::from(v);
                    }
                }
                // `--size 520x340`：逻辑尺寸，验证最小窗口下布局不破。
                "--size" => {
                    if let Some(v) = it.next() {
                        let mut parts = v.split(['x', 'X']);
                        if let (Some(w), Some(h)) = (parts.next(), parts.next())
                            && let (Ok(w), Ok(h)) = (w.parse::<f32>(), h.parse::<f32>())
                        {
                            l.size = Some([w, h]);
                        }
                    }
                }
                _ => {}
            }
        }
        l
    }
}

/// 左栏的四个视图（§4）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Active,
    Done,
    Failed,
}

impl Filter {
    const ALL: [Filter; 4] = [Filter::All, Filter::Active, Filter::Done, Filter::Failed];

    fn title(self) -> &'static str {
        match self {
            Filter::All => "全部下载",
            Filter::Active => "进行中",
            Filter::Done => "已完成",
            Filter::Failed => "失败",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Filter::All => Icon::Queue,
            Filter::Active => Icon::Down,
            Filter::Done => Icon::Check,
            Filter::Failed => Icon::Alert,
        }
    }

    fn matches(self, row: &Row) -> bool {
        match self {
            Filter::All => true,
            Filter::Active => matches!(
                row.status,
                Status::Queued | Status::Probing | Status::Active | Status::Seeding
            ),
            Filter::Done => row.status == Status::Done,
            Filter::Failed => row.status == Status::Failed,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AddOrigin {
    Manual,
    Clipboard,
    Browser,
    TorrentDrop,
}

#[derive(Clone)]
enum AddPreview {
    Empty,
    Loading,
    Http {
        name: String,
        size: Option<u64>,
        content_type: Option<String>,
        final_url: String,
    },
    Torrent {
        name: String,
        total: u64,
        files: Vec<TorrentFile>,
        info_hash: String,
    },
    Failed(String),
}

#[derive(Clone)]
struct DraftTorrent {
    url: String,
    source: TorSource,
}

struct Adding {
    text: String,
    fresh: bool,
    origin: AddOrigin,
    /// 浏览器捕获带下来的身份；只对原始 URL 生效，用户改了地址就清掉。
    headers: Vec<(String, String)>,
    header_url: String,
    /// 每次预览单独领一个 id：旧响应晚到时按 id 丢弃。
    preview_id: u64,
    preview_for: String,
    preview_due: Instant,
    preview: AddPreview,
    torrent: Option<DraftTorrent>,
    checked: Vec<bool>,
}

struct Toast(String, Instant, bool);

/// 多文件种子的勾选状态：打开时占住整个列表区，选完就关（PRD 5.3 的扁平清单）。
struct Pick {
    id: u64,
    files: Vec<TorrentFile>,
    checked: Vec<bool>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeleteIntent {
    Task,
    Files,
}

struct DeletePrompt {
    id: u64,
    intent: DeleteIntent,
    dont_ask: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BulkKind {
    Pause,
    Resume,
}

#[derive(Debug)]
struct BulkAction {
    kind: BulkKind,
    ids: Vec<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EnterAction {
    OpenFile,
    OpenDetail,
}

impl BulkAction {
    fn label(&self) -> &'static str {
        match self.kind {
            BulkKind::Pause => "全部暂停",
            BulkKind::Resume => "全部继续",
        }
    }

    fn icon(&self) -> Icon {
        match self.kind {
            BulkKind::Pause => Icon::Pause,
            BulkKind::Resume => Icon::Play,
        }
    }
}

pub struct App {
    paths: Paths,
    store: Store,
    settings: Settings,
    rows: Vec<Row>,
    /// 全列表最近 60 秒的聚合速度，只用于概览折线图。
    speed_history: VecDeque<(Instant, f64)>,
    next_id: u64,
    selected: Option<u64>,
    filter: Filter,
    adding: Option<Adding>,
    settings_open: bool,
    /// 设置对话框打开的时刻：只用来给对话框算 140ms 的淡入（§7）。
    settings_at: Instant,
    /// 上一帧的对话框矩形：遮罩上的点击要用它区分「点空白处」和「点对话框内部」。
    dialog_rect: Rect,
    /// 设置内容区的实测高度（上一帧）。对话框要先定高再画，不能靠 ScrollArea 撑。
    settings_body_h: Option<f32>,
    dir_edit: String,
    control: Option<Control>,
    events: Receiver<UiEvent>,
    clip: Option<Watch>,
    clip_urls: Receiver<String>,
    /// 管道监听只为了活着：`Drop` 时它会叫醒阻塞的线程。
    _capture: Listener,
    captured: Receiver<Capture>,
    toast: Option<Toast>,
    /// 多文件种子等用户勾选文件时才有值。
    pick: Option<Pick>,
    delete_prompt: Option<DeletePrompt>,
    dirty: bool,
    /// 上次落盘的时刻：下载中每 250 ms 就有一条进度事件，不能每条都写盘。
    last_save: Instant,
    /// 上次问系统「现在是浅色还是深色」的时刻（设置选了跟随系统时用）。
    theme_poll: Instant,
    /// 磁盘剩余空间是慢速 I/O；连续缩放时不能每帧都问一次。
    disk_cache: Option<(PathBuf, Instant, Option<(u64, u64)>)>,
    /// 详情栏滑入的起点（§7 的 140 ms）。
    detail_at: Instant,
    /// 上一帧的选中行：用来区分「刚打开详情栏」和「换了另一行」。
    sel_prev: Option<u64>,
    /// 行的出生时刻：新任务 120 ms 淡入用，1 秒后清掉。
    born: HashMap<u64, Instant>,
    /// 已从列表移除、但任务还没停下来的行：等 `Removed` 事件到了再清残留文件。
    cleanup: Vec<(u64, PathBuf, bool)>,
    /// 跳过确认后加入的种子：解析完成直接开始，不再弹一次文件勾选。
    direct_torrents: HashSet<u64>,
    launch: Launch,
    shot_at: Instant,
    shot_state: u8,
    shot_flag: Arc<AtomicBool>,
    started: Instant,
    startup_ms: Arc<AtomicU64>,
    /// 只用于 `--metrics-out`：数一共画了多少帧，判断空闲时是否还在空转。
    frames: Arc<AtomicU64>,
    first_frame: bool,
    maximized: bool,
    #[cfg(windows)]
    window: Option<isize>,
    #[cfg(windows)]
    resize_drag: Option<ResizeDrag>,
}

/// 「跟随系统」时定期问一次注册表：Windows 的深浅色随时会被改，只在启动时读一次
/// 会让这个选项看起来没生效。非跟随模式直接返回，不花任何钱。
///
/// 这里**不**主动要重绘。原本每帧 `request_repaint_after(2 s)` 想让窗口不动时也能轮询，
/// 实测反而把空闲变成 40–58 fps、CPU 9–10%（476 帧/12 s ↔ 去掉后 33 帧、0% CPU）：
/// 空转的代价远大于「晚几秒变色」。界面本来就会为鼠标、下载进度重画，那时顺手读一次
/// 注册表就够；真闲置着没人看，晚变色无所谓。
fn follow_system(settings: &Settings, poll: &mut Instant, ctx: &egui::Context) {
    if settings.theme != ThemeMode::System {
        return;
    }
    if poll.elapsed() < std::time::Duration::from_millis(1900) {
        return;
    }
    *poll = Instant::now();
    if let Some(light) = theme::system_prefers_light()
        && light != theme::is_light()
    {
        theme::set_light(light);
        theme::apply(ctx);
    }
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        launch: Launch,
        started: Instant,
        startup_ms: Arc<AtomicU64>,
    ) -> App {
        let paths = Paths::resolve();
        crate::log::init(&paths.home);
        let store = Store::new(paths.state.clone());
        let mut saved = store.load();
        if let Some(dir) = launch.dir.clone() {
            saved.settings.dir = dir;
        }
        let fallback_dir = paths.home.join("downloads");
        let (download_dir, dir_note) = writable_download_dir(&saved.settings.dir, &fallback_dir);
        saved.settings.dir = download_dir;
        // 主题要在装完字体、知道设置之后才定：先读设置里的三档，再装样式。
        // `--theme` 是给截图/指标用的覆盖开关，设了就不再跟随系统。
        let mode = launch.theme.unwrap_or(saved.settings.theme);
        theme::install_fonts(&cc.egui_ctx);
        theme::set_light(theme::resolve(mode));
        theme::apply(&cc.egui_ctx);
        #[cfg(windows)]
        let window = cc
            .window_handle()
            .ok()
            .and_then(|handle| match handle.as_raw() {
                RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
                _ => None,
            });
        let (control, events) = Control::start(
            cc.egui_ctx.clone(),
            saved.settings.clone(),
            paths.hosts.clone(),
            fallback_dir,
        );
        let (clip, clip_urls) = Watch::start(cc.egui_ctx.clone(), saved.settings.clipboard);
        // 管道监听常开：它只是一个阻塞在 ConnectNamedPipe 上的线程，空闲不花 CPU。
        let (capture, captured) = Listener::start(cc.egui_ctx.clone());
        let dir_edit = saved.settings.dir.to_string_lossy().to_string();
        let initial_w = launch.size.map_or(1180.0, |size| size[0]);
        crate::log::line(&format!(
            "配置 {} 设置 目录={} 同时={} 连接={} 限速={:?} 做种={} 分享率={} 任务={}",
            paths.home.display(),
            saved.settings.dir.display(),
            saved.settings.limit,
            saved.settings.connections,
            saved.settings.rate,
            saved.settings.seed,
            saved.settings.seed_ratio,
            saved.rows.len()
        ));

        let mut app = App {
            paths,
            store,
            settings: saved.settings,
            rows: saved.rows,
            speed_history: VecDeque::new(),
            next_id: saved.next_id,
            selected: None,
            filter: Filter::All,
            adding: None,
            settings_open: launch.open_settings,
            settings_at: started,
            dialog_rect: Rect::NOTHING,
            settings_body_h: None,
            dir_edit,
            control: Some(control),
            events,
            clip: Some(clip),
            clip_urls,
            _capture: capture,
            captured,
            toast: None,
            pick: None,
            delete_prompt: None,
            dirty: false,
            last_save: Instant::now(),
            theme_poll: Instant::now(),
            disk_cache: None,
            detail_at: started,
            sel_prev: None,
            born: HashMap::new(),
            cleanup: Vec::new(),
            direct_torrents: HashSet::new(),
            shot_at: started + launch.wait,
            launch,
            shot_state: 0,
            shot_flag: Arc::new(AtomicBool::new(false)),
            started,
            startup_ms,
            frames: Arc::new(AtomicU64::new(0)),
            first_frame: true,
            maximized: false,
            #[cfg(windows)]
            window,
            #[cfg(windows)]
            resize_drag: None,
        };

        // 窄窗默认先看任务列表；选中一行时才滑入详情。宽窗仍保留首行详情。
        if app.selected.is_none() && initial_w >= 860.0 {
            app.selected = app.rows.first().map(|r| r.id);
        }
        if app.launch.open_delete
            && let Some(id) = app.selected
        {
            app.delete_prompt = Some(DeletePrompt {
                id,
                intent: DeleteIntent::Task,
                dont_ask: false,
            });
        }
        if app.launch.open_add {
            let adds = std::mem::take(&mut app.launch.adds);
            if adds.len() == 1 {
                let path = Path::new(&adds[0]);
                if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("torrent"))
                    && path.is_file()
                {
                    app.open_add_torrent(path);
                } else {
                    app.open_add_text(adds[0].clone(), AddOrigin::Manual, Vec::new());
                }
            } else {
                app.open_add_text(adds.join("\n"), AddOrigin::Manual, Vec::new());
            }
        }
        if let Some(note) = dir_note {
            crate::log::line(&format!("下载目录回退：{note}"));
            app.toast(
                format!("原下载目录不可写，已改用 {}", app.settings.dir.display()),
                true,
            );
            app.mark_dirty();
        }
        // 配置目录被迫搬家（权限/沙箱）：日志留痕就够了——便携版把数据放在 exe 旁边
        // 本来就是正常行为，弹红条只会让人以为坏了。只有落到临时目录才值得打断。
        if let Some(note) = app.paths.note.clone() {
            crate::log::line(&format!(
                "配置目录：{note}，配置改放 {}",
                app.paths.home.display()
            ));
            if app.paths.home.starts_with(std::env::temp_dir()) {
                app.toast(
                    format!("配置临时放在 {}，可能被系统清理", app.paths.home.display()),
                    true,
                );
            }
        }
        let adds = app.launch.adds.clone();
        for url in adds {
            // `--add` 也接本地 `.torrent`：冒烟测试与拖入走同一条路。
            let p = Path::new(&url);
            if p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("torrent"))
                && p.is_file()
            {
                app.add_torrent_file(p);
            } else {
                app.add_url(&url);
            }
        }
        app
    }

    fn add_draft(&self, text: String, origin: AddOrigin, headers: Vec<(String, String)>) -> Adding {
        let header_url = first_add_target(&text);
        Adding {
            text,
            fresh: true,
            origin,
            headers,
            header_url,
            preview_id: 0,
            preview_for: String::new(),
            preview_due: Instant::now(),
            preview: AddPreview::Empty,
            torrent: None,
            checked: Vec::new(),
        }
    }

    fn open_add_text(
        &mut self,
        text: impl Into<String>,
        origin: AddOrigin,
        headers: Vec<(String, String)>,
    ) {
        let mut add = self.add_draft(text.into(), origin, headers);
        let target = first_add_target(&add.text);
        self.begin_add_preview(&mut add, &target);
        self.adding = Some(add);
    }

    fn cancel_add_preview(&mut self, add: &mut Adding) {
        if add.preview_id != 0 && add.torrent.is_some() {
            self.send(Cmd::RemoveTorrent {
                id: add.preview_id,
                delete_files: false,
            });
        }
        add.preview_id = 0;
        add.preview_for.clear();
        add.torrent = None;
        add.checked.clear();
        add.preview = AddPreview::Empty;
    }

    fn begin_add_preview(&mut self, add: &mut Adding, target: &str) {
        self.cancel_add_preview(add);
        add.preview_for = target.to_string();
        add.preview_due = Instant::now() + Duration::from_millis(450);
        if target.is_empty() {
            return;
        }

        let id = self.next_id;
        self.next_id += 1;
        add.preview_id = id;
        add.preview = AddPreview::Loading;
        let p = Path::new(target);
        if is_magnet(target) {
            add.torrent = Some(DraftTorrent {
                url: target.to_string(),
                source: TorSource::Magnet(target.to_string()),
            });
            self.send(Cmd::InspectTorrent {
                id,
                source: TorSource::Magnet(target.to_string()),
            });
            return;
        }
        if p.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("torrent"))
            && p.is_file()
        {
            add.torrent = Some(DraftTorrent {
                url: target.to_string(),
                source: TorSource::File(p.to_path_buf()),
            });
            self.send(Cmd::InspectTorrent {
                id,
                source: TorSource::File(p.to_path_buf()),
            });
            return;
        }
        if target.starts_with("http://") || target.starts_with("https://") {
            let headers = if target == add.header_url {
                add.headers.clone()
            } else {
                Vec::new()
            };
            self.send(Cmd::PreviewHttp {
                id,
                url: target.to_string(),
                headers,
            });
            return;
        }
        add.preview_id = 0;
        add.preview = AddPreview::Failed("只支持 http/https 链接、磁力链接或 .torrent 文件".into());
    }

    fn close_add_dialog(&mut self) {
        if let Some(mut add) = self.adding.take() {
            self.cancel_add_preview(&mut add);
        }
    }

    fn open_add_url(&mut self, url: &str, origin: AddOrigin, headers: Vec<(String, String)>) {
        let url = url.trim();
        if let Some(existing) = self.rows.iter().find(|r| r.url == url) {
            self.selected = Some(existing.id);
            self.toast("这个链接已经在列表里了", false);
            return;
        }
        self.open_add_text(url, origin, headers);
    }

    fn open_add_torrent(&mut self, path: &Path) {
        let url = path.to_string_lossy().to_string();
        if self.rows.iter().any(|r| r.url == url) {
            self.selected = self.rows.iter().find(|r| r.url == url).map(|r| r.id);
            self.toast("这个 .torrent 已经在列表里了", false);
            return;
        }
        self.open_add_text(url, AddOrigin::TorrentDrop, Vec::new());
    }

    fn commit_add(&mut self, add: Adding) {
        if let AddPreview::Torrent {
            name,
            total,
            files,
            info_hash,
        } = add.preview.clone()
            && let Some(draft) = add.torrent.clone()
            && add.preview_id != 0
        {
            let id = add.preview_id;
            let mut row = Row::torrent(id, draft.url);
            row.name = name;
            row.size = total;
            row.files = files;
            row.info_hash = info_hash;
            row.only = only_for_checked(&add.checked).flatten();
            if let TorSource::File(path) = &draft.source {
                let bytes = match std::fs::read(path) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        self.toast(format!("读不了这个 .torrent：{e}"), true);
                        self.adding = Some(add);
                        return;
                    }
                };
                if let Err(e) = std::fs::create_dir_all(&self.paths.torrents) {
                    self.toast(format!("存不下 .torrent 副本：{e}"), true);
                    self.adding = Some(add);
                    return;
                }
                let copy = self.paths.torrent_copy(id);
                if let Err(e) = std::fs::write(&copy, bytes) {
                    self.toast(format!("存不下 .torrent 副本：{e}"), true);
                    self.adding = Some(add);
                    return;
                }
                row.torrent = Some(copy);
            }
            row.status = Status::Queued;
            self.rows.insert(0, row);
            self.selected = Some(id);
            let only = self.row(id).and_then(|r| r.only.clone());
            self.send(Cmd::StartTorrent { id, only });
            self.adding = None;
            self.mark_dirty();
            return;
        }

        let preview = match &add.preview {
            AddPreview::Http {
                name,
                size,
                content_type,
                final_url,
            } if first_add_target(&add.text) == add.preview_for => {
                Some((name.clone(), *size, content_type.clone(), final_url.clone()))
            }
            _ => None,
        };
        let header_url = add.header_url.clone();
        let headers = add.headers.clone();
        let preview_id = add.preview_id;
        let mut added = 0usize;
        let mut first_new = true;
        for line in add
            .text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            let url = paste_target(line);
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                continue;
            }
            if let Some(existing) = self.rows.iter().find(|r| r.url == url) {
                self.selected = Some(existing.id);
                continue;
            }
            let id = if first_new && preview_id != 0 {
                preview_id
            } else {
                let id = self.next_id;
                self.next_id += 1;
                id
            };
            let line_headers = if url == header_url {
                headers.clone()
            } else {
                Vec::new()
            };
            let mut row = Row::new(id, url.clone());
            row.status = Status::Queued;
            row.headers = line_headers.clone();
            if first_new
                && url == add.preview_for
                && let Some((name, size, _content_type, _final_url)) = preview.clone()
            {
                row.name = name;
                row.size = size.unwrap_or(0);
            }
            self.rows.insert(0, row);
            self.selected = Some(id);
            self.send(Cmd::Add {
                id,
                url,
                headers: line_headers,
            });
            first_new = false;
            added += 1;
        }
        if added == 0 {
            self.toast("没有可加入的新链接", false);
            self.adding = Some(add);
        } else {
            self.adding = None;
            self.mark_dirty();
        }
    }

    fn cached_disk_space(&mut self) -> Option<(u64, u64)> {
        let ttl = std::time::Duration::from_secs(2);
        if let Some((dir, at, value)) = &self.disk_cache
            && dir == &self.settings.dir
            && at.elapsed() < ttl
        {
            return *value;
        }
        let value = disk_space(&self.settings.dir);
        self.disk_cache = Some((self.settings.dir.clone(), Instant::now(), value));
        value
    }

    /// 给这一帧第一次见到的行记下出生时刻（§7「行增删 120ms」）。
    ///
    /// 行高恒定是 `show_rows` 的硬要求，所以只淡入、不推高度；删除没有位置可留，直接跳掉。
    fn track_new_rows(&mut self) {
        // 只清掉已经不在列表里的行。不能按「活了 1 秒就清掉」来写：清掉之后同一个 id 还在
        // 列表里，下一句 `or_insert_with` 会把它重新记成「刚出生」，结果每帧都从 0 淡入。
        let alive: HashSet<u64> = self.rows.iter().map(|r| r.id).collect();
        self.born.retain(|id, _| alive.contains(id));
        for row in &self.rows {
            self.born.entry(row.id).or_insert_with(Instant::now);
        }
    }

    fn send(&self, cmd: Cmd) {
        crate::log::line(&format!("cmd {cmd:?}"));
        if let Some(c) = &self.control {
            c.send(cmd);
        }
    }

    fn toast(&mut self, msg: impl Into<String>, err: bool) {
        self.toast = Some(Toast(msg.into(), Instant::now(), err));
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// 把一行的链接（种子行就是种子来源）放进剪贴板：右键菜单与 Ctrl+C 共用。
    fn copy_link(&mut self, ctx: &egui::Context, id: u64) {
        let Some(url) = self.row(id).map(|r| r.url.clone()) else {
            return;
        };
        crate::log::line(&format!("复制链接 {url}"));
        ctx.copy_text(url);
        self.toast("链接已复制", false);
    }

    fn save(&mut self) {
        let p = Persisted {
            version: 1,
            next_id: self.next_id,
            settings: self.settings.clone(),
            rows: self.rows.clone(),
        };
        match self.store.save(&p) {
            Ok(()) => crate::log::line(&format!("保存 {}", self.store.path.display())),
            Err(e) => {
                crate::log::line(&format!("保存失败：{e}"));
                self.toast(format!("保存失败：{e}"), true);
            }
        }
        self.dirty = false;
        self.last_save = Instant::now();
    }

    fn row(&self, id: u64) -> Option<&Row> {
        self.rows.iter().find(|r| r.id == id)
    }

    // ---------------------------------------------------------------- 动作

    fn add_url(&mut self, url: &str) {
        let url = url.trim();
        if is_magnet(url) {
            self.add_magnet(url);
        } else {
            self.add_url_headers(url, Vec::new());
        }
    }

    /// 浏览器捕获进来的链接带着身份（Cookie/Referer/UA），要一路带到引擎。
    fn add_url_headers(&mut self, url: &str, headers: Vec<(String, String)>) {
        let url = url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            self.toast("只支持 http/https 链接", true);
            return;
        }
        if let Some(existing) = self.rows.iter().find(|r| r.url == url) {
            let id = existing.id;
            self.selected = Some(id);
            self.toast("这个链接已经在列表里了", false);
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        let mut row = Row::new(id, url.to_string());
        row.status = Status::Queued;
        row.headers = headers.clone();
        self.rows.insert(0, row);
        self.selected = Some(id);
        self.send(Cmd::Add {
            id,
            url: url.to_string(),
            headers,
        });
        self.mark_dirty();
    }

    fn add_magnet(&mut self, text: &str) {
        if self.rows.iter().any(|r| r.url == text) {
            self.selected = self.rows.iter().find(|r| r.url == text).map(|r| r.id);
            self.toast("这个种子已经在列表里了", false);
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        let mut row = Row::torrent(id, text.to_string());
        row.status = Status::Probing;
        self.rows.insert(0, row);
        self.selected = Some(id);
        self.send(Cmd::InspectTorrent {
            id,
            source: TorSource::Magnet(text.to_string()),
        });
        self.mark_dirty();
    }

    /// 拖进来的 `.torrent`：先拷一份进配置目录，重启后才解析得出来。
    fn add_torrent_file(&mut self, path: &Path) {
        self.add_torrent_file_mode(path, false);
    }

    fn add_torrent_file_direct(&mut self, path: &Path) {
        self.add_torrent_file_mode(path, true);
    }

    fn add_torrent_file_mode(&mut self, path: &Path, direct: bool) {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                self.toast(format!("读不了这个 .torrent：{e}"), true);
                return;
            }
        };
        let url = path.to_string_lossy().to_string();
        if self.rows.iter().any(|r| r.url == url) {
            self.toast("这个 .torrent 已经在列表里了", false);
            return;
        }
        let id = self.next_id;
        let copy = self.paths.torrent_copy(id);
        // 目录建不出来就说出来：以前这里吞掉错误，用户只看到后面那句「路径找不到」。
        if let Err(e) = std::fs::create_dir_all(&self.paths.torrents) {
            self.toast(format!("存不下 .torrent 副本：{e}"), true);
            return;
        }
        if let Err(e) = std::fs::write(&copy, &bytes) {
            self.toast(format!("存不下 .torrent 副本：{e}"), true);
            return;
        }
        self.next_id += 1;
        let mut row = Row::torrent(id, url);
        row.torrent = Some(copy.clone());
        row.status = Status::Probing;
        self.rows.insert(0, row);
        self.selected = Some(id);
        if direct {
            self.direct_torrents.insert(id);
        }
        self.send(Cmd::InspectTorrent {
            id,
            source: TorSource::File(copy),
        });
        self.mark_dirty();
    }

    /// 重新交给控制器解析一遍：会话是按需存在的，重启后它就没了。
    fn resume_torrent(&mut self, id: u64) {
        let Some(row) = self.row(id).cloned() else {
            return;
        };
        let source = match (&row.torrent, is_magnet(&row.url)) {
            (Some(p), _) => TorSource::File(p.clone()),
            (None, true) => TorSource::Magnet(row.url.clone()),
            (None, false) => {
                self.toast("找不到这个种子的来源了", true);
                return;
            }
        };
        if let Some(r) = self.rows.iter_mut().find(|r| r.id == id) {
            r.status = Status::Probing;
            r.error = None;
            r.speed = 0.0;
            r.sample = None;
        }
        self.send(Cmd::InspectTorrent { id, source });
        self.mark_dirty();
    }

    fn start_torrent(&mut self, id: u64) {
        let only = self.row(id).and_then(|r| r.only.clone());
        if let Some(r) = self.rows.iter_mut().find(|r| r.id == id) {
            r.status = Status::Queued;
            r.error = None;
        }
        self.send(Cmd::StartTorrent { id, only });
        self.mark_dirty();
    }

    fn toggle_pause(&mut self, id: u64) {
        let Some(row) = self.row(id).cloned() else {
            return;
        };
        if row.kind == Kind::Torrent {
            self.toggle_torrent(id, &row);
            return;
        }
        match row.status {
            Status::Active | Status::Probing | Status::Queued => self.send(Cmd::Pause(id)),
            Status::Paused => {
                if let Some(dest) = row.dest.clone() {
                    if let Some(r) = self.rows.iter_mut().find(|r| r.id == id) {
                        r.status = Status::Queued;
                        r.error = None;
                    }
                    self.send(Cmd::Resume {
                        id,
                        url: row.url.clone(),
                        dest,
                        headers: row.headers.clone(),
                    });
                    self.mark_dirty();
                } else {
                    self.restart(id, &row);
                }
            }
            Status::Failed => self.restart(id, &row),
            Status::Done => self.toast("这个文件已经下载完了", false),
            // 只有 torrent 会做种，走不到这里；写了是为了让 match 穷尽。
            Status::Seeding => self.resume_torrent(id),
        }
    }

    fn menu_start(&mut self, id: u64) {
        let Some(status) = self.row(id).map(|r| r.status) else {
            return;
        };
        match status {
            Status::Paused | Status::Failed => self.toggle_pause(id),
            Status::Queued | Status::Probing => self.toast("任务已经在队列里", false),
            Status::Active | Status::Seeding => self.toast("任务已经在运行", false),
            Status::Done => self.toast("这个文件已经下载完了", false),
        }
    }

    fn toggle_torrent(&mut self, id: u64, row: &Row) {
        match row.status {
            Status::Active | Status::Seeding | Status::Queued | Status::Probing => {
                // 选文件窗口开着时，“暂停”等同于取消：会话里那个种子已经不在跑了。
                if self.pick.as_ref().is_some_and(|p| p.id == id) {
                    self.pick = None;
                }
                self.send(Cmd::PauseTorrent(id));
            }
            // 暂停/完成/失败后再点，都要重新解析一遍：会话早随任务结束放掉了。
            _ => self.resume_torrent(id),
        }
    }

    fn restart(&mut self, id: u64, row: &Row) {
        if let Some(r) = self.rows.iter_mut().find(|r| r.id == id) {
            r.status = Status::Queued;
            r.error = None;
            r.dest = None;
            r.downloaded = 0;
            r.speed = 0.0;
            r.sample = None;
        }
        self.send(Cmd::Add {
            id,
            url: row.url.clone(),
            headers: row.headers.clone(),
        });
        self.mark_dirty();
    }

    fn request_delete(&mut self, id: u64, intent: DeleteIntent) {
        if delete_requires_confirmation(intent, self.settings.confirm_delete) {
            self.delete_prompt = Some(DeletePrompt {
                id,
                intent,
                dont_ask: false,
            });
        } else {
            self.remove(id, false);
        }
    }

    /// 选文件窗口关闭但任务保留：只暂停会话，不清掉解析出的文件和勾选上下文。
    fn defer_file_pick(&mut self, id: u64) {
        self.send(Cmd::PauseTorrent(id));
        if let Some(r) = self.rows.iter_mut().find(|r| r.id == id) {
            r.status = Status::Paused;
            r.connections = 0;
            r.speed = 0.0;
        }
        self.mark_dirty();
    }

    fn remove(&mut self, id: u64, delete_files: bool) {
        let Some(row) = self.row(id).cloned() else {
            return;
        };
        let running = matches!(
            row.status,
            Status::Active | Status::Probing | Status::Queued | Status::Seeding
        );
        self.rows.retain(|r| r.id != id);
        if self.pick.as_ref().is_some_and(|p| p.id == id) {
            self.pick = None;
        }
        if self.selected == Some(id) {
            self.selected = self.rows.first().map(|r| r.id);
        }
        if row.kind == Kind::Torrent {
            self.send(Cmd::RemoveTorrent { id, delete_files });
            if let Some(t) = &row.torrent {
                let _ = std::fs::remove_file(t);
            }
            if running {
                if let Some(dest) = &row.dest {
                    self.cleanup.push((id, dest.clone(), delete_files));
                }
            } else if delete_files && let Some(dest) = &row.dest {
                remove_download_data(dest);
            }
            self.mark_dirty();
            return;
        }
        self.send(Cmd::Remove(id));
        if let Some(dest) = row.dest {
            // 任务可能还在写盘，先删一次；等 Removed 事件到了再删一次（引擎收尾会补写 state）。
            remove_leftovers(&dest);
            if running {
                self.cleanup.push((id, dest, delete_files));
            } else if delete_files {
                remove_download_data(&dest);
            }
        }
        self.mark_dirty();
    }

    fn open_row(&mut self, id: u64) {
        let Some(row) = self.row(id) else {
            return;
        };
        match (&row.status, &row.dest) {
            (Status::Done, Some(dest)) if dest.exists() => open_path(dest, false),
            (_, Some(dest)) => open_path(dest, true),
            (_, None) => self.toast("文件还没开始下载", true),
        }
    }

    // ---------------------------------------------------------------- 事件

    fn drain_events(&mut self) {
        let mut had_progress = false;
        while let Ok(ev) = self.events.try_recv() {
            if !matches!(ev, UiEvent::Progress { .. }) {
                crate::log::line(&format!("ev {ev:?}"));
            }
            match ev {
                UiEvent::HttpPreview {
                    id,
                    name,
                    size,
                    content_type,
                    final_url,
                } => {
                    if let Some(add) = self.adding.as_mut()
                        && add.preview_id == id
                        && add.torrent.is_none()
                    {
                        add.preview = AddPreview::Http {
                            name,
                            size,
                            content_type,
                            final_url,
                        };
                        add.checked = vec![true];
                    }
                }
                UiEvent::HttpPreviewFailed { id, reason } => {
                    if let Some(add) = self.adding.as_mut()
                        && add.preview_id == id
                        && add.torrent.is_none()
                    {
                        add.preview = AddPreview::Failed(reason);
                    }
                }
                UiEvent::Started {
                    id,
                    dest,
                    name,
                    size,
                    resumed_from,
                    ..
                } => {
                    if let Some(r) = self.rows.iter_mut().find(|r| r.id == id) {
                        r.dest = Some(dest);
                        r.name = name;
                        r.size = size;
                        r.status = Status::Active;
                        r.resumed_from = resumed_from;
                        r.downloaded = resumed_from;
                        r.connections = 0;
                        r.error = None;
                        r.speed = 0.0;
                        r.sample = Some((Instant::now(), resumed_from));
                        r.speed_history.clear();
                    }
                    self.mark_dirty();
                }
                UiEvent::Progress {
                    id,
                    downloaded,
                    total,
                    connections,
                    elapsed_ms,
                } => {
                    if let Some(r) = self.rows.iter_mut().find(|r| r.id == id) {
                        if total > 0 {
                            r.size = total;
                        }
                        r.downloaded = downloaded;
                        r.connections = connections;
                        r.elapsed_ms = elapsed_ms;
                        r.status = Status::Active;
                        let now = Instant::now();
                        let mut sampled = false;
                        match r.sample {
                            Some((t, prev)) if now.duration_since(t).as_secs_f64() >= 0.4 => {
                                let dt = now.duration_since(t).as_secs_f64();
                                r.speed = downloaded.saturating_sub(prev) as f64 / dt;
                                r.sample = Some((now, downloaded));
                                sampled = true;
                            }
                            None => {
                                r.sample = Some((now, downloaded));
                                sampled = true;
                            }
                            _ => {}
                        }
                        if sampled {
                            r.push_speed_sample(now);
                        }
                    }
                    // 进度也要落盘（有 800 ms 节流兜着），否则崩溃/关闭后列表停在旧数字上。
                    self.mark_dirty();
                    had_progress = true;
                }
                UiEvent::Done {
                    id,
                    path,
                    size,
                    elapsed_ms,
                } => {
                    if let Some(r) = self.rows.iter_mut().find(|r| r.id == id) {
                        r.status = Status::Done;
                        r.dest = Some(path);
                        if size > 0 {
                            r.size = size;
                        }
                        r.downloaded = r.size;
                        r.connections = 0;
                        r.speed = 0.0;
                        r.elapsed_ms = elapsed_ms;
                        r.error = None;
                        r.push_speed_sample(Instant::now());
                    }
                    self.mark_dirty();
                }
                UiEvent::Failed { id, reason } => {
                    self.direct_torrents.remove(&id);
                    if let Some(add) = self.adding.as_mut()
                        && add.preview_id == id
                        && add.torrent.is_some()
                    {
                        add.preview = AddPreview::Failed(reason);
                        continue;
                    }
                    if let Some(r) = self.rows.iter_mut().find(|r| r.id == id) {
                        r.status = Status::Failed;
                        r.connections = 0;
                        r.speed = 0.0;
                        r.error = Some(reason.clone());
                        r.push_speed_sample(Instant::now());
                    }
                    self.toast(friendly_error(&reason), true);
                    self.mark_dirty();
                }
                UiEvent::Paused { id } => {
                    if let Some(r) = self.rows.iter_mut().find(|r| r.id == id) {
                        r.status = Status::Paused;
                        r.connections = 0;
                        r.speed = 0.0;
                        r.push_speed_sample(Instant::now());
                    }
                    self.mark_dirty();
                }
                UiEvent::Removed { id } => {
                    self.direct_torrents.remove(&id);
                    if let Some(pos) = self.cleanup.iter().position(|(i, _, _)| *i == id) {
                        let (_, dest, delete_files) = self.cleanup.remove(pos);
                        remove_leftovers(&dest);
                        if delete_files {
                            remove_download_data(&dest);
                        }
                    }
                }
                UiEvent::TorrentReady {
                    id,
                    name,
                    info_hash,
                    total,
                    files,
                } => {
                    if let Some(add) = self.adding.as_mut()
                        && add.preview_id == id
                        && add.torrent.is_some()
                    {
                        if self
                            .rows
                            .iter()
                            .any(|r| !r.info_hash.is_empty() && r.info_hash == info_hash)
                        {
                            add.checked.clear();
                            add.preview = AddPreview::Failed("这个种子已经在列表里了".into());
                        } else {
                            let n = files.len();
                            add.checked = vec![true; n];
                            add.preview = AddPreview::Torrent {
                                name,
                                total,
                                files,
                                info_hash,
                            };
                        }
                        continue;
                    }
                    let dup = self
                        .rows
                        .iter()
                        .any(|r| r.id != id && !r.info_hash.is_empty() && r.info_hash == info_hash);
                    if dup {
                        self.direct_torrents.remove(&id);
                        self.rows.retain(|r| r.id != id);
                        self.send(Cmd::RemoveTorrent {
                            id,
                            delete_files: false,
                        });
                        self.toast("这个种子已经在列表里了", false);
                        self.mark_dirty();
                        continue;
                    }
                    let dir = self.settings.dir.clone();
                    let direct = self.direct_torrents.remove(&id);
                    let mut ask = false;
                    let mut resume = false;
                    if let Some(r) = self.rows.iter_mut().find(|r| r.id == id) {
                        r.name = name.clone();
                        r.size = total;
                        r.info_hash = info_hash;
                        r.files = files.clone();
                        r.dest = Some(dir.join(&name));
                        // 只有一个文件就不必打断用户；目录布局（多文件种子）才问。
                        ask = torrent_needs_pick(files.len(), r.only.is_none(), direct);
                        if ask {
                            // 等用户挑文件，不算在解析中，也不占用下载位。
                            r.status = Status::Queued;
                        }
                        resume = !ask;
                    }
                    if ask {
                        let n = files.len();
                        self.pick = Some(Pick {
                            id,
                            files,
                            checked: vec![true; n],
                        });
                    } else if resume {
                        self.start_torrent(id);
                    }
                    self.mark_dirty();
                }
                UiEvent::TorrentProgress {
                    id,
                    dest,
                    downloaded,
                    total,
                    uploaded,
                    peers,
                    seeders,
                    finished,
                } => {
                    if let Some(r) = self.rows.iter_mut().find(|r| r.id == id) {
                        r.dest = Some(dest);
                        if total > 0 {
                            r.size = total;
                        }
                        r.downloaded = downloaded;
                        r.uploaded = uploaded;
                        r.peers = peers;
                        r.seeders = seeders;
                        r.connections = peers;
                        r.status = if finished {
                            if self.settings.seed {
                                Status::Seeding
                            } else {
                                Status::Done
                            }
                        } else {
                            Status::Active
                        };
                        let now = Instant::now();
                        let mut sampled = false;
                        match r.sample {
                            Some((t, prev)) if now.duration_since(t).as_secs_f64() >= 0.4 => {
                                let dt = now.duration_since(t).as_secs_f64();
                                r.speed = downloaded.saturating_sub(prev) as f64 / dt;
                                r.sample = Some((now, downloaded));
                                sampled = true;
                            }
                            None => {
                                r.sample = Some((now, downloaded));
                                sampled = true;
                            }
                            _ => {}
                        }
                        if sampled {
                            r.push_speed_sample(now);
                        }
                    }
                    self.mark_dirty();
                    had_progress = true;
                }
                UiEvent::Notice(msg) => self.toast(msg, true),
            }
        }
        if had_progress {
            let now = Instant::now();
            let total_speed: f64 = self
                .rows
                .iter()
                .filter(|r| matches!(r.status, Status::Active | Status::Seeding))
                .map(|r| r.speed)
                .sum();
            self.speed_history.push_back((now, total_speed));
            while self
                .speed_history
                .front()
                .is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(60))
            {
                self.speed_history.pop_front();
            }
        }
    }

    fn drain_clipboard(&mut self) {
        if self.adding.is_some() {
            return;
        }
        while let Ok(url) = self.clip_urls.try_recv() {
            if !self.settings.clipboard {
                continue;
            }
            if self.rows.iter().any(|r| r.url == url) {
                continue;
            }
            self.open_add_url(&url, AddOrigin::Clipboard, Vec::new());
            break;
        }
    }

    /// 浏览器来的捕获：入队并把「浏览器点击 → 这里收到」的延迟记下来。
    ///
    /// 延迟量的是 `Date.now()` 与本地时钟的差，所以两边必须是同一台机器
    /// （浏览器扩展和桌面端本来就只能同机通信，这个前提成立）。
    fn drain_capture(&mut self) {
        if self.adding.is_some() {
            return;
        }
        while let Ok(c) = self.captured.try_recv() {
            let url = c.url.trim().to_string();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                continue;
            }
            let lag = now_ms().saturating_sub(c.t);
            if let Ok(path) = std::env::var("VOLANT_CAPTURE_LOG") {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    let _ = writeln!(f, "{lag}\t{url}");
                }
            }
            if self.rows.iter().any(|r| r.url == url) {
                self.selected = self.rows.iter().find(|r| r.url == url).map(|r| r.id);
                continue;
            }
            if self.settings.skip_add_dialog {
                self.add_url_headers(&url, c.headers());
                self.toast("已拦截浏览器下载并加入队列", false);
            } else {
                self.open_add_url(&url, AddOrigin::Browser, c.headers());
            }
            break;
        }
    }

    /// 拖进来的文件：只认 `.torrent`，其他一律当误操作。
    fn drain_drops(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            let p = f.path();
            crate::log::line(&format!("drop {}", p.display()));
            if p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("torrent"))
            {
                if self.adding.is_some() {
                    self.toast("请先处理当前的新建下载窗口", true);
                } else if self.settings.skip_add_dialog {
                    self.add_torrent_file_direct(p);
                    self.toast("已加入下载队列", false);
                } else {
                    self.open_add_torrent(p);
                }
            } else {
                self.toast("拖进来的要是一个 .torrent 文件", true);
            }
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.text_edit_focused();
        let (space, del, enter, esc, copy, comma) = ctx.input(|i| {
            (
                i.key_pressed(Key::Space),
                i.key_pressed(Key::Delete),
                i.key_pressed(Key::Enter),
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::C) && i.modifiers.command,
                i.key_pressed(Key::Comma) && i.modifiers.command,
            )
        });
        let pasted = if typing || self.adding.is_some() {
            None
        } else {
            ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Paste(s) if !s.trim().is_empty() => Some(s.clone()),
                    _ => None,
                })
            })
        };

        if esc {
            if self.delete_prompt.is_some() {
                self.delete_prompt = None;
            } else if self.settings_open {
                self.settings_open = false;
            } else if self.adding.is_some() {
                self.close_add_dialog();
            } else if let Some(pick) = self.pick.take() {
                self.defer_file_pick(pick.id);
            } else {
                self.selected = None;
            }
        }
        // Ctrl+, 是设置的开关（§6 的键位）；对话框开着时 Enter 等于「完成」。
        let mut consumed = false;
        if comma {
            self.settings_open = !self.settings_open;
            if self.settings_open {
                self.settings_at = Instant::now();
                self.dir_edit = self.settings.dir.to_string_lossy().to_string();
            }
            consumed = true;
        }
        if self.settings_open && enter && !typing {
            self.settings_open = false;
            consumed = true;
        }
        if consumed {
            return;
        }
        // 模态弹窗在前台时，底层列表的 Space/Delete/Enter 不能穿透执行。
        if self.adding.is_some()
            || self.delete_prompt.is_some()
            || self.pick.is_some()
            || self.settings_open
        {
            return;
        }
        if let Some(text) = pasted {
            self.open_add_text(text, AddOrigin::Manual, Vec::new());
            return;
        }
        if typing {
            return;
        }
        let Some(id) = self.selected else {
            return;
        };
        if space {
            self.toggle_pause(id);
        } else if del {
            self.request_delete(id, DeleteIntent::Task);
        } else if copy {
            self.copy_link(ctx, id);
        } else if enter {
            match self.row(id).map(|r| enter_action(r.status)) {
                Some(EnterAction::OpenFile) => self.open_row(id),
                Some(EnterAction::OpenDetail) => self.selected = Some(id),
                None => {}
            }
        }
    }

    // ---------------------------------------------------------------- 标题栏

    /// §4 标题栏 44：左 logo + 名字 + 分隔线 + 视图名，右三个 36×44 窗口按钮。
    fn bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("bar")
            .exact_size(theme::TB_H)
            .show_separator_line(false)
            .frame(egui::Frame::NONE.fill(theme::pal().surface))
            .show(ui, |ui| {
                let w = ui.available_width();
                let (rect, _) = ui.allocate_exact_size(vec2(w, theme::TB_H), Sense::hover());
                let cy = rect.center().y;

                // 右边：关闭 / 最大化 / 最小化，各 36 宽贴右边缘；设置暂时挤在它们左边（第 4 刀进左栏）。
                let close = win_button(ui, rect, 0, Icon::Close, "关闭", true);
                let max = win_button(ui, rect, 1, Icon::Maximize, "最大化", false);
                let min = win_button(ui, rect, 2, Icon::Minimize, "最小化", false);
                // 拖动区：整条标题栏减去右边四个按钮。
                let drag = ui.interact(
                    Rect::from_min_max(rect.min, pos2(rect.right() - 3.0 * 36.0 - 8.0, rect.max.y)),
                    ui.scope_id().with("bar_drag"),
                    Sense::click_and_drag(),
                );
                if drag.drag_started() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                if drag.double_clicked() {
                    self.maximized = !self.maximized;
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Maximized(self.maximized));
                }

                // 左：18×18 黑色圆角方块 + 白色斜杠（§4 的 24 网格单斜杠路径，等比缩到 18）。
                let logo =
                    Rect::from_min_size(pos2(rect.left() + 16.0, cy - 9.0), vec2(18.0, 18.0));
                let p = ui.painter();
                p.rect_filled(logo, CornerRadius::same(5), Color32::BLACK);
                let k = logo.width() / 24.0;
                let lp = |x: f32, y: f32| logo.min + vec2(x, y) * k;
                p.add(egui::Shape::convex_polygon(
                    vec![lp(4.4, 20.4), lp(15.1, 3.6), lp(19.2, 3.6), lp(8.6, 20.4)],
                    theme::pal().ink,
                    Stroke::NONE,
                ));

                let name = theme::galley_sb(ui, "Volant", 12.5, theme::pal().ink, f32::INFINITY);
                let name_x = logo.right() + 8.0;
                let name_h = name.size().y;
                p.galley(pos2(name_x, cy - name_h / 2.0), name, theme::pal().ink);

                if close.clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if min.clicked() {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                if max.clicked() {
                    self.maximized = !self.maximized;
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Maximized(self.maximized));
                }
            });
    }

    /// 无边框窗口的原生缩放命中区。放在 Foreground 层，保证窗口四边和四角
    /// 不会被标题栏拖动区或内容控件抢走；最大化时交给系统，不注册命中区。
    fn resize_handles(&mut self, ctx: &egui::Context) {
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(self.maximized));
        if maximized {
            #[cfg(windows)]
            {
                self.resize_drag = None;
            }
            return;
        }

        for (edge, rect) in resize_zones(ctx.content_rect()) {
            egui::Area::new(egui::Id::unique(("window-resize", edge)))
                .order(egui::Order::Foreground)
                .fixed_pos(rect.min)
                .movable(false)
                .constrain(false)
                .show(ctx, |ui| {
                    let (_, response) = ui.allocate_exact_size(rect.size(), Sense::drag());
                    if response.hovered() {
                        ctx.set_cursor_icon(resize_cursor(edge));
                    }
                    if response.hovered()
                        && ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary))
                    {
                        #[cfg(windows)]
                        let started = self.begin_window_resize(edge);
                        #[cfg(windows)]
                        if !started {
                            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(
                                resize_direction(edge),
                            ));
                        }
                        #[cfg(not(windows))]
                        ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(
                            resize_direction(edge),
                        ));
                    }
                });
        }
    }

    /// Windows 的 `BeginResize` 会进入模态消息循环，界面在拖动时无法连续重绘。
    /// 这里自己跟踪屏幕坐标并逐帧调整窗口，事件循环保持畅通。
    #[cfg(windows)]
    fn begin_window_resize(&mut self, edge: ResizeEdge) -> bool {
        let Some(hwnd) = self.window else {
            return false;
        };
        let (Some(start), Some(cursor)) = (window_rect(hwnd), cursor_pos()) else {
            return false;
        };
        self.resize_drag = Some(ResizeDrag {
            edge,
            start,
            cursor,
            last: start,
        });
        true
    }

    #[cfg(windows)]
    fn drive_window_resize(&mut self, ctx: &egui::Context) {
        let Some(mut drag) = self.resize_drag else {
            return;
        };
        if !ctx.input(|i| i.pointer.primary_down()) {
            self.resize_drag = None;
            return;
        }
        let (Some(hwnd), Some(cursor)) = (self.window, cursor_pos()) else {
            self.resize_drag = None;
            return;
        };
        let scale = ctx.pixels_per_point();
        let target = resized_window(
            drag.start,
            drag.cursor,
            cursor,
            drag.edge,
            (520.0 * scale).round() as i32,
            (340.0 * scale).round() as i32,
        );
        if target != drag.last {
            resize_window(hwnd, target);
            drag.last = target;
            self.resize_drag = Some(drag);
        }
    }

    #[cfg(not(windows))]
    fn drive_window_resize(&mut self, _ctx: &egui::Context) {}

    // ---------------------------------------------------------------- 页头 + 概览条

    /// §4 页头（24 标题 + 12.5 副标题 + ghost/primary 按钮）+ 概览条 64（3 格）。
    /// §4 左栏 232 / 图标栏 56：队列四视图 + 底部下载目录、剪贴板、设置、版本号。
    fn rail(&mut self, ui: &mut egui::Ui) {
        let disk = self.cached_disk_space();
        let rows = &self.rows;
        let counts = [
            rows.len(),
            rows.iter().filter(|r| Filter::Active.matches(r)).count(),
            rows.iter().filter(|r| Filter::Done.matches(r)).count(),
            rows.iter().filter(|r| Filter::Failed.matches(r)).count(),
        ];
        let selected = self.filter;
        let dir = self.settings.dir.display().to_string();
        let clip_on = self.settings.clipboard;
        // 断点按**窗口**宽算，不能按扣掉详情栏之后的剩余宽度：1040 的窗口先扣 312 只剩 728，
        // 左栏就会误判成 56 的图标栏。§5 的三档全是按窗口宽写的。
        // 例外是 1040–1159 这一段：三栏在场时列表只剩 496–615，左栏必须先让位（收成图标栏）。
        let win_w = ui.ctx().content_rect().width();
        let wide = win_w >= 860.0 && !(1040.0..1160.0).contains(&win_w);
        let w = rail_width(win_w);
        let mut pick: Option<Filter> = None;
        let mut open_settings = false;
        let mut clip_click = false;
        egui::Panel::left("rail")
            .exact_size(w)
            .show_separator_line(false)
            .frame(egui::Frame::NONE.fill(theme::pal().rail))
            .show(ui, |ui| {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(w, ui.available_height()), Sense::hover());
                let mut hits: Vec<(Rect, Filter)> = Vec::new();
                let mut b = rect.bottom() - 12.0;
                let set_rect = Rect::from_min_size(
                    pos2(rect.left() + 8.0, b - theme::ITEM_H),
                    vec2(rect.width() - 16.0, theme::ITEM_H),
                );
                b -= theme::ITEM_H + 2.0;
                let clip_rect = Rect::from_min_size(
                    pos2(rect.left() + 8.0, b - theme::ITEM_H),
                    vec2(rect.width() - 16.0, theme::ITEM_H),
                );
                let divider_y = b - theme::ITEM_H - 10.0 + 6.0;
                {
                    let p = ui.painter();
                    if wide {
                        p.text(
                            pos2(rect.left() + 16.0, rect.top() + 16.0),
                            Align2::LEFT_TOP,
                            "队列",
                            FontId::proportional(theme::F_LABEL),
                            theme::pal().ink2,
                        );
                    }
                    let mut y = rect.top() + if wide { 40.0 } else { 14.0 };
                    for (i, f) in Filter::ALL.iter().enumerate() {
                        let item = Rect::from_min_size(
                            pos2(rect.left() + 8.0, y),
                            vec2(rect.width() - 16.0, if wide { theme::ITEM_H } else { 36.0 }),
                        );
                        let on = selected == *f;
                        if on {
                            p.rect_filled(item, CornerRadius::same(7), theme::pal().sel);
                        } else if ui.rect_contains_pointer(item) {
                            p.rect_filled(item, CornerRadius::same(7), theme::pal().hover);
                        }
                        let ink = if on {
                            theme::pal().ink
                        } else {
                            theme::pal().ink2
                        };
                        let cx = if wide {
                            item.left() + 18.0
                        } else {
                            item.center().x
                        };
                        theme::paint_icon(p, pos2(cx, item.center().y), ink, f.icon());
                        if wide {
                            p.text(
                                pos2(item.left() + 34.0, item.center().y),
                                Align2::LEFT_CENTER,
                                f.title(),
                                FontId::proportional(12.5),
                                ink,
                            );
                            p.text(
                                pos2(item.right() - 10.0, item.center().y),
                                Align2::RIGHT_CENTER,
                                counts[i].to_string(),
                                FontId::proportional(11.5),
                                if on {
                                    theme::pal().ink
                                } else {
                                    theme::pal().ink2
                                },
                            );
                        }
                        hits.push((item, *f));
                        y += if wide { theme::ITEM_H } else { 40.0 };
                    }

                    // 底部：版本号 → 设置 → 剪贴板 → 分隔线 → 下载目录。
                    if ui.rect_contains_pointer(set_rect) {
                        p.rect_filled(set_rect, CornerRadius::same(7), theme::pal().hover);
                    }
                    let set_cx = if wide {
                        set_rect.left() + 18.0
                    } else {
                        set_rect.center().x
                    };
                    theme::paint_icon(
                        p,
                        pos2(set_cx, set_rect.center().y),
                        theme::pal().ink2,
                        Icon::Gear,
                    );
                    if wide {
                        p.text(
                            pos2(set_rect.left() + 34.0, set_rect.center().y),
                            Align2::LEFT_CENTER,
                            "设置",
                            FontId::proportional(12.5),
                            theme::pal().ink2,
                        );
                        p.text(
                            pos2(set_rect.right() - 10.0, set_rect.center().y),
                            Align2::RIGHT_CENTER,
                            "Ctrl+,",
                            FontId::proportional(11.5),
                            theme::pal().ink2,
                        );
                    }

                    if wide {
                        theme::paint_icon(
                            p,
                            pos2(clip_rect.left() + 18.0, clip_rect.center().y),
                            theme::pal().ink2,
                            Icon::Link,
                        );
                        p.text(
                            pos2(clip_rect.left() + 36.0, clip_rect.center().y),
                            Align2::LEFT_CENTER,
                            "剪贴板监视",
                            FontId::proportional(12.5),
                            theme::pal().ink2,
                        );
                    } else {
                        theme::paint_icon(
                            p,
                            clip_rect.center(),
                            if clip_on {
                                theme::pal().ink2
                            } else {
                                theme::pal().ink3
                            },
                            Icon::Link,
                        );
                    }

                    if wide {
                        p.hline(
                            rect.left() + 16.0..=rect.right() - 16.0,
                            divider_y,
                            Stroke::new(1.0, theme::pal().line),
                        );
                    }
                    if wide {
                        let label_top = divider_y - 74.0;
                        theme::paint_icon(
                            p,
                            pos2(rect.left() + 22.0, label_top + 7.5),
                            theme::pal().ink2,
                            Icon::Folder,
                        );
                        p.text(
                            pos2(rect.left() + 40.0, label_top),
                            Align2::LEFT_TOP,
                            "下载目录",
                            FontId::proportional(12.5),
                            theme::pal().ink2,
                        );
                        if let Some((free, _total)) = disk {
                            p.text(
                                pos2(rect.right() - 16.0, label_top),
                                Align2::RIGHT_TOP,
                                format!("{} 可用", crate::fmt::bytes(free)),
                                FontId::proportional(theme::F_LABEL),
                                theme::pal().ink2,
                            );
                        }
                        let g = theme::galley(
                            ui,
                            &dir,
                            theme::F_META,
                            theme::pal().ink2,
                            rect.width() - 32.0,
                        );
                        p.galley(
                            pos2(rect.left() + 16.0, divider_y - 50.0),
                            g,
                            theme::pal().ink2,
                        );
                        let track = Rect::from_min_size(
                            pos2(rect.left() + 16.0, divider_y - 21.0),
                            vec2(rect.width() - 32.0, 4.0),
                        );
                        p.rect_filled(track, CornerRadius::same(2), theme::pal().line);
                        if let Some((free, total)) = disk
                            && total > 0
                        {
                            let used = 1.0 - (free as f32 / total as f32).clamp(0.0, 1.0);
                            p.rect_filled(
                                Rect::from_min_size(
                                    track.min,
                                    vec2(track.width() * used, track.height()),
                                ),
                                CornerRadius::same(2),
                                theme::pal().ink,
                            );
                        }
                    }
                }
                for (r, f) in &hits {
                    let resp =
                        ui.interact(*r, egui::Id::unique(("rail", f.title())), Sense::click());
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::Role::Button,
                            true,
                            selected == *f,
                            f.title(),
                        )
                    });
                    if resp.has_focus() {
                        ui.painter().rect_stroke(
                            *r,
                            CornerRadius::same(7),
                            Stroke::new(1.5, theme::pal().accent),
                            StrokeKind::Inside,
                        );
                    }
                    if resp.clicked() {
                        pick = Some(*f);
                    }
                    if !wide {
                        resp.on_hover_text(f.title());
                    }
                }
                let settings_resp =
                    ui.interact(set_rect, egui::Id::unique("rail_set"), Sense::click());
                settings_resp
                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "设置"));
                if settings_resp.has_focus() {
                    ui.painter().rect_stroke(
                        set_rect,
                        CornerRadius::same(7),
                        Stroke::new(1.5, theme::pal().accent),
                        StrokeKind::Inside,
                    );
                }
                if settings_resp.clicked() {
                    open_settings = true;
                }
                if !wide {
                    settings_resp.on_hover_text("设置");
                }
                if wide {
                    let sw = Rect::from_min_size(
                        pos2(
                            clip_rect.right() - 8.0 - theme::SWITCH_W,
                            clip_rect.center().y - theme::SWITCH_H / 2.0,
                        ),
                        vec2(theme::SWITCH_W, theme::SWITCH_H),
                    );
                    let clip_resp = theme::switch_at(ui, sw, clip_on, "rail_clip", "剪贴板监视");
                    if clip_resp.clicked() {
                        clip_click = true;
                    }
                } else {
                    let clip_resp = ui.interact(
                        clip_rect,
                        egui::Id::unique("rail_clip_icon"),
                        Sense::click(),
                    );
                    clip_resp.widget_info(|| {
                        egui::WidgetInfo::selected(egui::Role::Button, true, clip_on, "剪贴板监视")
                    });
                    if clip_resp.has_focus() {
                        ui.painter().rect_stroke(
                            clip_rect,
                            CornerRadius::same(7),
                            Stroke::new(1.5, theme::pal().accent),
                            StrokeKind::Inside,
                        );
                    }
                    if clip_resp.clicked() {
                        clip_click = true;
                    }
                    clip_resp.on_hover_text(if clip_on {
                        "剪贴板监视：已开启"
                    } else {
                        "剪贴板监视：已关闭"
                    });
                };
            });
        if let Some(f) = pick {
            self.filter = f;
            self.selected = None;
        }
        if open_settings && !self.settings_open {
            self.settings_open = true;
            self.settings_at = Instant::now();
            self.dir_edit = self.settings.dir.to_string_lossy().to_string();
        }
        if clip_click {
            self.settings.clipboard = !self.settings.clipboard;
            if let Some(c) = &self.clip {
                c.set_enabled(self.settings.clipboard);
            }
            self.mark_dirty();
        }
    }

    /// `inset` 是覆盖式详情栏盖住的宽度：页头与概览条要按看得见的那段排版，
    /// 否则主按钮会被盖进面板底下。
    fn head(&mut self, ui: &mut egui::Ui, inset: f32) {
        let filter = self.filter;
        let (total, running, speed, conns, remaining, bulk) = {
            let shown: Vec<&Row> = self.rows.iter().filter(|r| filter.matches(r)).collect();
            let running = shown
                .iter()
                .filter(|r| matches!(r.status, Status::Active | Status::Probing | Status::Seeding))
                .count();
            let speed: f64 = shown
                .iter()
                .filter(|r| r.status == Status::Active)
                .map(|r| r.speed)
                .sum();
            let conns: usize = shown
                .iter()
                .filter(|r| matches!(r.status, Status::Active | Status::Seeding))
                .map(|r| r.connections)
                .sum();
            let remaining: u64 = shown
                .iter()
                .filter(|r| r.status != Status::Done)
                .map(|r| r.size.saturating_sub(r.downloaded))
                .sum();
            (
                shown.len(),
                running,
                speed,
                conns,
                remaining,
                bulk_action(&shown),
            )
        };
        let mut new_download = false;
        let mut toggle_all = false;
        let window_h = ui.ctx().content_rect().height();
        let window_w = ui.ctx().content_rect().width();
        let layout = head_layout(window_h);
        let globally_empty = self.rows.is_empty();

        egui::Panel::top("head")
            .exact_size(layout.height)
            .show_separator_line(false)
            .frame(egui::Frame::NONE.fill(theme::pal().surface))
            .show(ui, |ui| {
                // 覆盖式详情栏在场时，页头与概览条只排到看得见的那一段为止。
                let w = (ui.available_width() - inset).max(0.0);
                let (rect, _) = ui.allocate_exact_size(vec2(w, layout.height), Sense::hover());
                let pad = if card_mode(window_w) {
                    16.0
                } else {
                    theme::PAD
                };
                let p = ui.painter();

                let title =
                    theme::galley(ui, self.filter.title(), theme::F_HEAD, theme::pal().ink, w);
                p.galley(
                    pos2(rect.left() + pad, rect.top() + layout.title_y),
                    title,
                    theme::pal().ink,
                );
                if let Some(y) = layout.subtitle_y {
                    let sub = format!(
                        "{total} 个任务 · {running} 个进行中 · 合计 {}",
                        crate::fmt::speed(speed)
                    );
                    p.text(
                        pos2(rect.left() + pad, rect.top() + y),
                        Align2::LEFT_TOP,
                        sub,
                        FontId::proportional(theme::F_SUB),
                        theme::pal().ink3,
                    );
                }

                // 概览条：SUNKEN + r12，三格，格间 1px 竖线。
                let band = Rect::from_min_size(
                    pos2(rect.left() + pad, rect.top() + layout.band_y),
                    vec2((w - pad * 2.0).max(0.0), layout.band_h),
                );
                p.rect_filled(
                    band,
                    CornerRadius::same(if layout.density == HeadDensity::Full {
                        12
                    } else {
                        8
                    }),
                    theme::pal().sunken,
                );
                let divider_inset = if layout.density == HeadDensity::Full {
                    14.0
                } else {
                    6.0
                };
                for i in 1..3 {
                    let x = band.left() + band.width() * (i as f32) / 3.0;
                    p.vline(
                        x,
                        band.top() + divider_inset..=band.bottom() - divider_inset,
                        Stroke::new(1.0, theme::pal().line),
                    );
                }
                let cells = [
                    ("当前速度", crate::fmt::speed(speed)),
                    ("活动连接", conns.to_string()),
                    ("剩余", crate::fmt::bytes(remaining)),
                ];
                for (i, (label, value)) in cells.iter().enumerate() {
                    let x = band.left() + band.width() * (i as f32) / 3.0 + 16.0;
                    if layout.density == HeadDensity::Full {
                        p.text(
                            pos2(x, band.top() + 14.0),
                            Align2::LEFT_TOP,
                            *label,
                            FontId::proportional(theme::F_LABEL),
                            theme::pal().ink4,
                        );
                        p.text(
                            pos2(x, band.top() + 33.0),
                            Align2::LEFT_TOP,
                            value,
                            FontId::proportional(theme::F_STAT),
                            theme::pal().ink,
                        );
                    } else {
                        p.text(
                            pos2(x, band.center().y),
                            Align2::LEFT_CENTER,
                            format!("{label}  {value}"),
                            FontId::proportional(theme::F_LABEL),
                            theme::pal().ink3,
                        );
                    }
                    if layout.density == HeadDensity::Full && i == 0 && band.width() / 3.0 >= 190.0
                    {
                        let cell_w = band.width() / 3.0;
                        let chart = Rect::from_min_size(
                            pos2(band.left() + cell_w - 116.0, band.top() + 20.0),
                            vec2(100.0, 28.0),
                        );
                        let _ = speed_sparkline(p, chart, &self.speed_history, theme::pal().ink3);
                    }
                }

                // 空态的唯一主操作在投放卡里，页头不再重复“新建下载”。
                if !globally_empty {
                    let cy = rect.top() + layout.title_y + 15.0;
                    let primary = "新建下载";
                    let prim_rect = Rect::from_min_size(
                        pos2(rect.right() - pad - theme::BTN_W, cy - theme::BTN_H / 2.0),
                        vec2(theme::BTN_W, theme::BTN_H),
                    );
                    if let Some(action) = &bulk {
                        let ghost = action.label();
                        let ghost_rect = Rect::from_min_size(
                            pos2(
                                prim_rect.left() - 8.0 - theme::BTN_W,
                                cy - theme::BTN_H / 2.0,
                            ),
                            vec2(theme::BTN_W, theme::BTN_H),
                        );
                        if head_btn(
                            ui,
                            ghost_rect,
                            ghost,
                            Some(action.icon()),
                            false,
                            "head_all",
                        )
                        .clicked()
                        {
                            toggle_all = true;
                        }
                    }
                    if head_btn(ui, prim_rect, primary, Some(Icon::Plus), true, "head_new")
                        .clicked()
                    {
                        new_download = true;
                    }
                }
            });

        if new_download {
            self.open_add_text(String::new(), AddOrigin::Manual, Vec::new());
        }
        if toggle_all && let Some(action) = bulk {
            let count = action.ids.len();
            for id in action.ids {
                self.toggle_pause(id);
            }
            self.toast(
                match action.kind {
                    BulkKind::Pause => format!("已暂停当前视图中的 {count} 个任务"),
                    BulkKind::Resume => format!("已继续当前视图中的 {count} 个任务"),
                },
                false,
            );
        }
    }

    // ---------------------------------------------------------------- 页脚

    /// 主列表区专属页脚：左栏和详情栏贯通到窗口底部，不经过这里。
    fn foot(&mut self, ui: &mut egui::Ui, inset: f32) {
        let active = self
            .rows
            .iter()
            .filter(|r| matches!(r.status, Status::Active | Status::Probing | Status::Seeding))
            .count();
        let clipboard = self.settings.clipboard;
        let limit = self.settings.limit;
        egui::Panel::bottom("foot")
            .exact_size(theme::FOOT_H)
            .show_separator_line(false)
            .frame(egui::Frame::NONE.fill(theme::pal().surface))
            .show(ui, |ui| {
                let w = (ui.available_width() - inset).max(0.0);
                let (rect, _) = ui.allocate_exact_size(vec2(w, theme::FOOT_H), Sense::hover());
                let p = ui.painter();
                p.hline(
                    rect.x_range(),
                    rect.top() + 0.5,
                    Stroke::new(1.0, theme::pal().line),
                );
                let left = format!(
                    "{}    {} 个活动任务 · 队列上限 {}",
                    if clipboard {
                        "剪贴板监视已开启"
                    } else {
                        "剪贴板监视已关闭"
                    },
                    active,
                    limit
                );
                p.circle_filled(
                    pos2(rect.left() + 18.0, rect.center().y),
                    3.0,
                    if clipboard {
                        theme::pal().ok
                    } else {
                        theme::pal().ink4
                    },
                );
                p.text(
                    pos2(rect.left() + 30.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    left,
                    FontId::proportional(theme::F_FOOT),
                    theme::pal().ink2,
                );
                p.text(
                    pos2(rect.right() - 16.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    format!("v{}", env!("CARGO_PKG_VERSION")),
                    FontId::proportional(theme::F_FOOT),
                    theme::pal().ink2,
                );
            });
    }

    fn add_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut add) = self.adding.take() else {
            return;
        };
        let paste_requested = ctx.input(|i| i.modifiers.command && i.key_pressed(Key::V))
            || crate::clipboard::ctrl_v_down();
        let event_paste = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Paste(text) => Some(text.clone()),
                _ => None,
            })
        });
        let pasted = event_paste.or_else(|| {
            if paste_requested {
                crate::clipboard::read_now()
            } else {
                None
            }
        });
        if pasted.is_some() {
            // 这个事件在下面由添加栏自己处理，不能再交给 TextEdit 追加一次。
            ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Paste(_))));
        }
        if let Some(text) = pasted {
            add.text = paste_links(&text);
            add.preview = AddPreview::Empty;
            add.checked.clear();
            add.preview_due = Instant::now() + Duration::from_millis(450);
        }
        let mut submit = false;
        let mut retry_preview = false;
        let mut close = false;
        let wheel_scroll = ctx.input(|i| i.smooth_scroll_delta.y.abs() > 0.01);
        let pointer = ctx.pointer_latest_pos();
        close |= modal_mask(ctx, "new-download-mask");
        let screen = ctx.content_rect();
        let safe = Rect::from_min_max(
            pos2(screen.left() + 8.0, screen.top() + theme::TB_H + 8.0),
            pos2(screen.right() - 8.0, screen.bottom() - theme::FOOT_H - 8.0),
        );
        let width = safe.width().min(640.0).max(320.0);
        let preferred_h = match add.origin {
            AddOrigin::TorrentDrop => 524.0,
            _ => 548.0,
        };
        let height = safe.height().max(240.0).min(preferred_h);
        let dy = safe.center().y - screen.center().y;
        egui::Window::new("new-download")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .fixed_size(vec2(width, height))
            .anchor(Align2::CENTER_CENTER, vec2(0.0, dy))
            .order(egui::Order::Foreground)
            .frame(dialog_frame_with_margin(0))
            .show(ctx, |ui| {
                ui.set_width(width);
                egui::Panel::top("add-dialog-header")
                    .exact_size(68.0)
                    .show_separator_line(false)
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        close |=
                            dialog_header(ui, ui.available_width(), "新建下载任务", "关闭新建下载");
                        let hint = match add.origin {
                            AddOrigin::Clipboard => "已从剪贴板识别链接",
                            AddOrigin::Browser => "已拦截浏览器下载，可确认后加入",
                            AddOrigin::TorrentDrop => "已读取种子文件，请选择要下载的内容",
                            AddOrigin::Manual => "粘贴链接、磁力链接，或拖入 .torrent 文件",
                        };
                        let header = ui.max_rect();
                        ui.painter().text(
                            pos2(header.left() + 16.0, header.top() + 55.0),
                            Align2::LEFT_CENTER,
                            hint,
                            FontId::proportional(theme::F_META),
                            theme::pal().ink3,
                        );
                    });
                egui::Panel::bottom("add-dialog-footer")
                    .exact_size(62.0)
                    .show_separator_line(false)
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        ui.add_space(10.0);
                        egui::Frame::NONE
                            .inner_margin(Margin::symmetric(16, 0))
                            .show(ui, |ui| {
                                let (footer, _) = ui.allocate_exact_size(
                                    vec2(ui.available_width(), theme::BTN_H),
                                    Sense::hover(),
                                );
                                let skip = self.settings.skip_add_dialog;
                                let skip_rect =
                                    Rect::from_min_size(footer.min, vec2(118.0, theme::BTN_H));
                                let skip_resp = ui.interact(
                                    skip_rect,
                                    egui::Id::unique("skip-new-download-dialog"),
                                    Sense::click(),
                                );
                                skip_resp.widget_info(|| {
                                    egui::WidgetInfo::selected(
                                        egui::Role::CheckBox,
                                        true,
                                        skip,
                                        "不再显示",
                                    )
                                });
                                if skip_resp.clicked() {
                                    self.settings.skip_add_dialog = !skip;
                                    self.mark_dirty();
                                }
                                let p = ui.painter().clone();
                                paint_add_check(
                                    &p,
                                    pos2(skip_rect.left() + 8.0, skip_rect.center().y),
                                    skip,
                                );
                                p.text(
                                    pos2(skip_rect.left() + 22.0, skip_rect.center().y),
                                    Align2::LEFT_CENTER,
                                    "不再显示",
                                    FontId::proportional(theme::F_META),
                                    theme::pal().ink2,
                                );
                                if skip_resp.has_focus() {
                                    p.rect_stroke(
                                        skip_rect.shrink(2.0),
                                        CornerRadius::same(6),
                                        Stroke::new(1.5, theme::pal().accent),
                                        StrokeKind::Inside,
                                    );
                                }
                                let button = Rect::from_min_size(
                                    pos2(footer.right() - theme::BTN_W, footer.top()),
                                    vec2(theme::BTN_W, theme::BTN_H),
                                );
                                if action_button(
                                    ui,
                                    button,
                                    "立即下载",
                                    DialogButtonKind::Primary,
                                    add_can_submit(&add),
                                    "立即下载",
                                )
                                .clicked()
                                {
                                    submit = true;
                                }
                            });
                    });
                let body_rect = ui.available_rect_before_wrap();
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(body_rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                    |ui| {
                        egui::Frame::NONE
                            .inner_margin(Margin::symmetric(16, 8))
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                                if add.origin == AddOrigin::TorrentDrop {
                                    add_torrent_source(ui, &add.text);
                                } else {
                                    let editor_h = 76.0;
                                    let resp = ui
                                        .scope(|ui| {
                                            ui.visuals_mut().weak_text_color =
                                                Some(theme::pal().hint);
                                            ui.add_sized(
                                                vec2(ui.available_width(), editor_h),
                                                TextEdit::multiline(&mut add.text)
                                                    .hint_text("每行一个链接；也可以粘贴磁力链接。")
                                                    .font(FontId::proportional(14.0))
                                                    .desired_width(f32::INFINITY)
                                                    .frame(
                                                        egui::Frame::NONE
                                                            .fill(theme::pal().sunken)
                                                            .stroke(Stroke::new(
                                                                1.0,
                                                                theme::pal().line,
                                                            ))
                                                            .corner_radius(CornerRadius::same(10))
                                                            .inner_margin(Margin::symmetric(
                                                                12, 10,
                                                            )),
                                                    ),
                                            )
                                        })
                                        .inner;
                                    if resp.changed() {
                                        add.preview = AddPreview::Empty;
                                        add.checked.clear();
                                        add.preview_due =
                                            Instant::now() + Duration::from_millis(450);
                                    }
                                    if resp.has_focus() {
                                        ui.painter().rect_stroke(
                                            resp.rect,
                                            CornerRadius::same(10),
                                            Stroke::new(1.5, theme::pal().ink3),
                                            StrokeKind::Inside,
                                        );
                                    }
                                    if add.fresh {
                                        resp.request_focus();
                                        add.fresh = false;
                                    }
                                    if resp.has_focus()
                                        && ui.input(|i| {
                                            i.modifiers.command && i.key_pressed(Key::Enter)
                                        })
                                        && add_can_submit(&add)
                                    {
                                        submit = true;
                                    }
                                    let state =
                                        egui::text_edit::TextEditState::load(ui.ctx(), resp.id)
                                            .unwrap_or_default();
                                    let current_range =
                                        state.cursor.char_range().unwrap_or_default();
                                    let has_selection = !current_range.is_empty();
                                    let can_undo =
                                        state.undoer().has_undo(&(current_range, add.text.clone()));
                                    let mut menu_action = None;
                                    let _ = resp.context_menu(|menu| {
                                        style_context_menu(menu);
                                        if context_menu_item(menu, "撤销", can_undo).clicked() {
                                            menu.close();
                                            menu_action = Some(TextMenuAction::Undo);
                                        }
                                        if context_menu_item(menu, "剪切", has_selection).clicked()
                                        {
                                            menu.close();
                                            menu_action = Some(TextMenuAction::Cut);
                                        }
                                        if context_menu_item(menu, "复制", has_selection).clicked()
                                        {
                                            menu.close();
                                            menu_action = Some(TextMenuAction::Copy);
                                        }
                                        if context_menu_item(menu, "粘贴", true).clicked() {
                                            menu.close();
                                            menu_action = Some(TextMenuAction::Paste);
                                        }
                                        if context_menu_item(menu, "删除", has_selection).clicked()
                                        {
                                            menu.close();
                                            menu_action = Some(TextMenuAction::Delete);
                                        }
                                        if context_menu_item(menu, "全选", !add.text.is_empty())
                                            .clicked()
                                        {
                                            menu.close();
                                            menu_action = Some(TextMenuAction::SelectAll);
                                        }
                                    });
                                    if let Some(action) = menu_action {
                                        let immediate = apply_text_menu_action(
                                            ui.ctx(),
                                            resp.id,
                                            &mut add.text,
                                            action,
                                        );
                                        add.fresh = false;
                                        if immediate {
                                            submit = true;
                                        }
                                        if add.text.ends_with('\n') {
                                            add.preview_due =
                                                Instant::now() + Duration::from_millis(450);
                                        }
                                    }
                                    let editor_scrolling = wheel_scroll
                                        && pointer.is_some_and(|p| resp.rect.contains(p));
                                    if !scroll_activity(
                                        ui,
                                        "add-editor-scroll",
                                        resp.rect,
                                        editor_scrolling,
                                        true,
                                    ) {
                                        let cover = Rect::from_min_max(
                                            pos2(resp.rect.right() - 12.0, resp.rect.top() + 1.0),
                                            pos2(resp.rect.right() - 1.0, resp.rect.bottom() - 1.0),
                                        );
                                        ui.painter().rect_filled(
                                            cover,
                                            CornerRadius::same(9),
                                            theme::pal().sunken,
                                        );
                                    }
                                }
                                ui.add_space(10.0);
                                retry_preview |= add_preview_card(ui, &mut add, 260.0);
                                ui.add_space(12.0);
                                let (dir_row, _) = ui.allocate_exact_size(
                                    vec2(ui.available_width(), 42.0),
                                    Sense::hover(),
                                );
                                ui.painter().text(
                                    pos2(dir_row.left(), dir_row.center().y),
                                    Align2::LEFT_CENTER,
                                    "下载到：",
                                    FontId::proportional(theme::F_META),
                                    theme::pal().ink,
                                );
                                let dir_rect = Rect::from_min_size(
                                    pos2(dir_row.left() + 54.0, dir_row.top()),
                                    vec2(dir_row.width() - 54.0, dir_row.height()),
                                );
                                let dir_resp = ui.interact(
                                    dir_rect,
                                    egui::Id::unique("add-dialog-dir"),
                                    Sense::click(),
                                );
                                ui.painter().rect_filled(
                                    dir_rect,
                                    CornerRadius::same(8),
                                    if dir_resp.hovered() {
                                        theme::pal().hover
                                    } else {
                                        theme::pal().sunken
                                    },
                                );
                                dir_resp.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::Role::Button,
                                        ui.is_enabled(),
                                        "更改下载目录",
                                    )
                                });
                                theme::paint_icon(
                                    ui.painter(),
                                    pos2(dir_rect.left() + 18.0, dir_rect.center().y),
                                    theme::pal().ink,
                                    Icon::Folder,
                                );
                                let dir_text = self.settings.dir.to_string_lossy().to_string();
                                let g = theme::galley(
                                    ui,
                                    &dir_text,
                                    theme::F_META,
                                    theme::pal().ink,
                                    dir_rect.width() - 104.0,
                                );
                                ui.painter().galley(
                                    pos2(dir_rect.left() + 38.0, dir_rect.center().y - 8.0),
                                    g,
                                    theme::pal().ink,
                                );
                                let change = "更改";
                                ui.painter().text(
                                    pos2(dir_rect.right() - 12.0, dir_rect.center().y),
                                    Align2::RIGHT_CENTER,
                                    change,
                                    FontId::proportional(theme::F_META),
                                    theme::pal().ink3,
                                );
                                if dir_resp.has_focus() {
                                    ui.painter().rect_stroke(
                                        dir_rect,
                                        CornerRadius::same(8),
                                        Stroke::new(1.5, theme::pal().accent),
                                        StrokeKind::Inside,
                                    );
                                }
                                dir_resp.clone().on_hover_text("更改下载目录");
                                if dir_resp.clicked()
                                    && let Some(dir) = pick_folder("选择下载目录")
                                {
                                    let fallback = self.paths.home.join("downloads");
                                    let (dir, note) = writable_download_dir(&dir, &fallback);
                                    if let Some(note) = note {
                                        self.toast(note, true);
                                    }
                                    self.settings.dir = dir.clone();
                                    self.dir_edit = dir.to_string_lossy().to_string();
                                    self.send(Cmd::SetDir(dir));
                                    self.mark_dirty();
                                }
                            });
                    },
                );
            });
        let target = first_add_target(&add.text);
        if retry_preview || (target != add.preview_for && Instant::now() >= add.preview_due) {
            self.begin_add_preview(&mut add, &target);
        } else if target != add.preview_for {
            ctx.request_repaint_after(add.preview_due.saturating_duration_since(Instant::now()));
        }
        if submit && add_can_submit(&add) {
            self.commit_add(add);
        } else if close {
            self.cancel_add_preview(&mut add);
        } else {
            self.adding = Some(add);
        }
    }

    fn delete_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut prompt) = self.delete_prompt.take() else {
            return;
        };
        let Some(name) = self.row(prompt.id).map(|r| r.display_name().to_owned()) else {
            return;
        };
        let mut close = false;
        let mut action = None;
        close |= modal_mask(ctx, "delete-download-mask");
        let screen = ctx.content_rect();
        let safe = Rect::from_min_max(
            pos2(screen.left() + 8.0, screen.top() + theme::TB_H + 8.0),
            pos2(screen.right() - 8.0, screen.bottom() - theme::FOOT_H - 8.0),
        );
        let width = safe.width().min(500.0).max(320.0);
        let dy = safe.center().y - screen.center().y;
        egui::Window::new("delete-download")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, dy))
            .order(egui::Order::Foreground)
            .frame(dialog_frame())
            .show(ctx, |ui| {
                ui.set_width(width - 36.0);
                let title = match prompt.intent {
                    DeleteIntent::Task => "删除下载",
                    DeleteIntent::Files => "彻底删除文件",
                };
                close |= dialog_header(ui, width - 36.0, title, "关闭删除确认");
                ui.add_space(12.0);
                ui.label(
                    RichText::new(name)
                        .size(theme::F_KV)
                        .color(theme::pal().ink),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new("删除任务只从列表移除并保留文件；彻底删除文件会永久删除下载内容，无法撤销。")
                        .size(theme::F_META)
                        .color(theme::pal().ink3),
                );
                ui.add_space(8.0);
                check_row(ui, "以后普通删除不再提示", &mut prompt.dont_ask);
                ui.add_space(12.0);
                let (row, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), theme::BTN_H), Sense::hover());
                let permanent = Rect::from_min_size(
                    pos2(row.right() - theme::BTN_W_WIDE, row.top()),
                    vec2(theme::BTN_W_WIDE, theme::BTN_H),
                );
                let delete_task = Rect::from_min_size(
                    pos2(permanent.left() - 8.0 - theme::BTN_W, row.top()),
                    vec2(theme::BTN_W, theme::BTN_H),
                );
                let cancel = Rect::from_min_size(
                    pos2(delete_task.left() - 8.0 - theme::BTN_W, row.top()),
                    vec2(theme::BTN_W, theme::BTN_H),
                );
                if action_button(
                    ui,
                    cancel,
                    "取消",
                    DialogButtonKind::Ghost,
                    true,
                    "取消删除",
                )
                .clicked()
                {
                    close = true;
                }
                if action_button(
                    ui,
                    delete_task,
                    "删除任务",
                    DialogButtonKind::Secondary,
                    true,
                    "删除任务",
                )
                .clicked()
                {
                    action = Some(false);
                }
                if action_button(
                    ui,
                    permanent,
                    "彻底删除文件",
                    DialogButtonKind::Danger,
                    true,
                    "彻底删除文件",
                )
                .clicked()
                {
                    action = Some(true);
                }
            });
        if let Some(delete_files) = action {
            if prompt.dont_ask && self.settings.confirm_delete {
                self.settings.confirm_delete = false;
                self.mark_dirty();
            }
            self.remove(prompt.id, delete_files);
        } else if !close {
            self.delete_prompt = Some(prompt);
        }
    }

    // ---------------------------------------------------------------- 列表

    fn list(&mut self, ui: &mut egui::Ui) {
        let selected = self.selected;
        let mut click_row = None;
        let mut double_row = None;
        let mut focus_row = None;
        let mut context_action = None;
        let mut act: Option<(u64, Action)> = None;
        let mut open_add = false;
        let mut show_all = false;
        {
            let filter = self.filter;
            let rows: Vec<&Row> = self.rows.iter().filter(|r| filter.matches(r)).collect();
            let has_speed = rows.iter().any(|row| row.speed >= 1.0);
            let born = &self.born;
            // 覆盖式详情栏（1040–1159）盖住右边 312：列要按**看得见**的那段重排，
            // 否则那条 312 的边界会正好切在百分比或胶囊中间。
            let win_w = ui.ctx().content_rect().width();
            let cards = card_mode(win_w);
            let inset = if detail_is_peek(win_w, selected.is_some()) {
                theme::INS_W
            } else {
                0.0
            };
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::NONE
                        .fill(theme::pal().surface)
                        .inner_margin(Margin::symmetric(if cards { 16 } else { 24 }, 0)),
                )
                .show(ui, |ui| {
                    if rows.is_empty() {
                        if self.rows.is_empty() {
                            open_add = empty_state(ui);
                        } else {
                            show_all = no_match(ui, filter.title());
                        }
                        return;
                    }
                    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                    // 概览条和表头之间留一口气（§5：概览条 64 之后是表头 26）。
                    ui.add_space(if cards { 8.0 } else { 12.0 });
                    if !cards {
                        let (head, _) = ui.allocate_exact_size(
                            vec2(ui.available_width(), theme::THEAD_H),
                            Sense::hover(),
                        );
                        table_head(
                            ui,
                            head,
                            &cols(head.right() - 12.0 - inset, head.width() - inset, has_speed),
                        );
                    }
                    let row_h = if cards { CARD_H } else { theme::ROW_H };
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show_rows(ui, row_h, rows.len(), |ui, range| {
                            for i in range {
                                let row = &rows[i];
                                let born = &born;
                                let alpha = born
                                    .get(&row.id)
                                    .map(|t| (t.elapsed().as_secs_f32() / 0.12).clamp(0.0, 1.0))
                                    .unwrap_or(1.0);
                                if alpha < 1.0 {
                                    ui.ctx().request_repaint_after(Duration::from_millis(16));
                                }
                                let width = ui.available_width();
                                let (rect, _) =
                                    ui.allocate_exact_size(vec2(width, row_h), Sense::hover());
                                // 固定 id 才记得住右键菜单开没开：自动 id 会随悬停按钮漂移。
                                let resp = ui.interact(
                                    rect,
                                    egui::Id::unique(("row", row.id)),
                                    Sense::click(),
                                );
                                resp.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::Role::Button,
                                        true,
                                        format!("{}，{}", row.display_name(), row.status.label()),
                                    )
                                });
                                if resp.clicked() {
                                    click_row = Some(row.id);
                                }
                                if resp.secondary_clicked() {
                                    click_row = Some(row.id);
                                }
                                let _ = resp.context_menu(|menu| {
                                    if let Some(action) = row_menu_action(menu, row) {
                                        context_action = Some((row.id, action));
                                    }
                                });
                                if cards {
                                    let _ = resp.clone().on_hover_text(row.display_name());
                                }
                                if resp.double_clicked() {
                                    double_row = Some(row.id);
                                }
                                if resp.has_focus() {
                                    focus_row = Some(row.id);
                                }
                                let pointer_in = ui.rect_contains_pointer(rect);
                                let is_sel = selected == Some(row.id);
                                if cards {
                                    paint_card(ui, rect, row, is_sel, pointer_in, alpha);
                                } else {
                                    let c = cols(
                                        rect.right() - 12.0 - inset,
                                        rect.width() - inset,
                                        has_speed,
                                    );
                                    paint_row(ui, rect, row, &c, is_sel, pointer_in, alpha);
                                    if let Some(a) =
                                        row_buttons(ui, rect, row, &c, pointer_in || is_sel, is_sel)
                                    {
                                        act = Some((row.id, a));
                                    }
                                }
                                if resp.has_focus() {
                                    ui.painter().rect_stroke(
                                        rect.shrink(2.0),
                                        CornerRadius::same(6),
                                        Stroke::new(1.5, theme::pal().accent),
                                        StrokeKind::Inside,
                                    );
                                }
                            }
                        });
                });
        }
        if show_all {
            self.filter = Filter::All;
            self.selected = None;
        }
        if open_add {
            self.open_add_text(String::new(), AddOrigin::Manual, Vec::new());
        }
        if let Some((id, action)) = context_action {
            match action {
                RowMenuAction::Start => self.menu_start(id),
                RowMenuAction::Pause => self.toggle_pause(id),
                RowMenuAction::OpenFile => self.open_row(id),
                RowMenuAction::OpenFolder => {
                    if let Some(dest) = self.row(id).and_then(|r| r.dest.clone()) {
                        open_path(&dest, true);
                    }
                }
                RowMenuAction::Delete => self.request_delete(id, DeleteIntent::Task),
                RowMenuAction::PermanentDelete => self.request_delete(id, DeleteIntent::Files),
                RowMenuAction::Copy => self.copy_link(ui.ctx(), id),
            }
        }
        if let Some(id) = click_row {
            self.selected = Some(id);
        }
        if let Some(id) = focus_row {
            self.selected = Some(id);
        }
        if let Some((id, a)) = act {
            match a {
                Action::Toggle => self.toggle_pause(id),
                Action::Delete => self.request_delete(id, DeleteIntent::Task),
                Action::Retry => {
                    if let Some(row) = self.row(id).cloned() {
                        self.restart(id, &row);
                    }
                }
            }
        }
        if let Some(id) = double_row {
            self.selected = Some(id);
            if self.row(id).is_some_and(|row| row.status == Status::Done) {
                self.open_row(id);
            }
        }
    }

    // ---------------------------------------------------------------- 选文件

    /// 多文件种子的扁平勾选清单（PRD 5.3）：不建树，就是一个带复选框的列表。
    fn pick_panel(&mut self, ctx: &egui::Context) {
        let Some(mut pick) = self.pick.take() else {
            return;
        };
        let name = self
            .row(pick.id)
            .map(|r| r.display_name())
            .unwrap_or_default();
        let mut start = false;
        let mut defer = false;
        let mut close = false;
        close |= modal_mask(ctx, "pick-files-mask");
        let screen = ctx.content_rect();
        let safe = Rect::from_min_max(
            pos2(screen.left() + 8.0, screen.top() + theme::TB_H + 8.0),
            pos2(screen.right() - 8.0, screen.bottom() - theme::FOOT_H - 8.0),
        );
        let width = safe.width().min(440.0).max(300.0);
        let height = safe.height().min(500.0).max(180.0);
        let scroll_h = (height - 36.0 - 44.0 - 22.0 - 22.0 - 40.0 - 8.0).max(48.0);
        let dy = safe.center().y - screen.center().y;

        egui::Window::new("pick-files")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .fixed_size(vec2(width, height))
            .anchor(Align2::CENTER_CENTER, vec2(0.0, dy))
            .frame(dialog_frame())
            .show(ctx, |ui| {
                ui.set_width(width - 36.0);
                close |= dialog_header(ui, width - 36.0, "选择要下载的文件", "关闭文件选择");
                let (sub_rect, sub_resp) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
                let sub = format!("{} · 共 {} 个文件", name, pick.files.len());
                let g = theme::galley(ui, &sub, theme::F_META, theme::pal().ink3, sub_rect.width());
                ui.painter().galley(
                    pos2(sub_rect.left(), sub_rect.center().y - g.size().y / 2.0),
                    g,
                    theme::pal().ink3,
                );
                sub_resp.on_hover_text(&sub);
                ui.add_space(8.0);
                let mut all = pick.checked.iter().all(|checked| *checked);
                if check_row(ui, "全选", &mut all).clicked() {
                    pick.checked.fill(all);
                }
                ui.add_space(4.0);

                egui::ScrollArea::vertical()
                    .max_height(scroll_h)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let w = ui.available_width();
                        for (i, f) in pick.files.iter().enumerate() {
                            let (rect, resp) =
                                ui.allocate_exact_size(vec2(w, 30.0), Sense::click());
                            if ui.rect_contains_pointer(rect) {
                                ui.painter().rect_filled(
                                    rect,
                                    CornerRadius::same(6),
                                    theme::pal().hover,
                                );
                            }
                            if resp.clicked() {
                                pick.checked[i] = !pick.checked[i];
                            }
                            resp.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::Role::CheckBox,
                                    true,
                                    pick.checked[i],
                                    &f.name,
                                )
                            });
                            let box_rect = Rect::from_center_size(
                                pos2(rect.left() + 12.0, rect.center().y),
                                vec2(15.0, 15.0),
                            );
                            let p = ui.painter();
                            p.rect_stroke(
                                box_rect,
                                CornerRadius::same(4),
                                Stroke::new(
                                    1.2,
                                    if pick.checked[i] {
                                        theme::pal().accent
                                    } else {
                                        theme::pal().ink3
                                    },
                                ),
                                StrokeKind::Inside,
                            );
                            if pick.checked[i] {
                                p.rect_filled(
                                    box_rect.shrink(3.0),
                                    CornerRadius::same(2),
                                    theme::pal().accent,
                                );
                            }
                            let size = crate::fmt::bytes(f.len);
                            let size_w = text_w(ui, &size, 12.0) + 12.0;
                            let text_max = (w - 40.0 - size_w).max(40.0);
                            let g = theme::galley(ui, &f.name, 12.5, theme::pal().ink, text_max);
                            ui.painter().galley(
                                pos2(rect.left() + 28.0, rect.center().y - 7.5),
                                g,
                                theme::pal().ink,
                            );
                            ui.painter().text(
                                pos2(rect.right() - 4.0, rect.center().y),
                                Align2::RIGHT_CENTER,
                                size,
                                FontId::proportional(12.0),
                                theme::pal().ink4,
                            );
                            if resp.has_focus() {
                                ui.painter().rect_stroke(
                                    rect,
                                    CornerRadius::same(6),
                                    Stroke::new(1.5, theme::pal().accent),
                                    StrokeKind::Inside,
                                );
                            }
                            resp.on_hover_text(&f.name);
                        }
                    });

                ui.add_space(8.0);
                let checked: Vec<usize> = pick
                    .checked
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| **c)
                    .map(|(i, _)| i)
                    .collect();
                let total: u64 = checked.iter().map(|i| pick.files[*i].len).sum();
                let (foot, _) = ui
                    .allocate_exact_size(vec2(ui.available_width(), theme::BTN_H), Sense::hover());
                ui.painter().text(
                    pos2(foot.left(), foot.center().y),
                    Align2::LEFT_CENTER,
                    format!("已选 {} 个 · {}", checked.len(), crate::fmt::bytes(total)),
                    FontId::proportional(theme::F_META),
                    theme::pal().ink3,
                );
                let go = Rect::from_min_size(
                    pos2(foot.right() - theme::BTN_W, foot.top()),
                    vec2(theme::BTN_W, theme::BTN_H),
                );
                let later = Rect::from_min_size(
                    pos2(go.left() - 8.0 - theme::BTN_W, foot.top()),
                    vec2(theme::BTN_W, theme::BTN_H),
                );
                if action_button(
                    ui,
                    later,
                    "稍后选择",
                    DialogButtonKind::Secondary,
                    true,
                    "稍后选择",
                )
                .clicked()
                {
                    defer = true;
                }
                if action_button(
                    ui,
                    go,
                    "开始下载",
                    DialogButtonKind::Primary,
                    !checked.is_empty(),
                    "开始下载",
                )
                .clicked()
                {
                    start = true;
                }
            });

        if start {
            if let Some(only) = only_for_checked(&pick.checked) {
                if let Some(r) = self.rows.iter_mut().find(|r| r.id == pick.id) {
                    r.only = only;
                }
                self.start_torrent(pick.id);
                return;
            }
        }
        if defer || close {
            self.defer_file_pick(pick.id);
            return;
        }
        self.pick = Some(pick);
    }

    // ---------------------------------------------------------------- 详情

    /// §4 详情栏 312：36 图标 + 文件名两行 + 胶囊 + 传输/来源/保存到三节 + 底部固定按钮。
    /// §4 的详情栏：≥1160 时是固定的一列（列表让出 312）。
    fn detail(&mut self, ui: &mut egui::Ui) {
        let Some(row) = self.selected.and_then(|id| self.row(id)).cloned() else {
            return;
        };
        let ctx = ui.ctx().clone();
        let mut out: (Option<DetailAction>, bool) = (None, false);
        egui::Panel::right("detail")
            .exact_size(theme::INS_W)
            .show_separator_line(false)
            .frame(egui::Frame::NONE.fill(theme::pal().surface))
            .show(ui, |ui| {
                let (rect, _) = ui.allocate_exact_size(
                    vec2(ui.available_width(), ui.available_height()),
                    Sense::hover(),
                );
                out = Self::detail_body(ui, rect, &row);
            });
        self.detail_done(&ctx, &row, out.0, out.1);
    }

    /// §5 的 860–1039 档：这一段没有固定栏的位置，选中行时详情栏**从右滑入**、盖住列表
    /// 右侧 312。140 ms 的位移靠 `Area::fixed_pos` 推——内容整体跟着走，不用改任何坐标。
    /// 淡入时不给列表让位，滑完就停在上面，点 X 关掉。
    fn detail_overlay(&mut self, ctx: &egui::Context, win_w: f32) {
        let Some(row) = self.selected.and_then(|id| self.row(id)).cloned() else {
            return;
        };
        let screen = ctx.content_rect();
        let top = screen.top() + theme::TB_H;
        let h = (screen.bottom() - top).max(0.0);
        // 1040–1159：列表还有 672，给详情偷看 312，剩下的列表照常看得到。
        // 860–1039：左栏就吃掉 232，再偷看只剩 356——那点宽度连一行都排不下，
        // 索性整片盖上（这一段本来也只能显示一半的列）。
        let w = if win_w >= 1040.0 {
            theme::INS_W
        } else {
            (screen.right() - screen.left() - rail_width(win_w)).max(theme::INS_W)
        };
        let t = (self.detail_at.elapsed().as_secs_f32() / 0.14).clamp(0.0, 1.0);
        if t < 1.0 {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        let x = screen.right() - w * t;
        let mut out: (Option<DetailAction>, bool) = (None, false);
        egui::Area::new(egui::Id::unique("detail-overlay"))
            .order(egui::Order::Middle)
            .fixed_pos(pos2(x, top))
            .show(ctx, |ui| {
                // click 感知是为了把落在空处的点击吃掉，否则会穿到下面的列表上换选中行。
                let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::click());
                ui.painter()
                    .rect_filled(rect, CornerRadius::ZERO, theme::pal().surface);
                out = Self::detail_body(ui, rect, &row);
            });
        self.detail_done(ctx, &row, out.0, out.1);
    }

    /// 详情栏上三个动作的处理，两种形态共用。
    fn detail_done(
        &mut self,
        ctx: &egui::Context,
        row: &Row,
        act: Option<DetailAction>,
        close: bool,
    ) {
        if close {
            self.selected = None;
        }
        match act {
            Some(DetailAction::Primary) => {
                if row.status == Status::Done {
                    self.open_row(row.id);
                } else {
                    self.toggle_pause(row.id);
                }
            }
            Some(DetailAction::Reveal) => {
                if let Some(dest) = &row.dest {
                    open_path(dest, true);
                }
            }
            Some(DetailAction::Copy) => self.copy_link(ctx, row.id),
            Some(DetailAction::OpenFile) => self.open_row(row.id),
            Some(DetailAction::Readd) => {
                if let Some(row) = self.row(row.id).cloned() {
                    self.restart(row.id, &row);
                }
            }
            Some(DetailAction::Delete) => self.request_delete(row.id, DeleteIntent::Files),
            None => {}
        }
    }

    /// 详情栏的内容：全部画在给定的 `rect` 里。固定栏与覆盖层共用这一份，
    /// 免得两套布局各漂各的。
    fn detail_body(ui: &mut egui::Ui, rect: Rect, row: &Row) -> (Option<DetailAction>, bool) {
        let mut act = None;
        let mut close = false;
        let body_rect = Rect::from_min_max(
            rect.min,
            pos2(
                rect.right(),
                (rect.bottom() - DETAIL_FOOTER_H).max(rect.top() + 1.0),
            ),
        );
        let scroll_bar_visibility = if body_rect.height() < DETAIL_SCROLL_MIN_H {
            egui::scroll_area::ScrollBarVisibility::AlwaysVisible
        } else {
            egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded
        };
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(body_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                if body_rect.height() < DETAIL_SCROLL_MIN_H {
                    let scroll = &mut ui.style_mut().spacing.scroll;
                    scroll.floating = true;
                    scroll.floating_width = 5.0;
                    scroll.floating_allocated_width = 5.0;
                    scroll.dormant_background_opacity = 0.0;
                    scroll.active_background_opacity = 0.0;
                    scroll.interact_background_opacity = 0.0;
                    scroll.dormant_handle_opacity = 1.0;
                    scroll.active_handle_opacity = 1.0;
                    scroll.interact_handle_opacity = 1.0;
                    let visuals = ui.visuals_mut();
                    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, theme::pal().ink4);
                    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, theme::pal().ink3);
                    visuals.widgets.active.fg_stroke = Stroke::new(1.0, theme::pal().ink2);
                }
                act = egui::ScrollArea::vertical()
                    .id_salt(("detail-scroll", row.id))
                    .scroll_bar_visibility(scroll_bar_visibility)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let content_h = ui.available_height().max(DETAIL_SCROLL_MIN_H);
                        let (content_rect, _) = ui.allocate_exact_size(
                            vec2(ui.available_width(), content_h),
                            Sense::hover(),
                        );
                        Self::detail_scrolled(ui, content_rect, row)
                    })
                    .inner;
            },
        );

        let x1 = rect.right() - 18.0;
        let close_rect = Rect::from_min_size(
            pos2(x1 - theme::BTN_ICON, rect.top() + 14.0),
            vec2(theme::BTN_ICON, theme::BTN_ICON),
        );
        let cr = ui.interact(close_rect, ui.scope_id().with("d_close"), Sense::click());
        if cr.hovered() || cr.has_focus() {
            ui.painter()
                .rect_filled(close_rect, CornerRadius::same(7), theme::pal().hover);
        }
        if cr.has_focus() {
            ui.painter().rect_stroke(
                close_rect,
                CornerRadius::same(7),
                Stroke::new(1.5, theme::pal().accent),
                StrokeKind::Inside,
            );
        }
        theme::paint_icon(
            ui.painter(),
            close_rect.center(),
            if cr.hovered() {
                theme::pal().ink
            } else {
                theme::pal().ink3
            },
            Icon::Close,
        );
        cr.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "关闭详情"));
        cr.clone().on_hover_text("关闭详情");
        if cr.clicked() {
            close = true;
        }

        ui.painter().vline(
            rect.left() + 0.5,
            rect.y_range(),
            Stroke::new(1.0, theme::pal().line),
        );
        ui.painter().hline(
            rect.x_range(),
            rect.top() + 0.5,
            Stroke::new(1.0, theme::pal().line),
        );
        if let Some(action) = Self::detail_footer(ui, rect, row) {
            act = Some(action);
        }
        (act, close)
    }

    fn detail_scrolled(ui: &mut egui::Ui, rect: Rect, row: &Row) -> Option<DetailAction> {
        let mut act: Option<DetailAction> = None;
        let x0 = rect.left() + 18.0;
        let x1 = rect.right() - 18.0;
        let row_step = 30.0;
        let section_step = 32.0;
        let section_gap = 10.0;
        let utility_y;

        let color = theme::status_color(row.status);
        let tile = Rect::from_min_size(pos2(x0, rect.top() + 18.0), vec2(52.0, 52.0));
        let close_rect = Rect::from_min_size(
            pos2(x1 - theme::BTN_ICON, rect.top() + 14.0),
            vec2(theme::BTN_ICON, theme::BTN_ICON),
        );
        let head_x = tile.right() + 14.0;
        let name_max = (close_rect.left() - 10.0 - head_x).max(60.0);

        {
            let p = ui.painter();
            p.rect_filled(tile, CornerRadius::same(10), theme::tint(color, 0x1F));
            theme::paint_icon(p, tile.center(), color, theme::icon_for(row.status));

            let name = theme::galley_filename(
                ui,
                row.display_name(),
                theme::font_sb(16.0),
                theme::pal().ink,
                name_max,
                2,
            );
            p.galley(pos2(head_x, rect.top() + 18.0), name, theme::pal().ink);

            let sub = format!(
                "{} · {}",
                if row.size > 0 {
                    crate::fmt::bytes(row.size)
                } else {
                    "大小未知".to_string()
                },
                if row.kind == Kind::Torrent {
                    "BitTorrent"
                } else {
                    "HTTP 分段下载"
                }
            );
            p.text(
                pos2(head_x, rect.top() + 62.0),
                Align2::LEFT_TOP,
                sub,
                FontId::proportional(theme::F_META),
                theme::pal().ink3,
            );

            // 胶囊行：状态 + 连接/做种/可续传。
            let py = rect.top() + 92.0;
            let mut cx = x0;
            cx += chip(p, ui, cx, py, row.status.label(), color, true);
            if row.connections > 0 {
                cx += chip(
                    p,
                    ui,
                    cx + 6.0,
                    py,
                    &format!("{} 连接", row.connections),
                    theme::pal().ink2,
                    false,
                ) + 6.0;
            }
            if row.kind == Kind::Torrent && row.seeders > 0 {
                cx += chip(
                    p,
                    ui,
                    cx,
                    py,
                    &format!("{} 做种", row.seeders),
                    theme::pal().ink2,
                    false,
                ) + 6.0;
            }
            if row.resumed_from > 0 {
                chip(p, ui, cx, py, "可续传", theme::pal().ink2, false);
            }

            let mut y = rect.top() + 126.0;
            if row.status == Status::Failed {
                let msg = row.error.clone().unwrap_or_default();
                let g = theme::galley_wrap(
                    ui,
                    &msg,
                    FontId::proportional(theme::F_META),
                    theme::pal().err,
                    x1 - x0,
                    2,
                );
                let gh = g.size().y;
                p.galley(pos2(x0, y), g, theme::pal().err);
                y += gh + 10.0;
            }

            // 传输
            y = sec_title(p, x0, x1, y, "传输", section_step);
            let speed = if row.speed > 1.0 {
                crate::fmt::speed(row.speed)
            } else {
                "—".to_string()
            };
            kv(p, x0, x1, y, "速度", &speed);
            y += row_step;
            kv(
                p,
                x0,
                x1,
                y,
                "已下载",
                &format!(
                    "{} / {}",
                    crate::fmt::bytes(row.downloaded),
                    crate::fmt::bytes(row.size)
                ),
            );
            y += row_step;
            if row.kind == Kind::Torrent {
                kv(p, x0, x1, y, "已上传", &crate::fmt::bytes(row.uploaded));
                y += row_step;
                kv(p, x0, x1, y, "分享率", &format!("{:.2}", row.ratio()));
                y += row_step;
                kv(
                    p,
                    x0,
                    x1,
                    y,
                    "连接 / 做种",
                    &format!("{} / {}", row.peers, row.seeders),
                );
                y += row_step;
            } else {
                let eta = row
                    .eta_secs()
                    .map(crate::fmt::eta)
                    .unwrap_or_else(|| "—".to_string());
                kv(p, x0, x1, y, "剩余时间", &eta);
                y += row_step;
                let conn = if row.connections > 0 {
                    row.connections.to_string()
                } else {
                    "—".to_string()
                };
                kv(p, x0, x1, y, "连接", &conn);
                y += row_step;
            }

            // 最近 60 秒：放在来源和路径之前，默认窗口首屏即可看到趋势。
            y += section_gap;
            y = speed_chart(ui, x0, x1, y, row);

            // 来源
            y += section_gap;
            y = sec_title(p, x0, x1, y, "来源", section_step);
            let (source_label, source_value) = if is_magnet(&row.url) {
                ("磁力链接", "已嵌入任务".to_string())
            } else if let Some(torrent) = &row.torrent {
                (
                    "种子文件",
                    torrent
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| torrent.to_string_lossy().into_owned()),
                )
            } else {
                let host = host_of(&row.url);
                (
                    "来源主机",
                    if host.is_empty() {
                        "—".to_string()
                    } else {
                        host
                    },
                )
            };
            y += kv_wrapped(ui, p, x0, x1, y, source_label, &source_value);
            let proto = if row.kind == Kind::Torrent {
                "BitTorrent"
            } else if row.url.starts_with("https") {
                "HTTPS"
            } else {
                "HTTP"
            };
            kv(p, x0, x1, y, "协议", proto);
            y += row_step;

            // 保存到
            y += section_gap;
            y = sec_title(p, x0, x1, y, "保存到", section_step);
            let box_rect = Rect::from_min_size(pos2(x0, y), vec2(x1 - x0, 58.0));
            p.rect_filled(box_rect, CornerRadius::same(7), theme::pal().sunken);
            if let Some(dest) = &row.dest {
                let parent = dest
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let file = dest
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| dest.to_string_lossy().into_owned());
                let parent_g = theme::galley(
                    ui,
                    &parent,
                    11.5,
                    theme::pal().ink3,
                    box_rect.width() - 16.0,
                );
                p.galley(box_rect.min + vec2(8.0, 6.0), parent_g, theme::pal().ink3);
                let file_g = theme::galley_filename(
                    ui,
                    &file,
                    FontId::proportional(theme::F_META),
                    theme::pal().ink,
                    box_rect.width() - 16.0,
                    1,
                );
                p.galley(box_rect.min + vec2(8.0, 28.0), file_g, theme::pal().ink);
            }

            utility_y = box_rect.bottom() + 10.0;
        }

        let half = (x1 - x0 - 8.0) / 2.0;
        let reveal_rect = Rect::from_min_size(pos2(x0, utility_y), vec2(half, theme::BTN_H));
        let copy_rect =
            Rect::from_min_size(pos2(x0 + half + 8.0, utility_y), vec2(half, theme::BTN_H));
        if soft_btn(
            ui,
            reveal_rect,
            "打开文件夹",
            Some(Icon::Folder),
            "d_reveal",
        )
        .clicked()
        {
            act = Some(DetailAction::Reveal);
        }
        if soft_btn(ui, copy_rect, "复制链接", Some(Icon::Link), "d_copy").clicked() {
            act = Some(DetailAction::Copy);
        }
        ui.expand_to_include_rect(Rect::from_min_max(
            pos2(rect.left(), rect.top()),
            pos2(rect.right(), utility_y + theme::BTN_H),
        ));
        act
    }

    fn detail_footer(ui: &mut egui::Ui, rect: Rect, row: &Row) -> Option<DetailAction> {
        let mut act = None;
        let x0 = rect.left() + 18.0;
        let x1 = rect.right() - 18.0;
        let half = (x1 - x0 - 8.0) / 2.0;
        let del_rect = Rect::from_min_size(
            pos2(x0, rect.bottom() - 16.0 - theme::BTN_H),
            vec2(x1 - x0, theme::BTN_H),
        );
        let secondary_y = del_rect.top() - 10.0 - theme::BTN_H;
        let pri_y = secondary_y - 8.0 - theme::BTN_H;
        let pri_rect = Rect::from_min_size(pos2(x0, pri_y), vec2(x1 - x0, theme::BTN_H));
        let file_rect = Rect::from_min_size(pos2(x0, secondary_y), vec2(half, theme::BTN_H));
        let readd_rect =
            Rect::from_min_size(pos2(x0 + half + 8.0, secondary_y), vec2(half, theme::BTN_H));

        ui.painter().hline(
            x0..=x1,
            pri_rect.top() - 10.0,
            Stroke::new(1.0, theme::pal().line),
        );

        let primary = match row.status {
            Status::Active | Status::Probing | Status::Queued => "暂停下载",
            Status::Seeding => "停止做种",
            Status::Paused => "继续下载",
            Status::Failed => "重试",
            Status::Done => "打开文件",
        };
        let primary_icon = match row.status {
            Status::Active | Status::Probing | Status::Queued | Status::Seeding => {
                Some(Icon::Pause)
            }
            Status::Paused | Status::Failed => Some(Icon::Play),
            Status::Done => Some(Icon::Doc),
        };
        if head_btn(ui, pri_rect, primary, primary_icon, true, "d_primary").clicked() {
            act = Some(DetailAction::Primary);
        }
        if soft_btn(ui, file_rect, "打开文件", Some(Icon::Doc), "d_file").clicked() {
            act = Some(DetailAction::OpenFile);
        }
        if soft_btn(ui, readd_rect, "重新下载", Some(Icon::Refresh), "d_readd").clicked() {
            act = Some(DetailAction::Readd);
        }
        let dr = ui.interact(del_rect, ui.scope_id().with("d_del"), Sense::click());
        if dr.hovered() {
            ui.painter()
                .rect_filled(del_rect, CornerRadius::same(7), theme::pal().hover);
        }
        let dp = ui.painter();
        let dtw = text_w(ui, "删除任务与文件", 12.5);
        let dx = del_rect.center().x - (dtw + 20.0) / 2.0;
        theme::paint_icon(
            dp,
            pos2(dx + 6.0, del_rect.center().y),
            theme::pal().err,
            Icon::Trash,
        );
        dp.text(
            pos2(dx + 20.0, del_rect.center().y),
            Align2::LEFT_CENTER,
            "删除任务与文件",
            FontId::proportional(12.5),
            theme::pal().err,
        );
        if dr.clicked() {
            act = Some(DetailAction::Delete);
        }
        act
    }

    // ---------------------------------------------------------------- 设置对话框

    /// §6：600 宽的居中对话框。遮罩只盖标题栏以下——标题栏还看得见，说明窗口没被换掉。
    ///
    /// 关闭有三条路：Esc（在 `shortcuts` 里）、头部 X / 底部「完成」、点遮罩。
    /// 与旧版的两处行为差别（§6 明写）：做种与剪贴板用开关而不是二选一分段控件；
    /// 分享率在关掉做种时**常显但不可点**，不是整组藏起来。
    fn settings_dialog(&mut self, ctx: &egui::Context) {
        let mut close = false;
        let mut mask_hit: Option<Pos2> = None;
        let mut changed: Vec<Cmd> = Vec::new();
        let screen = ctx.content_rect();
        // 对话框 140ms 淡入（§7）。缩放 0.98→1 这一半没做：egui 里改 Area 的尺寸会让
        // 里面的换行重算，字会在动画期间跳一行，得不偿失。
        let t = (self.settings_at.elapsed().as_secs_f32() / 0.14).clamp(0.0, 1.0);
        if t < 1.0 {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        let below_bar =
            Rect::from_min_max(pos2(screen.left(), screen.top() + theme::TB_H), screen.max);

        // 遮罩：CANVAS @ 38%，点它等于取消。跟着对话框一起淡入。
        egui::Area::new(egui::Id::unique("settings-mask"))
            .order(egui::Order::Middle)
            .fixed_pos(below_bar.min)
            .show(ctx, |ui| {
                let (rect, resp) = ui.allocate_exact_size(below_bar.size(), Sense::click());
                let scrim = theme::pal().scrim;
                let a = (scrim.a() as f32 * t) as u8;
                ui.painter()
                    .rect_filled(rect, CornerRadius::ZERO, theme::tint(scrim, a));
                if resp.clicked() {
                    mask_hit = resp.interact_pointer_pos();
                }
            });

        // 宽 min(600, 窗口 - 64)；高 min(内容, 窗口 - 44 - 32)，超了内容区滚动（§6）。
        // 内容高度是**上一帧实测**的（首帧用 366 兜底）：先定高再画，Area 不会按内容撑破；
        // 留白给到 8px（§6 写的是 16px）——500 高的窗口里 16px 会让最后一行被页脚切掉半行。
        let body_h = self.settings_body_h.unwrap_or(366.0);
        let w = theme::DIALOG_W.min(below_bar.width() - 64.0).max(360.0);
        let max_h = (below_bar.height() - 16.0).max(220.0);
        let dlg_h = (body_h + 88.0).min(max_h);
        let dy = below_bar.center().y - screen.center().y;

        egui::Area::new(egui::Id::unique("settings-dialog"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, dy))
            .show(ctx, |ui| {
                // 先占出一块定高的画布：ScrollArea 的视口和裁剪都以它为准。
                let (outer, _) = ui.allocate_exact_size(vec2(w, dlg_h), Sense::hover());
                let mut _dlg_resp = None;
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(outer)
                        .layout(egui::Layout::top_down(egui::Align::LEFT)),
                    |ui| {
                        let dlg = egui::Frame::NONE
                            .fill(theme::tint(theme::pal().surface, (255.0 * t) as u8))
                            .stroke(Stroke::new(1.0, theme::pal().line))
                            .corner_radius(CornerRadius::same(14))
                            .show(ui, |ui| {
                                ui.set_width(w);
                                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                                ui.multiply_opacity(t);

                                // 头部 44：标题 + 右 X，和其他弹窗共用同一外壳。
                                if dialog_header(ui, w, "设置", "关闭设置") {
                                    close = true;
                                }

                                egui::ScrollArea::vertical()
                                    .max_height((dlg_h - 88.0).max(96.0))
                                    .auto_shrink([false, true])
                                    .show(ui, |ui| {
                                        set_section(ui, "下载");
                                        set_row(ui, "同时下载", |ui| {
                                            if let Some(v) = theme::segmented(
                                                ui,
                                                self.settings.limit,
                                                &[
                                                    (1, "1"),
                                                    (2, "2"),
                                                    (3, "3"),
                                                    (4, "4"),
                                                    (5, "5"),
                                                    (10, "10"),
                                                    (15, "15"),
                                                    (MAX_PARALLEL_DOWNLOADS, "20"),
                                                ],
                                            ) {
                                                self.settings.limit = v;
                                                changed.push(Cmd::SetLimit(v));
                                                self.mark_dirty();
                                            }
                                        });
                                        set_row(ui, "每任务连接数", |ui| {
                                            if let Some(v) = theme::segmented(
                                                ui,
                                                self.settings.connections,
                                                &[
                                                    (4, "4"),
                                                    (8, "8"),
                                                    (16, "16"),
                                                    (24, "24"),
                                                    (MAX_CONNECTIONS_PER_TASK, "32"),
                                                ],
                                            ) {
                                                self.settings.connections = v;
                                                changed.push(Cmd::SetConnections(v));
                                                self.mark_dirty();
                                            }
                                        });
                                        set_row(ui, "限速", |ui| {
                                            if let Some(v) = theme::segmented(
                                                ui,
                                                self.settings.rate,
                                                &[
                                                    (None, "不限"),
                                                    (Some(1_048_576), "1M"),
                                                    (Some(5_242_880), "5M"),
                                                    (Some(10_485_760), "10M"),
                                                    (Some(15_728_640), "15M"),
                                                    (Some(20_971_520), "20M"),
                                                ],
                                            ) {
                                                self.settings.rate = v;
                                                changed.push(Cmd::SetRate(v));
                                                self.mark_dirty();
                                            }
                                        });
                                        set_row(ui, "下载目录", |ui| {
                                            let resp = ui.add(
                                                egui::TextEdit::singleline(&mut self.dir_edit)
                                                    .font(FontId::proportional(12.5))
                                                    .desired_width(190.0)
                                                    .margin(egui::Margin::symmetric(8, 6)),
                                            );
                                            // 手输和原生选择器都写这一格：输入框失焦/回车生效，
                                            // 右边的文件夹按钮直接弹系统目录树。
                                            let commit = resp.lost_focus()
                                                || (resp.has_focus()
                                                    && ui.input(|i| i.key_pressed(Key::Enter)));
                                            if commit {
                                                let p = PathBuf::from(self.dir_edit.trim());
                                                if !p.as_os_str().is_empty()
                                                    && p != self.settings.dir
                                                {
                                                    let fallback =
                                                        self.paths.home.join("downloads");
                                                    let (p, note) =
                                                        writable_download_dir(&p, &fallback);
                                                    if let Some(note) = note {
                                                        self.toast(note, true);
                                                    }
                                                    self.settings.dir = p.clone();
                                                    self.dir_edit = p.to_string_lossy().to_string();
                                                    changed.push(Cmd::SetDir(p));
                                                    self.mark_dirty();
                                                }
                                            }
                                            ui.add_space(8.0);
                                            if ghost_icon(ui, Icon::Folder, "选择下载目录")
                                                .clicked()
                                                && let Some(dir) = pick_folder("选择下载目录")
                                                && dir.is_dir()
                                                && dir != self.settings.dir
                                            {
                                                let fallback = self.paths.home.join("downloads");
                                                let (dir, note) =
                                                    writable_download_dir(&dir, &fallback);
                                                if let Some(note) = note {
                                                    self.toast(note, true);
                                                }
                                                self.settings.dir = dir.clone();
                                                self.dir_edit = dir.to_string_lossy().to_string();
                                                changed.push(Cmd::SetDir(dir));
                                                self.mark_dirty();
                                            }
                                        });
                                        set_row(ui, "跳过新建下载确认", |ui| {
                                            let (r, _) = ui.allocate_exact_size(
                                                vec2(theme::SWITCH_W, theme::SWITCH_H),
                                                Sense::hover(),
                                            );
                                            if theme::switch_at(
                                                ui,
                                                r,
                                                self.settings.skip_add_dialog,
                                                "skip-add-sw",
                                                "跳过新建下载确认",
                                            )
                                            .clicked()
                                            {
                                                self.settings.skip_add_dialog =
                                                    !self.settings.skip_add_dialog;
                                                self.mark_dirty();
                                            }
                                        });

                                        ui.add_space(2.0);
                                        set_section(ui, "种子");
                                        let seeding = self.settings.seed;
                                        set_row(ui, "继续做种", |ui| {
                                            // 右端先放开关，再放分享率；关掉时分享率常显但不可点（§6）。
                                            let (r, _) = ui.allocate_exact_size(
                                                vec2(theme::SWITCH_W, theme::SWITCH_H),
                                                Sense::hover(),
                                            );
                                            if theme::switch_at(
                                                ui,
                                                r,
                                                seeding,
                                                "seed-sw",
                                                "继续做种",
                                            )
                                            .clicked()
                                            {
                                                let on = !seeding;
                                                self.settings.seed = on;
                                                let ratio = self.settings.seed_ratio;
                                                changed.push(Cmd::SetSeed { seed: on, ratio });
                                                self.mark_dirty();
                                            }
                                            ui.add_space(14.0);
                                            let options = [
                                                (1.0, "1.0"),
                                                (1.5, "1.5"),
                                                (2.0, "2.0"),
                                                (3.0, "3.0"),
                                            ];
                                            let picked = if seed_ratio_enabled(seeding) {
                                                theme::segmented(
                                                    ui,
                                                    self.settings.seed_ratio,
                                                    &options,
                                                )
                                            } else {
                                                ui.scope(|ui| {
                                                    ui.disable();
                                                    theme::segmented(
                                                        ui,
                                                        self.settings.seed_ratio,
                                                        &options,
                                                    )
                                                })
                                                .inner
                                            };
                                            if let Some(v) = picked {
                                                self.settings.seed_ratio = v;
                                                changed.push(Cmd::SetSeed {
                                                    seed: true,
                                                    ratio: v,
                                                });
                                                self.mark_dirty();
                                            }
                                        });

                                        ui.add_space(2.0);
                                        set_section(ui, "外观");
                                        set_row(ui, "主题", |ui| {
                                            if let Some(v) = theme::segmented(
                                                ui,
                                                self.settings.theme,
                                                &[
                                                    (ThemeMode::System, "跟随系统"),
                                                    (ThemeMode::Light, "浅色"),
                                                    (ThemeMode::Dark, "深色"),
                                                ],
                                            ) {
                                                self.settings.theme = v;
                                                theme::set_light(theme::resolve(v));
                                                theme::apply(ui.ctx());
                                                self.mark_dirty();
                                            }
                                        });
                                        ui.add_space(2.0);
                                        set_section(ui, "集成");
                                        set_row(ui, "剪贴板监视", |ui| {
                                            let (r, _) = ui.allocate_exact_size(
                                                vec2(theme::SWITCH_W, theme::SWITCH_H),
                                                Sense::hover(),
                                            );
                                            if theme::switch_at(
                                                ui,
                                                r,
                                                self.settings.clipboard,
                                                "clip-sw",
                                                "剪贴板监视",
                                            )
                                            .clicked()
                                            {
                                                self.settings.clipboard = !self.settings.clipboard;
                                                if let Some(c) = &self.clip {
                                                    c.set_enabled(self.settings.clipboard);
                                                }
                                                self.mark_dirty();
                                            }
                                        });
                                        set_row(ui, "浏览器捕获", |ui| {
                                            let installed = self.host_installed();
                                            let (r, _) = ui.allocate_exact_size(
                                                vec2(78.0, 22.0),
                                                Sense::hover(),
                                            );
                                            let (label, color) = if installed {
                                                ("已连接", theme::pal().ok)
                                            } else {
                                                ("未安装", theme::pal().warn)
                                            };
                                            chip(
                                                ui.painter(),
                                                ui,
                                                r.left(),
                                                r.top(),
                                                label,
                                                color,
                                                true,
                                            );
                                            ui.add_space(10.0);
                                            let (btn, _) = ui.allocate_exact_size(
                                                vec2(theme::BTN_W, theme::BTN_H),
                                                Sense::hover(),
                                            );
                                            if head_btn(
                                                ui,
                                                btn,
                                                if installed {
                                                    "重装宿主"
                                                } else {
                                                    "安装宿主"
                                                },
                                                None,
                                                false,
                                                "host",
                                            )
                                            .clicked()
                                            {
                                                match crate::native::install() {
                                                    Ok(lines) => {
                                                        crate::log::line(&lines.join(" / "));
                                                        self.toast("浏览器宿主已安装", false);
                                                    }
                                                    Err(e) => {
                                                        self.toast(format!("装宿主失败：{e}"), true)
                                                    }
                                                }
                                            }
                                        });
                                        ui.add_space(2.0);
                                        self.settings_body_h = Some(ui.min_rect().height());
                                    });

                                // 底部 44：版本与配置目录在左，「完成」在右。
                                let (foot, _) =
                                    ui.allocate_exact_size(vec2(w, 44.0), Sense::hover());
                                let p = ui.painter();
                                p.hline(
                                    foot.x_range(),
                                    foot.top(),
                                    Stroke::new(1.0, theme::pal().soft),
                                );
                                let note = format!(
                                    "Volant {} · 配置在 {}",
                                    env!("CARGO_PKG_VERSION"),
                                    self.paths.home.display()
                                );
                                let g = theme::galley(
                                    ui,
                                    &note,
                                    11.0,
                                    theme::pal().ink4,
                                    w - 32.0 - 104.0,
                                );
                                let gh = g.size().y;
                                p.galley(
                                    pos2(foot.left() + 16.0, foot.center().y - gh / 2.0),
                                    g,
                                    theme::pal().ink4,
                                );
                                let done = Rect::from_min_size(
                                    pos2(
                                        foot.right() - 16.0 - theme::BTN_W,
                                        foot.center().y - theme::BTN_H / 2.0,
                                    ),
                                    vec2(theme::BTN_W, theme::BTN_H),
                                );
                                if head_btn(ui, done, "完成", None, true, "settings-done").clicked()
                                {
                                    close = true;
                                }
                            });
                        _dlg_resp = Some(dlg.response.rect);
                    },
                );
                if let Some(r) = _dlg_resp {
                    self.dialog_rect = r;
                }
            });

        for cmd in changed {
            self.send(cmd);
        }
        // 遮罩只盖到标题栏以下，所以「点遮罩」与「点对话框空白处」需要靠矩形区分；
        // 矩形的更新比判定晚一帧，对话框要动也只有窗口缩放那一下，肉眼看不出来。
        if let Some(p) = mask_hit
            && !self.dialog_rect.contains(p)
        {
            close = true;
        }
        if close {
            self.settings_open = false;
        }
    }

    /// 浏览器宿主装没装：看扩展要加载的那份清单在不在（不查注册表，省一次 reg 进程）。
    fn host_installed(&self) -> bool {
        self.paths
            .home
            .join("native")
            .join(format!("{}.json", crate::native::HOST))
            .is_file()
    }

    fn toast_area(&mut self, ctx: &egui::Context) {
        let Some(t) = &self.toast else {
            return;
        };
        let life = Duration::from_secs(4);
        let elapsed = t.1.elapsed();
        if elapsed > life {
            self.toast = None;
            return;
        }
        let color = if t.2 {
            theme::pal().err
        } else {
            theme::pal().ink
        };
        egui::Area::new(egui::Id::unique("toast"))
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -18.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(theme::pal().sel)
                    .inner_margin(Margin::symmetric(14, 9))
                    .corner_radius(CornerRadius::same(8))
                    .show(ui, |ui| {
                        ui.set_max_width(480.0);
                        ui.label(RichText::new(&t.0).size(13.0).color(color));
                    });
            });
        ctx.request_repaint_after(life - elapsed);
    }

    fn shot_tick(&mut self, ctx: &egui::Context) {
        let Some(path) = self.launch.shot.clone() else {
            return;
        };
        match self.shot_state {
            0 => {
                if Instant::now() >= self.shot_at {
                    self.shot_state = 1;
                    let flag = self.shot_flag.clone();
                    let ctx2 = ctx.clone();
                    ctx.request_screenshot(move |img| {
                        if let Err(e) = write_png(&path, &img) {
                            eprintln!("截图失败：{e}");
                        }
                        flag.store(true, Ordering::SeqCst);
                        ctx2.request_repaint();
                    });
                    ctx.request_repaint();
                } else {
                    ctx.request_repaint_after(self.shot_at - Instant::now());
                }
            }
            1 if self.shot_flag.load(Ordering::SeqCst) => {
                self.shot_state = 2;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            _ => {}
        }
    }

    fn start_metrics(&self, ctx: &egui::Context) {
        let Some(out) = self.launch.metrics.clone() else {
            return;
        };
        let ctx = ctx.clone();
        let startup = self.startup_ms.clone();
        let frames = self.frames.clone();
        let secs = self.launch.seconds;
        std::thread::Builder::new()
            .name("volant-metrics".into())
            .spawn(move || crate::metrics::watch(ctx, startup, frames, secs, out))
            .ok();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frames.fetch_add(1, Ordering::Relaxed);
        let ctx = ui.ctx().clone();
        self.drive_window_resize(&ctx);
        if self.launch.theme.is_none() {
            follow_system(&self.settings, &mut self.theme_poll, &ctx);
        }
        if self.first_frame {
            self.first_frame = false;
            self.startup_ms
                .store(self.started.elapsed().as_millis() as u64, Ordering::SeqCst);
            if self.launch.metrics.is_some() {
                self.start_metrics(&ctx);
            }
            if self.launch.shot.is_some() {
                ctx.request_repaint_after(self.shot_at.saturating_duration_since(Instant::now()));
            }
        }

        self.drain_events();
        self.drain_clipboard();
        self.drain_capture();
        self.drain_drops(&ctx);
        self.shortcuts(&ctx);
        let drop_hint = if self.adding.is_none()
            && self.delete_prompt.is_none()
            && self.pick.is_none()
            && !self.settings_open
        {
            drag_hover(&ctx)
        } else {
            None
        };

        // 只把「没有选中 → 有选中」当成新打开一次详情栏：换行不该再滑一遍。
        if self.selected.is_some() && self.sel_prev.is_none() {
            self.detail_at = Instant::now();
        }
        self.sel_prev = self.selected;
        self.track_new_rows();

        self.bar(ui);
        // 详情栏三档：≥1160 固定一列；1040–1159 偷看 312；更窄时选中行后滑入主区。
        // 1160 这个数比 §5 的 1040 高，是因为 1040 - 232 - 312 只剩 496 给列表。
        let win_w = ui.available_width();
        if detail_is_fixed(win_w) {
            self.detail(ui);
        }
        self.rail(ui);
        // 覆盖层盖住的那一条也要从页头和列表里让出来（否则主按钮被盖进面板底下）。
        let peek = if detail_is_peek(win_w, self.selected.is_some()) {
            theme::INS_W
        } else {
            0.0
        };
        self.foot(ui, peek);
        self.head(ui, peek);
        self.list(ui);
        self.add_dialog(&ctx);
        self.delete_dialog(&ctx);
        // 覆盖层必须画在列表之后，否则被列表盖住。
        if detail_is_overlay(win_w, self.selected.is_some()) {
            self.detail_overlay(&ctx, win_w);
        }
        if let Some(valid) = drop_hint {
            drop_overlay(&ctx, valid);
        }
        self.pick_panel(&ctx);
        // 设置对话框是覆盖层：排在最后画，遮罩才能盖住列表与详情栏。
        if self.settings_open {
            self.settings_dialog(&ctx);
        }
        self.toast_area(&ctx);
        self.resize_handles(&ctx);
        self.shot_tick(&ctx);

        let closing = ctx.input(|i| i.viewport().close_requested());
        let due = Duration::from_millis(800);
        if self.dirty {
            if closing || self.last_save.elapsed() >= due {
                self.save();
            } else {
                // 去抖窗口里 egui 空闲就不重绘：自己约一帧，否则这次改动一直存不下去。
                ctx.request_repaint_after(due.saturating_sub(self.last_save.elapsed()));
            }
        }
    }
}

fn paste_target(text: &str) -> String {
    for token in text.split_whitespace() {
        for prefix in ["https://", "http://", "magnet:"] {
            if let Some(at) = token.to_ascii_lowercase().find(prefix) {
                return token[at..].to_string();
            }
        }
    }
    text.trim().to_string()
}

fn paste_links(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(paste_target)
        .collect::<Vec<_>>()
        .join("\n")
}

fn first_add_target(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(paste_target)
        .unwrap_or_default()
}

fn add_can_submit(add: &Adding) -> bool {
    let target = first_add_target(&add.text);
    if target.is_empty() || target != add.preview_for {
        return false;
    }
    match &add.preview {
        AddPreview::Http { .. } => add.torrent.is_none(),
        AddPreview::Torrent { .. } => {
            add.torrent.is_some() && add.checked.iter().any(|checked| *checked)
        }
        AddPreview::Empty | AddPreview::Loading | AddPreview::Failed(_) => false,
    }
}

fn scroll_activity(
    ui: &egui::Ui,
    key: &'static str,
    rect: Rect,
    scrolling: bool,
    overflow_hint: bool,
) -> bool {
    let ctx = ui.ctx();
    let now = ctx.input(|i| i.time);
    let active = egui::Id::unique(("add-scroll-active", key));
    let overflow = egui::Id::unique(("add-scroll-overflow", key));
    if scrolling {
        ui.data_mut(|d| d.insert_temp(active, now + 0.8));
    }
    let until = ui.data(|d| d.get_temp::<f64>(active)).unwrap_or(0.0);
    let has_overflow = ui
        .data(|d| d.get_temp::<bool>(overflow))
        .unwrap_or(overflow_hint && rect.height() > 0.0);
    let show = now < until && has_overflow;
    if show {
        ctx.request_repaint_after(Duration::from_millis(16));
    } else if until > now {
        ctx.request_repaint_after(Duration::from_secs_f64((until - now).max(0.0)));
    }
    show
}

fn note_scroll_output(ui: &egui::Ui, key: &'static str, overflow: bool, offset: f32) {
    let now = ui.ctx().input(|i| i.time);
    let active = egui::Id::unique(("add-scroll-active", key));
    let overflow_id = egui::Id::unique(("add-scroll-overflow", key));
    let offset_id = egui::Id::unique(("add-scroll-offset", key));
    let previous = ui.data(|d| d.get_temp::<f32>(offset_id));
    if previous.is_some_and(|previous| (previous - offset).abs() > 0.1) {
        ui.data_mut(|d| d.insert_temp(active, now + 0.8));
        ui.ctx().request_repaint();
    }
    ui.data_mut(|d| {
        d.insert_temp(overflow_id, overflow);
        d.insert_temp(offset_id, offset);
    });
}

fn add_torrent_source(ui: &mut egui::Ui, path: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 54.0), Sense::hover());
    let p = ui.painter().clone();
    p.rect_filled(rect, CornerRadius::same(10), theme::pal().sunken);
    p.rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, theme::pal().line),
        StrokeKind::Inside,
    );
    theme::paint_icon(
        &p,
        pos2(rect.left() + 24.0, rect.center().y),
        theme::pal().ink2,
        Icon::Doc,
    );
    p.text(
        pos2(rect.left() + 44.0, rect.top() + 17.0),
        Align2::LEFT_CENTER,
        "种子文件",
        FontId::proportional(theme::F_META),
        theme::pal().ink2,
    );
    let name = Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let g = theme::galley(
        ui,
        &name,
        theme::F_NAME,
        theme::pal().ink,
        rect.width() - 58.0,
    );
    ui.painter().galley(
        pos2(rect.left() + 44.0, rect.top() + 27.0),
        g,
        theme::pal().ink,
    );
}

/// 新建下载弹窗里的信息卡。高度固定，文件列表只在卡片内部滚动。
fn add_preview_card(ui: &mut egui::Ui, add: &mut Adding, card_h: f32) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), card_h), Sense::hover());
    let p = ui.painter().clone();
    p.rect_filled(rect, CornerRadius::same(10), theme::pal().sunken);
    p.rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, theme::pal().line),
        StrokeKind::Inside,
    );

    match &add.preview {
        AddPreview::Empty => {
            theme::paint_icon(
                &p,
                pos2(rect.left() + 24.0, rect.top() + 38.0),
                theme::pal().ink4,
                Icon::Link,
            );
            p.text(
                pos2(rect.left() + 44.0, rect.top() + 32.0),
                Align2::LEFT_CENTER,
                "等待链接",
                theme::font_sb(14.0),
                theme::pal().ink2,
            );
            p.text(
                pos2(rect.left() + 44.0, rect.top() + 53.0),
                Align2::LEFT_CENTER,
                "粘贴后会自动读取文件名、类型与大小",
                FontId::proportional(theme::F_META),
                theme::pal().ink3,
            );
        }
        AddPreview::Loading => {
            ui.ctx().request_repaint_after(Duration::from_millis(120));
            p.rect_filled(
                Rect::from_min_size(
                    pos2(rect.left() + 1.0, rect.top() + 1.0),
                    vec2((rect.width() - 2.0) * 0.28, 2.0),
                ),
                CornerRadius::same(1),
                theme::pal().accent,
            );
            p.text(
                pos2(rect.left() + 18.0, rect.top() + 28.0),
                Align2::LEFT_CENTER,
                "正在读取文件信息…",
                theme::font_sb(14.0),
                theme::pal().ink,
            );
            for (i, width) in [0.72, 0.48].into_iter().enumerate() {
                let y = rect.top() + 58.0 + i as f32 * 22.0;
                p.rect_filled(
                    Rect::from_min_size(
                        pos2(rect.left() + 18.0, y),
                        vec2((rect.width() - 36.0) * width, 8.0),
                    ),
                    CornerRadius::same(4),
                    theme::pal().line,
                );
            }
        }
        AddPreview::Failed(reason) => {
            theme::paint_icon(
                &p,
                pos2(rect.left() + 24.0, rect.top() + 28.0),
                theme::pal().err,
                Icon::Alert,
            );
            p.text(
                pos2(rect.left() + 44.0, rect.top() + 27.0),
                Align2::LEFT_CENTER,
                "无法读取文件信息",
                theme::font_sb(14.0),
                theme::pal().ink,
            );
            let g = theme::galley(
                ui,
                reason,
                theme::F_META,
                theme::pal().ink3,
                rect.width() - 112.0,
            );
            ui.painter().galley(
                pos2(rect.left() + 18.0, rect.top() + 50.0),
                g,
                theme::pal().ink3,
            );
            let retry = Rect::from_min_size(
                pos2(rect.right() - 86.0, rect.bottom() - 40.0),
                vec2(68.0, 28.0),
            );
            return action_button(
                ui,
                retry,
                "重试",
                DialogButtonKind::Secondary,
                true,
                "重试读取文件信息",
            )
            .clicked();
        }
        AddPreview::Http {
            name,
            size,
            content_type,
            final_url,
        } => {
            paint_add_table_header(ui, rect, rect.top() + 20.0);
            add.checked = vec![true];
            let row = Rect::from_min_size(
                pos2(rect.left(), rect.top() + 34.0),
                vec2(rect.width(), 34.0),
            );
            paint_add_file_row(
                ui,
                row,
                "add-preview-http",
                name,
                size.unwrap_or(0),
                add_file_type(name, content_type.as_deref()),
                true,
                false,
            );
            let host = host_of(final_url);
            let footer_y = rect.bottom() - 26.0;
            ui.painter().hline(
                rect.x_range(),
                footer_y - 10.0,
                Stroke::new(1.0, theme::pal().line),
            );
            ui.painter().text(
                pos2(rect.left() + 18.0, footer_y),
                Align2::LEFT_CENTER,
                if host.is_empty() {
                    "1 个文件".to_string()
                } else {
                    format!("来源 {host} · 1 个文件")
                },
                FontId::proportional(theme::F_META),
                theme::pal().ink3,
            );
            ui.painter().text(
                pos2(rect.right() - 18.0, footer_y),
                Align2::RIGHT_CENTER,
                if size.is_some() {
                    crate::fmt::bytes(size.unwrap_or(0))
                } else {
                    "大小未知".to_string()
                },
                FontId::proportional(theme::F_META),
                theme::pal().ink3,
            );
        }
        AddPreview::Torrent {
            name: _,
            total: _,
            files,
            info_hash: _,
        } => {
            let all_checked = !add.checked.is_empty() && add.checked.iter().all(|checked| *checked);
            let all_resp = ui.interact(
                Rect::from_min_size(pos2(rect.left() + 12.0, rect.top() + 4.0), vec2(96.0, 24.0)),
                egui::Id::unique("add-preview-all"),
                Sense::click(),
            );
            if all_resp.clicked() {
                add.checked.fill(!all_checked);
            }
            paint_add_check(
                ui.painter(),
                pos2(rect.left() + 20.0, rect.top() + 18.0),
                all_checked,
            );
            ui.painter().text(
                pos2(rect.left() + 38.0, rect.top() + 18.0),
                Align2::LEFT_CENTER,
                "全选",
                FontId::proportional(theme::F_META),
                theme::pal().ink,
            );
            paint_add_table_header(ui, rect, rect.top() + 45.0);
            ui.painter().hline(
                rect.x_range(),
                rect.top() + 57.0,
                Stroke::new(1.0, theme::pal().line),
            );
            let files = files.clone();
            let list_rect = Rect::from_min_max(
                pos2(rect.left(), rect.top() + 58.0),
                pos2(rect.right(), rect.bottom() - 6.0),
            );
            let pointer = ui.ctx().pointer_latest_pos();
            let wheel = ui.input(|input| input.smooth_scroll_delta.y.abs() > 0.01);
            let scrolling = wheel && pointer.is_some_and(|pointer| list_rect.contains(pointer));
            let overflow_hint = files.len() as f32 * 30.0 > list_rect.height();
            let show_bar =
                scroll_activity(ui, "add-files-scroll", list_rect, scrolling, overflow_hint);
            let out = ui
                .scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(list_rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                    |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("add-files-list")
                            .max_height(list_rect.height())
                            .auto_shrink([false, false])
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                            )
                            .show_rows(ui, 30.0, files.len(), |ui, range| {
                                for i in range {
                                    let file = &files[i];
                                    let (row, _) = ui.allocate_exact_size(
                                        vec2(ui.available_width(), 30.0),
                                        Sense::hover(),
                                    );
                                    let row_resp = paint_add_file_row(
                                        ui,
                                        row,
                                        &format!("add-preview-file-{i}"),
                                        &file.name,
                                        file.len,
                                        file_type(&file.name),
                                        add.checked.get(i).copied().unwrap_or(true),
                                        true,
                                    );
                                    if let Some(checked) = add.checked.get_mut(i)
                                        && row_resp.clicked()
                                    {
                                        *checked = !*checked;
                                    }
                                }
                            })
                    },
                )
                .inner;
            let overflow = out.content_size.y > out.inner_rect.height() + 0.5;
            if show_bar && overflow {
                let viewport_h = out.inner_rect.height();
                let content_h = out.content_size.y.max(viewport_h);
                let thumb_h =
                    (viewport_h / content_h * viewport_h).clamp(24.0, viewport_h.max(24.0));
                let max_offset = (content_h - viewport_h).max(0.0);
                let t = if max_offset > 0.0 {
                    (out.state.clamped_offset().y / max_offset).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let track = Rect::from_min_max(
                    pos2(out.inner_rect.right() - 7.0, out.inner_rect.top() + 4.0),
                    pos2(out.inner_rect.right() - 3.0, out.inner_rect.bottom() - 4.0),
                );
                let thumb = Rect::from_min_size(
                    pos2(
                        track.left(),
                        track.top() + (track.height() - thumb_h).max(0.0) * t,
                    ),
                    vec2(track.width(), thumb_h.min(track.height())),
                );
                ui.painter().rect_filled(
                    thumb,
                    CornerRadius::same(2),
                    theme::tint(theme::pal().ink3, 0x99),
                );
            }
            note_scroll_output(
                ui,
                "add-files-scroll",
                overflow,
                out.state.clamped_offset().y,
            );
            if scrolling {
                ui.ctx().request_repaint();
            }
        }
    }
    false
}

fn paint_add_table_header(ui: &mut egui::Ui, card: Rect, y: f32) {
    let p = ui.painter();
    p.text(
        pos2(card.left() + 38.0, y),
        Align2::LEFT_CENTER,
        "文件名",
        FontId::proportional(theme::F_META),
        theme::pal().ink4,
    );
    p.text(
        pos2(card.right() - 118.0, y),
        Align2::CENTER_CENTER,
        "类型",
        FontId::proportional(theme::F_META),
        theme::pal().ink4,
    );
    p.text(
        pos2(card.right() - 22.0, y),
        Align2::RIGHT_CENTER,
        "大小",
        FontId::proportional(theme::F_META),
        theme::pal().ink4,
    );
}

#[allow(clippy::too_many_arguments)]
fn paint_add_file_row(
    ui: &mut egui::Ui,
    row: Rect,
    id: &str,
    name: &str,
    size: u64,
    kind: String,
    checked: bool,
    interactive: bool,
) -> egui::Response {
    let resp = ui.interact(
        row,
        egui::Id::unique(("add-file-row", id)),
        if interactive {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if resp.hovered() {
        ui.painter().rect_filled(
            row.shrink2(vec2(6.0, 1.0)),
            CornerRadius::same(6),
            theme::pal().hover,
        );
    }
    let p = ui.painter().clone();
    paint_add_check(&p, pos2(row.left() + 20.0, row.center().y), checked);
    theme::paint_icon(
        &p,
        pos2(row.left() + 44.0, row.center().y),
        if checked {
            theme::pal().ink2
        } else {
            theme::pal().ink4
        },
        Icon::Doc,
    );
    let name_w = (row.width() - 190.0).max(40.0);
    let g = theme::galley(ui, name, 12.5, theme::pal().ink, name_w);
    p.galley(
        pos2(row.left() + 60.0, row.center().y - g.size().y / 2.0),
        g,
        theme::pal().ink,
    );
    p.text(
        pos2(row.right() - 118.0, row.center().y),
        Align2::CENTER_CENTER,
        kind,
        FontId::proportional(theme::F_META),
        theme::pal().ink3,
    );
    p.text(
        pos2(row.right() - 22.0, row.center().y),
        Align2::RIGHT_CENTER,
        if size > 0 {
            crate::fmt::bytes(size)
        } else {
            "未知".into()
        },
        FontId::proportional(theme::F_META),
        theme::pal().ink3,
    );
    if resp.has_focus() {
        p.rect_stroke(
            row.shrink(2.0),
            CornerRadius::same(6),
            Stroke::new(1.5, theme::pal().accent),
            StrokeKind::Inside,
        );
    }
    resp.on_hover_text(name)
}

fn paint_add_check(p: &egui::Painter, center: Pos2, checked: bool) {
    let box_rect = Rect::from_center_size(center, vec2(15.0, 15.0));
    p.rect_filled(
        box_rect,
        CornerRadius::same(4),
        if checked {
            theme::pal().ink
        } else {
            Color32::TRANSPARENT
        },
    );
    p.rect_stroke(
        box_rect,
        CornerRadius::same(4),
        Stroke::new(
            1.2,
            if checked {
                theme::pal().ink
            } else {
                theme::pal().ink3
            },
        ),
        StrokeKind::Inside,
    );
    if checked {
        p.line_segment(
            [
                pos2(box_rect.left() + 3.5, center.y),
                pos2(center.x - 1.0, box_rect.bottom() - 4.0),
            ],
            Stroke::new(1.6, theme::pal().canvas),
        );
        p.line_segment(
            [
                pos2(center.x - 1.0, box_rect.bottom() - 4.0),
                pos2(box_rect.right() - 3.3, box_rect.top() + 4.0),
            ],
            Stroke::new(1.6, theme::pal().canvas),
        );
    }
}

fn file_type(name: &str) -> String {
    Path::new(name)
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .filter(|ext| !ext.is_empty())
        .unwrap_or_else(|| "文件".into())
}

fn add_file_type(name: &str, content_type: Option<&str>) -> String {
    let by_name = file_type(name);
    if by_name != "文件" {
        return by_name;
    }
    content_type
        .unwrap_or_default()
        .split(';')
        .next()
        .and_then(|kind| kind.split('/').nth(1))
        .filter(|kind| !kind.is_empty() && *kind != "octet-stream")
        .unwrap_or("文件")
        .to_string()
}

fn seed_ratio_enabled(seeding: bool) -> bool {
    seeding
}

fn delete_requires_confirmation(intent: DeleteIntent, confirm_delete: bool) -> bool {
    intent == DeleteIntent::Files || confirm_delete
}

fn can_start(status: Status) -> bool {
    matches!(status, Status::Paused | Status::Failed)
}

fn can_pause(status: Status) -> bool {
    matches!(
        status,
        Status::Active | Status::Queued | Status::Probing | Status::Seeding
    )
}

fn enter_action(status: Status) -> EnterAction {
    if status == Status::Done {
        EnterAction::OpenFile
    } else {
        EnterAction::OpenDetail
    }
}

/// 当前可见行决定页头批量动作；返回的 ID 就是执行时唯一允许触达的集合。
fn bulk_action(visible: &[&Row]) -> Option<BulkAction> {
    let has_running = visible.iter().any(|row| {
        matches!(
            row.status,
            Status::Active | Status::Probing | Status::Seeding
        )
    });
    if has_running {
        let ids = visible
            .iter()
            .filter(|row| can_pause(row.status))
            .map(|row| row.id)
            .collect::<Vec<_>>();
        return (!ids.is_empty()).then_some(BulkAction {
            kind: BulkKind::Pause,
            ids,
        });
    }

    let ids = visible
        .iter()
        .filter(|row| row.status == Status::Paused)
        .map(|row| row.id)
        .collect::<Vec<_>>();
    (!ids.is_empty()).then_some(BulkAction {
        kind: BulkKind::Resume,
        ids,
    })
}

/// `None` = 没有勾选；`Some(None)` = 全部文件；`Some(Some(..))` = 只下载勾选项。
fn only_for_checked(checked: &[bool]) -> Option<Option<Vec<usize>>> {
    let selected = checked
        .iter()
        .enumerate()
        .filter_map(|(i, checked)| checked.then_some(i))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        None
    } else if selected.len() == checked.len() {
        Some(None)
    } else {
        Some(Some(selected))
    }
}

fn torrent_needs_pick(file_count: usize, no_selection: bool, direct: bool) -> bool {
    file_count > 1 && no_selection && !direct
}

fn head_layout(window_h: f32) -> HeadLayout {
    if window_h >= 680.0 {
        HeadLayout {
            density: HeadDensity::Full,
            height: theme::HEAD_H,
            title_y: 18.0,
            subtitle_y: Some(54.0),
            band_y: 87.0,
            band_h: theme::BAND_H,
        }
    } else if window_h >= 520.0 {
        HeadLayout {
            density: HeadDensity::Compact,
            height: 104.0,
            title_y: 10.0,
            subtitle_y: Some(37.0),
            band_y: 55.0,
            band_h: 32.0,
        }
    } else {
        HeadLayout {
            density: HeadDensity::Minimal,
            height: 68.0,
            title_y: 7.0,
            subtitle_y: None,
            band_y: 38.0,
            band_h: 24.0,
        }
    }
}

fn card_mode(window_w: f32) -> bool {
    window_w < 620.0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ResizeEdge {
    North,
    South,
    East,
    West,
    NorthEast,
    SouthEast,
    NorthWest,
    SouthWest,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NativeWindowRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[cfg(windows)]
#[derive(Clone, Copy)]
struct ResizeDrag {
    edge: ResizeEdge,
    start: NativeWindowRect,
    cursor: (i32, i32),
    last: NativeWindowRect,
}

/// 窗口边框的 8 个交互区。边缘区让开四角，避免同一像素同时命中两个方向。
fn resize_zones(window: Rect) -> [(ResizeEdge, Rect); 8] {
    const EDGE: f32 = 6.0;
    const CORNER: f32 = 16.0;
    let inner_x = (
        (window.left() + CORNER).min(window.right()),
        (window.right() - CORNER).max(window.left()),
    );
    let inner_y = (
        (window.top() + CORNER).min(window.bottom()),
        (window.bottom() - CORNER).max(window.top()),
    );
    let edge = |edge, x0, y0, x1, y1| (edge, Rect::from_min_max(pos2(x0, y0), pos2(x1, y1)));
    [
        edge(
            ResizeEdge::North,
            inner_x.0,
            window.top(),
            inner_x.1,
            window.top() + EDGE,
        ),
        edge(
            ResizeEdge::South,
            inner_x.0,
            window.bottom() - EDGE,
            inner_x.1,
            window.bottom(),
        ),
        edge(
            ResizeEdge::West,
            window.left(),
            inner_y.0,
            window.left() + EDGE,
            inner_y.1,
        ),
        edge(
            ResizeEdge::East,
            window.right() - EDGE,
            inner_y.0,
            window.right(),
            inner_y.1,
        ),
        edge(
            ResizeEdge::NorthWest,
            window.left(),
            window.top(),
            window.left() + CORNER,
            window.top() + CORNER,
        ),
        edge(
            ResizeEdge::NorthEast,
            window.right() - CORNER,
            window.top(),
            window.right(),
            window.top() + CORNER,
        ),
        edge(
            ResizeEdge::SouthWest,
            window.left(),
            window.bottom() - CORNER,
            window.left() + CORNER,
            window.bottom(),
        ),
        edge(
            ResizeEdge::SouthEast,
            window.right() - CORNER,
            window.bottom() - CORNER,
            window.right(),
            window.bottom(),
        ),
    ]
}

fn resize_direction(edge: ResizeEdge) -> egui::viewport::ResizeDirection {
    use egui::viewport::ResizeDirection;
    match edge {
        ResizeEdge::North => ResizeDirection::North,
        ResizeEdge::South => ResizeDirection::South,
        ResizeEdge::East => ResizeDirection::East,
        ResizeEdge::West => ResizeDirection::West,
        ResizeEdge::NorthEast => ResizeDirection::NorthEast,
        ResizeEdge::SouthEast => ResizeDirection::SouthEast,
        ResizeEdge::NorthWest => ResizeDirection::NorthWest,
        ResizeEdge::SouthWest => ResizeDirection::SouthWest,
    }
}

fn resize_cursor(edge: ResizeEdge) -> egui::CursorIcon {
    use egui::CursorIcon;
    match edge {
        ResizeEdge::North => CursorIcon::ResizeNorth,
        ResizeEdge::South => CursorIcon::ResizeSouth,
        ResizeEdge::East => CursorIcon::ResizeEast,
        ResizeEdge::West => CursorIcon::ResizeWest,
        ResizeEdge::NorthEast => CursorIcon::ResizeNorthEast,
        ResizeEdge::SouthEast => CursorIcon::ResizeSouthEast,
        ResizeEdge::NorthWest => CursorIcon::ResizeNorthWest,
        ResizeEdge::SouthWest => CursorIcon::ResizeSouthWest,
    }
}

#[cfg(windows)]
fn cursor_pos() -> Option<(i32, i32)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut point = POINT { x: 0, y: 0 };
    (unsafe { GetCursorPos(&mut point) } != 0).then_some((point.x, point.y))
}

#[cfg(windows)]
fn window_rect(hwnd: isize) -> Option<NativeWindowRect> {
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;
    let mut rect: RECT = unsafe { std::mem::zeroed() };
    let hwnd = hwnd as *mut c_void;
    (unsafe { GetWindowRect(hwnd, &mut rect) } != 0).then_some(NativeWindowRect {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    })
}

#[cfg(windows)]
fn resize_window(hwnd: isize, rect: NativeWindowRect) {
    use std::ffi::c_void;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOSENDCHANGING, SWP_NOZORDER, SetWindowPos,
    };
    let hwnd = hwnd as *mut c_void;
    unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_NOZORDER | SWP_NOSENDCHANGING,
        );
    }
}

/// 根据鼠标从按下点到当前点的位移计算新窗口矩形，并在左上方向缩放时保持另一侧不动。
#[cfg(windows)]
fn resized_window(
    start: NativeWindowRect,
    from: (i32, i32),
    to: (i32, i32),
    edge: ResizeEdge,
    min_width: i32,
    min_height: i32,
) -> NativeWindowRect {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let west = matches!(
        edge,
        ResizeEdge::West | ResizeEdge::NorthWest | ResizeEdge::SouthWest
    );
    let east = matches!(
        edge,
        ResizeEdge::East | ResizeEdge::NorthEast | ResizeEdge::SouthEast
    );
    let north = matches!(
        edge,
        ResizeEdge::North | ResizeEdge::NorthWest | ResizeEdge::NorthEast
    );
    let south = matches!(
        edge,
        ResizeEdge::South | ResizeEdge::SouthWest | ResizeEdge::SouthEast
    );

    let mut left = start.left;
    let mut right = start.right;
    let mut top = start.top;
    let mut bottom = start.bottom;
    if west {
        left += dx;
    } else if east {
        right += dx;
    }
    if north {
        top += dy;
    } else if south {
        bottom += dy;
    }

    if right - left < min_width {
        if west {
            left = right - min_width;
        } else if east {
            right = left + min_width;
        }
    }
    if bottom - top < min_height {
        if north {
            top = bottom - min_height;
        } else if south {
            bottom = top + min_height;
        }
    }
    NativeWindowRect {
        left,
        top,
        right,
        bottom,
    }
}

fn detail_is_fixed(window_w: f32) -> bool {
    window_w >= 1160.0
}

fn detail_is_peek(window_w: f32, has_selection: bool) -> bool {
    has_selection && (1040.0..1160.0).contains(&window_w)
}

fn detail_is_overlay(window_w: f32, has_selection: bool) -> bool {
    has_selection && window_w < 1160.0
}

// -------------------------------------------------------------------- 绘制

#[derive(Clone, Copy)]
enum Action {
    Toggle,
    Delete,
    Retry,
}

enum RowMenuAction {
    Start,
    Pause,
    OpenFile,
    OpenFolder,
    Delete,
    PermanentDelete,
    Copy,
}

fn style_context_menu(menu: &mut egui::Ui) {
    const MENU_FONT_SIZE: f32 = 12.5;
    let font = FontId::proportional(MENU_FONT_SIZE);
    let widest = menu
        .painter()
        .layout_no_wrap("粘贴并立即下载".to_owned(), font.clone(), theme::pal().ink2)
        .size()
        .x;
    let width = widest + 24.0;
    menu.set_width(width);
    menu.set_min_width(width);
    menu.set_max_width(width);
    let row_h = menu.fonts_mut(|fonts| fonts.row_height(&font)) * 1.5;
    menu.spacing_mut().button_padding = vec2(12.0, 0.0);
    menu.spacing_mut().interact_size.y = row_h;
    menu.spacing_mut().item_spacing.y = 0.0;
}

fn context_menu_item(menu: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    let color = if enabled {
        theme::pal().ink2
    } else {
        theme::pal().ink4
    };
    context_menu_item_colored(menu, label, enabled, color)
}

fn context_menu_danger_item(menu: &mut egui::Ui, label: &str) -> egui::Response {
    context_menu_item_colored(menu, label, true, theme::pal().err)
}

fn context_menu_item_colored(
    menu: &mut egui::Ui,
    label: &str,
    enabled: bool,
    color: Color32,
) -> egui::Response {
    menu.add_enabled(
        enabled,
        egui::Button::new(RichText::new(label).size(12.5).color(color)).frame(false),
    )
}

fn row_menu_action(menu: &mut egui::Ui, row: &Row) -> Option<RowMenuAction> {
    style_context_menu(menu);
    let can_start = can_start(row.status);
    let can_pause = can_pause(row.status);
    let can_open_file =
        row.status == Status::Done && row.dest.as_ref().is_some_and(|dest| dest.exists());
    let can_open_folder = row.dest.as_ref().is_some_and(|dest| dest.exists());
    if can_start && context_menu_item(menu, "继续", true).clicked() {
        menu.close();
        return Some(RowMenuAction::Start);
    }
    if can_pause && context_menu_item(menu, "暂停", true).clicked() {
        menu.close();
        return Some(RowMenuAction::Pause);
    }
    if can_open_file && context_menu_item(menu, "打开文件", true).clicked() {
        menu.close();
        return Some(RowMenuAction::OpenFile);
    }
    if can_open_folder && context_menu_item(menu, "打开文件夹", true).clicked() {
        menu.close();
        return Some(RowMenuAction::OpenFolder);
    }
    if context_menu_item(menu, "复制下载链接", true).clicked() {
        menu.close();
        return Some(RowMenuAction::Copy);
    }
    if context_menu_item(menu, "删除任务", true).clicked() {
        menu.close();
        return Some(RowMenuAction::Delete);
    }
    if context_menu_danger_item(menu, "彻底删除文件").clicked() {
        menu.close();
        return Some(RowMenuAction::PermanentDelete);
    }
    None
}

enum DetailAction {
    Primary,
    Reveal,
    Copy,
    OpenFile,
    Readd,
    Delete,
}

/// 标题栏的窗口按钮：36×44，贴右边缘，slot 0 = 最右（关闭）。
fn win_button(
    ui: &mut egui::Ui,
    bar: Rect,
    slot: usize,
    icon: Icon,
    tip: &str,
    danger: bool,
) -> egui::Response {
    let rect = Rect::from_min_size(
        pos2(bar.right() - 36.0 * (slot as f32 + 1.0), bar.top()),
        vec2(36.0, bar.height()),
    );
    let resp = ui.interact(rect, ui.scope_id().with(("win", tip)), Sense::click());
    let hot = resp.hovered();
    if hot || resp.has_focus() {
        let fill = if danger {
            theme::pal().err
        } else {
            theme::pal().hover
        };
        ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
    }
    if resp.has_focus() && !hot {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::ZERO,
            Stroke::new(1.5, theme::pal().accent),
            StrokeKind::Inside,
        );
    }
    let color = if hot {
        theme::pal().ink
    } else {
        theme::pal().ink2
    };
    theme::paint_icon(ui.painter(), rect.center(), color, icon);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tip));
    resp.on_hover_text(tip)
}

fn light_content_button_border(p: &egui::Painter, rect: Rect, radius: CornerRadius) {
    if theme::is_light() {
        p.rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, theme::pal().line),
            StrokeKind::Inside,
        );
    }
}

/// 页头按钮：primary = INK 底 + 画布色字；ghost = 透明底，悬停 SOFT（§4）。
fn head_btn(
    ui: &mut egui::Ui,
    rect: Rect,
    label: &str,
    icon: Option<Icon>,
    primary: bool,
    id: &str,
) -> egui::Response {
    let resp = ui.interact(rect, ui.scope_id().with(id), Sense::click());
    let p = ui.painter();
    let (fill, fg) = if primary {
        (theme::pal().ink, theme::pal().canvas)
    } else if resp.hovered() {
        (theme::pal().soft, theme::pal().ink)
    } else {
        (Color32::TRANSPARENT, theme::pal().ink2)
    };
    if fill != Color32::TRANSPARENT {
        p.rect_filled(rect, CornerRadius::same(7), fill);
    }
    light_content_button_border(p, rect, CornerRadius::same(7));
    if resp.has_focus() {
        p.rect_stroke(
            rect,
            CornerRadius::same(7),
            Stroke::new(1.5, theme::pal().accent),
            StrokeKind::Inside,
        );
    }
    let font = FontId::proportional(12.5);
    let tw = p
        .layout_no_wrap(label.to_owned(), font.clone(), fg)
        .size()
        .x;
    let icon_w = if icon.is_some() { 22.0 } else { 0.0 };
    let x = rect.center().x - (tw + icon_w) / 2.0;
    if let Some(icon) = icon {
        theme::paint_icon(p, pos2(x + 8.0, rect.center().y), fg, icon);
    }
    p.text(
        pos2(x + icon_w, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        fg,
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    resp
}

fn text_w_font(ui: &egui::Ui, text: &str, font: FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font, theme::pal().ink)
        .size()
        .x
}

/// 设置里的一节：11px 小标签 + 一条到右边缘的 1px 线（§6）。
fn set_section(ui: &mut egui::Ui, title: &str) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
    ui.painter().text(
        pos2(rect.left() + 16.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(theme::F_LABEL),
        theme::pal().ink4,
    );
    let tw = text_w(ui, title, theme::F_LABEL);
    ui.painter().hline(
        egui::Rangef::new(rect.left() + 26.0 + tw, rect.right() - 16.0),
        rect.center().y,
        Stroke::new(1.0, theme::pal().soft),
    );
}

/// 设置里的一行 40：左标签（可带说明），右控件右对齐、贴 PAD 16（§6）。
///
/// `dim` 是「常显但禁用」的行文字色（INK4@50%）——控件本身的可点不可点由调用方
/// 用 `ui.set_enabled` 管。
fn set_row(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.allocate_ui_with_layout(
            vec2(248.0, 40.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(RichText::new(title).size(12.5).color(theme::pal().ink));
            },
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(16.0);
            add(ui);
        });
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DialogButtonKind {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TextMenuAction {
    Undo,
    Cut,
    Copy,
    Paste,
    PasteAndDownload,
    Delete,
    SelectAll,
}

fn apply_text_menu_action(
    ctx: &egui::Context,
    id: egui::Id,
    text: &mut String,
    action: TextMenuAction,
) -> bool {
    let mut state = egui::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
    let range = state
        .cursor
        .char_range()
        .unwrap_or_else(|| egui::text::CCursorRange::select_all_str(text));
    let has_selection = !range.is_empty();
    let current = (range, text.clone());
    let mut undoer = state.undoer();

    match action {
        TextMenuAction::Undo => {
            if let Some((previous_range, previous_text)) = undoer.undo(&current).cloned() {
                *text = previous_text;
                state.cursor.set_char_range(Some(previous_range));
            }
        }
        TextMenuAction::Copy => {
            if has_selection {
                ctx.copy_text(range.slice_str(text).to_owned());
            }
        }
        TextMenuAction::Cut if has_selection => {
            ctx.copy_text(range.slice_str(text).to_owned());
            undoer.add_undo(&current);
            let cursor = replace_char_range(text, range, "");
            state.cursor.set_char_range(Some(cursor));
        }
        TextMenuAction::Paste | TextMenuAction::PasteAndDownload => {
            if let Some(clipboard) = crate::clipboard::read_now() {
                undoer.add_undo(&current);
                let inserted = if action == TextMenuAction::PasteAndDownload {
                    paste_links(&clipboard)
                } else {
                    clipboard
                };
                let cursor = replace_char_range(text, range, &inserted);
                state.cursor.set_char_range(Some(cursor));
                state.set_undoer(undoer);
                state.store(ctx, id);
                ctx.request_repaint();
                return action == TextMenuAction::PasteAndDownload;
            }
        }
        TextMenuAction::Delete if has_selection => {
            undoer.add_undo(&current);
            let cursor = replace_char_range(text, range, "");
            state.cursor.set_char_range(Some(cursor));
        }
        TextMenuAction::SelectAll => {
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::select_all_str(text)));
        }
        TextMenuAction::Cut | TextMenuAction::Delete => {}
    }
    state.set_undoer(undoer);
    state.store(ctx, id);
    ctx.request_repaint();
    false
}

fn replace_char_range(
    text: &mut String,
    range: egui::text::CCursorRange,
    replacement: &str,
) -> egui::text::CCursorRange {
    let chars = range.as_sorted_char_range();
    let start = char_to_byte(text, chars.start.into());
    let end = char_to_byte(text, chars.end.into());
    text.replace_range(start..end, replacement);
    let start_char: usize = chars.start.into();
    let cursor = egui::text::CCursor::new(start_char + replacement.chars().count());
    egui::text::CCursorRange::one(cursor)
}

fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

fn dialog_frame() -> egui::Frame {
    dialog_frame_with_margin(18)
}

fn dialog_frame_with_margin(margin: i8) -> egui::Frame {
    egui::Frame::NONE
        .fill(theme::pal().surface)
        .inner_margin(Margin::same(margin))
        .corner_radius(CornerRadius::same(14))
        .stroke(Stroke::new(1.0, theme::pal().line))
        .shadow(egui::epaint::Shadow {
            offset: [0, 10],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha(48),
        })
}

fn modal_mask(ctx: &egui::Context, id: &str) -> bool {
    let screen = ctx.content_rect();
    let rect = Rect::from_min_max(pos2(screen.left(), screen.top() + theme::TB_H), screen.max);
    let mut clicked = false;
    egui::Area::new(egui::Id::unique(id))
        .order(egui::Order::Middle)
        .fixed_pos(rect.min)
        .show(ctx, |ui| {
            let (_, resp) = ui.allocate_exact_size(rect.size(), Sense::click());
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, theme::pal().scrim);
            clicked = resp.clicked();
        });
    clicked
}

fn drag_hover(ctx: &egui::Context) -> Option<bool> {
    let files = ctx.input(|input| input.raw.hovered_files.clone());
    (!files.is_empty()).then(|| {
        files.iter().all(|file| {
            file.path
                .as_ref()
                .and_then(|path| path.extension())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("torrent"))
        })
    })
}

fn drop_overlay(ctx: &egui::Context, valid: bool) {
    let screen = ctx.content_rect();
    let rect = Rect::from_min_max(
        pos2(screen.left(), screen.top() + theme::TB_H),
        pos2(screen.right(), screen.bottom() - theme::FOOT_H),
    );
    let color = if valid {
        theme::pal().accent
    } else {
        theme::pal().err
    };
    egui::Area::new(egui::Id::unique("drop-overlay"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .interactable(false)
        .show(ctx, |ui| {
            let (rect, _) = ui.allocate_exact_size(rect.size(), Sense::hover());
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, theme::tint(color, 0x12));
            ui.painter().rect_stroke(
                rect.shrink(1.0),
                CornerRadius::ZERO,
                Stroke::new(1.5, color),
                StrokeKind::Inside,
            );
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                if valid {
                    "松开以添加种子"
                } else {
                    "只支持 .torrent 文件"
                },
                FontId::proportional(15.0),
                color,
            );
        });
}

fn dialog_header(ui: &mut egui::Ui, width: f32, title: &str, close_id: &str) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 44.0), Sense::hover());
    ui.painter().text(
        pos2(rect.left() + 16.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        theme::font_sb(15.0),
        theme::pal().ink,
    );
    let close = mini(
        ui,
        pos2(rect.right() - 24.0, rect.center().y),
        Icon::Close,
        close_id,
    )
    .clicked();
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, theme::pal().soft),
    );
    close
}

fn action_button(
    ui: &mut egui::Ui,
    rect: Rect,
    label: &str,
    kind: DialogButtonKind,
    enabled: bool,
    id: &str,
) -> egui::Response {
    ui.scope(|ui| {
        if !enabled {
            ui.disable();
            ui.multiply_opacity(0.48);
        }
        let resp = ui.interact(rect, ui.scope_id().with(id), Sense::click());
        let hot = enabled && resp.hovered();
        let (fill, stroke, fg) = match kind {
            DialogButtonKind::Primary => (
                if hot {
                    theme::pal().ink2
                } else {
                    theme::pal().ink
                },
                Stroke::NONE,
                theme::pal().canvas,
            ),
            DialogButtonKind::Secondary => (
                if hot {
                    theme::pal().hover
                } else {
                    theme::pal().surface
                },
                Stroke::new(1.0, theme::pal().line),
                theme::pal().ink,
            ),
            DialogButtonKind::Ghost => (
                if hot {
                    theme::pal().soft
                } else {
                    Color32::TRANSPARENT
                },
                Stroke::NONE,
                theme::pal().ink3,
            ),
            DialogButtonKind::Danger => (
                if hot {
                    theme::pal().fail_bg
                } else {
                    Color32::TRANSPARENT
                },
                if hot {
                    Stroke::new(1.0, theme::tint(theme::pal().err, 0x55))
                } else {
                    Stroke::NONE
                },
                theme::pal().err,
            ),
        };
        if fill != Color32::TRANSPARENT {
            ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
        }
        light_content_button_border(ui.painter(), rect, CornerRadius::same(8));
        if stroke != Stroke::NONE {
            ui.painter()
                .rect_stroke(rect, CornerRadius::same(8), stroke, StrokeKind::Inside);
        }
        if resp.has_focus() {
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(8),
                Stroke::new(1.5, theme::pal().accent),
                StrokeKind::Inside,
            );
        }
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(12.5),
            fg,
        );
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
        resp
    })
    .inner
}

fn check_row(ui: &mut egui::Ui, label: &str, checked: &mut bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
    if resp.clicked() {
        *checked = !*checked;
    }
    let box_rect =
        Rect::from_center_size(pos2(rect.left() + 8.0, rect.center().y), vec2(15.0, 15.0));
    let p = ui.painter();
    p.rect_stroke(
        box_rect,
        CornerRadius::same(4),
        Stroke::new(
            1.2,
            if *checked {
                theme::pal().accent
            } else {
                theme::pal().ink3
            },
        ),
        StrokeKind::Inside,
    );
    if *checked {
        p.line_segment(
            [
                box_rect.left_top() + vec2(3.0, 7.0),
                box_rect.left_top() + vec2(6.0, 10.5),
            ],
            Stroke::new(1.6, theme::pal().accent),
        );
        p.line_segment(
            [
                box_rect.left_top() + vec2(6.0, 10.5),
                box_rect.left_top() + vec2(12.0, 4.0),
            ],
            Stroke::new(1.6, theme::pal().accent),
        );
    }
    p.text(
        pos2(rect.left() + 22.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(theme::F_META),
        theme::pal().ink2,
    );
    if resp.has_focus() {
        p.rect_stroke(
            rect,
            CornerRadius::same(5),
            Stroke::new(1.5, theme::pal().accent),
            StrokeKind::Inside,
        );
    }
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), *checked, label)
    });
    resp
}

/// 28×28 的幽灵图标按钮（悬停才出 HOVER 底）：设置里的「打开目录」这类次要动作。
fn ghost_icon(ui: &mut egui::Ui, icon: Icon, tip: &str) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(vec2(theme::BTN_ICON, theme::BTN_ICON), Sense::click());
    let hot = resp.hovered();
    if hot || resp.has_focus() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(7), theme::pal().hover);
    }
    light_content_button_border(ui.painter(), rect, CornerRadius::same(7));
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(7),
            Stroke::new(1.5, theme::pal().accent),
            StrokeKind::Inside,
        );
    }
    theme::paint_icon(
        ui.painter(),
        rect.center(),
        if hot {
            theme::pal().ink
        } else {
            theme::pal().ink3
        },
        icon,
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tip));
    resp.on_hover_text(tip)
}

fn mini(ui: &mut egui::Ui, center: egui::Pos2, icon: Icon, tip: &str) -> egui::Response {
    let rect = Rect::from_center_size(center, vec2(theme::BTN_ICON, theme::BTN_ICON));
    let resp = ui.interact(rect, ui.scope_id().with(("mini", tip)), Sense::click());
    let hot = resp.hovered();
    if hot || resp.has_focus() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(7), theme::pal().hover);
    }
    light_content_button_border(ui.painter(), rect, CornerRadius::same(7));
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(7),
            Stroke::new(1.5, theme::pal().accent),
            StrokeKind::Inside,
        );
    }
    let color = if icon == Icon::Plus {
        theme::pal().accent
    } else if hot {
        theme::pal().ink
    } else {
        theme::pal().ink3
    };
    theme::paint_icon(ui.painter(), rect.center(), color, icon);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tip));
    resp.on_hover_text(tip)
}

/// 现在的时间（epoch 毫秒），和浏览器 `Date.now()` 同一把尺子。
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn text_w(ui: &egui::Ui, text: &str, size: f32) -> f32 {
    ui.painter()
        .layout_no_wrap(
            text.to_owned(),
            FontId::proportional(size),
            theme::pal().ink,
        )
        .size()
        .x
}

/// 详情栏的胶囊：8% 同色底 + 5px 圆点 + 11.5 字，返回占宽。
fn chip(
    p: &egui::Painter,
    ui: &egui::Ui,
    x: f32,
    y: f32,
    label: &str,
    color: Color32,
    dot: bool,
) -> f32 {
    let tw = text_w(ui, label, theme::F_PILL);
    let w = tw + if dot { 26.0 } else { 16.0 };
    let rect = Rect::from_min_size(pos2(x, y), vec2(w, theme::PILL_H));
    let neutral =
        color == theme::pal().ink2 || color == theme::pal().ink3 || color == theme::pal().ink4;
    let bg = if color == theme::pal().err {
        theme::pal().fail_bg
    } else if neutral {
        theme::pal().surface
    } else {
        theme::tint(color, 0x10)
    };
    p.rect_filled(rect, CornerRadius::same(6), bg);
    p.rect_stroke(
        rect,
        CornerRadius::same(6),
        Stroke::new(
            1.0,
            if neutral {
                theme::pal().line
            } else {
                theme::tint(color, 0x46)
            },
        ),
        StrokeKind::Inside,
    );
    let mut tx = x + 8.0;
    if dot {
        p.circle_filled(pos2(x + 11.0, y + theme::PILL_H / 2.0), 2.5, color);
        tx = x + 18.0;
    }
    p.text(
        pos2(tx, y + theme::PILL_H / 2.0),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(theme::F_PILL),
        color,
    );
    w
}

/// 详情栏的小节标题：只留灰色小标题，靠留白分组。
fn sec_title(p: &egui::Painter, x0: f32, _x1: f32, y: f32, title: &str, step: f32) -> f32 {
    p.text(
        pos2(x0, y),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(theme::F_LABEL),
        theme::pal().ink4,
    );
    y + step
}

/// 详情栏的一行 kv：标签左、值右，行高 28。
fn kv(p: &egui::Painter, x0: f32, x1: f32, y: f32, label: &str, value: &str) {
    p.text(
        pos2(x0, y + 11.0),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(theme::F_KV),
        theme::pal().ink3,
    );
    p.text(
        pos2(x1, y + 11.0),
        Align2::RIGHT_CENTER,
        value,
        FontId::proportional(theme::F_KV),
        theme::pal().ink,
    );
}

/// 详情栏 kv 的换行版本：短值仍右对齐；长路径从下一行起占满可用宽度并返回实际高度。
fn kv_wrapped(
    ui: &egui::Ui,
    p: &egui::Painter,
    x0: f32,
    x1: f32,
    y: f32,
    label: &str,
    value: &str,
) -> f32 {
    let label_reserve = 64.0;
    let one_line_room = (x1 - x0 - label_reserve).max(1.0);
    let font = FontId::proportional(theme::F_KV);
    if text_w_font(ui, value, font.clone()) <= one_line_room {
        kv(p, x0, x1, y, label, value);
        return 22.0;
    }

    p.text(
        pos2(x0, y + 11.0),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(theme::F_KV),
        theme::pal().ink3,
    );
    let g = theme::galley_wrap(ui, value, font, theme::pal().ink, x1 - x0, 4);
    let height = g.size().y;
    p.galley(pos2(x0, y + 20.0), g, theme::pal().ink);
    height + 26.0
}

fn speed_chart(ui: &egui::Ui, x0: f32, x1: f32, y: f32, row: &Row) -> f32 {
    let chart_y = sec_title(ui.painter(), x0, x1, y, "最近 60 秒", 20.0);
    let chart_bg = Rect::from_min_size(pos2(x0, chart_y), vec2(x1 - x0, 56.0));
    ui.painter()
        .rect_filled(chart_bg, CornerRadius::same(7), theme::pal().sunken);
    if row.speed_history.is_empty() {
        ui.painter().text(
            chart_bg.center(),
            Align2::CENTER_CENTER,
            "开始下载后显示速度曲线",
            FontId::proportional(theme::F_LABEL),
            theme::pal().ink4,
        );
        return chart_bg.bottom() + 18.0;
    }

    let peak = speed_sparkline(
        ui.painter(),
        chart_bg.shrink2(vec2(10.0, 9.0)),
        &row.speed_history,
        theme::pal().ink3,
    );
    let p = ui.painter();
    p.text(
        pos2(x0 + 2.0, chart_bg.bottom() + 9.0),
        Align2::LEFT_CENTER,
        "60 秒前",
        FontId::proportional(theme::F_LABEL),
        theme::pal().ink4,
    );
    p.text(
        pos2(
            (chart_bg.left() + chart_bg.right()) / 2.0,
            chart_bg.bottom() + 9.0,
        ),
        Align2::CENTER_CENTER,
        format!("峰值 {}", crate::fmt::speed(peak)),
        FontId::proportional(theme::F_LABEL),
        theme::pal().ink4,
    );
    p.text(
        pos2(x1 - 2.0, chart_bg.bottom() + 9.0),
        Align2::RIGHT_CENTER,
        "现在",
        FontId::proportional(theme::F_LABEL),
        theme::pal().ink4,
    );
    chart_bg.bottom() + 24.0
}

/// 最近 60 秒速度折线。空历史也画一条基线，避免图表区域忽然消失。
fn speed_sparkline(
    p: &egui::Painter,
    rect: Rect,
    samples: &VecDeque<(Instant, f64)>,
    color: Color32,
) -> f64 {
    p.hline(
        rect.x_range(),
        rect.bottom(),
        Stroke::new(1.0, theme::pal().line),
    );
    let now = Instant::now();
    let peak = samples
        .iter()
        .map(|(_, speed)| *speed)
        .fold(0.0f64, f64::max);
    let scale = peak.max(1.0);
    let points: Vec<Pos2> = samples
        .iter()
        .filter_map(|(t, speed)| {
            let age = now.duration_since(*t).as_secs_f64();
            (age <= 60.0).then(|| {
                let x = rect.right() - (age / 60.0) as f32 * rect.width();
                let y = rect.bottom() - ((*speed / scale) as f32).clamp(0.0, 1.0) * rect.height();
                pos2(x, y)
            })
        })
        .collect();
    match points.as_slice() {
        [] => {}
        [only] => {
            p.circle_filled(*only, 2.0, color);
        }
        _ => {
            let fill = color.gamma_multiply(0.12);
            let mut mesh = egui::Mesh::default();
            for pair in points.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                let i = mesh.vertices.len() as u32;
                mesh.colored_vertex(a, fill);
                mesh.colored_vertex(pos2(a.x, rect.bottom()), fill);
                mesh.colored_vertex(b, fill);
                mesh.colored_vertex(pos2(b.x, rect.bottom()), fill);
                mesh.add_triangle(i, i + 1, i + 2);
                mesh.add_triangle(i + 1, i + 3, i + 2);
            }
            p.add(egui::Shape::mesh(mesh));
            p.add(egui::Shape::line(points.clone(), Stroke::new(1.5, color)));
            if let Some(last) = points.last() {
                p.circle_filled(*last, 2.5, color);
            }
        }
    }
    peak
}

/// 次要按钮：SOFT 底 + INK 字，悬停 SEL。
fn soft_btn(
    ui: &mut egui::Ui,
    rect: Rect,
    label: &str,
    icon: Option<Icon>,
    id: &str,
) -> egui::Response {
    let resp = ui.interact(rect, ui.scope_id().with(id), Sense::click());
    let p = ui.painter();
    p.rect_filled(
        rect,
        CornerRadius::same(8),
        if resp.hovered() {
            theme::pal().hover
        } else {
            theme::pal().surface
        },
    );
    p.rect_stroke(
        rect,
        CornerRadius::same(8),
        Stroke::new(1.0, theme::pal().line),
        StrokeKind::Inside,
    );
    let tw = text_w(ui, label, 12.5);
    let iw = if icon.is_some() { 22.0 } else { 0.0 };
    let mut x = rect.center().x - (tw + iw) / 2.0;
    if let Some(ic) = icon {
        theme::paint_icon(p, pos2(x + 8.0, rect.center().y), theme::pal().ink2, ic);
        x += iw;
    }
    p.text(
        pos2(x, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.5),
        theme::pal().ink,
    );
    if resp.has_focus() {
        p.rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(1.5, theme::pal().accent),
            StrokeKind::Inside,
        );
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    resp
}

/// 当前视图没有任务（但别的视图有）时的提示。
fn no_match(ui: &mut egui::Ui, view: &str) -> bool {
    let rect = ui.available_rect_before_wrap();
    let center = rect.center();
    ui.painter().text(
        center - vec2(0.0, 28.0),
        Align2::CENTER_CENTER,
        format!("“{view}”里还没有任务"),
        FontId::proportional(theme::F_NAME),
        theme::pal().ink4,
    );
    action_button(
        ui,
        Rect::from_center_size(
            center + vec2(0.0, 14.0),
            vec2(theme::BTN_W_WIDE, theme::BTN_H),
        ),
        "查看全部下载",
        DialogButtonKind::Secondary,
        true,
        "查看全部下载",
    )
    .clicked()
}

fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .to_string()
}

// ---------------------------------------------------------------- 列表行

/// 行内列坐标的唯一来源（IMPLEMENT §5）。`x1` 是行右边缘（已扣掉行内 12 的内边距），
/// 左栏宽度：232 文字栏 / 56 图标栏。`rail()` 和覆盖式详情栏都按这一份算，
/// 免得两边对「这一档左栏多宽」各有一套说法。
fn rail_width(win_w: f32) -> f32 {
    if win_w >= 860.0 && !(1040.0..1160.0).contains(&win_w) {
        theme::RAIL_W
    } else {
        theme::RAIL_SM
    }
}

/// `list_w` 是整行的宽度。
struct Cols {
    prog_x: f32,
    prog_w: f32,
    /// 百分比列的右边界；`pct_w == 0` 时这一列不存在。
    pct_right: f32,
    pct_w: f32,
    size_right: f32,
    speed_right: f32,
    speed_w: f32,
    pill_x: f32,
    pill_w: f32,
    act_x: f32,
    /// 名称列的右边界。名称宽度**由这里给**，别在 `paint_row` 里另算一次。
    name_right: f32,
}

/// 列宽按「名称还剩多少」逐级让位，而不是按列表宽度硬分档。
///
/// 1180 固定三栏时行宽约 588：动作不再占用固定列，名称仍能拿到 200px，
/// 进度、百分比、大小和状态都保留。速度只在宽列表且确有可见速度时加入。
fn cols(x1: f32, list_w: f32, has_speed: bool) -> Cols {
    const GAP: f32 = 7.0;
    /// 行内左侧占位：20 内边距 + 36 图标块 + 12 间距。
    const LEFT: f32 = 68.0;
    /// 名称列的可读下限；低于它就继续撤掉可选宽度。
    const NAME_MIN: f32 = 196.0;

    // (进度宽, 百分比宽, 大小宽, 速度宽, 状态宽)，从左往后依次让位。
    const STEPS: [(f32, f32, f32, f32, f32); 6] = [
        (100.0, 44.0, 84.0, 0.0, 76.0),
        (88.0, 40.0, 80.0, 0.0, 72.0),
        (64.0, 36.0, 72.0, 0.0, 68.0),
        (52.0, 32.0, 64.0, 0.0, 64.0),
        (120.0, 40.0, 84.0, 68.0, 76.0),
        (88.0, 36.0, 76.0, 60.0, 68.0),
    ];

    // `x1` 是屏幕绝对坐标，`LEFT` 是相对行左边的距离：算可用宽度必须用行宽，
    // 否则会凭空多出一条左栏（232）出来，名称反而被压成 40px。
    let rel_right = list_w - 12.0;
    let room = |prog: f32, pct: f32, size: f32, speed: f32, pill: f32| {
        let fixed = prog + pct + size + speed + pill;
        let gaps = if speed > 0.0 { GAP * 5.0 } else { GAP * 4.0 };
        rel_right - LEFT - fixed - gaps
    };

    let standard = STEPS[..4]
        .iter()
        .copied()
        .find(|s| room(s.0, s.1, s.2, s.3, s.4) >= NAME_MIN)
        .unwrap_or(STEPS[3]);
    let wide = STEPS[4..]
        .iter()
        .copied()
        .find(|s| room(s.0, s.1, s.2, s.3, s.4) >= 260.0);
    let (prog_w, pct_w, size_w, speed_w, pill_w) = if has_speed {
        wide.unwrap_or(standard)
    } else {
        standard
    };

    // 列序：名称 → 进度 → 百分比 → 速度 → 大小 → 状态；动作覆盖右侧，不占宽。
    let pill_x = x1 - pill_w;
    let size_right = pill_x - GAP;
    let size_left = size_right - size_w;
    let speed_right = size_left - GAP;
    let speed_left = speed_right - speed_w;
    let value_left = if speed_w > 0.0 { speed_left } else { size_left };
    let pct_right = value_left - GAP;
    let prog_right = pct_right - pct_w;
    let prog_x = prog_right - prog_w;
    Cols {
        prog_x,
        prog_w,
        pct_right,
        pct_w,
        size_right,
        speed_right,
        speed_w,
        pill_x,
        pill_w,
        act_x: x1 - 76.0,
        name_right: if prog_w > 0.0 {
            prog_x - GAP
        } else {
            pct_right
        },
    }
}

/// 表头 26：名称/进度/速度/大小/状态，列坐标和行共用 `cols`（§4）。
fn table_head(ui: &egui::Ui, rect: Rect, c: &Cols) {
    let p = ui.painter();
    let font = FontId::proportional(theme::F_LABEL);
    let y = rect.center().y;
    let name_x = rect.left() + 20.0 + theme::TILE + 12.0;
    p.text(
        pos2(name_x, y),
        Align2::LEFT_CENTER,
        "名称",
        font.clone(),
        theme::pal().ink4,
    );
    if c.prog_w > 0.0 {
        p.text(
            pos2(c.prog_x, y),
            Align2::LEFT_CENTER,
            "进度",
            font.clone(),
            theme::pal().ink4,
        );
    }
    // 数值列表头与值统一右对齐；状态表头与固定状态槽中心对齐。
    if c.speed_w > 0.0 {
        p.text(
            pos2(c.speed_right, y),
            Align2::RIGHT_CENTER,
            "速度",
            font.clone(),
            theme::pal().ink4,
        );
    }
    p.text(
        pos2(c.size_right, y),
        Align2::RIGHT_CENTER,
        "大小",
        font.clone(),
        theme::pal().ink4,
    );
    p.text(
        pos2(c.pill_x + c.pill_w / 2.0, y),
        Align2::CENTER_CENTER,
        "状态",
        font,
        theme::pal().ink4,
    );
}

/// 一行 66：图标块、名字、元信息、进度条、百分比、大小、胶囊、行内操作（§4）。
fn paint_row(
    ui: &egui::Ui,
    rect: Rect,
    row: &Row,
    c: &Cols,
    selected: bool,
    pointer_in: bool,
    alpha: f32,
) {
    // 整行一次性给足透明度（§7 的新行淡入）：Painter 的 opacity 会乘到之后画的所有形状上，
    // 比逐个颜色乘省事，也不会漏掉图标与进度条。
    let mut owned = ui.painter().clone();
    if alpha < 1.0 {
        owned.set_opacity(alpha);
    }
    let p = &owned;
    if selected {
        p.rect_filled(rect, CornerRadius::same(8), theme::pal().sel);
        // 选中态左侧 2×28 的强调条，垂直居中（§4）。
        p.rect_filled(
            Rect::from_min_size(pos2(rect.left(), rect.center().y - 14.0), vec2(2.0, 28.0)),
            CornerRadius::ZERO,
            theme::pal().accent,
        );
    } else {
        // 悬停 100ms 淡入（§7）：底色从 SURFACE 混到 HOVER，不能直接铺 HOVER，
        // 否则进出只要一帧就跳完。
        let t = ui.ctx().animate_bool_with_time(
            egui::Id::unique(("row-hover", row.id)),
            pointer_in,
            0.10,
        );
        if t > 0.001 {
            p.rect_filled(
                rect,
                CornerRadius::same(8),
                theme::mix(theme::pal().surface, theme::pal().hover, t),
            );
        }
    }

    let color = theme::status_color(row.status);

    // 36×36 图标块：12% 同色底 + 同色线条图标。
    let tile = Rect::from_center_size(
        pos2(rect.left() + 20.0 + theme::TILE / 2.0, rect.center().y),
        vec2(theme::TILE, theme::TILE),
    );
    p.rect_filled(tile, CornerRadius::same(9), theme::tint(color, 0x1F));
    theme::paint_icon(p, tile.center(), color, theme::icon_for(row.status));

    let text_x = tile.right() + 12.0;
    let text_max = (c.name_right - text_x).max(40.0);

    let name = theme::galley(
        ui,
        row.display_name(),
        theme::F_NAME,
        theme::pal().ink,
        text_max,
    );
    p.galley(pos2(text_x, rect.top() + 13.5), name, theme::pal().ink);

    let meta_color = if row.status == Status::Failed {
        theme::pal().err
    } else {
        theme::pal().ink3
    };
    let mut note = row.note();
    if c.speed_w == 0.0 && row.speed >= 1024.0 {
        note = format!("{} · {note}", crate::fmt::speed(row.speed));
    }
    let meta = theme::galley(ui, &note, theme::F_META, meta_color, text_max);
    p.galley(pos2(text_x, rect.top() + 36.0), meta, meta_color);

    // 下面这些都在同一条水平线上：进度条、百分比、大小第一行、胶囊。
    let cy = rect.top() + 25.0;
    let font_meta = FontId::proportional(theme::F_META);

    let bar = Rect::from_min_size(pos2(c.prog_x, cy - 2.5), vec2(c.prog_w, 5.0));
    let fill = match row.status {
        Status::Active => theme::pal().ink,
        Status::Done | Status::Seeding => theme::pal().ok,
        Status::Failed => theme::pal().err,
        _ => theme::pal().ink4.gamma_multiply(0.6),
    };
    if c.prog_w > 0.0 {
        // 进度 0.18 s 插值（§7）：下载中每 250 ms 才来一条进度事件，不插值会一跳一跳。
        let frac = ui.ctx().animate_value_with_time(
            egui::Id::unique(("row-prog", row.id)),
            row.frac(),
            0.18,
        );
        theme::progress_bar(p, bar, frac, fill);
    }
    if c.pct_w > 0.0 {
        p.text(
            pos2(c.pct_right, cy),
            Align2::RIGHT_CENTER,
            crate::fmt::percent(row.downloaded, row.size),
            FontId::proportional(theme::F_PILL),
            theme::pal().ink3,
        );
    }

    let (main, sub) = size_texts(row);
    p.text(
        pos2(c.size_right, cy),
        Align2::RIGHT_CENTER,
        main,
        font_meta.clone(),
        theme::pal().ink,
    );
    if !sub.is_empty() {
        p.text(
            pos2(c.size_right, cy + 18.0),
            Align2::RIGHT_CENTER,
            sub,
            FontId::proportional(theme::F_LABEL),
            theme::pal().ink4,
        );
    }

    if c.speed_w > 0.0 {
        let speed = if row.speed >= 1.0 {
            crate::fmt::speed(row.speed)
        } else {
            "—".to_string()
        };
        let color = if row.speed >= 1.0 {
            theme::pal().ink2
        } else {
            theme::pal().ink4
        };
        p.text(
            pos2(c.speed_right, cy),
            Align2::RIGHT_CENTER,
            speed,
            font_meta.clone(),
            color,
        );
    }

    // 悬停/选中时同一位置由行内动作接管，避免图标与胶囊重叠。
    if !(selected || pointer_in) {
        let label = row.status.label();
        let label_w = 28.0 + text_w(ui, label, theme::F_PILL);
        let pill = Rect::from_center_size(
            pos2(c.pill_x + c.pill_w / 2.0, cy),
            vec2(label_w, theme::PILL_H),
        );
        let bg = if row.status == Status::Failed {
            theme::pal().fail_bg
        } else {
            theme::tint(color, 0x14)
        };
        p.rect_filled(pill, CornerRadius::same(6), bg);
        p.circle_filled(pos2(pill.left() + 10.5, pill.center().y), 2.5, color);
        p.text(
            pos2(pill.left() + 18.0, pill.center().y),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(theme::F_PILL),
            color,
        );
    }
}

/// `<620` 的任务卡：名称最多两行，下面保留准确百分比、进度和可读状态。
fn paint_card(ui: &egui::Ui, rect: Rect, row: &Row, selected: bool, pointer_in: bool, alpha: f32) {
    let mut owned = ui.painter().clone();
    if alpha < 1.0 {
        owned.set_opacity(alpha);
    }
    let p = &owned;
    if selected {
        p.rect_filled(rect, CornerRadius::same(9), theme::pal().sel);
        p.rect_filled(
            Rect::from_min_size(pos2(rect.left(), rect.center().y - 14.0), vec2(2.0, 28.0)),
            CornerRadius::ZERO,
            theme::pal().accent,
        );
    } else {
        let t = ui.ctx().animate_bool_with_time(
            egui::Id::unique(("card-hover", row.id)),
            pointer_in,
            0.10,
        );
        if t > 0.001 {
            p.rect_filled(
                rect,
                CornerRadius::same(9),
                theme::mix(theme::pal().surface, theme::pal().hover, t),
            );
        }
    }

    let color = theme::status_color(row.status);
    let tile = Rect::from_min_size(
        pos2(rect.left() + 12.0, rect.top() + 10.0),
        vec2(32.0, 32.0),
    );
    p.rect_filled(tile, CornerRadius::same(8), theme::tint(color, 0x1F));
    theme::paint_icon(p, tile.center(), color, theme::icon_for(row.status));

    let text_x = tile.right() + 10.0;
    let chevron = pos2(rect.right() - 16.0, rect.top() + 24.0);
    p.line_segment(
        [chevron + vec2(-3.0, -4.0), chevron + vec2(1.0, 0.0)],
        Stroke::new(1.5, theme::pal().ink3),
    );
    p.line_segment(
        [chevron + vec2(1.0, 0.0), chevron + vec2(-3.0, 4.0)],
        Stroke::new(1.5, theme::pal().ink3),
    );

    let name_right = rect.right() - 34.0;
    let name = theme::galley_filename(
        ui,
        row.display_name(),
        FontId::proportional(theme::F_NAME),
        theme::pal().ink,
        (name_right - text_x).max(40.0),
        2,
    );
    p.galley(pos2(text_x, rect.top() + 8.0), name, theme::pal().ink);

    let bar_y = rect.top() + 49.0;
    let pct = if row.size == 0 {
        "—".to_string()
    } else {
        crate::fmt::percent(row.downloaded, row.size)
    };
    let pct_right = rect.right() - 30.0;
    let bar = Rect::from_min_max(
        pos2(text_x, bar_y),
        pos2((pct_right - 38.0).max(text_x + 20.0), bar_y + 5.0),
    );
    if row.size > 0 {
        theme::progress_bar(
            p,
            bar,
            ui.ctx().animate_value_with_time(
                egui::Id::unique(("card-prog", row.id)),
                row.frac(),
                0.18,
            ),
            match row.status {
                Status::Active => theme::pal().ink,
                Status::Done | Status::Seeding => theme::pal().ok,
                Status::Failed => theme::pal().err,
                _ => theme::pal().ink4.gamma_multiply(0.6),
            },
        );
    }
    p.text(
        pos2(pct_right, bar_y + 2.5),
        Align2::RIGHT_CENTER,
        pct,
        FontId::proportional(theme::F_PILL),
        theme::pal().ink3,
    );

    let label = row.status.label();
    let note = row.note();
    let meta = if note.starts_with(label) {
        note
    } else {
        format!("{label} · {note}")
    };
    let meta_color = if row.status == Status::Failed {
        theme::pal().err
    } else {
        theme::pal().ink3
    };
    let meta = theme::galley(
        ui,
        &meta,
        theme::F_META,
        meta_color,
        (rect.right() - 26.0 - text_x).max(40.0),
    );
    p.galley(pos2(text_x, rect.top() + 65.0), meta, meta_color);
}

/// 大小列两行：总大小 + 已下载（`§4`：主值 12.5 右对齐，第二行 11）。
fn size_texts(row: &Row) -> (String, String) {
    if row.size == 0 {
        return ("—".to_owned(), String::new());
    }
    let sub = if row.downloaded > 0 {
        crate::fmt::bytes(row.downloaded)
    } else {
        String::new()
    };
    (crate::fmt::bytes(row.size), sub)
}

/// 28×28 的行内图标按钮：悬停才有 HOVER 底，键盘焦点由外层行负责（§4）。
fn icon_button(ui: &mut egui::Ui, rect: Rect, id: (&str, u64), icon: Icon, tip: &str) -> bool {
    let resp = ui.interact(rect, ui.scope_id().with(id), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(7), theme::pal().hover);
    }
    light_content_button_border(ui.painter(), rect, CornerRadius::same(7));
    if resp.has_focus() {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(7),
            Stroke::new(1.5, theme::pal().accent),
            StrokeKind::Inside,
        );
    }
    theme::paint_icon(
        ui.painter(),
        rect.center(),
        if resp.hovered() {
            theme::pal().ink
        } else {
            theme::pal().ink3
        },
        icon,
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tip));
    let clicked = resp.clicked();
    resp.on_hover_text(tip);
    clicked
}

/// 行内操作：只在悬停/选中时出现；失败行常显「重试」（§4）。
fn row_buttons(
    ui: &mut egui::Ui,
    rect: Rect,
    row: &Row,
    c: &Cols,
    hot: bool,
    selected: bool,
) -> Option<Action> {
    if c.act_x <= 0.0 {
        return None; // 窄窗没有操作列
    }
    // 与大小列主值共用同一水平中线，而不是行本身的垂直中心。
    let y = rect.top() + 25.0;
    let del = Rect::from_min_size(
        pos2(c.act_x + 40.0, y - theme::BTN_H / 2.0),
        vec2(theme::BTN_ICON, theme::BTN_ICON),
    );
    if row.status == Status::Failed {
        ui.painter().rect_filled(
            Rect::from_min_max(
                pos2(c.pill_x, y - 12.0),
                pos2(c.pill_x + c.pill_w, y + 12.0),
            ),
            CornerRadius::same(6),
            if selected {
                theme::pal().sel
            } else {
                theme::pal().surface
            },
        );
    }

    if row.status == Status::Failed {
        let r = Rect::from_min_size(
            pos2(c.act_x, y - theme::BTN_H / 2.0),
            vec2(40.0, theme::BTN_H),
        );
        let resp = ui.interact(r, ui.scope_id().with(("row-retry", row.id)), Sense::click());
        if resp.hovered() {
            ui.painter()
                .rect_filled(r, CornerRadius::same(7), theme::pal().sel);
        }
        light_content_button_border(ui.painter(), r, CornerRadius::same(7));
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            "重试",
            FontId::proportional(theme::F_PILL),
            if resp.hovered() {
                theme::pal().ink
            } else {
                theme::pal().err
            },
        );
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "重试"));
        if resp.clicked() {
            return Some(Action::Retry);
        }
        if hot && icon_button(ui, del, ("row-del", row.id), Icon::Trash, "删除任务") {
            return Some(Action::Delete);
        }
        return None;
    }

    if !hot {
        return None;
    }

    let toggle = Rect::from_min_size(
        pos2(c.act_x + 8.0, y - theme::BTN_H / 2.0),
        vec2(theme::BTN_ICON, theme::BTN_ICON),
    );
    let can_toggle = matches!(
        row.status,
        Status::Active | Status::Seeding | Status::Paused | Status::Queued | Status::Probing
    );
    if can_toggle {
        let (icon, tip) = if row.status == Status::Paused {
            (Icon::Play, "继续")
        } else {
            (Icon::Pause, "暂停")
        };
        if icon_button(ui, toggle, ("row-toggle", row.id), icon, tip) {
            return Some(Action::Toggle);
        }
    }
    if icon_button(ui, del, ("row-del", row.id), Icon::Trash, "删除任务") {
        return Some(Action::Delete);
    }
    None
}

fn empty_state(ui: &mut egui::Ui) -> bool {
    let rect = ui.max_rect();
    let width = (rect.width() - 32.0).min(640.0).max(280.0);
    let height = (rect.height() - 16.0).min(220.0).max(164.0);
    let card = Rect::from_center_size(rect.center(), vec2(width, height));
    let p = ui.painter().clone();
    p.rect_filled(card, CornerRadius::same(14), theme::pal().sunken);
    p.rect_stroke(
        card,
        CornerRadius::same(14),
        Stroke::new(1.0, theme::pal().line),
        StrokeKind::Inside,
    );

    let compact = height < 230.0;
    let title_y = card.top() + if compact { 62.0 } else { 88.0 };
    p.text(
        pos2(card.center().x, title_y),
        Align2::CENTER_CENTER,
        "把下载链接粘进来",
        FontId::proportional(16.0),
        theme::pal().ink,
    );
    p.text(
        pos2(card.center().x, title_y + 24.0),
        Align2::CENTER_CENTER,
        "Ctrl+V 粘贴链接或磁力，也可以把 .torrent 拖进来",
        FontId::proportional(12.5),
        theme::pal().ink3,
    );

    let clicked = action_button(
        ui,
        Rect::from_center_size(
            pos2(card.center().x, title_y + if compact { 58.0 } else { 64.0 }),
            vec2(theme::BTN_W_WIDE, theme::BTN_H),
        ),
        "添加链接",
        DialogButtonKind::Primary,
        true,
        "空态添加链接",
    )
    .clicked();

    clicked
}

fn write_png(path: &Path, img: &egui::ColorImage) -> std::io::Result<()> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::File::create(path)?;
    let w = std::io::BufWriter::new(file);
    let mut enc = png::Encoder::new(w, img.size[0] as u32, img.size[1] as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc
        .write_header()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    writer
        .write_image_data(img.as_raw())
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(())
}

/// 「选择下载目录」：调系统的目录树。
///
/// 用 `SHBrowseForFolderW` 而不是 Vista 的 `IFileOpenDialog`：后者要在 windows-sys 上
/// 手工搭 COM vtable，五十行 `unsafe` 只为换一个新一点的边框；这一个二十行、零 COM。
/// 选完是模态的，期间窗口不重绘——原生对话框本来就该是模态的。
#[cfg(windows)]
fn pick_folder(title: &str) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoTaskMemFree,
    };
    use windows_sys::Win32::UI::Shell::{
        BIF_EDITBOX, BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW, SHBrowseForFolderW,
        SHGetPathFromIDListW,
    };

    let title: Vec<u16> = std::ffi::OsStr::new(title)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut shown = [0u16; 260];
    unsafe {
        // BIF_NEWDIALOGSTYLE 要求调用线程先初始化过 COM。
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        );
        let bi = BROWSEINFOW {
            hwndOwner: std::ptr::null_mut(),
            pidlRoot: std::ptr::null_mut(),
            pszDisplayName: shown.as_mut_ptr(),
            lpszTitle: title.as_ptr(),
            ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE | BIF_EDITBOX,
            lpfn: None,
            lParam: 0,
            iImage: 0,
        };
        let pidl = SHBrowseForFolderW(&bi);
        if pidl.is_null() {
            return None;
        }
        let mut path = [0u16; 260];
        let ok = SHGetPathFromIDListW(pidl, path.as_mut_ptr());
        CoTaskMemFree(pidl as *const core::ffi::c_void);
        if ok == 0 {
            return None;
        }
        let len = path.iter().position(|&c| c == 0).unwrap_or(0);
        if len == 0 {
            return None;
        }
        Some(PathBuf::from(String::from_utf16_lossy(&path[..len])))
    }
}

#[cfg(not(windows))]
fn pick_folder(_title: &str) -> Option<PathBuf> {
    None
}

fn open_path(path: &Path, folder: bool) {
    #[cfg(windows)]
    {
        let mut cmd = if folder {
            let mut c = std::process::Command::new("explorer");
            c.arg(path);
            c
        } else {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "start", ""]);
            c.arg(path);
            c
        };
        let _ = cmd.spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = (path, folder);
    }
}

fn remove_download_data(path: &Path) {
    if path.is_dir() {
        let _ = std::fs::remove_dir_all(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Instant;

    use eframe::egui;

    use super::{
        AddOrigin, AddPreview, Adding, BulkKind, DeleteIntent, DraftTorrent, EnterAction,
        HeadDensity, ResizeEdge, Row, Status, TorSource, add_can_submit, bulk_action, can_pause,
        can_start, card_mode, cols, delete_requires_confirmation, detail_is_fixed,
        detail_is_overlay, detail_is_peek, enter_action, file_type, first_add_target, head_layout,
        only_for_checked, paste_target, remove_download_data, resize_zones, seed_ratio_enabled,
        torrent_needs_pick,
    };
    #[cfg(windows)]
    use super::{NativeWindowRect, resized_window};

    #[test]
    fn paste_uses_the_link_inside_shared_text() {
        assert_eq!(
            paste_target("下载这个：https://example.com/a.zip。"),
            "https://example.com/a.zip。"
        );
        assert_eq!(
            paste_target("magnet:?xt=urn:btih:abc"),
            "magnet:?xt=urn:btih:abc"
        );
        assert_eq!(paste_target("  plain  "), "plain");
    }

    #[test]
    fn add_preview_uses_the_first_non_empty_link_and_known_file_type() {
        assert_eq!(
            first_add_target("  \n 下载 https://example.com/archive.7z \n"),
            "https://example.com/archive.7z"
        );
        assert_eq!(file_type("movie.mkv"), "mkv");
        assert_eq!(file_type("Readme"), "文件");
    }

    #[test]
    fn add_dialog_only_allows_submit_after_preview_is_ready() {
        let mut add = Adding {
            text: "https://example.com/a.zip".into(),
            fresh: true,
            origin: AddOrigin::Manual,
            headers: Vec::new(),
            header_url: String::new(),
            preview_id: 7,
            preview_for: "https://example.com/a.zip".into(),
            preview_due: Instant::now(),
            preview: AddPreview::Loading,
            torrent: None,
            checked: Vec::new(),
        };
        assert!(!add_can_submit(&add));
        add.preview = AddPreview::Http {
            name: "a.zip".into(),
            size: Some(10),
            content_type: Some("application/zip".into()),
            final_url: "https://example.com/a.zip".into(),
        };
        assert!(add_can_submit(&add));

        add.text = "magnet:?xt=urn:btih:abc".into();
        add.preview_for = add.text.clone();
        add.preview = AddPreview::Torrent {
            name: "demo".into(),
            total: 10,
            files: vec![super::TorrentFile {
                name: "a.bin".into(),
                len: 10,
            }],
            info_hash: "abc".into(),
        };
        add.torrent = Some(DraftTorrent {
            url: add.text.clone(),
            source: TorSource::Magnet(add.text.clone()),
        });
        add.checked = vec![false];
        assert!(!add_can_submit(&add));
        add.checked[0] = true;
        assert!(add_can_submit(&add));
    }

    #[test]
    fn wide_rows_reserve_aligned_size_speed_and_status_columns() {
        let c = cols(1600.0, 1500.0, true);
        assert!(c.speed_w > 0.0);
        assert!(c.speed_right < c.size_right);
        assert!(c.size_right + 7.0 <= c.pill_x);
    }

    #[test]
    fn standard_rows_keep_a_readable_name_and_exact_percentage() {
        let c = cols(900.0, 588.0, false);
        let name_w = c.name_right - (900.0 - 588.0 + 68.0);
        assert!(name_w >= 196.0, "1180 三栏名称只有 {name_w}px");
        assert!(c.pct_w > 0.0);
        assert_eq!(c.speed_w, 0.0);
        assert!(c.act_x > c.size_right);
    }

    #[test]
    fn speed_column_requires_visible_speed_and_wide_space() {
        let no_speed = cols(900.0, 848.0, false);
        assert_eq!(no_speed.speed_w, 0.0);
        let narrow_speed = cols(900.0, 620.0, true);
        assert_eq!(narrow_speed.speed_w, 0.0);
        let wide_speed = cols(1100.0, 900.0, true);
        assert!(wide_speed.speed_w > 0.0);
    }

    #[test]
    fn permanent_delete_removes_files_and_directories() {
        let root = std::env::temp_dir().join(format!("volant-delete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("a.bin");
        std::fs::write(&file, b"x").unwrap();
        remove_download_data(&file);
        assert!(!file.exists());
        let dir = root.join("pack");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.bin"), b"x").unwrap();
        remove_download_data(&dir);
        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn seed_ratio_is_only_interactive_while_seeding_is_enabled() {
        assert!(!seed_ratio_enabled(false));
        assert!(seed_ratio_enabled(true));
    }

    #[test]
    fn permanent_delete_always_requires_confirmation() {
        assert!(delete_requires_confirmation(DeleteIntent::Files, false));
        assert!(delete_requires_confirmation(DeleteIntent::Task, true));
        assert!(!delete_requires_confirmation(DeleteIntent::Task, false));
    }

    #[test]
    fn completed_and_failed_rows_do_not_offer_start_or_pause() {
        assert!(!can_start(Status::Done));
        assert!(!can_pause(Status::Done));
        assert!(can_start(Status::Failed));
        assert!(!can_pause(Status::Failed));
    }

    #[test]
    fn bulk_action_only_targets_the_rows_in_the_current_view() {
        let mut running = Row::new(1, "http://x/running".into());
        running.status = Status::Active;
        let mut queued = Row::new(2, "http://x/queued".into());
        queued.status = Status::Queued;
        let mut paused = Row::new(3, "http://x/paused".into());
        paused.status = Status::Paused;
        let mut failed = Row::new(4, "http://x/failed".into());
        failed.status = Status::Failed;
        let mut done = Row::new(5, "http://x/done".into());
        done.status = Status::Done;

        let action = bulk_action(&[&running, &queued, &paused]).unwrap();
        assert_eq!(action.kind, BulkKind::Pause);
        assert_eq!(action.ids, vec![1, 2]);
        assert!(!action.ids.contains(&3), "暂停动作不能带上已暂停任务");

        let action = bulk_action(&[&paused, &failed]).unwrap();
        assert_eq!(action.kind, BulkKind::Resume);
        assert_eq!(action.ids, vec![3]);

        assert!(bulk_action(&[&done, &failed]).is_none());
        assert!(bulk_action(&[]).is_none());
    }

    #[test]
    fn file_pick_distinguishes_all_some_and_none() {
        assert_eq!(only_for_checked(&[]), None);
        assert_eq!(only_for_checked(&[false, false]), None);
        assert_eq!(only_for_checked(&[true, true]), Some(None));
        assert_eq!(
            only_for_checked(&[true, false, true]),
            Some(Some(vec![0, 2]))
        );
    }

    #[test]
    fn direct_torrents_skip_the_second_file_picker() {
        assert!(torrent_needs_pick(2, true, false));
        assert!(!torrent_needs_pick(2, false, true));
        assert!(!torrent_needs_pick(2, false, false));
        assert!(!torrent_needs_pick(1, true, false));
    }

    #[test]
    fn short_windows_compress_the_page_header_in_two_steps() {
        assert_eq!(head_layout(760.0).density, HeadDensity::Full);
        assert_eq!(head_layout(680.0).density, HeadDensity::Full);
        assert_eq!(head_layout(679.0).density, HeadDensity::Compact);
        assert_eq!(head_layout(520.0).density, HeadDensity::Compact);
        assert_eq!(head_layout(519.0).density, HeadDensity::Minimal);
        assert_eq!(head_layout(340.0).height, 68.0);
    }

    #[test]
    fn resize_zones_cover_all_edges_but_not_the_content_center() {
        let window = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(800.0, 600.0));
        let zones = resize_zones(window);
        assert_eq!(zones.len(), 8);
        for edge in [
            ResizeEdge::North,
            ResizeEdge::South,
            ResizeEdge::East,
            ResizeEdge::West,
            ResizeEdge::NorthEast,
            ResizeEdge::SouthEast,
            ResizeEdge::NorthWest,
            ResizeEdge::SouthWest,
        ] {
            assert!(zones.iter().any(|(candidate, _)| *candidate == edge));
        }
        assert!(
            zones
                .iter()
                .all(|(_, rect)| !rect.contains(window.center()))
        );
    }

    #[cfg(windows)]
    #[test]
    fn manual_resize_moves_the_dragged_edges_and_honors_minimum_size() {
        let start = NativeWindowRect {
            left: 100,
            top: 100,
            right: 700,
            bottom: 500,
        };
        assert_eq!(
            resized_window(
                start,
                (700, 300),
                (760, 340),
                ResizeEdge::SouthEast,
                520,
                340
            ),
            NativeWindowRect {
                left: 100,
                top: 100,
                right: 760,
                bottom: 540,
            }
        );
        assert_eq!(
            resized_window(start, (100, 300), (180, 300), ResizeEdge::West, 520, 340),
            NativeWindowRect {
                left: 180,
                top: 100,
                right: 700,
                bottom: 500,
            }
        );
        assert_eq!(
            resized_window(
                start,
                (700, 500),
                (450, 250),
                ResizeEdge::SouthEast,
                520,
                340
            ),
            NativeWindowRect {
                left: 100,
                top: 100,
                right: 620,
                bottom: 440,
            }
        );
        assert_eq!(
            resized_window(
                start,
                (100, 100),
                (150, 140),
                ResizeEdge::NorthWest,
                520,
                340
            ),
            NativeWindowRect {
                left: 150,
                top: 140,
                right: 700,
                bottom: 500,
            }
        );
    }

    #[test]
    fn compact_widths_use_cards_and_keep_a_detail_entry() {
        assert!(!card_mode(620.0));
        assert!(card_mode(619.0));
        assert!(card_mode(520.0));

        assert!(detail_is_fixed(1180.0));
        assert!(detail_is_peek(1040.0, true));
        assert!(!detail_is_peek(1039.0, true));
        assert!(detail_is_overlay(1039.0, true));
        assert!(detail_is_overlay(520.0, true));
        assert!(!detail_is_overlay(520.0, false));
    }

    #[test]
    fn enter_opens_the_file_only_for_completed_tasks() {
        assert_eq!(enter_action(Status::Done), EnterAction::OpenFile);
        for status in [
            Status::Queued,
            Status::Probing,
            Status::Active,
            Status::Paused,
            Status::Failed,
            Status::Seeding,
        ] {
            assert_eq!(enter_action(status), EnterAction::OpenDetail);
        }
    }

    #[test]
    fn drop_hover_only_accepts_torrent_files() {
        let ctx = egui::Context::default();
        let mut valid = egui::RawInput::default();
        valid.hovered_files.push(egui::HoveredFile {
            path: Some(PathBuf::from("release.torrent")),
            mime: String::new(),
        });
        let mut valid_output = ctx.run_ui(valid, |ui| {
            assert_eq!(super::drag_hover(ui.ctx()), Some(true))
        });
        valid_output.textures_delta.clear();

        let mut invalid = egui::RawInput::default();
        invalid.hovered_files.push(egui::HoveredFile {
            path: Some(PathBuf::from("release.zip")),
            mime: String::new(),
        });
        let mut invalid_output = ctx.run_ui(invalid, |ui| {
            assert_eq!(super::drag_hover(ui.ctx()), Some(false))
        });
        invalid_output.textures_delta.clear();
    }

    #[test]
    fn text_menu_replaces_unicode_ranges_by_character_index() {
        let mut text = "ab中文cd".to_string();
        let range =
            egui::text::CCursorRange::two(egui::text::CCursor::new(1), egui::text::CCursor::new(4));
        let cursor = super::replace_char_range(&mut text, range, "下载");
        assert_eq!(text, "a下载cd");
        assert_eq!(cursor.primary, egui::text::CCursor::new(3));
    }
}
