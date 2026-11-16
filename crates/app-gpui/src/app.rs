use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt;
use gpui_kit::component::{
    Disableable, FocusableExt, Icon, IndexPath, Selectable, Sizable, Size, TitleBar, WindowExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt, PopupMenuItem},
    progress::Progress,
    scroll::ScrollableElement,
    searchable_list::SearchableVec,
    select::{Select, SelectEvent, SelectState},
    spinner::Spinner,
    switch::Switch,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use volant_app_core::capture::{Capture, CaptureReply, Listener};
use volant_app_core::clipboard::Watch;
use volant_app_core::control::{Cmd, Control, UiEvent};
use volant_app_core::fmt;
use volant_app_core::model::{
    Kind, Paths, Persisted, ProxyMode, Row, Settings, Status, Store, ThemeMode as AppThemeMode,
    TorrentFile, friendly_error, is_magnet, is_valid_http_proxy, writable_download_dir,
};
use volant_app_core::notify::{Notifier, UiNotifier};
use volant_app_core::video::{self, VideoContext, VideoLookup, VideoTask};

use crate::platform::{TrayEvent, TrayIcon};
use crate::theme::{self, VolantColors};

const ERROR_COPY: &str = "无法读取这个链接。请检查链接是否有效，或确认服务器允许下载。";
const VIDEO_LOOKUP_TTL: Duration = Duration::from_secs(10 * 60);
const RATE_VALUES: [Option<u64>; 5] = [
    None,
    Some(1_048_576),
    Some(5_242_880),
    Some(10_485_760),
    Some(20_971_520),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Active,
    Done,
    Failed,
}

enum VideoLookupSlot {
    Pending,
    Ready(Box<VideoLookup>, Instant),
    Failed(String, Instant),
}

impl Filter {
    const ALL: [Filter; 4] = [Filter::All, Filter::Active, Filter::Done, Filter::Failed];

    fn label(self) -> &'static str {
        match self {
            Filter::All => "全部下载",
            Filter::Active => "进行中",
            Filter::Done => "已完成",
            Filter::Failed => "失败",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Filter::All => "all",
            Filter::Active => "active",
            Filter::Done => "done",
            Filter::Failed => "failed",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Filter::All => IconName::List,
            Filter::Active => IconName::Download,
            Filter::Done => IconName::Check,
            Filter::Failed => IconName::CircleX,
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

struct GpuiNotifier {
    wake: tokio::sync::mpsc::UnboundedSender<()>,
}

impl UiNotifier for GpuiNotifier {
    fn notify(&self) {
        let _ = self.wake.send(());
    }
}

enum AddPreview {
    Empty,
    Loading,
    Ready {
        name: String,
        size: Option<u64>,
        final_url: String,
    },
    Failed,
}

#[derive(Clone, PartialEq, Eq)]
enum AddSource {
    Http(String),
    Magnet(String),
    Torrent(PathBuf),
}

struct AddDownloadView {
    parent: WeakEntity<VolantShell>,
    input: Entity<InputState>,
    preview_id: u64,
    requested_url: Option<String>,
    preview: AddPreview,
    source: Option<AddSource>,
    preview_generation: u64,
    preview_task: Option<Task<()>>,
    headers: Vec<(String, String)>,
    destination: PathBuf,
    focus_requested: bool,
    _input_subscription: Subscription,
}

impl AddDownloadView {
    fn new(
        parent: WeakEntity<VolantShell>,
        destination: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("粘贴链接，或拖入 .torrent 文件"));
        let _input_subscription = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.preview = AddPreview::Empty;
                    this.requested_url = None;
                    this.headers.clear();
                    this.source = this.source_from_input(cx);
                    this.schedule_auto_preview(window, cx);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    if this.is_ready(cx) {
                        this.submit_now(window, cx);
                    } else {
                        this.start_preview(cx);
                    }
                }
                _ => {}
            },
        );
        input.update(cx, |input, cx| input.focus(window, cx));
        Self {
            parent,
            input,
            preview_id: 0,
            requested_url: None,
            preview: AddPreview::Empty,
            source: None,
            preview_generation: 0,
            preview_task: None,
            headers: Vec::new(),
            destination,
            focus_requested: false,
            _input_subscription,
        }
    }

    fn source_from_input(&self, cx: &App) -> Option<AddSource> {
        let value = self.input.read(cx).value().trim().to_string();
        if value.starts_with("http://") || value.starts_with("https://") {
            Some(AddSource::Http(value))
        } else if is_magnet(&value) {
            Some(AddSource::Magnet(value))
        } else if value.to_ascii_lowercase().ends_with(".torrent") && Path::new(&value).is_file() {
            Some(AddSource::Torrent(PathBuf::from(value)))
        } else {
            None
        }
    }

    fn schedule_auto_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.preview_generation = self.preview_generation.wrapping_add(1);
        let generation = self.preview_generation;
        self.preview_task.take();
        self.preview_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(380))
                .await;
            let _ = this.update_in(cx, |this, _window, cx| {
                if this.preview_generation == generation {
                    this.start_preview(cx);
                }
            });
        }));
    }

    fn start_preview(&mut self, cx: &mut Context<Self>) {
        if matches!(self.preview, AddPreview::Loading) {
            return;
        }
        let source = self.source_from_input(cx).or_else(|| self.source.clone());
        let Some(source) = source else {
            self.requested_url = Some(self.input.read(cx).value().to_string());
            self.preview = AddPreview::Failed;
            cx.notify();
            return;
        };
        self.preview_id = self.preview_id.wrapping_add(1);
        self.preview_id |= 1_u64 << 63;
        let preview_id = self.preview_id;
        self.requested_url = Some(match &source {
            AddSource::Http(url) | AddSource::Magnet(url) => url.clone(),
            AddSource::Torrent(path) => path.display().to_string(),
        });
        self.source = Some(source.clone());
        self.preview = AddPreview::Loading;
        let headers = self.headers.clone();
        let _ = self.parent.update(cx, |parent, cx| {
            match source {
                AddSource::Http(url) => parent.request_preview(preview_id, url, headers),
                AddSource::Magnet(url) => parent.request_torrent_preview(
                    preview_id,
                    volant_app_core::control::TorSource::Magnet(url),
                ),
                AddSource::Torrent(path) => parent.request_torrent_preview(
                    preview_id,
                    volant_app_core::control::TorSource::File(path),
                ),
            }
            cx.notify();
        });
        cx.notify();
    }

    fn retry_preview(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.start_preview(cx);
    }

    fn add_torrent(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = volant_app_core::native::pick_torrent_file("选择要读取的种子文件")
        {
            self.load_torrent(path, window, cx);
        }
    }

    fn load_torrent(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.preview_task.take();
        self.input.update(cx, |input, cx| {
            input.set_value(path.display().to_string(), window, cx);
        });
        self.source = Some(AddSource::Torrent(path));
        self.headers.clear();
        self.preview = AddPreview::Empty;
        self.start_preview(cx);
    }

    fn set_preview(&mut self, id: u64, name: String, size: Option<u64>, final_url: String) {
        if id == self.preview_id {
            self.preview = AddPreview::Ready {
                name,
                size,
                final_url,
            };
        }
    }

    fn set_preview_error(&mut self, id: u64, _: String) {
        if id == self.preview_id {
            self.preview = AddPreview::Failed;
        }
    }

    fn set_torrent_preview(&mut self, id: u64, name: String, size: u64) {
        if id == self.preview_id {
            let final_url = match &self.source {
                Some(AddSource::Torrent(path)) => path.display().to_string(),
                Some(AddSource::Magnet(url)) => url.clone(),
                _ => String::new(),
            };
            self.preview = AddPreview::Ready {
                name,
                size: Some(size),
                final_url,
            };
        }
    }

    fn is_ready(&self, cx: &App) -> bool {
        let current = self.source_from_input(cx).or_else(|| self.source.clone());
        matches!(&self.preview, AddPreview::Ready { .. })
            && current.is_some()
            && current == self.source
    }

    fn submit_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_ready(cx) || matches!(self.preview, AddPreview::Loading) {
            return;
        }
        let Some(source) = self.source.clone() else {
            return;
        };
        let headers = self.headers.clone();
        let _ = self.parent.update(cx, |parent, cx| {
            match source {
                AddSource::Http(url) | AddSource::Magnet(url) => {
                    parent.queue_url_with_headers(url, headers);
                }
                AddSource::Torrent(path) => {
                    parent.queue_torrent_file(&path);
                }
            }
            cx.notify();
        });
        window.close_dialog(cx);
    }

    fn submit(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.submit_now(window, cx);
    }

    fn set_url(
        &mut self,
        url: String,
        headers: Vec<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.input.update(cx, |input, cx| {
            input.set_value(url, window, cx);
        });
        self.preview = AddPreview::Empty;
        self.requested_url = None;
        self.headers = headers;
        self.source = self.source_from_input(cx);
        self.schedule_auto_preview(window, cx);
        cx.notify();
    }

    fn set_destination(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = volant_app_core::native::pick_folder("选择下载目录") {
            let accepted = self
                .parent
                .update(cx, |parent, cx| {
                    let accepted = parent.set_download_dir(path.clone());
                    cx.notify();
                    accepted
                })
                .unwrap_or(false);
            if accepted {
                self.destination = path;
                cx.notify();
            }
        }
        let _ = window;
    }

    fn seed_capture_state(
        &mut self,
        state: &str,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.input.update(cx, |input, cx| {
            input.set_value(url, window, cx);
        });
        self.requested_url = Some(url.to_string());
        self.source = self.source_from_input(cx);
        self.preview = match state {
            "loading" => AddPreview::Loading,
            "failed" => AddPreview::Failed,
            "ready" => AddPreview::Ready {
                name: "debian-13.2.0-amd64-netinst.iso".into(),
                size: Some(715_653_120),
                final_url: url.into(),
            },
            _ => AddPreview::Empty,
        };
        cx.notify();
    }

    fn state_area(&self, colors: VolantColors) -> AnyElement {
        match &self.preview {
            AddPreview::Empty => div()
                .h(px(55.0))
                .flex()
                .items_center()
                .px(px(1.0))
                .text_size(px(theme::text::BODY))
                .text_color(colors.muted)
                .child("粘贴链接后显示文件名、大小和保存位置。")
                .into_any_element(),
            AddPreview::Loading => div()
                .h(px(55.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .px(px(1.0))
                .child(Spinner::new().with_size(Size::Small).color(colors.current))
                .child(
                    div()
                        .text_size(px(theme::text::BODY))
                        .text_color(colors.muted)
                        .child("正在读取文件信息…"),
                )
                .into_any_element(),
            AddPreview::Ready {
                name,
                size,
                final_url,
            } => {
                let source_label = match &self.source {
                    Some(AddSource::Http(_)) => source_domain(final_url),
                    Some(AddSource::Magnet(_)) => "Magnet 链接".to_string(),
                    Some(AddSource::Torrent(_)) => "本地种子".to_string(),
                    None => source_domain(final_url),
                };
                div()
                    .h(px(55.0))
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .border_t_1()
                    .border_b_1()
                    .border_color(colors.border)
                    .child(
                        div()
                            .w(px(29.0))
                            .h(px(33.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(colors.border)
                            .child(
                                Icon::new(IconName::File)
                                    .with_size(Size::Size(px(14.0)))
                                    .text_color(colors.current),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .v_flex()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_size(px(13.0))
                                    .font_weight(FontWeight(600.0))
                                    .text_color(colors.text)
                                    .child(name.clone()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(px(5.0))
                                    .text_size(px(theme::text::BODY))
                                    .text_color(colors.muted)
                                    .child(source_label)
                                    .child(div().text_color(colors.faint).child("支持断点续传")),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(theme::text::BODY))
                            .font_family("Cascadia Mono")
                            .text_color(colors.text)
                            .child(size.map_or_else(|| "大小未知".into(), fmt::bytes)),
                    )
                    .child(status_inline_text("可下载", colors.current))
                    .into_any_element()
            }
            AddPreview::Failed => div()
                .h(px(55.0))
                .flex()
                .items_center()
                .gap(px(12.0))
                .text_size(px(theme::text::BODY))
                .text_color(colors.danger)
                .child(div().flex_1().min_w(px(0.0)).child(ERROR_COPY))
                .into_any_element(),
        }
    }
}

impl Render for AddDownloadView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focus_requested {
            self.focus_requested = true;
            let handle = self.input.read(cx).focus_handle(cx);
            window.defer(cx, move |window, cx| handle.focus(window, cx));
        }
        let colors = theme::current(cx);
        let ready = self.is_ready(cx);
        let failed = matches!(self.preview, AddPreview::Failed);
        let loading = matches!(self.preview, AddPreview::Loading);
        let focused = self.input.read(cx).focus_handle(cx).is_focused(window);
        let border = if failed {
            colors.danger
        } else if focused || ready {
            colors.current.opacity(0.55)
        } else {
            colors.border_strong
        };
        let destination = self.destination.display().to_string();
        let state_area = self.state_area(colors);
        let ready_row = matches!(self.preview, AddPreview::Ready { .. }).then(|| {
            div()
                .h(px(36.0))
                .flex()
                .items_center()
                .gap(px(7.0))
                .border_b_1()
                .border_color(colors.border)
                .child(
                    Icon::new(IconName::Folder)
                        .with_size(Size::Size(px(12.0)))
                        .text_color(colors.muted),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .font_family("Cascadia Mono")
                        .text_size(px(theme::text::BODY))
                        .text_color(colors.muted)
                        .child(destination),
                )
                .child(
                    Button::new("change-destination")
                        .ghost()
                        .compact()
                        .label("更改")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.set_destination(window, cx)),
                        ),
                )
        });

        div()
            .v_flex()
            .w_full()
            .h(px(theme::size::ADD_H))
            .overflow_hidden()
            .rounded(px(10.0))
            .border_1()
            .border_color(colors.border)
            .bg(colors.surface)
            .text_color(colors.text)
            .key_context("NewDownload")
            .on_drop::<ExternalPaths>(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                if let Some(path) = paths.paths().iter().find(|path| {
                    path.extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("torrent"))
                }) {
                    cx.stop_propagation();
                    this.load_torrent(path.to_path_buf(), window, cx);
                } else {
                    this.preview = AddPreview::Failed;
                    cx.notify();
                }
            }))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                if key == "escape" && !event.keystroke.modifiers.modified() {
                    window.prevent_default();
                    cx.stop_propagation();
                    window.close_dialog(cx);
                    return;
                }
                if key == "enter" && !event.keystroke.modifiers.modified() {
                    window.prevent_default();
                    cx.stop_propagation();
                    if this.is_ready(cx) {
                        this.submit_now(window, cx);
                    } else {
                        this.start_preview(cx);
                    }
                }
            }))
            .child(dialog_title_bar("新建下载", colors))
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .px(px(24.0))
                    .pt(px(17.0))
                    .gap(px(13.0))
                    .child(
                        div()
                            .v_flex()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .text_size(px(21.0))
                                    .font_weight(FontWeight(630.0))
                                    .line_height(px(24.0))
                                    .child("新建下载"),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::text::BODY))
                                    .text_color(colors.muted)
                                    .child("粘贴链接，确认后立即开始。"),
                            ),
                    )
                    .child(
                        div()
                            .h(px(38.0))
                            .flex()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .relative()
                                    .child(Styled::h(
                                        Input::new(&self.input)
                                            .id("new-download-url")
                                            .aria_label("下载链接或种子文件")
                                            .focus_ring(false)
                                            .border_color(border)
                                            .pr(px(34.0))
                                            .w_full(),
                                        px(38.0),
                                    ))
                                    .when(failed, |this| {
                                        this.child(
                                            Button::new("retry-preview-icon")
                                                .ghost()
                                                .compact()
                                                .icon(IconName::RotateCcw)
                                                .accessibility_label("重试读取")
                                                .tooltip("重试读取")
                                                .absolute()
                                                .right(px(4.0))
                                                .top(px(4.0))
                                                .w(px(30.0))
                                                .h(px(30.0))
                                                .on_click(cx.listener(Self::retry_preview)),
                                        )
                                    }),
                            )
                            .child(
                                Button::new("add-torrent-file")
                                    .secondary()
                                    .icon(IconName::Plus)
                                    .label("添加")
                                    .border_1()
                                    .border_color(colors.border_strong)
                                    .h(px(38.0))
                                    .w(px(76.0))
                                    .disabled(loading)
                                    .on_click(cx.listener(Self::add_torrent)),
                            ),
                    )
                    .child(state_area)
                    .when_some(ready_row, |this, row| this.child(row)),
            )
            .child(
                div()
                    .h(px(theme::size::ADD_FOOTER))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(colors.border)
                    .px(px(24.0))
                    .child(
                        div()
                            .text_size(px(theme::text::BODY))
                            .text_color(colors.faint)
                            .child("Enter 开始下载"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(8.0))
                            .child(
                                Button::new("cancel-new-download")
                                    .secondary()
                                    .label("取消")
                                    .border_1()
                                    .border_color(colors.border_strong)
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(
                                Button::new("confirm-download")
                                    .primary()
                                    .label("立即下载")
                                    .disabled(!ready)
                                    .on_click(cx.listener(Self::submit)),
                            ),
                    ),
            )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsCategory {
    Download,
    Connection,
    Proxy,
    Video,
    Integration,
    Appearance,
}

impl SettingsCategory {
    const ALL: [Self; 6] = [
        Self::Download,
        Self::Connection,
        Self::Proxy,
        Self::Video,
        Self::Integration,
        Self::Appearance,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Download => "下载",
            Self::Connection => "连接",
            Self::Proxy => "代理",
            Self::Video => "视频",
            Self::Integration => "集成",
            Self::Appearance => "外观",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Download => IconName::Download,
            Self::Connection => IconName::Network,
            Self::Proxy => IconName::Shield,
            Self::Video => IconName::Play,
            Self::Integration => IconName::Link,
            Self::Appearance => IconName::Monitor,
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Download => "控制新任务的并发数量与默认保存位置。",
            Self::Connection => "调整每个任务的连接数和总下载带宽。",
            Self::Proxy => "决定新任务如何通过系统网络或指定代理连接。",
            Self::Video => "准备视频解析与 MP4 合成组件，检查运行状态。",
            Self::Integration => "管理浏览器捕获、托盘行为、开机启动和本机宿主。",
            Self::Appearance => "选择跟随系统，或固定浅色和深色外观。",
        }
    }
}

struct SettingsView {
    parent: WeakEntity<VolantShell>,
    settings: Settings,
    dir_input: Entity<InputState>,
    proxy_input: Entity<InputState>,
    limit_select: Entity<SelectState<SearchableVec<String>>>,
    connections_select: Entity<SelectState<SearchableVec<String>>>,
    rate_select: Entity<SelectState<SearchableVec<String>>>,
    notice: Option<String>,
    proxy_notice: Option<String>,
    category: SettingsCategory,
    host_status: volant_app_core::native::HostStatus,
    host_notice: Option<String>,
    dependencies: video::VideoDependencyStatus,
    dependency_notice: Option<String>,
    installing_dependencies: bool,
    save_error: Option<String>,
    _dependency_task: Option<Task<()>>,
    _dir_subscription: Subscription,
    _proxy_subscription: Subscription,
    _limit_select_subscription: Subscription,
    _connections_select_subscription: Subscription,
    _rate_select_subscription: Subscription,
}

impl SettingsView {
    fn new(
        parent: WeakEntity<VolantShell>,
        settings: Settings,
        category: SettingsCategory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let dir = settings.dir.to_string_lossy().to_string();
        let dir_input = cx.new(|cx| InputState::new(window, cx).default_value(dir));
        let proxy_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(settings.proxy_url.clone())
                .placeholder("http://127.0.0.1:7890")
        });
        let limit_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new((1..=20).map(|value| value.to_string()).collect::<Vec<_>>()),
                Some(IndexPath::new(settings.limit.saturating_sub(1))),
                window,
                cx,
            )
        });
        let connections_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new((1..=32).map(|value| value.to_string()).collect::<Vec<_>>()),
                Some(IndexPath::new(settings.connections.saturating_sub(1))),
                window,
                cx,
            )
        });
        let rate_select = cx.new(|cx| {
            let selected_index = RATE_VALUES
                .iter()
                .position(|rate| *rate == settings.rate)
                .unwrap_or(0);
            SelectState::new(
                SearchableVec::new(
                    RATE_VALUES
                        .iter()
                        .map(|rate| rate_label(*rate))
                        .collect::<Vec<_>>(),
                ),
                Some(IndexPath::new(selected_index)),
                window,
                cx,
            )
        });
        let _dir_subscription = cx.subscribe_in(
            &dir_input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    this.apply_dir(window, cx);
                }
            },
        );
        let _proxy_subscription = cx.subscribe_in(
            &proxy_input,
            window,
            |this: &mut Self, _, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    this.apply_manual_proxy(cx);
                }
            },
        );
        let _limit_select_subscription = cx.subscribe_in(
            &limit_select,
            window,
            |this: &mut Self, _, event: &SelectEvent<SearchableVec<String>>, _, cx| {
                if let SelectEvent::Confirm(Some(value)) = event
                    && let Ok(value) = value.parse::<usize>()
                {
                    this.set_limit(value, cx);
                }
            },
        );
        let _connections_select_subscription = cx.subscribe_in(
            &connections_select,
            window,
            |this: &mut Self, _, event: &SelectEvent<SearchableVec<String>>, _, cx| {
                if let SelectEvent::Confirm(Some(value)) = event
                    && let Ok(value) = value.parse::<usize>()
                {
                    this.set_connections(value, cx);
                }
            },
        );
        let _rate_select_subscription = cx.subscribe_in(
            &rate_select,
            window,
            |this: &mut Self, _, event: &SelectEvent<SearchableVec<String>>, _, cx| {
                if let SelectEvent::Confirm(Some(value)) = event
                    && let Some(rate) = RATE_VALUES
                        .iter()
                        .copied()
                        .find(|rate| rate_label(*rate) == *value)
                {
                    this.set_rate(rate, cx);
                }
            },
        );
        Self {
            parent,
            settings,
            dir_input,
            proxy_input,
            limit_select,
            connections_select,
            rate_select,
            notice: None,
            proxy_notice: None,
            category,
            host_status: volant_app_core::native::host_status(),
            host_notice: None,
            dependencies: video::dependency_status(),
            dependency_notice: None,
            installing_dependencies: false,
            save_error: None,
            _dependency_task: None,
            _dir_subscription,
            _proxy_subscription,
            _limit_select_subscription,
            _connections_select_subscription,
            _rate_select_subscription,
        }
    }

    fn persist(&mut self, command: Option<Cmd>, cx: &mut Context<Self>) {
        let settings = self.settings.clone();
        let result = self.parent.update(cx, |parent, cx| {
            let saved = parent.apply_settings(settings, command);
            let error = (!saved).then(|| {
                parent
                    .notice
                    .clone()
                    .unwrap_or_else(|| "无法保存配置文件".into())
            });
            cx.notify();
            error
        });
        if let Ok(error) = result {
            self.save_error = error;
        }
        cx.notify();
    }

    fn set_limit(&mut self, value: usize, cx: &mut Context<Self>) {
        self.settings.limit = value.clamp(1, 20);
        self.persist(Some(Cmd::SetLimit(self.settings.limit)), cx);
    }

    fn set_connections(&mut self, value: usize, cx: &mut Context<Self>) {
        self.settings.connections = value.clamp(1, 32);
        self.persist(Some(Cmd::SetConnections(self.settings.connections)), cx);
    }

    fn set_rate(&mut self, rate: Option<u64>, cx: &mut Context<Self>) {
        self.settings.rate = rate;
        self.persist(Some(Cmd::SetRate(self.settings.rate)), cx);
    }

    fn set_proxy_mode(&mut self, mode: ProxyMode, cx: &mut Context<Self>) {
        self.settings.proxy_mode = mode;
        if mode == ProxyMode::Manual {
            let url = self.proxy_input.read(cx).value().trim().to_string();
            if !is_valid_http_proxy(&url) {
                self.proxy_notice = Some("请输入包含主机名的完整 HTTP 或 HTTPS 代理地址。".into());
                cx.notify();
                return;
            }
            self.settings.proxy_url = url.clone();
            self.proxy_notice = None;
            self.persist(Some(Cmd::SetProxy { mode, url }), cx);
        } else {
            self.proxy_notice = None;
            self.persist(
                Some(Cmd::SetProxy {
                    mode,
                    url: String::new(),
                }),
                cx,
            );
        }
    }

    fn apply_manual_proxy(&mut self, cx: &mut Context<Self>) {
        let url = self.proxy_input.read(cx).value().trim().to_string();
        if !is_valid_http_proxy(&url) {
            self.proxy_notice = Some("请输入包含主机名的完整 HTTP 或 HTTPS 代理地址。".into());
            cx.notify();
            return;
        }
        self.settings.proxy_mode = ProxyMode::Manual;
        self.settings.proxy_url = url.clone();
        self.proxy_notice = None;
        self.persist(
            Some(Cmd::SetProxy {
                mode: ProxyMode::Manual,
                url,
            }),
            cx,
        );
    }

    fn install_host(&mut self, cx: &mut Context<Self>) {
        match volant_app_core::native::install() {
            Ok(lines) => {
                self.host_status = volant_app_core::native::host_status();
                self.host_notice = Some(
                    lines
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "宿主安装完成。".into()),
                );
            }
            Err(error) => {
                self.host_notice = Some(format!("宿主安装失败：{error}"));
            }
        }
        cx.notify();
    }

    fn apply_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.dir_input.read(cx).value().trim().to_string();
        if value.is_empty() {
            self.notice = Some("下载目录不能为空。请选择可写目录。".into());
            self.save_error = None;
            cx.notify();
            return;
        }
        let requested = PathBuf::from(value);
        let (_, note) = writable_download_dir(&requested, &requested);
        if let Some(reason) = note {
            self.notice = Some(reason);
            self.save_error = None;
            cx.notify();
            return;
        }
        self.notice = None;
        self.settings.dir = requested.clone();
        self.dir_input.update(cx, |input, cx| {
            input.set_value(requested.to_string_lossy().to_string(), window, cx);
        });
        self.persist(Some(Cmd::SetDir(requested)), cx);
    }

    fn set_clipboard(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.settings.clipboard = enabled;
        let _ = self.parent.update(cx, |parent, _| {
            parent.set_clipboard_enabled(enabled);
        });
        self.persist(None, cx);
    }

    fn set_skip_add(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.settings.skip_add_dialog = enabled;
        self.persist(None, cx);
    }

    fn set_confirm_delete(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.settings.confirm_delete = enabled;
        self.persist(None, cx);
    }

    fn set_theme(&mut self, mode: AppThemeMode, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.theme = mode;
        let resolved = theme::resolve_mode(mode, window);
        theme::apply(cx, resolved);
        self.persist(None, cx);
    }

    fn set_close_to_tray(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.settings.close_to_tray = enabled;
        self.persist(None, cx);
    }

    fn set_launch_at_startup(&mut self, enabled: bool, cx: &mut Context<Self>) {
        match crate::platform::set_autostart(enabled) {
            Ok(()) => {
                self.settings.launch_at_startup = enabled;
                self.dependency_notice = None;
                self.persist(None, cx);
            }
            Err(error) => {
                self.save_error = Some(error);
                cx.notify();
            }
        }
    }

    fn install_video_dependencies(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.installing_dependencies {
            return;
        }
        self.installing_dependencies = true;
        self.dependency_notice =
            Some("正在准备 yt-dlp 与 ffmpeg。首次下载约 105 MB，请保持网络连接…".into());
        cx.notify();
        self._dependency_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = video::install_dependencies().await;
            let _ = this.update(cx, |this, cx| {
                this.installing_dependencies = false;
                match result {
                    Ok(status) => {
                        this.dependencies = status.clone();
                        if let Some(parent) = this.parent.upgrade() {
                            parent.update(cx, move |parent, cx| {
                                parent.video_dependencies = status;
                                cx.notify();
                            });
                        }
                        this.dependency_notice =
                            Some("视频组件已就绪，可以解析并合成 MP4。".into());
                    }
                    Err(error) => {
                        let status = video::dependency_status();
                        this.dependencies = status.clone();
                        if let Some(parent) = this.parent.upgrade() {
                            parent.update(cx, move |parent, cx| {
                                parent.video_dependencies = status;
                                cx.notify();
                            });
                        }
                        this.dependency_notice = Some(format!("组件安装失败：{error}"));
                    }
                }
                cx.notify();
            });
        }));
    }

    fn seed_directory_error(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = r"Z:\Blocked\Downloads";
        self.dir_input.update(cx, |input, cx| {
            input.set_value(path, window, cx);
        });
        self.notice = Some(format!(
            "下载目录 {path} 写不进（系统找不到指定的路径），未更改保存位置"
        ));
        cx.notify();
    }

    fn setting_group(&self, title: &str, colors: VolantColors) -> Div {
        div()
            .pt(px(8.0))
            .pb(px(4.0))
            .text_size(px(theme::text::META))
            .text_color(colors.faint)
            .child(title.to_string())
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::current(cx);
        let settings = self.settings.clone();
        let limit_control = settings_select(
            &self.limit_select,
            "settings-limit-select",
            "同时下载",
            colors,
        );
        let connection_control = settings_select(
            &self.connections_select,
            "settings-connections-select",
            "每任务连接",
            colors,
        );
        let rate_control = settings_select(
            &self.rate_select,
            "settings-rate-select",
            "全局限速",
            colors,
        );
        let category_buttons = SettingsCategory::ALL
            .into_iter()
            .map(|category| {
                let selected = self.category == category;
                Button::new(format!("settings-category-{}", category.label()))
                    .ghost()
                    .selected(selected)
                    .accessibility_label(category.label())
                    .w(px(154.0))
                    .h(px(32.0))
                    .px(px(8.0))
                    .rounded(px(7.0))
                    .when(selected, |this| {
                        this.border_1().border_color(colors.selected_border)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.category = category;
                        if category == SettingsCategory::Integration {
                            this.host_status = volant_app_core::native::host_status();
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap(px(9.0))
                            .child(
                                Icon::new(category.icon())
                                    .with_size(Size::Size(px(13.0)))
                                    .text_color(if selected {
                                        colors.current
                                    } else {
                                        colors.muted
                                    }),
                            )
                            .child(div().text_size(px(13.0)).child(category.label())),
                    )
            })
            .collect::<Vec<_>>();
        let footer_text = self.save_error.as_deref().unwrap_or("所有更改自动保存");
        let footer_color = if self.save_error.is_some() {
            colors.danger
        } else {
            colors.success
        };
        let config_dir = self
            .parent
            .upgrade()
            .map(|parent| cx.read_entity(&parent, |parent, _| PathBuf::from(&parent.home)));
        let directory_control = div()
            .w(px(244.0))
            .h(px(32.0))
            .flex()
            .items_center()
            .gap(px(7.0))
            .child(
                div().flex_1().min_w(px(0.0)).child(Styled::h(
                    Input::new(&self.dir_input)
                        .aria_label("下载目录")
                        .focus_ring(false)
                        .w_full(),
                    px(32.0),
                )),
            )
            .child(
                Button::new("browse-dir")
                    .secondary()
                    .border_1()
                    .border_color(colors.border_strong)
                    .icon(IconName::FolderOpen)
                    .accessibility_label("选择下载目录")
                    .tooltip("选择下载目录")
                    .w(px(32.0))
                    .h(px(32.0))
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(path) = volant_app_core::native::pick_folder("选择下载目录")
                        {
                            let value = path.to_string_lossy().to_string();
                            this.dir_input.update(cx, |input, cx| {
                                input.set_value(value, window, cx);
                            });
                            this.apply_dir(window, cx);
                        }
                    })),
            )
            .into_any_element();
        let clipboard_control = Switch::new("settings-clipboard")
            .checked(settings.clipboard)
            .accessibility_label("监视剪贴板")
            .color(colors.primary)
            .on_change(cx.listener(|this, checked, _, cx| this.set_clipboard(*checked, cx)))
            .into_any_element();
        let skip_control = Switch::new("settings-skip-add")
            .checked(settings.skip_add_dialog)
            .accessibility_label("捕获后直接开始")
            .color(colors.primary)
            .on_change(cx.listener(|this, checked, _, cx| this.set_skip_add(*checked, cx)))
            .into_any_element();
        let confirm_delete_control = Switch::new("settings-confirm-delete")
            .checked(settings.confirm_delete)
            .accessibility_label("删除任务前确认")
            .color(colors.primary)
            .on_change(cx.listener(|this, checked, _, cx| this.set_confirm_delete(*checked, cx)))
            .into_any_element();
        let close_to_tray_control = Switch::new("settings-close-to-tray")
            .checked(settings.close_to_tray)
            .accessibility_label("关闭窗口后留在托盘")
            .color(colors.primary)
            .on_change(cx.listener(|this, checked, _, cx| this.set_close_to_tray(*checked, cx)))
            .into_any_element();
        let launch_at_startup_control = Switch::new("settings-launch-at-startup")
            .checked(settings.launch_at_startup)
            .accessibility_label("开机启动")
            .color(colors.primary)
            .on_change(cx.listener(|this, checked, _, cx| this.set_launch_at_startup(*checked, cx)))
            .into_any_element();
        let theme_control = div()
            .w(px(180.0))
            .h(px(32.0))
            .flex()
            .items_center()
            .gap(px(2.0))
            .p(px(2.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(colors.border)
            .child(theme_choice(
                "系统",
                AppThemeMode::System,
                settings.theme,
                colors,
                cx,
            ))
            .child(theme_choice(
                "浅色",
                AppThemeMode::Light,
                settings.theme,
                colors,
                cx,
            ))
            .child(theme_choice(
                "深色",
                AppThemeMode::Dark,
                settings.theme,
                colors,
                cx,
            ))
            .into_any_element();
        let proxy_mode_control = div()
            .w(px(240.0))
            .h(px(32.0))
            .flex()
            .items_center()
            .gap(px(2.0))
            .p(px(2.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(colors.border)
            .child(proxy_choice(
                "直连",
                ProxyMode::Direct,
                settings.proxy_mode,
                colors,
                cx,
            ))
            .child(proxy_choice(
                "系统代理",
                ProxyMode::System,
                settings.proxy_mode,
                colors,
                cx,
            ))
            .child(proxy_choice(
                "手动",
                ProxyMode::Manual,
                settings.proxy_mode,
                colors,
                cx,
            ))
            .into_any_element();
        let proxy_input_control = div()
            .w(px(300.0))
            .child(Styled::h(
                Input::new(&self.proxy_input)
                    .aria_label("手动代理地址")
                    .focus_ring(false)
                    .disabled(settings.proxy_mode != ProxyMode::Manual)
                    .w_full(),
                px(32.0),
            ))
            .into_any_element();
        let host_connected = self
            .parent
            .upgrade()
            .is_some_and(|parent| cx.read_entity(&parent, |parent, _| parent.host_connected));
        let host_state = if host_connected {
            "已连接"
        } else if self.host_status.installed() {
            "已安装，等待浏览器连接"
        } else {
            "未安装"
        };
        let host_color = if host_connected || self.host_status.installed() {
            colors.success
        } else {
            colors.danger
        };
        let host_control = div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(status_inline_text(host_state, host_color))
            .child(
                Button::new("install-host")
                    .secondary()
                    .border_1()
                    .border_color(colors.border_strong)
                    .label("安装宿主")
                    .on_click(cx.listener(|this, _, _, cx| this.install_host(cx))),
            )
            .into_any_element();
        let component_state = |ready: bool, label: &str| {
            let text = if ready {
                format!("{label} · 已就绪")
            } else {
                format!("{label} · 缺少")
            };
            status_inline_text(
                &text,
                if ready {
                    colors.success
                } else {
                    colors.warning
                },
            )
            .into_any_element()
        };
        let dependency_control = div()
            .w(px(270.0))
            .flex()
            .items_center()
            .justify_end()
            .gap(px(8.0))
            .child(
                Button::new("prepare-video-components")
                    .secondary()
                    .border_1()
                    .border_color(colors.border_strong)
                    .label(if self.installing_dependencies {
                        "正在下载…"
                    } else if self.dependencies.ready {
                        "重新检测"
                    } else {
                        "下载组件"
                    })
                    .disabled(self.installing_dependencies || !self.dependencies.can_install)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.install_video_dependencies(window, cx)
                    })),
            )
            .child(
                Button::new("open-video-components")
                    .ghost()
                    .icon(IconName::FolderOpen)
                    .accessibility_label("打开视频组件目录")
                    .tooltip("打开视频组件目录")
                    .on_click({
                        let dir = PathBuf::from(&self.dependencies.helper_dir);
                        move |_, _, _| open_directory(&dir)
                    }),
            )
            .into_any_element();
        let notice_for_page = self.notice.clone();
        let proxy_notice_for_page = self.proxy_notice.clone();
        let host_notice = self.host_notice.clone();
        let dependency_notice = self.dependency_notice.clone();
        let ytdlp_status = component_state(self.dependencies.ytdlp.ready, "yt-dlp");
        let ffmpeg_status = component_state(self.dependencies.ffmpeg.ready, "ffmpeg");
        let js_status = component_state(self.dependencies.js_runtime.ready, "JavaScript 运行时");
        let mut page_rows = Vec::new();
        match self.category {
            SettingsCategory::Download => {
                page_rows.push(self.setting_group("并发", colors).into_any_element());
                page_rows.push(
                    setting_row(
                        "同时下载",
                        "超过上限的任务会留在队列中。",
                        limit_control,
                        colors,
                    )
                    .into_any_element(),
                );
                page_rows.push(self.setting_group("保存", colors).into_any_element());
                page_rows.push(
                    setting_row(
                        "下载目录",
                        "空间不足或不可写时会提示，不静默换目录。",
                        directory_control,
                        colors,
                    )
                    .into_any_element(),
                );
                if let Some(notice) = notice_for_page {
                    page_rows.push(
                        div()
                            .pb(px(6.0))
                            .text_size(px(theme::text::META))
                            .text_color(colors.danger)
                            .child(notice)
                            .into_any_element(),
                    );
                }
            }
            SettingsCategory::Connection => {
                page_rows.push(self.setting_group("连接", colors).into_any_element());
                page_rows.push(
                    setting_row(
                        "每任务连接",
                        "按站点反馈自动压低，不需要手动试。",
                        connection_control,
                        colors,
                    )
                    .into_any_element(),
                );
                page_rows.push(
                    setting_row(
                        "全局限速",
                        "只限制 Volant，不影响其他应用。",
                        rate_control,
                        colors,
                    )
                    .into_any_element(),
                );
            }
            SettingsCategory::Proxy => {
                page_rows.push(self.setting_group("代理", colors).into_any_element());
                page_rows.push(
                    setting_row(
                        "代理模式",
                        "直连不使用代理；系统代理读取 Windows 设置。",
                        proxy_mode_control,
                        colors,
                    )
                    .into_any_element(),
                );
                page_rows.push(
                    setting_row(
                        "代理地址",
                        "手动模式支持 HTTP 和 HTTPS 代理。",
                        proxy_input_control,
                        colors,
                    )
                    .into_any_element(),
                );
                if let Some(notice) = proxy_notice_for_page {
                    page_rows.push(
                        div()
                            .pb(px(6.0))
                            .text_size(px(theme::text::META))
                            .text_color(colors.danger)
                            .child(notice)
                            .into_any_element(),
                    );
                }
            }
            SettingsCategory::Video => {
                page_rows.push(self.setting_group("视频组件", colors).into_any_element());
                page_rows.push(
                    setting_row(
                        "yt-dlp",
                        "负责解析视频页面、登录状态和可用画质。",
                        ytdlp_status,
                        colors,
                    )
                    .into_any_element(),
                );
                page_rows.push(
                    setting_row(
                        "ffmpeg",
                        "负责合并分离的音视频流，并最终输出 MP4。",
                        ffmpeg_status,
                        colors,
                    )
                    .into_any_element(),
                );
                page_rows.push(
                    setting_row(
                        "JavaScript 运行时",
                        "YouTube 等站点需要 Node、Deno、QuickJS 或 Bun。",
                        js_status,
                        colors,
                    )
                    .into_any_element(),
                );
                page_rows.push(
                    setting_row(
                        "安装与位置",
                        "首次下载约 105 MB；也可使用系统 PATH 中已有的版本。",
                        dependency_control,
                        colors,
                    )
                    .into_any_element(),
                );
                if let Some(notice) = dependency_notice {
                    page_rows.push(
                        div()
                            .py(px(7.0))
                            .text_size(px(theme::text::META))
                            .text_color(if self.dependencies.ready {
                                colors.success
                            } else {
                                colors.muted
                            })
                            .child(notice)
                            .into_any_element(),
                    );
                }
            }
            SettingsCategory::Integration => {
                page_rows.push(self.setting_group("浏览器捕获", colors).into_any_element());
                page_rows.push(
                    setting_row(
                        "监视剪贴板",
                        "识别到链接时打开新建下载窗口。",
                        clipboard_control,
                        colors,
                    )
                    .into_any_element(),
                );
                page_rows.push(
                    setting_row(
                        "捕获后直接开始",
                        "跳过确认；重名文件仍会询问。",
                        skip_control,
                        colors,
                    )
                    .into_any_element(),
                );
                page_rows.push(self.setting_group("任务管理", colors).into_any_element());
                page_rows.push(
                    setting_row(
                        "删除任务前确认",
                        "避免误删未完成的下载。",
                        confirm_delete_control,
                        colors,
                    )
                    .into_any_element(),
                );
                page_rows.push(self.setting_group("窗口与启动", colors).into_any_element());
                page_rows.push(
                    setting_row(
                        "关闭窗口后留在托盘",
                        "关闭整个程序请使用托盘菜单中的“退出 Volant”。",
                        close_to_tray_control,
                        colors,
                    )
                    .into_any_element(),
                );
                page_rows.push(
                    setting_row(
                        "开机启动",
                        "登录 Windows 后在托盘中静默启动，不主动弹窗。",
                        launch_at_startup_control,
                        colors,
                    )
                    .into_any_element(),
                );
                page_rows.push(self.setting_group("浏览器宿主", colors).into_any_element());
                page_rows.push(
                    setting_row(
                        "宿主连接",
                        "安装后重启浏览器，并加载随包提供的扩展。",
                        host_control,
                        colors,
                    )
                    .into_any_element(),
                );
                if let Some(notice) = host_notice {
                    page_rows.push(
                        div()
                            .py(px(7.0))
                            .text_size(px(theme::text::META))
                            .text_color(colors.muted)
                            .child(notice)
                            .into_any_element(),
                    );
                }
            }
            SettingsCategory::Appearance => {
                page_rows.push(self.setting_group("外观", colors).into_any_element());
                page_rows.push(
                    setting_row(
                        "主题",
                        "跟随系统或固定为浅色 / 深色。",
                        theme_control,
                        colors,
                    )
                    .into_any_element(),
                );
            }
        }

        div()
            .v_flex()
            .w_full()
            .h(px(theme::size::SETTINGS_H))
            .overflow_hidden()
            .rounded(px(10.0))
            .border_1()
            .border_color(colors.border)
            .bg(colors.surface)
            .text_color(colors.text)
            .capture_key_down(cx.listener(|_, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key.as_str() == "escape" && !event.keystroke.modifiers.modified()
                {
                    window.prevent_default();
                    cx.stop_propagation();
                    window.close_dialog(cx);
                }
            }))
            .child(dialog_title_bar("Volant · 设置", colors))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(
                        div()
                            .w(px(theme::size::SETTINGS_RAIL))
                            .h_full()
                            .flex_shrink_0()
                            .v_flex()
                            .px(px(11.0))
                            .pt(px(14.0))
                            .gap(px(5.0))
                            .border_r_1()
                            .border_color(colors.border)
                            .child(
                                div()
                                    .px(px(8.0))
                                    .pb(px(6.0))
                                    .text_size(px(theme::text::BODY))
                                    .text_color(colors.faint)
                                    .child("设置"),
                            )
                            .children(category_buttons),
                    )
                    .child(
                        div()
                            .id("settings-form")
                            .flex_1()
                            .min_w(px(0.0))
                            .h_full()
                            .overflow_y_scrollbar()
                            .px(px(25.0))
                            .pt(px(14.0))
                            .child(
                                div()
                                    .text_size(px(21.0))
                                    .font_weight(FontWeight(620.0))
                                    .line_height(px(25.0))
                                    .child(self.category.label()),
                            )
                            .child(
                                div()
                                    .pt(px(5.0))
                                    .pb(px(8.0))
                                    .text_size(px(theme::text::BODY))
                                    .text_color(colors.muted)
                                    .child(self.category.description()),
                            )
                            .child(div().pt(px(4.0)).border_t_1().border_color(colors.border))
                            .children(page_rows),
                    ),
            )
            .child(
                div()
                    .h(px(theme::size::SETTINGS_FOOTER))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(colors.border)
                    .px(px(24.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .text_size(px(theme::text::META))
                            .text_color(colors.muted)
                            .child(status_dot(footer_color, 4.0))
                            .child(footer_text.to_string()),
                    )
                    .child(
                        Button::new("open-config-dir")
                            .ghost()
                            .compact()
                            .label("打开配置目录")
                            .on_click(cx.listener(move |_, _, _, _| {
                                if let Some(dir) = config_dir.clone() {
                                    open_directory(&dir);
                                }
                            })),
                    ),
            )
    }
}

fn theme_choice(
    label: &str,
    mode: AppThemeMode,
    current: AppThemeMode,
    colors: VolantColors,
    cx: &mut Context<SettingsView>,
) -> Button {
    let selected = mode == current;
    Button::new(format!("theme-{}", label))
        .ghost()
        .selected(selected)
        .label(label)
        .h(px(26.0))
        .flex_1()
        .rounded(px(5.0))
        .when(selected, |this| {
            this.bg(colors.sunken).text_color(colors.text)
        })
        .on_click(cx.listener(move |this, _, window, cx| this.set_theme(mode, window, cx)))
}

fn proxy_choice(
    label: &str,
    mode: ProxyMode,
    current: ProxyMode,
    colors: VolantColors,
    cx: &mut Context<SettingsView>,
) -> Button {
    let selected = mode == current;
    Button::new(format!("proxy-{label}"))
        .ghost()
        .selected(selected)
        .label(label)
        .h(px(26.0))
        .flex_1()
        .rounded(px(5.0))
        .when(selected, |this| {
            this.bg(colors.sunken).text_color(colors.text)
        })
        .on_click(cx.listener(move |this, _, _, cx| this.set_proxy_mode(mode, cx)))
}

fn setting_label(label: &str, description: &str, colors: VolantColors) -> Div {
    div()
        .v_flex()
        .gap(px(2.0))
        .child(
            div()
                .text_size(px(13.0))
                .font_weight(FontWeight(600.0))
                .child(label.to_string()),
        )
        .child(
            div()
                .text_size(px(theme::text::META))
                .text_color(colors.muted)
                .child(description.to_string()),
        )
}

fn setting_row(label: &str, description: &str, control: AnyElement, colors: VolantColors) -> Div {
    div()
        .min_h(px(50.0))
        .max_h(px(58.0))
        .py(px(6.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(16.0))
        .border_b_1()
        .border_color(colors.border)
        .child(setting_label(label, description, colors))
        .child(control)
}

fn settings_select(
    state: &Entity<SelectState<SearchableVec<String>>>,
    id: &'static str,
    label: &'static str,
    colors: VolantColors,
) -> AnyElement {
    div()
        .ml_auto()
        .w(px(theme::size::SELECT_W))
        .h(px(32.0))
        .flex_shrink_0()
        .child(
            Select::new(state)
                .id(id)
                .accessibility_label(label)
                .menu_width(px(theme::size::SELECT_W))
                .menu_max_h(px((theme::size::SELECT_VISIBLE_ITEMS as f32 * 28.0) + 16.0))
                .focus_ring(false)
                .w_full()
                .h_full()
                .border_1()
                .border_color(colors.border),
        )
        .into_any_element()
}

struct TorrentPickView {
    parent: WeakEntity<VolantShell>,
    id: u64,
    files: Vec<TorrentFile>,
    checked: Vec<bool>,
}

impl TorrentPickView {
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected: Vec<usize> = self
            .checked
            .iter()
            .enumerate()
            .filter_map(|(index, checked)| checked.then_some(index))
            .collect();
        if selected.is_empty() {
            return;
        }
        let only = (selected.len() != self.files.len()).then_some(selected);
        let _ = self.parent.update(cx, |parent, cx| {
            parent.start_picked_torrent(self.id, only);
            cx.notify();
        });
        window.close_dialog(cx);
    }
}

impl Render for TorrentPickView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::current(cx);
        let all_checked = self.checked.iter().all(|checked| *checked);
        let selected_count = self.checked.iter().filter(|checked| **checked).count();
        let selected_size: u64 = self
            .files
            .iter()
            .zip(&self.checked)
            .filter(|(_, checked)| **checked)
            .map(|(file, _)| file.len)
            .sum();
        let rows = self.files.iter().enumerate().map(|(index, file)| {
            div()
                .h(px(37.0))
                .flex()
                .items_center()
                .gap(px(12.0))
                .px(px(8.0))
                .border_b_1()
                .border_color(colors.border)
                .child(
                    div().w(px(24.0)).flex_shrink_0().child(
                        Checkbox::new(("torrent-file", index))
                            .accessibility_label(file.name.clone())
                            .checked(self.checked[index])
                            .on_change(cx.listener(move |this, checked, _, cx| {
                                this.checked[index] = *checked;
                                cx.notify();
                            })),
                    ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(theme::text::BODY))
                        .text_color(colors.text)
                        .child(file.name.clone()),
                )
                .child(
                    div()
                        .w(px(86.0))
                        .flex_shrink_0()
                        .text_size(px(theme::text::BODY))
                        .text_color(colors.muted)
                        .child(file_kind(&file.name)),
                )
                .child(
                    div()
                        .w(px(92.0))
                        .flex_shrink_0()
                        .text_right()
                        .text_size(px(theme::text::BODY))
                        .text_color(colors.muted)
                        .child(fmt::bytes(file.len)),
                )
        });

        div()
            .v_flex()
            .w(px(520.0))
            .gap(px(12.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(theme::text::BODY))
                            .text_color(colors.muted)
                            .child(format!(
                                "已选 {selected_count}/{} 个文件 · {}",
                                self.files.len(),
                                fmt::bytes(selected_size)
                            )),
                    )
                    .child(
                        Button::new("toggle-all-files")
                            .ghost()
                            .compact()
                            .label(if all_checked {
                                "全部取消"
                            } else {
                                "全选"
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let checked = this.checked.iter().all(|value| *value);
                                this.checked.fill(!checked);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .h(px(31.0))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .px(px(8.0))
                    .border_b_1()
                    .border_color(colors.border)
                    .text_size(px(theme::text::META))
                    .text_color(colors.faint)
                    .child(
                        div().w(px(24.0)).flex_shrink_0().child(
                            Checkbox::new("torrent-select-all")
                                .accessibility_label("全选所有文件")
                                .checked(all_checked)
                                .on_change(cx.listener(|this, checked, _, cx| {
                                    this.checked.fill(*checked);
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(div().flex_1().min_w(px(0.0)).child("文件"))
                    .child(div().w(px(86.0)).flex_shrink_0().child("类型"))
                    .child(div().w(px(92.0)).flex_shrink_0().text_right().child("大小")),
            )
            .child(
                div()
                    .id("torrent-file-list")
                    .max_h(px(340.0))
                    .overflow_y_scrollbar()
                    .v_flex()
                    .children(rows),
            )
            .child(
                div().flex().justify_end().child(
                    Button::new("start-torrent-download")
                        .primary()
                        .label("开始下载")
                        .disabled(selected_count == 0)
                        .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
                ),
            )
    }
}

struct DeleteTaskView {
    parent: WeakEntity<VolantShell>,
    id: u64,
    name: String,
    preferred_delete_files: bool,
    dont_ask: bool,
}

impl DeleteTaskView {
    fn delete(&mut self, delete_files: bool, window: &mut Window, cx: &mut Context<Self>) {
        let dont_ask = self.dont_ask;
        let _ = self.parent.update(cx, |parent, cx| {
            parent.delete_row_with_files(self.id, delete_files, dont_ask);
            cx.notify();
        });
        window.close_dialog(cx);
    }
}

impl Render for DeleteTaskView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::current(cx);
        div()
            .w(px(360.0))
            .v_flex()
            .p(px(20.0))
            .gap(px(13.0))
            .rounded(px(10.0))
            .border_1()
            .border_color(colors.border)
            .bg(colors.surface)
            .text_color(colors.text)
            .capture_key_down(cx.listener(|_, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key.as_str() == "escape" {
                    window.close_dialog(cx);
                }
            }))
            .child(
                div()
                    .text_size(px(18.0))
                    .font_weight(FontWeight(620.0))
                    .child("删除任务？"),
            )
            .child(
                div()
                    .text_size(px(13.0))
                    .line_height(px(18.0))
                    .text_color(colors.muted)
                    .child(format!(
                        "“{}”可以只从列表移除，也可以同时删除已下载的文件。",
                        self.name
                    )),
            )
            .child(
                div()
                    .pt(px(2.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .child(
                        Checkbox::new("delete-do-not-ask")
                            .label("不再显示")
                            .checked(self.dont_ask)
                            .on_change(cx.listener(|this, checked, _, cx| {
                                this.dont_ask = *checked;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                Button::new("cancel-delete")
                                    .ghost()
                                    .label("取消")
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(
                                Button::new("delete-task-only")
                                    .secondary()
                                    .border_1()
                                    .border_color(colors.border_strong)
                                    .label("删除")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.delete(false, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("delete-task-files")
                                    .ghost()
                                    .border_1()
                                    .border_color(colors.danger)
                                    .text_color(colors.danger)
                                    .label("彻底删除")
                                    .when(self.preferred_delete_files, |this| {
                                        this.font_weight(FontWeight(650.0))
                                    })
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.delete(true, window, cx)
                                    })),
                            ),
                    ),
            )
    }
}

struct VolantShell {
    home: String,
    torrents_dir: PathBuf,
    rows: Vec<Row>,
    filter: Filter,
    selected: Option<u64>,
    detail_open: bool,
    settings: Settings,
    video_dependencies: video::VideoDependencyStatus,
    store: Option<Store>,
    control: Option<Control>,
    _clip: Option<Watch>,
    _capture: Option<Listener>,
    add_dialog: Option<Entity<AddDownloadView>>,
    settings_dialog: Option<Entity<SettingsView>>,
    pick_dialog: Option<Entity<TorrentPickView>>,
    next_id: u64,
    dirty: bool,
    last_save: Instant,
    notice: Option<String>,
    host_connected: bool,
    pending_file_deletes: HashMap<u64, PathBuf>,
    capture_replies: HashMap<u64, CaptureReply>,
    video_lookups: HashMap<String, VideoLookupSlot>,
    video_tokens: HashMap<u64, String>,
    detail_restore_width: Option<f32>,
    last_task_click: Option<(u64, Instant)>,
    quitting: bool,
    focus_handle: FocusHandle,
    _events_task: Option<Task<()>>,
    _tray: Option<TrayIcon>,
    _tray_task: Option<Task<()>>,
    _appearance_subscription: Subscription,
}

impl VolantShell {
    fn new(
        demo: bool,
        background: bool,
        tray: Option<TrayIcon>,
        tray_events: Option<tokio::sync::mpsc::UnboundedReceiver<TrayEvent>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let paths = Paths::resolve();
        volant_app_core::log::init(&paths.home);
        let store = Store::new(paths.state.clone());
        let mut saved = store.load();
        let fallback = paths.home.join("downloads");
        let (download_dir, dir_note) = writable_download_dir(&saved.settings.dir, &fallback);
        saved.settings.dir = download_dir;
        saved.settings.normalize();
        theme::apply(cx, theme::resolve_mode(saved.settings.theme, window));

        let (rows, control, events_task, clip, capture) = if demo {
            (sample_rows(), None, None, None, None)
        } else {
            let (wake, mut wake_rx) = tokio::sync::mpsc::unbounded_channel();
            let notifier: Notifier = Arc::new(GpuiNotifier { wake });
            let (clip, clip_urls) = Watch::start(notifier.clone(), saved.settings.clipboard);
            let (capture, captured) = Listener::start(notifier.clone());
            let (control, events) = Control::start(
                notifier,
                saved.settings.clone(),
                paths.hosts.clone(),
                fallback,
            );
            let events_task = cx.spawn_in(window, async move |this, cx| {
                while wake_rx.recv().await.is_some() {
                    volant_app_core::log::line("ui wake");
                    let mut batch = Vec::new();
                    while let Ok(event) = events.try_recv() {
                        batch.push(event);
                    }
                    let mut clipboard_urls = Vec::new();
                    while let Ok(url) = clip_urls.try_recv() {
                        clipboard_urls.push(url);
                    }
                    let mut captures = Vec::new();
                    while let Ok(capture) = captured.try_recv() {
                        captures.push(capture);
                    }
                    if batch.is_empty() && clipboard_urls.is_empty() && captures.is_empty() {
                        continue;
                    }
                    let updated = this.update_in(cx, |this, window, cx| {
                        for event in batch {
                            this.handle_event(event, window, cx);
                        }
                        for url in clipboard_urls {
                            this.handle_clipboard_url(url, window, cx);
                        }
                        for capture in captures {
                            this.handle_capture(capture, window, cx);
                        }
                        let has_active = this.rows.iter().any(|row| {
                            matches!(
                                row.status,
                                Status::Active | Status::Seeding | Status::Probing
                            )
                        });
                        if has_active {
                            this.save_if_due();
                        } else {
                            this.save_now();
                        }
                        window.refresh();
                        cx.notify();
                    });
                    if updated.is_err() {
                        break;
                    }
                }
            });
            (
                saved.rows,
                Some(control),
                Some(events_task),
                Some(clip),
                Some(capture),
            )
        };

        let selected = None;
        let focus_handle = cx.focus_handle().tab_stop(true);
        focus_handle.focus(window, cx);
        let mut video_dependencies = video::dependency_status();
        if demo {
            video_dependencies.ready = true;
            video_dependencies.ytdlp.ready = true;
            video_dependencies.ffmpeg.ready = true;
            video_dependencies.js_runtime.ready = true;
        }
        let _appearance_subscription = cx.observe_window_appearance(window, |this, window, cx| {
            if this.settings.theme == AppThemeMode::System {
                theme::apply(cx, theme::resolve_mode(AppThemeMode::System, window));
                cx.notify();
            }
        });
        if !demo {
            let _ = crate::platform::set_autostart(saved.settings.launch_at_startup);
        }
        let tray_task = tray_events.map(|mut events| {
            cx.spawn_in(window, async move |this, cx| {
                while let Some(event) = events.recv().await {
                    let updated = this.update_in(cx, |this, window, cx| match event {
                        TrayEvent::Open => {
                            crate::platform::show_window(window);
                            cx.notify();
                        }
                        TrayEvent::Quit => {
                            this.save_now();
                            this.quitting = true;
                            cx.quit();
                        }
                    });
                    if updated.is_err() {
                        break;
                    }
                }
            })
        });
        Self {
            home: paths.home.display().to_string(),
            torrents_dir: paths.torrents,
            rows,
            filter: Filter::All,
            selected,
            detail_open: false,
            settings: saved.settings,
            video_dependencies,
            store: (!demo).then_some(store),
            control,
            _clip: clip,
            _capture: capture,
            add_dialog: None,
            settings_dialog: None,
            pick_dialog: None,
            next_id: saved.next_id,
            dirty: false,
            last_save: Instant::now(),
            notice: if background {
                Some("Volant 正在系统托盘中运行".into())
            } else {
                dir_note
            },
            host_connected: false,
            pending_file_deletes: HashMap::new(),
            capture_replies: HashMap::new(),
            video_lookups: HashMap::new(),
            video_tokens: HashMap::new(),
            detail_restore_width: None,
            last_task_click: None,
            quitting: false,
            focus_handle,
            _events_task: events_task,
            _tray: tray,
            _tray_task: tray_task,
            _appearance_subscription,
        }
    }

    fn queue_url(&mut self, url: String) -> bool {
        self.queue_url_with_headers(url, Vec::new())
    }

    fn queue_url_with_headers(&mut self, url: String, headers: Vec<(String, String)>) -> bool {
        let url = url.trim().to_string();
        if url.is_empty() {
            self.notice = Some("先粘贴一个下载链接".into());
            return false;
        }
        if !(url.starts_with("http://") || url.starts_with("https://") || is_magnet(&url)) {
            self.notice = Some("目前只支持 HTTP、HTTPS 和 magnet 链接".into());
            return false;
        }

        let id = self.next_id;
        self.next_id += 1;
        let mut row = if is_magnet(&url) {
            Row::torrent(id, url.clone())
        } else {
            Row::new(id, url.clone())
        };
        row.headers = headers.clone();
        row.status = Status::Probing;
        self.rows.insert(0, row);
        self.selected = Some(id);
        self.filter = Filter::All;

        if let Some(control) = &self.control {
            if is_magnet(&url) {
                control.send(Cmd::InspectTorrent {
                    id,
                    source: volant_app_core::control::TorSource::Magnet(url),
                });
            } else {
                control.send(Cmd::Add { id, url, headers });
            }
        }
        self.notice = Some("任务已加入队列".into());
        self.mark_dirty();
        self.save_now();
        true
    }

    fn queue_video(&mut self, task: VideoTask, destination: PathBuf) -> bool {
        let id = self.next_id;
        self.next_id += 1;
        let mut row = Row::video(id, task.clone());
        row.status = Status::Probing;
        row.dest = Some(video::expected_path(&task, &destination));
        self.rows.insert(0, row);
        self.selected = Some(id);
        self.filter = Filter::All;
        if let Some(control) = &self.control {
            control.send(Cmd::AddVideo {
                id,
                task: Box::new(task),
                destination,
            });
            self.notice = Some("视频已加入队列".into());
            self.mark_dirty();
            self.save_now();
            true
        } else {
            self.notice = Some("视频下载组件尚未启动".into());
            self.rows.retain(|row| row.id != id);
            false
        }
    }

    fn reply_capture(&mut self, request_id: u64, reply: serde_json::Value) {
        if let Some(sender) = self.capture_replies.remove(&request_id) {
            let _ = sender.send(&reply);
        }
    }

    fn request_preview(&mut self, id: u64, url: String, headers: Vec<(String, String)>) {
        if let Some(control) = &self.control {
            control.send(Cmd::PreviewHttp { id, url, headers });
        }
    }

    fn request_torrent_preview(&mut self, id: u64, source: volant_app_core::control::TorSource) {
        if let Some(control) = &self.control {
            control.send(Cmd::InspectTorrent { id, source });
        }
    }

    fn open_add_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let parent = cx.weak_entity();
        let destination = self.settings.dir.clone();
        let view = cx.new(|cx| AddDownloadView::new(parent, destination, window, cx));
        self.add_dialog = Some(view.clone());
        let colors = theme::current(cx);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .p_0()
                .gap_0()
                .w(px(theme::size::ADD_W))
                .rounded(px(10.0))
                .border_color(colors.border)
                .centered()
                .close_button(false)
                .on_ok(|_, _, _| false)
                .child(view.clone())
        });
    }

    fn open_add_dialog_with_url(
        &mut self,
        url: String,
        headers: Vec<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.add_dialog.is_none() {
            self.open_add_dialog(window, cx);
        }
        if let Some(view) = &self.add_dialog {
            view.update(cx, |view, cx| view.set_url(url, headers, window, cx));
        }
    }

    fn handle_clipboard_url(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.clipboard && !window.has_active_dialog(cx) {
            self.open_add_dialog_with_url(url, Vec::new(), window, cx);
            self.notice = Some("已从剪贴板识别到链接".into());
        }
    }

    fn handle_capture(&mut self, capture: Capture, window: &mut Window, cx: &mut Context<Self>) {
        self.host_connected = true;
        let action = if capture.action.is_empty() {
            "download"
        } else {
            capture.action.as_str()
        };
        let request_id = capture.request_id;
        let reply = capture.reply.clone();
        if !matches!(action, "ping" | "rules") {
            crate::platform::show_window(window);
        }
        match action {
            "ping" | "rules" => {
                if let Some(reply) = reply {
                    let _ = reply.send(&serde_json::json!({
                        "ok": true,
                        "version": env!("CARGO_PKG_VERSION"),
                        "capture": true,
                        "videoLookup": true,
                        "videoDependencies": video::dependency_status(),
                        "opened": true,
                    }));
                }
                return;
            }
            "videoLookup" => {
                let Some(reply) = reply else {
                    return;
                };
                let token = video::lookup_token(request_id);
                let context = VideoContext {
                    page: if capture.page.is_empty() {
                        capture.url.clone()
                    } else {
                        capture.page.clone()
                    },
                    src: capture.src.clone(),
                    title: capture.title.clone(),
                    referrer: capture.referer.clone(),
                    origin: capture.origin.clone(),
                    user_agent: capture.user_agent.clone(),
                    cookies: capture.cookies.clone(),
                };
                self.video_lookups.retain(|_, slot| match slot {
                    VideoLookupSlot::Pending => true,
                    VideoLookupSlot::Ready(_, at) | VideoLookupSlot::Failed(_, at) => {
                        at.elapsed() < VIDEO_LOOKUP_TTL
                    }
                });
                if self.video_lookups.len() >= 24
                    && let Some(oldest) = self
                        .video_lookups
                        .iter()
                        .min_by_key(|(_, slot)| match slot {
                            VideoLookupSlot::Pending => Instant::now(),
                            VideoLookupSlot::Ready(_, at) | VideoLookupSlot::Failed(_, at) => *at,
                        })
                        .map(|(token, _)| token.clone())
                {
                    self.video_lookups.remove(&oldest);
                }
                self.video_lookups
                    .insert(token.clone(), VideoLookupSlot::Pending);
                self.video_tokens.insert(request_id, token.clone());
                if let Some(control) = &self.control {
                    let _ = reply.send(&serde_json::json!({
                        "ok": true,
                        "lookup": {"state": "pending", "token": token}
                    }));
                    control.send(Cmd::VideoLookup {
                        id: request_id,
                        token,
                        context,
                    });
                } else {
                    self.video_lookups.remove(&token);
                    self.video_tokens.remove(&request_id);
                    let _ = reply.send(&serde_json::json!({"ok": false, "error": "桌面端未启动"}));
                }
                return;
            }
            "videoPoll" => {
                let lookup = match self.video_lookups.get(&capture.token) {
                    Some(VideoLookupSlot::Pending) => {
                        serde_json::json!({"state": "pending", "token": capture.token})
                    }
                    Some(VideoLookupSlot::Ready(lookup, at)) if at.elapsed() < VIDEO_LOOKUP_TTL => {
                        serde_json::to_value(lookup.as_ref()).unwrap_or_default()
                    }
                    Some(VideoLookupSlot::Failed(error, at)) if at.elapsed() < VIDEO_LOOKUP_TTL => {
                        serde_json::json!({"state": "failed", "error": error})
                    }
                    _ => serde_json::json!({"state": "gone"}),
                };
                self.reply_capture(
                    request_id,
                    serde_json::json!({"ok": true, "lookup": lookup}),
                );
                return;
            }
            "videoPick" => {
                let lookup = match self.video_lookups.remove(&capture.token) {
                    Some(VideoLookupSlot::Ready(lookup, _)) => lookup,
                    Some(VideoLookupSlot::Pending) => {
                        self.video_lookups
                            .insert(capture.token.clone(), VideoLookupSlot::Pending);
                        self.reply_capture(
                            request_id,
                            serde_json::json!({
                                "ok": false,
                                "lookup": {"state": "pending", "token": capture.token}
                            }),
                        );
                        return;
                    }
                    other => {
                        if let Some(other) = other {
                            self.video_lookups.insert(capture.token.clone(), other);
                        }
                        self.reply_capture(
                            request_id,
                            serde_json::json!({
                                "ok": false,
                                "lookup": {"state": "gone"},
                                "error": "画质信息已过期，请重新解析"
                            }),
                        );
                        return;
                    }
                };
                let Some(choice) = lookup.choices.get(capture.choice).cloned() else {
                    self.video_lookups.insert(
                        capture.token.clone(),
                        VideoLookupSlot::Ready(lookup, Instant::now()),
                    );
                    self.reply_capture(
                        request_id,
                        serde_json::json!({
                            "ok": false,
                            "lookup": {"state": "use-window"},
                            "error": "没有这个画质"
                        }),
                    );
                    return;
                };
                let task = VideoTask {
                    context: lookup.context.clone(),
                    choice,
                    title: lookup.title.clone(),
                    subtitles: capture.subtitles,
                };
                let destination = self.settings.dir.clone();
                let ok = self.queue_video(task, destination);
                self.reply_capture(
                    request_id,
                    serde_json::json!({
                        "ok": ok,
                        "lookup": {"state": if ok { "sent" } else { "failed" }}
                    }),
                );
                cx.notify();
                return;
            }
            "video" | "mediaDownload" => {
                let target = if capture.src.starts_with("http") {
                    capture.src.clone()
                } else {
                    capture.url.clone()
                };
                let ok =
                    !target.is_empty() && self.queue_url_with_headers(target, capture.headers());
                self.reply_capture(request_id, serde_json::json!({"ok": ok}));
                cx.notify();
                return;
            }
            "open" => {
                crate::platform::show_window(window);
                self.reply_capture(request_id, serde_json::json!({"ok": true}));
                return;
            }
            "openVideoSettings" => {
                crate::platform::show_window(window);
                self.open_settings_category(SettingsCategory::Video, window, cx);
                self.reply_capture(request_id, serde_json::json!({"ok": true}));
                return;
            }
            _ => {}
        }
        if capture.url.is_empty() {
            self.reply_capture(
                request_id,
                serde_json::json!({"ok": false, "error": "下载地址为空"}),
            );
            return;
        }
        window.activate_window();
        let lag = now_ms().saturating_sub(capture.t);
        if let Ok(path) = std::env::var("VOLANT_CAPTURE_LOG") {
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(file, "{lag}\t{}", capture.url);
            }
        }
        let headers = capture.headers();
        let accepted = if self.settings.skip_add_dialog {
            self.queue_url_with_headers(capture.url.clone(), headers.clone())
        } else {
            if window.has_active_dialog(cx) && self.add_dialog.is_none() {
                window.close_dialog(cx);
                self.settings_dialog = None;
                self.pick_dialog = None;
            }
            self.open_add_dialog_with_url(capture.url.clone(), headers.clone(), window, cx);
            self.notice = Some("已接收浏览器下载请求".into());
            true
        };
        self.reply_capture(
            request_id,
            serde_json::json!({"ok": accepted, "why": if accepted { "" } else { "refused" }}),
        );
    }

    fn handle_external_paths(
        &mut self,
        paths: &ExternalPaths,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut accepted = 0;
        for path in paths.paths() {
            accepted += usize::from(self.queue_torrent_file(path));
        }
        if accepted > 0 {
            self.notice = Some(format!("已读取 {accepted} 个种子文件"));
            self.mark_dirty();
            self.save_now();
        } else {
            self.notice = Some("拖入的文件里没有 .torrent".into());
        }
        cx.notify();
    }

    fn queue_torrent_file(&mut self, path: &Path) -> bool {
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("torrent"))
        {
            return false;
        }
        let id = self.next_id;
        self.next_id += 1;
        let target = self.torrents_dir.join(format!("{id}.torrent"));
        if let Some(parent) = target.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            self.notice = Some(format!("无法保存种子副本：{error}"));
            return false;
        }
        if let Err(error) = std::fs::copy(path, &target) {
            self.notice = Some(format!("无法读取种子文件：{error}"));
            return false;
        }
        let mut row = Row::torrent(id, path.display().to_string());
        row.status = Status::Probing;
        row.torrent = Some(target.clone());
        self.rows.insert(0, row);
        self.selected = Some(id);
        self.filter = Filter::All;
        if let Some(control) = &self.control {
            control.send(Cmd::InspectTorrent {
                id,
                source: volant_app_core::control::TorSource::File(target),
            });
        }
        true
    }

    fn open_settings_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings_category(SettingsCategory::Download, window, cx);
    }

    fn open_settings_category(
        &mut self,
        category: SettingsCategory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let parent = cx.weak_entity();
        let view =
            cx.new(|cx| SettingsView::new(parent, self.settings.clone(), category, window, cx));
        self.settings_dialog = Some(view.clone());
        let colors = theme::current(cx);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .p_0()
                .gap_0()
                .w(px(theme::size::SETTINGS_W))
                .rounded(px(10.0))
                .border_color(colors.border)
                .centered()
                .close_button(false)
                .on_ok(|_, _, _| false)
                .child(view.clone())
        });
    }

    fn open_torrent_picker(
        &mut self,
        id: u64,
        name: String,
        files: Vec<TorrentFile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let parent = cx.weak_entity();
        let checked = vec![true; files.len()];
        let view = cx.new(|_| TorrentPickView {
            parent,
            id,
            files,
            checked,
        });
        self.pick_dialog = Some(view.clone());
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(format!("选择下载内容 · {name}"))
                .w(px(580.0))
                .centered()
                .child(view.clone())
        });
    }

    fn start_picked_torrent(&mut self, id: u64, only: Option<Vec<usize>>) {
        if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
            row.only = only.clone();
            row.status = Status::Queued;
        }
        if let Some(control) = &self.control {
            control.send(Cmd::StartTorrent { id, only });
        }
        self.mark_dirty();
        self.save_now();
    }

    fn apply_settings(&mut self, settings: Settings, command: Option<Cmd>) -> bool {
        self.settings = settings;
        if let (Some(control), Some(command)) = (&self.control, command) {
            control.send(command);
        }
        self.mark_dirty();
        self.save_now()
    }

    fn set_download_dir(&mut self, requested: PathBuf) -> bool {
        let (checked, note) = writable_download_dir(&requested, &requested);
        if note.is_some() {
            self.notice = note;
            return false;
        }
        let mut settings = self.settings.clone();
        settings.dir = checked.clone();
        self.apply_settings(settings, Some(Cmd::SetDir(checked)))
    }

    fn set_clipboard_enabled(&mut self, enabled: bool) {
        if let Some(watch) = &self._clip {
            watch.set_enabled(enabled);
        }
    }

    fn confirm_delete(
        &mut self,
        id: u64,
        delete_files: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.settings.confirm_delete {
            self.delete_row_with_files(id, delete_files, false);
            cx.notify();
            return;
        }
        let Some(row) = self.rows.iter().find(|row| row.id == id).cloned() else {
            return;
        };
        let parent = cx.weak_entity();
        let name = row.display_name().to_string();
        let view = cx.new(|_| DeleteTaskView {
            parent,
            id,
            name,
            preferred_delete_files: delete_files,
            dont_ask: false,
        });
        let colors = theme::current(cx);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .w(px(360.0))
                .centered()
                .p_0()
                .gap_0()
                .border_color(colors.border)
                .close_button(false)
                .on_ok(|_, _, _| false)
                .child(view.clone())
        });
    }

    fn delete_row_with_files(&mut self, id: u64, delete_files: bool, dont_ask: bool) {
        let Some(row) = self.rows.iter().find(|row| row.id == id).cloned() else {
            return;
        };
        if dont_ask {
            self.settings.confirm_delete = false;
        }
        if row.kind == Kind::Torrent {
            if let Some(control) = &self.control {
                control.send(Cmd::RemoveTorrent { id, delete_files });
            }
        } else {
            if delete_files && let Some(path) = &row.dest {
                if self.control.is_some() {
                    self.pending_file_deletes.insert(id, path.clone());
                } else {
                    let _ = std::fs::remove_file(path);
                    volant_app_core::model::remove_leftovers(path);
                }
            }
            if let Some(control) = &self.control {
                control.send(Cmd::Remove(id));
            }
        }
        self.rows.retain(|row| row.id != id);
        self.select_first_visible();
        self.notice = Some("任务已从列表移除".into());
        self.mark_dirty();
        self.save_now();
    }

    fn retry(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(row) = self.rows.iter().find(|row| row.id == id).cloned() else {
            return;
        };
        let Some(control) = &self.control else {
            return;
        };
        if row.kind == Kind::Torrent {
            if let Some(path) = row.torrent.clone() {
                control.send(Cmd::InspectTorrent {
                    id,
                    source: volant_app_core::control::TorSource::File(path),
                });
            } else {
                control.send(Cmd::InspectTorrent {
                    id,
                    source: volant_app_core::control::TorSource::Magnet(row.url.clone()),
                });
            }
        } else if row.kind == Kind::Video {
            if let Some(task) = row.video.clone() {
                control.send(Cmd::AddVideo {
                    id,
                    task: Box::new(task),
                    destination: row
                        .dest
                        .as_deref()
                        .and_then(Path::parent)
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| self.settings.dir.clone()),
                });
            }
        } else if let Some(dest) = row.dest.clone() {
            control.send(Cmd::Resume {
                id,
                url: row.url.clone(),
                dest,
                headers: row.headers.clone(),
            });
        } else {
            control.send(Cmd::Add {
                id,
                url: row.url.clone(),
                headers: row.headers.clone(),
            });
        }
        if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
            row.status = Status::Probing;
            row.error = None;
            row.speed = 0.0;
            row.connections = 0;
        }
        self.notice = Some("正在重试下载".into());
        self.mark_dirty();
        self.save_now();
        cx.notify();
    }

    fn start_task(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(status) = self
            .rows
            .iter()
            .find(|row| row.id == id)
            .map(|row| row.status)
        else {
            return;
        };
        match status {
            Status::Paused => self.toggle(id, cx),
            Status::Failed => self.retry(id, cx),
            _ => {}
        }
    }

    fn pause_task(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.rows.iter().any(|row| {
            row.id == id
                && matches!(
                    row.status,
                    Status::Active | Status::Seeding | Status::Queued | Status::Probing
                )
        }) {
            self.toggle(id, cx);
        }
    }

    fn activate_task(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(status) = self
            .rows
            .iter()
            .find(|row| row.id == id)
            .map(|row| row.status)
        else {
            return;
        };
        match status {
            Status::Active
            | Status::Seeding
            | Status::Queued
            | Status::Probing
            | Status::Paused => self.toggle(id, cx),
            Status::Done => self.open_task_output(id, cx),
            Status::Failed => self.retry(id, cx),
        }
    }

    fn open_task_output(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(path) = self
            .rows
            .iter()
            .find(|row| row.id == id)
            .and_then(|row| row.dest.clone())
        else {
            self.notice = Some("找不到已下载文件的位置".into());
            cx.notify();
            return;
        };
        if path.is_dir() {
            open_directory(&path);
        } else if path.is_file() {
            let _ = std::process::Command::new("explorer.exe")
                .arg(&path)
                .spawn();
        } else {
            self.notice = Some("文件已被移动或删除".into());
            cx.notify();
        }
    }

    fn copy_task_link(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(url) = self
            .rows
            .iter()
            .find(|row| row.id == id)
            .map(|row| row.url.clone())
        else {
            return;
        };
        if let Some(watch) = &self._clip {
            watch.ignore_next();
        }
        cx.write_to_clipboard(ClipboardItem::new_string(url));
        self.notice = Some("已复制下载链接".into());
        cx.notify();
    }

    fn toggle(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(row) = self.rows.iter().find(|row| row.id == id).cloned() else {
            return;
        };
        let Some(control) = &self.control else {
            return;
        };
        match row.status {
            Status::Paused => {
                if let Some(dest) = row.dest.clone() {
                    if row.kind == Kind::Torrent {
                        control.send(Cmd::StartTorrent {
                            id,
                            only: row.only.clone(),
                        });
                    } else if row.kind == Kind::Video {
                        if let Some(task) = row.video.clone() {
                            control.send(Cmd::AddVideo {
                                id,
                                task: Box::new(task),
                                destination: dest
                                    .parent()
                                    .map(Path::to_path_buf)
                                    .unwrap_or_else(|| self.settings.dir.clone()),
                            });
                        }
                    } else {
                        control.send(Cmd::Resume {
                            id,
                            url: row.url,
                            dest,
                            headers: row.headers,
                        });
                    }
                } else if row.kind == Kind::Torrent {
                    control.send(Cmd::StartTorrent {
                        id,
                        only: row.only.clone(),
                    });
                } else if row.kind == Kind::Video {
                    if let Some(task) = row.video.clone() {
                        control.send(Cmd::AddVideo {
                            id,
                            task: Box::new(task),
                            destination: self.settings.dir.clone(),
                        });
                    }
                } else {
                    control.send(Cmd::Add {
                        id,
                        url: row.url,
                        headers: row.headers,
                    });
                }
                if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
                    row.status = Status::Probing;
                    row.error = None;
                }
            }
            Status::Active | Status::Seeding | Status::Queued | Status::Probing => {
                if row.kind == Kind::Torrent {
                    control.send(Cmd::PauseTorrent(id));
                } else {
                    control.send(Cmd::Pause(id));
                }
            }
            Status::Done | Status::Failed => return,
        }
        self.mark_dirty();
        self.save_now();
        cx.notify();
    }

    fn handle_event(&mut self, event: UiEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            UiEvent::HttpPreview {
                id,
                name,
                size,
                final_url,
                ..
            } => {
                if let Some(view) = &self.add_dialog {
                    view.update(cx, |view, cx| {
                        view.set_preview(id, name, size, final_url);
                        cx.notify();
                    });
                }
            }
            UiEvent::HttpPreviewFailed { id, reason } => {
                if let Some(view) = &self.add_dialog {
                    view.update(cx, |view, cx| {
                        view.set_preview_error(id, reason);
                        cx.notify();
                    });
                }
            }
            UiEvent::Started {
                id,
                dest,
                name,
                size,
                resumed_from,
            } => {
                if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
                    row.dest = Some(dest);
                    row.name = name;
                    row.size = size;
                    row.status = Status::Active;
                    row.resumed_from = resumed_from;
                    row.downloaded = resumed_from;
                    row.connections = 0;
                    row.error = None;
                    row.speed = 0.0;
                    row.sample = Some((Instant::now(), resumed_from));
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
                if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
                    if total > 0 {
                        row.size = total;
                    }
                    row.downloaded = downloaded;
                    row.connections = connections;
                    row.elapsed_ms = elapsed_ms;
                    row.status = Status::Active;
                    update_speed(row, downloaded);
                }
                self.mark_dirty();
            }
            UiEvent::Done {
                id,
                path,
                size,
                elapsed_ms,
            } => {
                if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
                    row.status = Status::Done;
                    row.dest = Some(path);
                    if size > 0 {
                        row.size = size;
                    }
                    row.downloaded = row.size;
                    row.connections = 0;
                    row.speed = 0.0;
                    row.elapsed_ms = elapsed_ms;
                    row.error = None;
                }
                self.notice = Some("下载完成".into());
                self.mark_dirty();
            }
            UiEvent::Failed { id, reason } => {
                if id & (1_u64 << 63) != 0 {
                    if let Some(view) = &self.add_dialog {
                        view.update(cx, |view, cx| {
                            view.set_preview_error(id, reason);
                            cx.notify();
                        });
                    }
                    if let Some(control) = &self.control {
                        control.send(Cmd::ForgetTorrentPreview(id));
                    }
                    return;
                }
                if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
                    row.status = Status::Failed;
                    row.connections = 0;
                    row.speed = 0.0;
                    row.error = Some(reason.clone());
                }
                self.notice = Some(friendly_error(&reason));
                self.mark_dirty();
            }
            UiEvent::Paused { id } => {
                if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
                    row.status = Status::Paused;
                    row.connections = 0;
                    row.speed = 0.0;
                }
                self.mark_dirty();
            }
            UiEvent::Removed { id } => {
                if let Some(path) = self.pending_file_deletes.remove(&id) {
                    let _ = std::fs::remove_file(&path);
                    volant_app_core::model::remove_leftovers(&path);
                }
                self.rows.retain(|row| row.id != id);
                self.select_first_visible();
                self.mark_dirty();
            }
            UiEvent::TorrentReady {
                id,
                name,
                info_hash,
                total,
                files,
            } => {
                if id & (1_u64 << 63) != 0 {
                    if let Some(view) = &self.add_dialog {
                        view.update(cx, |view, cx| {
                            view.set_torrent_preview(id, name, total);
                            cx.notify();
                        });
                    }
                    if let Some(control) = &self.control {
                        control.send(Cmd::ForgetTorrentPreview(id));
                    }
                    return;
                }
                if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
                    row.name = name.clone();
                    row.size = total;
                    row.info_hash = info_hash;
                    row.files = files.clone();
                    row.status = Status::Queued;
                }
                self.notice = Some(format!("种子已解析：{} 个文件", files.len()));
                self.open_torrent_picker(id, name, files, window, cx);
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
                if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
                    row.dest = Some(dest);
                    if total > 0 {
                        row.size = total;
                    }
                    row.downloaded = downloaded;
                    row.uploaded = uploaded;
                    row.peers = peers;
                    row.seeders = seeders;
                    row.connections = peers;
                    row.status = if finished {
                        if self.settings.seed {
                            Status::Seeding
                        } else {
                            Status::Done
                        }
                    } else {
                        Status::Active
                    };
                    update_speed(row, downloaded);
                }
                self.mark_dirty();
            }
            UiEvent::VideoLookup { id, result } => match *result {
                Ok(lookup) => {
                    let token = lookup.token.clone();
                    self.video_tokens.remove(&id);
                    self.video_lookups.insert(
                        token,
                        VideoLookupSlot::Ready(Box::new(lookup), Instant::now()),
                    );
                }
                Err(reason) => {
                    if let Some(token) = self.video_tokens.remove(&id) {
                        self.video_lookups
                            .insert(token, VideoLookupSlot::Failed(reason, Instant::now()));
                    }
                }
            },
            UiEvent::Notice(message) => {
                self.notice = Some(message);
            }
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn save_if_due(&mut self) {
        if self.dirty && self.last_save.elapsed() >= Duration::from_millis(800) {
            self.save_now();
        }
    }

    fn save_now(&mut self) -> bool {
        let Some(store) = &self.store else {
            self.dirty = false;
            return true;
        };
        let persisted = Persisted {
            version: 1,
            next_id: self.next_id,
            settings: self.settings.clone(),
            rows: self.rows.clone(),
        };
        let result = store.save(&persisted);
        if let Err(error) = &result {
            self.notice = Some(format!("保存状态失败：{error}"));
        }
        self.dirty = false;
        self.last_save = Instant::now();
        result.is_ok()
    }

    fn visible_rows(&self) -> Vec<Row> {
        self.rows
            .iter()
            .filter(|row| self.filter.matches(row))
            .cloned()
            .collect()
    }

    fn select_first_visible(&mut self) {
        self.selected = self
            .rows
            .iter()
            .find(|row| self.filter.matches(row))
            .map(|row| row.id);
    }

    fn select_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.filter = filter;
        self.select_first_visible();
        cx.notify();
    }

    fn select_adjacent(&mut self, direction: isize, cx: &mut Context<Self>) {
        let rows = self.visible_rows();
        if rows.is_empty() {
            self.selected = None;
            cx.notify();
            return;
        }
        let current = self
            .selected
            .and_then(|id| rows.iter().position(|row| row.id == id));
        let next = match (current, direction.is_negative()) {
            (Some(index), true) => index.saturating_sub(1),
            (Some(index), false) => (index + 1).min(rows.len() - 1),
            (None, true) => rows.len() - 1,
            (None, false) => 0,
        };
        self.selected = Some(rows[next].id);
        cx.notify();
    }

    fn close_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.detail_open = false;
        if let Some(width) = self.detail_restore_width.take() {
            let height = window.viewport_size().height;
            window.resize(size(px(width), height));
        }
        cx.notify();
    }

    fn fit_inspector_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = window.viewport_size();
        let width = viewport.width.as_f32();
        if !self.detail_open || self.selected.is_none() {
            if let Some(restore_width) = self.detail_restore_width.take() {
                window.resize(size(px(restore_width), viewport.height));
            }
            return;
        }
        if width >= 1200.0 {
            return;
        }

        let display_width = window
            .display(cx)
            .map(|display| display.visible_bounds().size.width.as_f32())
            .unwrap_or(width + theme::size::INSPECTOR);
        let maximum = (display_width - 32.0).max(960.0);
        let rail_growth = if width < 1000.0 {
            theme::size::RAIL - theme::size::RAIL_ICON
        } else {
            0.0
        };
        let target = (width + theme::size::INSPECTOR + rail_growth)
            .max(1200.0)
            .min(maximum);
        if target > width {
            self.detail_restore_width.get_or_insert(width);
            window.resize(size(px(target), viewport.height));
        }
    }

    fn open_selected_location(&self) {
        let Some(row) = self
            .selected
            .and_then(|id| self.rows.iter().find(|row| row.id == id))
        else {
            return;
        };
        let path = row
            .dest
            .clone()
            .unwrap_or_else(|| self.settings.dir.clone());
        open_path(&path, row.dest.is_some());
    }

    fn filter_button(&self, filter: Filter, cx: &mut Context<Self>) -> Button {
        let count = self.rows.iter().filter(|row| filter.matches(row)).count();
        let selected = self.filter == filter;
        Button::new(ElementId::Name(format!("filter-{}", filter.key()).into()))
            .ghost()
            .selected(selected)
            .accessibility_label(format!("{}，{} 个任务", filter.label(), count))
            .h(px(32.0))
            .w_full()
            .px(px(8.0))
            .rounded(px(7.0))
            .justify_start()
            .when(selected, |this| {
                this.border_1()
                    .border_color(theme::current(cx).selected_border)
            })
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .child(
                        Icon::new(filter.icon())
                            .with_size(Size::Size(px(13.0)))
                            .text_color(if selected {
                                theme::current(cx).current
                            } else {
                                theme::current(cx).muted
                            }),
                    )
                    .child(div().flex_1().text_size(px(13.0)).child(filter.label()))
                    .child(
                        div()
                            .text_size(px(theme::text::META))
                            .font_family("Cascadia Mono")
                            .text_color(theme::current(cx).faint)
                            .child(count.to_string()),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select_filter(filter, cx);
            }))
    }

    fn speed_band(&self, rows: &[Row], colors: VolantColors) -> Div {
        let active_rows: Vec<&Row> = rows
            .iter()
            .filter(|row| row.status == Status::Active)
            .collect();
        let speed = active_rows.iter().map(|row| row.speed).sum::<f64>();
        let connections = active_rows.iter().map(|row| row.connections).sum::<usize>();
        let speed_text = fmt::speed(speed);
        let (speed_value, speed_unit) = speed_text
            .rsplit_once(' ')
            .map_or(("—", ""), |(value, unit)| (value, unit));
        let eta = active_rows
            .iter()
            .filter_map(|row| row.eta_secs())
            .reduce(f64::min)
            .map_or_else(|| "—".to_string(), |secs| format!("约 {}", fmt::eta(secs)));
        let sample_source = active_rows
            .iter()
            .max_by(|a, b| a.speed.total_cmp(&b.speed))
            .copied();
        let mut amplitudes = vec![0.0_f64; 16];
        let mut has_flow = false;
        if let Some(row) = sample_source {
            let recent: Vec<f64> = row
                .speed_history
                .iter()
                .rev()
                .take(16)
                .map(|(_, speed)| *speed)
                .collect();
            let max = recent.iter().copied().fold(0.0_f64, f64::max);
            has_flow = max > 0.0;
            for (index, value) in recent.into_iter().rev().enumerate() {
                amplitudes[16usize.saturating_sub(recent_len(row)) + index] =
                    if max > 0.0 { value / max } else { 0.28 };
            }
        }
        let levels = amplitudes
            .into_iter()
            .map(|value| value as f32)
            .collect::<Vec<_>>();
        let line_color = colors.current;
        let line = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                if !has_flow || levels.is_empty() {
                    return;
                }
                let step = bounds.size.width / (levels.len() - 1).max(1) as f32;
                let points = levels
                    .iter()
                    .enumerate()
                    .map(|(index, level)| {
                        point(
                            bounds.left() + step * index as f32,
                            bounds.bottom() - px(5.0 + level.clamp(0.0, 1.0) * 34.0),
                        )
                    })
                    .collect::<Vec<_>>();

                let mut area = PathBuilder::fill();
                area.move_to(point(points[0].x, bounds.bottom()));
                area.line_to(points[0]);
                for index in 1..points.len() {
                    let previous = points[index - 1];
                    let current = points[index];
                    let before = points[index.saturating_sub(2)];
                    let after = points[(index + 1).min(points.len() - 1)];
                    let control_a = point(
                        px(previous.x.as_f32() + (current.x.as_f32() - before.x.as_f32()) / 6.0),
                        px(previous.y.as_f32() + (current.y.as_f32() - before.y.as_f32()) / 6.0),
                    );
                    let control_b = point(
                        px(current.x.as_f32() - (after.x.as_f32() - previous.x.as_f32()) / 6.0),
                        px(current.y.as_f32() - (after.y.as_f32() - previous.y.as_f32()) / 6.0),
                    );
                    area.cubic_bezier_to(current, control_a, control_b);
                }
                area.line_to(point(points[points.len() - 1].x, bounds.bottom()));
                area.line_to(point(points[0].x, bounds.bottom()));
                if let Ok(path) = area.build() {
                    window.paint_path(
                        path,
                        linear_gradient(
                            180.0,
                            linear_color_stop(line_color.opacity(0.18), 0.0),
                            linear_color_stop(line_color.opacity(0.01), 1.0),
                        ),
                    );
                }

                let mut stroke = PathBuilder::stroke(px(2.0));
                stroke.move_to(points[0]);
                for index in 1..points.len() {
                    let previous = points[index - 1];
                    let current = points[index];
                    let before = points[index.saturating_sub(2)];
                    let after = points[(index + 1).min(points.len() - 1)];
                    let control_a = point(
                        px(previous.x.as_f32() + (current.x.as_f32() - before.x.as_f32()) / 6.0),
                        px(previous.y.as_f32() + (current.y.as_f32() - before.y.as_f32()) / 6.0),
                    );
                    let control_b = point(
                        px(current.x.as_f32() - (after.x.as_f32() - previous.x.as_f32()) / 6.0),
                        px(current.y.as_f32() - (after.y.as_f32() - previous.y.as_f32()) / 6.0),
                    );
                    stroke.cubic_bezier_to(current, control_a, control_b);
                }
                if let Ok(path) = stroke.build() {
                    window.paint_path(path, line_color);
                }

                let end = points[points.len() - 1];
                window.paint_quad(quad(
                    Bounds {
                        origin: point(end.x - px(6.0), end.y - px(6.0)),
                        size: size(px(12.0), px(12.0)),
                    },
                    px(99.0),
                    line_color.opacity(0.14),
                    px(0.0),
                    gpui_kit::transparent_black(),
                    Default::default(),
                ));
                window.paint_quad(quad(
                    Bounds {
                        origin: point(end.x - px(3.0), end.y - px(3.0)),
                        size: size(px(6.0), px(6.0)),
                    },
                    px(99.0),
                    line_color,
                    px(0.0),
                    gpui_kit::transparent_black(),
                    Default::default(),
                ));
            },
        )
        .size_full();

        div()
            .h(px(theme::size::SPEED_MIN))
            .min_h(px(theme::size::SPEED_MIN))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(20.0))
            .px(px(16.0))
            .py(px(12.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(colors.border)
            .bg(colors.surface)
            .child(
                div()
                    .w(px(150.0))
                    .flex_shrink_0()
                    .flex()
                    .items_end()
                    .gap(px(4.0))
                    .child(
                        div()
                            .font_family("Cascadia Mono")
                            .font_weight(FontWeight(620.0))
                            .text_size(px(22.0))
                            .line_height(px(26.0))
                            .child(speed_value.to_string()),
                    )
                    .child(
                        div()
                            .pb(px(2.0))
                            .text_size(px(theme::text::BODY))
                            .text_color(colors.muted)
                            .child(speed_unit.to_string()),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .h(px(52.0))
                    .px(px(12.0))
                    .child(line),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_start()
                    .gap(px(20.0))
                    .child(speed_meta(
                        "活动连接",
                        &format!("{connections} / 8"),
                        colors,
                    ))
                    .child(speed_meta("剩余", &eta, colors)),
            )
    }

    fn queue_header(&self, colors: VolantColors) -> Div {
        div()
            .h(px(theme::size::QUEUE_HEADER))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(theme::size::ROW_GAP))
            .px(px(theme::size::ROW_PAD))
            .border_b_1()
            .border_color(colors.border)
            .text_size(px(theme::text::META))
            .text_color(colors.faint)
            .child(div().flex_1().min_w(px(0.0)).child("文件"))
            .child(div().w(px(theme::size::PROGRESS_COL)).child("进度"))
            .child(div().w(px(theme::size::SIZE_COL)).child("大小"))
            .child(div().w(px(theme::size::STATUS_COL)).child("状态"))
            .child(div().w(px(theme::size::ACTION_COL)))
    }

    fn transfer_row(
        &self,
        row: Row,
        colors: VolantColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let selected = self.selected == Some(row.id);
        let id = row.id;
        let percent = fmt::percent(row.downloaded, row.size);
        let size = if row.size == 0 {
            "—".to_string()
        } else {
            fmt::bytes(row.size)
        };
        let tone = status_color(row.status, colors);
        let note = row_note(&row);
        let context_parent = cx.entity().downgrade();
        let context_path = row
            .dest
            .clone()
            .unwrap_or_else(|| self.settings.dir.clone());
        let can_start = matches!(row.status, Status::Paused | Status::Failed);
        let can_pause = matches!(
            row.status,
            Status::Active | Status::Seeding | Status::Queued | Status::Probing
        );
        let can_open = row.status == Status::Done;
        let toggle_icon = match row.status {
            Status::Paused => IconName::Play,
            Status::Failed => IconName::RotateCcw,
            Status::Done => IconName::FolderOpen,
            Status::Active | Status::Seeding | Status::Queued | Status::Probing => IconName::Pause,
        };
        let toggle_label = match row.status {
            Status::Paused => "开始",
            Status::Failed => "重试",
            Status::Active | Status::Seeding | Status::Queued | Status::Probing => "暂停",
            Status::Done => "打开文件",
        };
        let details_open = self.detail_open && self.selected == Some(id);
        let details_label = if details_open {
            "收起详情"
        } else {
            "展开详情"
        };
        let progress = Progress::new(ElementId::Name(format!("row-progress-{id}").into()))
            .value(row.frac() * 100.0)
            .color(if row.status == Status::Queued {
                colors.border_strong
            } else {
                colors.current
            })
            .with_size(Size::Size(px(3.0)))
            .w_full();
        div()
            .id(ElementId::Name(format!("transfer-row-{id}").into()))
            .flex()
            .items_center()
            .w_full()
            .min_w(px(0.0))
            .min_h(px(theme::size::TASK_ROW))
            .gap(px(theme::size::ROW_GAP))
            .px(px(theme::size::ROW_PAD))
            .py(px(7.0))
            .rounded(px(6.0))
            .border_b_1()
            .border_color(colors.border)
            .bg(if selected {
                colors.selected
            } else {
                colors.surface
            })
            .when(selected, |this| {
                this.border_1().border_color(colors.selected_border)
            })
            .when(!selected, |this| this.hover(|this| this.bg(colors.hover)))
            .cursor_pointer()
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .items_center()
                    .gap(px(9.0))
                    .child(
                        div()
                            .w(px(30.0))
                            .h(px(30.0))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(7.0))
                            .border_1()
                            .border_color(if selected {
                                colors.selected_border
                            } else {
                                colors.border
                            })
                            .bg(colors.surface)
                            .child(
                                Icon::new(IconName::File)
                                    .with_size(Size::Size(px(14.0)))
                                    .text_color(colors.muted),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .v_flex()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_size(px(13.0))
                                    .font_weight(FontWeight(580.0))
                                    .text_color(colors.text)
                                    .child(row.display_name().to_string()),
                            )
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_size(px(theme::text::META))
                                    .text_color(colors.muted)
                                    .child(note),
                            ),
                    ),
            )
            .child(
                div()
                    .w(px(theme::size::PROGRESS_COL))
                    .flex_shrink_0()
                    .v_flex()
                    .gap(px(5.0))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_size(px(theme::text::META))
                            .font_family("Cascadia Mono")
                            .text_color(colors.muted)
                            .child(if row.status == Status::Active {
                                fmt::speed(row.speed)
                            } else {
                                "—".to_string()
                            })
                            .child(percent),
                    )
                    .child(progress),
            )
            .child(
                div()
                    .w(px(theme::size::SIZE_COL))
                    .flex_shrink_0()
                    .text_size(px(theme::text::BODY))
                    .font_family("Cascadia Mono")
                    .text_color(colors.text)
                    .child(size),
            )
            .child(
                div()
                    .w(px(theme::size::STATUS_COL))
                    .flex_shrink_0()
                    .child(status_inline_text(status_label(row.status), tone)),
            )
            .child(
                div()
                    .w(px(theme::size::ACTION_COL))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(2.0))
                    .child(
                        Button::new(format!("row-toggle-{id}"))
                            .ghost()
                            .compact()
                            .when(can_pause, |this| this.child(pause_glyph(colors)))
                            .when(!can_pause, |this| this.icon(toggle_icon))
                            .accessibility_label(toggle_label)
                            .tooltip(toggle_label)
                            .text_color(colors.muted)
                            .w(px(26.0))
                            .h(px(26.0))
                            .disabled(!(can_start || can_pause || can_open))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if can_open {
                                    this.open_task_output(id, cx);
                                } else if can_start {
                                    this.start_task(id, cx);
                                } else {
                                    this.pause_task(id, cx);
                                }
                            })),
                    )
                    .child(
                        Button::new(format!("row-delete-{id}"))
                            .ghost()
                            .compact()
                            .icon(IconName::Trash)
                            .accessibility_label("删除任务")
                            .tooltip("删除任务")
                            .text_color(colors.muted)
                            .w(px(26.0))
                            .h(px(26.0))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_delete(id, false, window, cx);
                            })),
                    )
                    .child(
                        Button::new(format!("row-details-{id}"))
                            .ghost()
                            .compact()
                            .icon(if details_open {
                                IconName::ChevronLeft
                            } else {
                                IconName::ChevronRight
                            })
                            .accessibility_label(details_label)
                            .tooltip(details_label)
                            .text_color(colors.muted)
                            .w(px(26.0))
                            .h(px(26.0))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if this.detail_open && this.selected == Some(id) {
                                    this.close_detail(window, cx);
                                } else {
                                    this.selected = Some(id);
                                    this.detail_open = true;
                                    cx.notify();
                                }
                            })),
                    ),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                if !event.standard_click() {
                    return;
                }
                let now = Instant::now();
                let repeated = event.click_count() >= 2
                    || this.last_task_click.is_some_and(|(last_id, last_time)| {
                        last_id == id && now.duration_since(last_time) <= Duration::from_millis(600)
                    });
                this.last_task_click = Some((id, now));
                this.selected = Some(id);
                if repeated {
                    this.last_task_click = None;
                    this.activate_task(id, cx);
                }
                cx.notify();
            }))
            .context_menu(move |menu, _, _| {
                let start_parent = context_parent.clone();
                let pause_parent = context_parent.clone();
                let delete_parent = context_parent.clone();
                let purge_parent = context_parent.clone();
                let copy_parent = context_parent.clone();
                let task_path = context_path.clone();
                menu.item(PopupMenuItem::new("开始").disabled(!can_start).on_click(
                    move |_, _, cx| {
                        let _ = start_parent.update(cx, |this, cx| this.start_task(id, cx));
                    },
                ))
                .item(
                    PopupMenuItem::new("暂停")
                        .disabled(!can_pause)
                        .on_click(move |_, _, cx| {
                            let _ = pause_parent.update(cx, |this, cx| this.pause_task(id, cx));
                        }),
                )
                .item(PopupMenuItem::new("删除").on_click(move |_, window, cx| {
                    let _ = delete_parent.update(cx, |this, cx| {
                        this.confirm_delete(id, false, window, cx);
                    });
                }))
                .item(
                    PopupMenuItem::new("彻底删除").on_click(move |_, window, cx| {
                        let _ = purge_parent.update(cx, |this, cx| {
                            this.confirm_delete(id, true, window, cx);
                        });
                    }),
                )
                .item(
                    PopupMenuItem::new("复制下载链接").on_click(move |_, _, cx| {
                        let _ = copy_parent.update(cx, |this, cx| this.copy_task_link(id, cx));
                    }),
                )
                .item(
                    PopupMenuItem::new("打开所在位置")
                        .on_click(move |_, _, _| open_path(&task_path, true)),
                )
            })
    }

    fn inspector(&self, colors: VolantColors, cx: &mut Context<Self>) -> Div {
        let selected = self
            .selected
            .and_then(|id| self.rows.iter().find(|row| row.id == id))
            .cloned();

        if let Some(row) = selected {
            let tone = status_color(row.status, colors);
            let downloaded = fmt::bytes(row.downloaded);
            let total = if row.size == 0 {
                "—".to_string()
            } else {
                fmt::bytes(row.size)
            };
            let speed = fmt::speed(row.speed);
            let eta = row
                .eta_secs()
                .map_or_else(|| "—".to_string(), |secs| format!("约 {}", fmt::eta(secs)));
            let path = row
                .dest
                .as_ref()
                .map_or_else(|| self.settings.dir.clone(), |path| path.clone());
            let path_text = path.display().to_string();
            let can_toggle = matches!(
                row.status,
                Status::Active
                    | Status::Seeding
                    | Status::Paused
                    | Status::Queued
                    | Status::Probing
            );
            let action_label = if row.status == Status::Paused {
                "继续下载"
            } else {
                "暂停下载"
            };
            let row_id = row.id;
            return div()
                .w(px(theme::size::INSPECTOR))
                .h_full()
                .flex_shrink_0()
                .v_flex()
                .px(px(18.0))
                .pt(px(16.0))
                .pb(px(18.0))
                .border_l_1()
                .border_color(colors.border)
                .bg(colors.rail)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(theme::text::META))
                                .text_color(colors.faint)
                                .child("任务详情"),
                        )
                        .child(
                            Button::new("close-selected-detail")
                                .ghost()
                                .compact()
                                .icon(IconName::WindowClose)
                                .accessibility_label("关闭任务详情")
                                .tooltip("关闭任务详情")
                                .w(px(28.0))
                                .h(px(28.0))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.close_detail(window, cx);
                                })),
                        ),
                )
                .child(
                    div().pt(px(16.0)).child(
                        div()
                            .w(px(48.0))
                            .h(px(48.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(11.0))
                            .border_1()
                            .border_color(colors.border)
                            .bg(colors.surface)
                            .child(
                                Icon::new(IconName::File)
                                    .with_size(Size::Size(px(23.0)))
                                    .text_color(colors.current),
                            ),
                    ),
                )
                .child(
                    div()
                        .pt(px(11.0))
                        .pb(px(9.0))
                        .text_size(px(16.0))
                        .line_height(px(20.0))
                        .font_weight(FontWeight(610.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(row.display_name().to_string()),
                )
                .child(status_pill(status_label(row.status), tone, colors))
                .child(divider(colors, true))
                .child(section_label("传输", colors))
                .child(detail_value(
                    "已下载",
                    &format!("{downloaded} / {total}"),
                    colors,
                ))
                .child(detail_value("当前速度", &speed, colors))
                .child(detail_value(
                    "连接",
                    &if row.connections == 0 {
                        "—".to_string()
                    } else {
                        format!("{} 条 · 自适应", row.connections)
                    },
                    colors,
                ))
                .child(detail_value("预计剩余", &eta, colors))
                .child(divider(colors, true))
                .child(section_label("保存位置", colors))
                .child(
                    div()
                        .py(px(5.0))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .font_family("Cascadia Mono")
                                .text_size(px(theme::text::META))
                                .text_color(colors.muted)
                                .child(path_text),
                        )
                        .child(
                            Button::new("reveal-path")
                                .ghost()
                                .compact()
                                .icon(IconName::Folder)
                                .accessibility_label("打开所在位置")
                                .tooltip(format!("打开所在位置\n{}", path.display()))
                                .w(px(24.0))
                                .h(px(24.0))
                                .on_click({
                                    let path = path.clone();
                                    move |_, _, _| open_path(&path, true)
                                }),
                        ),
                )
                .child(detail_value(
                    "可续传",
                    if row.resumed_from > 0 || row.status == Status::Paused {
                        "是"
                    } else {
                        "—"
                    },
                    colors,
                ))
                .when_some(row.error.as_ref(), |this, error| {
                    this.child(divider(colors, true))
                        .child(section_label("错误", colors))
                        .child(
                            div()
                                .py(px(5.0))
                                .text_size(px(theme::text::META))
                                .line_height(px(14.0))
                                .text_color(colors.danger)
                                .child(friendly_error(error)),
                        )
                        .child(
                            Button::new("retry-task")
                                .ghost()
                                .compact()
                                .label("重试")
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.retry(row_id, cx)),
                                ),
                        )
                })
                .child(div().flex_1())
                .child(
                    div()
                        .v_flex()
                        .gap(px(7.0))
                        .when(can_toggle, |this| {
                            this.child(
                                Button::new("toggle-selected")
                                    .secondary()
                                    .border_1()
                                    .border_color(colors.border_strong)
                                    .icon(if row.status == Status::Paused {
                                        IconName::Play
                                    } else {
                                        IconName::Pause
                                    })
                                    .label(action_label)
                                    .w_full()
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| this.toggle(row_id, cx)),
                                    ),
                            )
                        })
                        .child(
                            Button::new("open-selected-location")
                                .secondary()
                                .border_1()
                                .border_color(colors.border_strong)
                                .icon(IconName::FolderOpen)
                                .label("打开所在位置")
                                .w_full()
                                .on_click(cx.listener(|this, _, _, _| {
                                    this.open_selected_location();
                                })),
                        )
                        .child(
                            Button::new("delete-selected-files")
                                .ghost()
                                .icon(IconName::Trash)
                                .label("删除")
                                .text_color(colors.danger)
                                .w_full()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm_delete(row_id, true, window, cx);
                                })),
                        ),
                );
        }

        div()
            .w(px(theme::size::INSPECTOR))
            .h_full()
            .flex_shrink_0()
            .v_flex()
            .px(px(18.0))
            .pt(px(16.0))
            .border_l_1()
            .border_color(colors.border)
            .bg(colors.rail)
            .child(
                div()
                    .text_size(px(theme::text::META))
                    .text_color(colors.faint)
                    .child("任务详情"),
            )
            .child(
                div()
                    .mt(px(22.0))
                    .text_size(px(16.0))
                    .font_weight(FontWeight(610.0))
                    .child("选择一个任务"),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .text_size(px(theme::text::BODY))
                    .text_color(colors.muted)
                    .child("从左侧列表选择任务。"),
            )
    }

    fn compact_queue_rail(&self, colors: VolantColors, cx: &mut Context<Self>) -> Div {
        let filters = Filter::ALL
            .into_iter()
            .map(|filter| {
                let count = self.rows.iter().filter(|row| filter.matches(row)).count();
                let selected = self.filter == filter;
                Button::new(format!("compact-filter-{}", filter.key()))
                    .ghost()
                    .selected(selected)
                    .icon(filter.icon())
                    .accessibility_label(format!("{}，{} 个任务", filter.label(), count))
                    .tooltip(format!("{} · {}", filter.label(), count))
                    .w(px(40.0))
                    .h(px(36.0))
                    .when(selected, |this| {
                        this.border_1()
                            .border_color(colors.selected_border)
                            .bg(colors.selected)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_filter(filter, cx);
                    }))
            })
            .collect::<Vec<_>>();

        div()
            .w(px(theme::size::RAIL_ICON))
            .h_full()
            .flex_shrink_0()
            .v_flex()
            .items_center()
            .justify_between()
            .py(px(11.0))
            .bg(colors.rail)
            .border_r_1()
            .border_color(colors.border)
            .child(
                div()
                    .v_flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .w(px(30.0))
                            .h(px(30.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(7.0))
                            .bg(colors.primary)
                            .text_size(px(13.0))
                            .font_weight(FontWeight(800.0))
                            .text_color(colors.primary_text)
                            .child("V"),
                    )
                    .child(
                        Button::new("compact-new-download")
                            .primary()
                            .icon(IconName::Plus)
                            .accessibility_label("新建下载")
                            .tooltip("新建下载")
                            .w(px(40.0))
                            .h(px(36.0))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_add_dialog(window, cx);
                            })),
                    )
                    .child(div().h(px(1.0)).w(px(32.0)).bg(colors.border))
                    .child(div().v_flex().items_center().gap(px(4.0)).children(filters)),
            )
            .child(
                div()
                    .v_flex()
                    .items_center()
                    .gap(px(5.0))
                    .child(
                        Button::new("compact-download-directory")
                            .ghost()
                            .icon(IconName::Folder)
                            .accessibility_label("下载目录")
                            .tooltip("打开下载目录")
                            .w(px(40.0))
                            .h(px(36.0))
                            .on_click({
                                let path = self.settings.dir.clone();
                                move |_, _, _| open_directory(&path)
                            }),
                    )
                    .child(
                        Button::new("compact-settings")
                            .ghost()
                            .icon(IconName::Settings)
                            .accessibility_label("设置")
                            .tooltip("设置")
                            .w(px(40.0))
                            .h(px(36.0))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_settings_dialog(window, cx);
                            })),
                    ),
            )
    }

    fn queue_rail(
        &self,
        colors: VolantColors,
        cx: &mut Context<Self>,
        width: f32,
        compact: bool,
    ) -> Div {
        if compact {
            return self.compact_queue_rail(colors, cx);
        }
        let filters = Filter::ALL
            .into_iter()
            .map(|filter| self.filter_button(filter, cx))
            .collect::<Vec<_>>();
        div()
            .w(px(width))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .justify_between()
            .px(px(11.0))
            .pt(px(11.0))
            .pb(px(12.0))
            .bg(colors.rail)
            .border_r_1()
            .border_color(colors.border)
            .child(
                div()
                    .v_flex()
                    .gap(px(11.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(9.0))
                            .child(
                                div()
                                    .w(px(26.0))
                                    .h(px(26.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(6.0))
                                    .bg(colors.primary)
                                    .text_size(px(12.0))
                                    .font_weight(FontWeight(800.0))
                                    .text_color(colors.primary_text)
                                    .child("V"),
                            )
                            .child(
                                div()
                                    .v_flex()
                                    .gap(px(1.0))
                                    .child(
                                        div()
                                            .text_size(px(theme::text::META))
                                            .font_weight(FontWeight(650.0))
                                            .child("Volant"),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(theme::text::META))
                                            .text_color(colors.muted)
                                            .child("下载队列"),
                                    ),
                            ),
                    )
                    .child(
                        Button::new("ribbon-new-download")
                            .primary()
                            .icon(IconName::Plus)
                            .label("新建下载")
                            .w_full()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_add_dialog(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .pt(px(5.0))
                            .px(px(3.0))
                            .text_size(px(theme::text::META))
                            .text_color(colors.faint)
                            .child("队列"),
                    )
                    .child(div().v_flex().gap(px(2.0)).children(filters)),
            )
            .child(
                div()
                    .v_flex()
                    .gap(px(2.0))
                    .child(
                        Button::new("download-directory")
                            .ghost()
                            .accessibility_label("下载目录")
                            .w_full()
                            .h(px(32.0))
                            .px(px(8.0))
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .gap(px(9.0))
                                    .justify_start()
                                    .child(
                                        Icon::new(IconName::Folder)
                                            .with_size(Size::Size(px(13.0)))
                                            .text_color(colors.muted),
                                    )
                                    .child(div().child("下载目录")),
                            )
                            .on_click({
                                let path = self.settings.dir.clone();
                                move |_, _, _| open_directory(&path)
                            }),
                    )
                    .child(
                        Button::new("settings")
                            .ghost()
                            .accessibility_label("设置")
                            .w_full()
                            .h(px(32.0))
                            .px(px(8.0))
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .gap(px(9.0))
                                    .justify_start()
                                    .child(
                                        Icon::new(IconName::Settings)
                                            .with_size(Size::Size(px(13.0)))
                                            .text_color(colors.muted),
                                    )
                                    .child(div().child("设置")),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_settings_dialog(window, cx);
                            })),
                    ),
            )
    }
}

impl Render for VolantShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if window.has_active_dialog(cx) {
            return div()
                .size_full()
                .bg(theme::current(cx).canvas)
                .into_any_element();
        }
        self.add_dialog = None;
        self.settings_dialog = None;
        self.pick_dialog = None;

        self.fit_inspector_window(window, cx);
        let viewport_width = window.viewport_size().width.as_f32();
        let show_inspector =
            self.detail_open && self.selected.is_some() && viewport_width >= 1200.0;
        let compact_rail = viewport_width < 1000.0;
        let rail_width = if compact_rail {
            theme::size::RAIL_ICON
        } else {
            theme::size::RAIL
        };
        let colors = theme::current(cx);
        let rows = self.visible_rows();
        let row_elements = rows
            .iter()
            .map(|row| self.transfer_row(row.clone(), colors, cx))
            .collect::<Vec<_>>();
        let active_count = rows
            .iter()
            .filter(|row| row.status == Status::Active)
            .count();
        let count_copy = format!("{} 个任务 · {} 个正在传输", rows.len(), active_count);
        let rail = self.queue_rail(colors, cx, rail_width, compact_rail);
        let speed_band = self.speed_band(&rows, colors);
        let queue_header = self.queue_header(colors);
        let inspector = self.inspector(colors, cx);
        let empty = rows.is_empty().then(|| {
            div()
                .flex_1()
                .min_h(px(180.0))
                .flex()
                .v_flex()
                .items_center()
                .justify_center()
                .gap(px(7.0))
                .child(
                    div()
                        .w(px(42.0))
                        .h(px(42.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(10.0))
                        .bg(colors.sunken)
                        .child(
                            Icon::new(IconName::Inbox)
                                .with_size(Size::Size(px(20.0)))
                                .text_color(colors.muted),
                        ),
                )
                .child(
                    div()
                        .pt(px(4.0))
                        .text_size(px(16.0))
                        .font_weight(FontWeight(600.0))
                        .child("还没有下载任务"),
                )
                .child(
                    div()
                        .text_size(px(theme::text::BODY))
                        .text_color(colors.muted)
                        .child("粘贴链接或从浏览器开始一次下载。"),
                )
                .child(
                    Button::new("empty-add")
                        .primary()
                        .icon(IconName::Plus)
                        .label("添加链接")
                        .mt(px(5.0))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_add_dialog(window, cx);
                        })),
                )
        });
        let video_setup = (!self.video_dependencies.ready).then(|| {
            div()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(12.0))
                .py(px(8.0))
                .rounded(px(7.0))
                .border_1()
                .border_color(colors.warning.opacity(0.35))
                .bg(colors.warning.opacity(0.08))
                .child(
                    Icon::new(IconName::Play)
                        .with_size(Size::Size(px(14.0)))
                        .text_color(colors.warning),
                )
                .child(
                    div()
                        .flex_1()
                        .v_flex()
                        .gap(px(1.0))
                        .child(
                            div()
                                .text_size(px(theme::text::BODY))
                                .font_weight(FontWeight(590.0))
                                .child("视频组件尚未准备"),
                        )
                        .child(
                            div()
                                .text_size(px(theme::text::META))
                                .text_color(colors.muted)
                                .child("yt-dlp 负责解析，ffmpeg 负责合成 MP4。"),
                        ),
                )
                .child(
                    Button::new("main-prepare-video")
                        .secondary()
                        .compact()
                        .label("去准备")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_settings_category(SettingsCategory::Video, window, cx);
                        })),
                )
        });

        div()
            .v_flex()
            .size_full()
            .overflow_hidden()
            .bg(colors.canvas)
            .text_color(colors.text)
            .track_focus(&self.focus_handle)
            .key_context("Volant")
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                let modifiers = event.keystroke.modifiers;
                if modifiers.control && key == "n" {
                    window.prevent_default();
                    cx.stop_propagation();
                    this.open_add_dialog(window, cx);
                    return;
                }
                if modifiers.modified() {
                    return;
                }
                if key == "escape" && this.detail_open {
                    window.prevent_default();
                    cx.stop_propagation();
                    this.close_detail(window, cx);
                    return;
                }
                match key {
                    "space" => {
                        if let Some(id) = this.selected {
                            window.prevent_default();
                            cx.stop_propagation();
                            this.toggle(id, cx);
                        }
                    }
                    "delete" => {
                        if let Some(id) = this.selected {
                            window.prevent_default();
                            cx.stop_propagation();
                            this.confirm_delete(id, false, window, cx);
                        }
                    }
                    "up" => {
                        window.prevent_default();
                        cx.stop_propagation();
                        this.select_adjacent(-1, cx);
                    }
                    "down" => {
                        window.prevent_default();
                        cx.stop_propagation();
                        this.select_adjacent(1, cx);
                    }
                    _ => {}
                }
            }))
            .on_drop::<ExternalPaths>(cx.listener(|this, paths, window, cx| {
                this.handle_external_paths(paths, window, cx);
            }))
            .child(
                TitleBar::new()
                    .bg(colors.titlebar)
                    .border_color(colors.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .child(
                                div()
                                    .w(px(17.0))
                                    .h(px(17.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(4.0))
                                    .bg(colors.primary)
                                    .text_color(colors.primary_text)
                                    .text_size(px(10.0))
                                    .font_weight(FontWeight(800.0))
                                    .child("V"),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::text::TITLEBAR))
                                    .font_weight(FontWeight(600.0))
                                    .child("Volant"),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(rail)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .h_full()
                            .overflow_x_scrollbar()
                            .child(
                                div()
                                    .w_full()
                                    .min_w(px(650.0))
                                    .h_full()
                                    .overflow_hidden()
                                    .v_flex()
                                    .px(px(theme::size::MAIN_PAD))
                                    .pt(px(theme::size::MAIN_PAD))
                                    .pb(px(14.0))
                                    .gap(px(theme::size::MAIN_GAP))
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .flex()
                                            .items_end()
                                            .justify_between()
                                            .child(
                                                div()
                                                    .v_flex()
                                                    .gap(px(2.0))
                                                    .child(
                                                        div()
                                                            .text_size(px(24.0))
                                                            .line_height(px(28.0))
                                                            .font_weight(FontWeight(630.0))
                                                            .child(self.filter.label()),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(px(theme::text::META))
                                                            .text_color(colors.muted)
                                                            .child(count_copy),
                                                    ),
                                            )
                                            .child(
                                                Button::new("add-link")
                                                    .primary()
                                                    .icon(IconName::Plus)
                                                    .label("添加链接")
                                                    .on_click(cx.listener(
                                                        |this, _, window, cx| {
                                                            this.open_add_dialog(window, cx);
                                                        },
                                                    )),
                                            ),
                                    )
                                    .when_some(video_setup, |this, setup| this.child(setup))
                                    .child(speed_band)
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_h(px(0.0))
                                            .v_flex()
                                            .child(queue_header)
                                            .when_some(empty, |this, empty| this.child(empty))
                                            .when(!rows.is_empty(), |this| {
                                                this.child(
                                                    div()
                                                        .id("task-list")
                                                        .flex_1()
                                                        .min_h(px(0.0))
                                                        .overflow_y_scrollbar()
                                                        .children(row_elements),
                                                )
                                            }),
                                    ),
                            ),
                    )
                    .when(show_inspector, |this| this.child(inspector)),
            )
            .into_any_element()
    }
}

fn dialog_title_bar(title: &str, colors: VolantColors) -> Div {
    let close_id = format!("dialog-close-{title}");
    div()
        .h(px(theme::size::TITLEBAR))
        .flex_shrink_0()
        .flex()
        .items_center()
        .pl(px(12.0))
        .bg(colors.titlebar)
        .border_b_1()
        .border_color(colors.border)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(7.0))
                .child(
                    div()
                        .w(px(17.0))
                        .h(px(17.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.0))
                        .bg(colors.primary)
                        .text_color(colors.primary_text)
                        .text_size(px(10.0))
                        .font_weight(FontWeight(800.0))
                        .child("V"),
                )
                .child(
                    div()
                        .text_size(px(theme::text::TITLEBAR))
                        .font_weight(FontWeight(600.0))
                        .child(title.to_string()),
                ),
        )
        .child(div().flex_1())
        .child(
            div()
                .flex()
                .items_center()
                .child(
                    div()
                        .w(px(29.0))
                        .h(px(theme::size::TITLEBAR))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            Icon::new(IconName::WindowMinimize)
                                .with_size(Size::Size(px(10.0)))
                                .text_color(colors.muted),
                        ),
                )
                .child(
                    div()
                        .w(px(29.0))
                        .h(px(theme::size::TITLEBAR))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            Icon::new(IconName::WindowMaximize)
                                .with_size(Size::Size(px(10.0)))
                                .text_color(colors.muted),
                        ),
                )
                .child(
                    Button::new(close_id)
                        .ghost()
                        .icon(IconName::WindowClose)
                        .accessibility_label("关闭")
                        .tooltip("关闭")
                        .w(px(29.0))
                        .h(px(theme::size::TITLEBAR))
                        .rounded(px(0.0))
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
        )
}

fn speed_meta(label: &str, value: &str, colors: VolantColors) -> Div {
    div()
        .v_flex()
        .gap(px(4.0))
        .child(
            div()
                .text_size(px(theme::text::META))
                .text_color(colors.faint)
                .child(label.to_string()),
        )
        .child(
            div()
                .font_family("Cascadia Mono")
                .text_size(px(theme::text::BODY))
                .text_color(colors.text)
                .child(value.to_string()),
        )
}

fn pause_glyph(colors: VolantColors) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(4.0))
        .child(
            div()
                .w(px(2.5))
                .h(px(12.0))
                .rounded(px(1.0))
                .bg(colors.muted),
        )
        .child(
            div()
                .w(px(2.5))
                .h(px(12.0))
                .rounded(px(1.0))
                .bg(colors.muted),
        )
}

fn rate_label(rate: Option<u64>) -> String {
    rate.map_or_else(
        || "不限".to_string(),
        |rate| format!("{}/s", fmt::bytes(rate)),
    )
}

fn divider(colors: VolantColors, vertical_margin: bool) -> Div {
    div()
        .h(px(1.0))
        .w_full()
        .bg(colors.border)
        .when(vertical_margin, |this| this.my(px(12.0)))
}

fn section_label(label: &str, colors: VolantColors) -> Div {
    div()
        .pb(px(5.0))
        .text_size(px(theme::text::META))
        .text_color(colors.faint)
        .child(label.to_string())
}

fn detail_value(label: &str, value: &str, colors: VolantColors) -> Div {
    div()
        .min_h(px(22.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(10.0))
        .text_size(px(theme::text::META))
        .child(div().text_color(colors.faint).child(label.to_string()))
        .child(
            div()
                .min_w(px(0.0))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .font_family("Cascadia Mono")
                .text_size(px(theme::text::META))
                .text_color(colors.text)
                .child(value.to_string()),
        )
}

fn status_label(status: Status) -> &'static str {
    match status {
        Status::Active => "下载中",
        Status::Probing => "正在读取",
        Status::Queued => "等待",
        Status::Paused => "已暂停",
        Status::Done => "已完成",
        Status::Seeding => "做种中",
        Status::Failed => "失败",
    }
}

fn status_color(status: Status, colors: VolantColors) -> Hsla {
    match status {
        Status::Active | Status::Probing => colors.current,
        Status::Queued => colors.faint,
        Status::Paused => colors.warning,
        Status::Done | Status::Seeding => colors.success,
        Status::Failed => colors.danger,
    }
}

fn status_dot(color: Hsla, size: f32) -> Div {
    div()
        .w(px(size))
        .h(px(size))
        .flex_shrink_0()
        .rounded_full()
        .bg(color)
}

fn status_inline_text(label: &str, color: Hsla) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(5.0))
        .text_size(px(theme::text::META))
        .text_color(color)
        .child(status_dot(color, 6.0))
        .child(label.to_string())
}

fn status_pill(label: &str, color: Hsla, colors: VolantColors) -> Div {
    div()
        .self_start()
        .flex()
        .items_center()
        .gap(px(6.0))
        .px(px(8.0))
        .py(px(3.0))
        .rounded(px(999.0))
        .bg(color.opacity(0.1))
        .text_size(px(theme::text::META))
        .text_color(color)
        .border_1()
        .border_color(color.opacity(0.22))
        .child(status_dot(color, 6.0))
        .child(label.to_string())
        .when(colors.current == color, |this| this)
}

fn row_note(row: &Row) -> String {
    match row.status {
        Status::Active => {
            let eta = row
                .eta_secs()
                .map_or_else(String::new, |secs| format!(" · 剩余约 {}", fmt::eta(secs)));
            if row.connections > 0 {
                format!("{} 连接{}", row.connections, eta)
            } else {
                "正在传输…".into()
            }
        }
        Status::Queued => row.note(),
        _ => row.note(),
    }
}

fn source_domain(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .to_string()
}

fn file_kind(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())
        .map(|extension| extension.to_ascii_uppercase())
        .unwrap_or_else(|| "文件".to_string())
}

fn recent_len(row: &Row) -> usize {
    row.speed_history.len().min(16)
}

fn open_directory(path: &Path) {
    let _ = std::fs::create_dir_all(path);
    open_path(path, false);
}

fn open_path(path: &Path, reveal_file: bool) {
    #[cfg(windows)]
    {
        if reveal_file && path.is_file() {
            let _ = std::process::Command::new("explorer.exe")
                .arg("/select,")
                .arg(path)
                .spawn();
        } else {
            let directory = if path.is_dir() {
                path
            } else {
                path.parent().unwrap_or(path)
            };
            if !directory.exists() {
                let _ = std::fs::create_dir_all(directory);
            }
            let _ = std::process::Command::new("explorer.exe")
                .arg(directory)
                .spawn();
        }
    }
    #[cfg(not(windows))]
    {
        let _ = reveal_file;
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

fn update_speed(row: &mut Row, downloaded: u64) {
    let now = Instant::now();
    if let Some((then, previous)) = row.sample
        && now.duration_since(then).as_secs_f64() >= 0.4
    {
        let seconds = now.duration_since(then).as_secs_f64();
        row.speed = downloaded.saturating_sub(previous) as f64 / seconds;
        row.sample = Some((now, downloaded));
        row.push_speed_sample(now);
    } else if row.sample.is_none() {
        row.sample = Some((now, downloaded));
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn sample_rows() -> Vec<Row> {
    let mut rows = Vec::new();
    let mut push = |id, name: &str, status, size, downloaded, speed| {
        let mut row = Row::new(id, format!("https://example.com/{name}"));
        row.name = name.to_string();
        row.status = status;
        row.size = size;
        row.downloaded = downloaded;
        row.speed = speed;
        row.connections = if status == Status::Active { 8 } else { 0 };
        row.dest = Some(PathBuf::from(r"C:\Users\Evan\Downloads").join(name));
        let now = Instant::now();
        let samples = [
            0.56, 0.80, 0.76, 0.95, 0.45, 0.88, 0.74, 0.62, 0.92, 0.70, 0.55, 0.86, 0.64, 0.78,
            0.72, 0.90,
        ];
        for (index, ratio) in samples.into_iter().enumerate() {
            row.speed_history.push_back((
                now - Duration::from_millis((15 - index) as u64 * 450),
                speed * ratio,
            ));
        }
        rows.push(row);
    };
    push(
        1,
        "debian-13.2.0-amd64-netinst.iso",
        Status::Active,
        682 * 1024 * 1024,
        421 * 1024 * 1024,
        18.7 * 1024.0 * 1024.0,
    );
    push(
        2,
        "openai-paper-collection.zip",
        Status::Queued,
        128 * 1024 * 1024,
        0,
        0.0,
    );
    push(
        3,
        "rustup-init.exe",
        Status::Paused,
        9_200_000,
        4_718_592,
        0.0,
    );
    push(
        4,
        "ubuntu-26.04-desktop-amd64.iso",
        Status::Done,
        3_500_000_000,
        3_500_000_000,
        0.0,
    );
    push(
        5,
        "archive-dataset.tar.zst",
        Status::Failed,
        940_000_000,
        87_031_808,
        0.0,
    );
    rows[4].error = Some("server returned HTTP 403".into());
    rows
}

pub fn run(requested_background: bool) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let screen = arg_value(&args, "--screen").unwrap_or_else(|| "main".into());
    let add_state = arg_value(&args, "--add-state").unwrap_or_else(|| "empty".into());
    let theme_override = arg_value(&args, "--theme").and_then(|value| match value.as_str() {
        "light" => Some(AppThemeMode::Light),
        "dark" => Some(AppThemeMode::Dark),
        "system" => Some(AppThemeMode::System),
        _ => None,
    });
    let empty_demo = args.iter().any(|arg| arg == "--empty");
    let select_failed = args.iter().any(|arg| arg == "--select-failed");
    let select_paused = args.iter().any(|arg| arg == "--select-paused");
    let settings_error = args.iter().any(|arg| arg == "--settings-error");
    let demo = args.iter().any(|arg| arg == "--demo") || screen != "main";
    let initial_add = arg_value(&args, "--add");
    let initial_torrent = arg_value(&args, "--torrent").map(PathBuf::from);
    let explicit_size = arg_value(&args, "--size").and_then(|value| parse_size(&value));
    let requested_background = requested_background || args.iter().any(|arg| arg == "--background");
    let (tray, tray_events) = if demo {
        (None, None)
    } else if let Some((tray, events)) = TrayIcon::start() {
        (Some(tray), Some(events))
    } else {
        (None, None)
    };
    let background = requested_background && tray.is_some();

    application()
        .with_quit_mode(QuitMode::Explicit)
        .with_assets(assets::AllAssets)
        .run(move |cx| {
        init(cx);
        let (mut width, mut height) = explicit_size
            .or_else(|| {
                std::env::var("VOLANT_WINDOW_SIZE")
                    .ok()
                    .and_then(|value| parse_size(&value))
            })
            .unwrap_or_else(crate::platform::startup_size);
        if let Some(display) = cx.primary_display() {
            let available = display.visible_bounds().size;
            width = width.min((available.width.as_f32() - 32.0).max(960.0));
            height = height.min((available.height.as_f32() - 64.0).max(560.0));
        }
        let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(960.0), px(560.0))),
            show: !background,
            focus: !background,
            ..TitleBar::window_options()
        };
        let opened = open_window(options, cx, |window, cx| {
            window.set_window_title("Volant");
            cx.new(|cx| VolantShell::new(demo, background, tray, tray_events, window, cx))
        })
        .expect("open Volant GPUI window");
        let view = opened.1;
        opened
            .0
            .update(cx, |_, window, cx| {
                let close_view = view.clone();
                window.on_window_should_close(cx, move |window, cx| {
                    let (quitting, stay) = {
                        let state = close_view.read(cx);
                        (
                            state.quitting,
                            state.settings.close_to_tray && state._tray.is_some(),
                        )
                    };
                    if quitting {
                        return true;
                    }
                    if stay {
                        crate::platform::hide_window(window);
                        false
                    } else {
                        close_view.update(cx, |this, _| this.quitting = true);
                        cx.quit();
                        true
                    }
                });
                view.update(cx, |this, cx| {
                if empty_demo {
                    this.rows.clear();
                    this.selected = None;
                    this.filter = Filter::All;
                }
                if let Some(mode) = theme_override {
                    this.settings.theme = mode;
                    theme::apply(cx, theme::resolve_mode(mode, window));
                }
                if select_failed
                    && let Some(row) = this.rows.iter().find(|row| row.status == Status::Failed)
                {
                    this.selected = Some(row.id);
                }
                if select_paused
                    && let Some(row) = this.rows.iter().find(|row| row.status == Status::Paused)
                {
                    this.selected = Some(row.id);
                }
                match screen.as_str() {
                    "settings" => {
                        this.open_settings_dialog(window, cx);
                        if settings_error
                            && let Some(settings) = &this.settings_dialog
                        {
                            settings.update(cx, |settings, cx| {
                                settings.seed_directory_error(window, cx)
                            });
                        }
                    }
                    "new" => {
                        this.open_add_dialog(window, cx);
                        if let Some(add) = &this.add_dialog {
                            let url = if add_state == "empty" {
                                ""
                            } else {
                                "https://cdnimage.debian.org/debian-cd/current/amd64/iso-cd/debian-13.2.0-amd64-netinst.iso"
                            };
                            add.update(cx, |add, cx| {
                                add.seed_capture_state(&add_state, url, window, cx)
                            });
                        }
                    }
                    _ => {}
                }
                cx.notify();
                });
            })
            .expect("configure Volant window");
        view.update(cx, |this, cx| {
            if let Some(url) = initial_add.clone() {
                this.queue_url(url);
                cx.notify();
            }
            if let Some(path) = initial_torrent.clone() {
                this.queue_torrent_file(&path);
                cx.notify();
            }
        });
        });
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn parse_size(value: &str) -> Option<(f32, f32)> {
    let (width, height) = value.split_once(['x', 'X'])?;
    Some((width.parse().ok()?, height.parse().ok()?))
}
