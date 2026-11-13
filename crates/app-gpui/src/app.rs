use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::base::StyledExt;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, Theme, ThemeMode, TitleBar, WindowExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::{Input, InputState},
    progress::Progress,
    switch::Switch,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use volant_app_core::capture::{Capture, Listener};
use volant_app_core::clipboard::Watch;
use volant_app_core::control::{Cmd, Control, UiEvent};
use volant_app_core::fmt;
use volant_app_core::model::{
    Kind, Paths, Persisted, Row, Settings, Status, Store, ThemeMode as AppThemeMode, TorrentFile,
    friendly_error, is_magnet, writable_download_dir,
};
use volant_app_core::notify::{Notifier, UiNotifier};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Active,
    Done,
    Failed,
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
    Failed(String),
}

struct AddDownloadView {
    parent: WeakEntity<VolantShell>,
    input: Entity<InputState>,
    preview_id: u64,
    requested_url: Option<String>,
    preview: AddPreview,
}

impl AddDownloadView {
    fn new(parent: WeakEntity<VolantShell>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            parent,
            input: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("粘贴 HTTP / HTTPS 链接，例如 https://example.com/file.zip")
            }),
            preview_id: 0,
            requested_url: None,
            preview: AddPreview::Empty,
        }
    }

    fn request_preview(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let url = self.input.read(cx).value().trim().to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            self.preview = AddPreview::Failed("请输入有效的 HTTP 或 HTTPS 链接".into());
            cx.notify();
            return;
        }
        self.preview_id += 1;
        self.preview_id |= 1_u64 << 63;
        let preview_id = self.preview_id;
        self.requested_url = Some(url.clone());
        self.preview = AddPreview::Loading;
        let _ = self.parent.update(cx, |parent, cx| {
            parent.request_preview(preview_id, url);
            cx.notify();
        });
        cx.notify();
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

    fn set_preview_error(&mut self, id: u64, reason: String) {
        if id == self.preview_id {
            self.preview = AddPreview::Failed(friendly_error(&reason));
        }
    }

    fn submit(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.input.read(cx).value().trim().to_string();
        let ready = matches!(&self.preview, AddPreview::Ready { .. })
            && self.requested_url.as_deref() == Some(url.as_str());
        if !ready {
            self.preview = AddPreview::Failed("请先预览并确认文件信息".into());
            cx.notify();
            return;
        }
        let _ = self.parent.update(cx, |parent, cx| {
            parent.queue_url(url);
            cx.notify();
        });
        window.close_dialog(cx);
    }

    fn set_url(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(url, window, cx);
        });
        self.preview = AddPreview::Empty;
        self.requested_url = None;
        cx.notify();
    }
}

impl Render for AddDownloadView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;
        let border = theme.border;
        let secondary = theme.secondary;
        let url = self.input.read(cx).value().to_string();
        let can_submit = matches!(&self.preview, AddPreview::Ready { .. })
            && self.requested_url.as_deref() == Some(url.as_str());

        div()
            .v_flex()
            .gap_3()
            .w_full()
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(muted)
                    .child("链接会先探测文件名、大小和断点续传能力，确认后才加入队列。"),
            )
            .child(
                Input::new(&self.input)
                    .id("new-download-url")
                    .aria_label("下载链接")
                    .w_full(),
            )
            .child(
                div()
                    .p_4()
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(border)
                    .bg(secondary)
                    .v_flex()
                    .gap_1()
                    .child(match &self.preview {
                        AddPreview::Empty => div()
                            .text_size(px(12.0))
                            .text_color(muted)
                            .child("尚未预览")
                            .into_any_element(),
                        AddPreview::Loading => div()
                            .text_size(px(12.0))
                            .text_color(muted)
                            .child("正在读取文件信息…")
                            .into_any_element(),
                        AddPreview::Ready {
                            name,
                            size,
                            final_url,
                        } => div()
                            .v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_color(foreground)
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(name.clone()),
                            )
                            .child(div().text_size(px(11.0)).text_color(muted).child(format!(
                                "{} · {}",
                                size.map(fmt::bytes).unwrap_or_else(|| "大小未知".into()),
                                final_url
                            )))
                            .into_any_element(),
                        AddPreview::Failed(reason) => div()
                            .text_size(px(12.0))
                            .text_color(theme.danger)
                            .child(reason.clone())
                            .into_any_element(),
                    }),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("preview-download")
                            .secondary()
                            .label("预览")
                            .on_click(cx.listener(Self::request_preview)),
                    )
                    .child(
                        Button::new("confirm-download")
                            .primary()
                            .label("立即下载")
                            .disabled(!can_submit)
                            .on_click(cx.listener(Self::submit)),
                    ),
            )
    }
}

struct SettingsView {
    parent: WeakEntity<VolantShell>,
    settings: Settings,
    dir_input: Entity<InputState>,
    fallback_dir: std::path::PathBuf,
    notice: Option<String>,
}

impl SettingsView {
    fn new(
        parent: WeakEntity<VolantShell>,
        settings: Settings,
        fallback_dir: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let dir = settings.dir.to_string_lossy().to_string();
        Self {
            parent,
            settings: settings.clone(),
            dir_input: cx.new(|cx| InputState::new(window, cx).default_value(dir)),
            fallback_dir,
            notice: None,
        }
    }

    fn apply_cmd(&mut self, command: Cmd, cx: &mut Context<Self>) {
        let settings = self.settings.clone();
        let _ = self.parent.update(cx, |parent, cx| {
            parent.apply_settings(settings, Some(command));
            cx.notify();
        });
        cx.notify();
    }

    fn adjust_limit(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.settings.limit = (self.settings.limit as isize + delta).clamp(1, 20) as usize;
        self.apply_cmd(Cmd::SetLimit(self.settings.limit), cx);
    }

    fn adjust_connections(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.settings.connections =
            (self.settings.connections as isize + delta).clamp(1, 32) as usize;
        self.apply_cmd(Cmd::SetConnections(self.settings.connections), cx);
    }

    fn cycle_rate(&mut self, cx: &mut Context<Self>) {
        const RATES: [Option<u64>; 5] = [
            None,
            Some(1_048_576),
            Some(5_242_880),
            Some(10_485_760),
            Some(20_971_520),
        ];
        let current = RATES
            .iter()
            .position(|rate| *rate == self.settings.rate)
            .unwrap_or(0);
        self.settings.rate = RATES[(current + 1) % RATES.len()];
        self.apply_cmd(Cmd::SetRate(self.settings.rate), cx);
    }

    fn apply_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.dir_input.read(cx).value().trim().to_string();
        if value.is_empty() {
            self.notice = Some("下载目录不能为空".into());
            cx.notify();
            return;
        }
        let requested = std::path::PathBuf::from(value);
        let (dir, note) = writable_download_dir(&requested, &self.fallback_dir);
        self.settings.dir = dir.clone();
        let display = dir.to_string_lossy().to_string();
        self.dir_input.update(cx, |input, cx| {
            input.set_value(display, window, cx);
        });
        self.notice = note;
        self.apply_cmd(Cmd::SetDir(dir), cx);
    }

    fn toggle_clipboard(&mut self, checked: &bool, _: &mut Window, cx: &mut Context<Self>) {
        let enabled = *checked;
        let _ = self.parent.update(cx, |parent, cx| {
            parent.set_clipboard_enabled(enabled);
            let mut next = parent.settings.clone();
            next.clipboard = enabled;
            parent.settings = next;
            parent.mark_dirty();
            parent.save_now();
            cx.notify();
        });
        self.settings.clipboard = enabled;
        cx.notify();
    }

    fn set_theme(&mut self, mode: AppThemeMode, cx: &mut Context<Self>) {
        self.settings.theme = mode;
        let theme = match mode {
            AppThemeMode::System => cx.theme().mode,
            AppThemeMode::Light => ThemeMode::Light,
            AppThemeMode::Dark => ThemeMode::Dark,
        };
        let settings = self.settings.clone();
        let _ = self.parent.update(cx, |parent, cx| {
            parent.apply_settings(settings, None);
            Theme::change(theme, None, cx);
            cx.notify();
        });
        cx.notify();
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let settings = self.settings.clone();
        let rate_label = settings.rate.map_or_else(
            || "不限".to_string(),
            |rate| format!("{}/s", fmt::bytes(rate)),
        );

        div()
            .v_flex()
            .gap_4()
            .w_full()
            .child(setting_row(
                "同时下载",
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("limit-minus")
                            .ghost()
                            .compact()
                            .label("−")
                            .on_click(cx.listener(|this, _, _, cx| this.adjust_limit(-1, cx))),
                    )
                    .child(format!("{}", settings.limit))
                    .child(
                        Button::new("limit-plus")
                            .ghost()
                            .compact()
                            .label("+")
                            .on_click(cx.listener(|this, _, _, cx| this.adjust_limit(1, cx))),
                    )
                    .into_any_element(),
                border,
            ))
            .child(setting_row(
                "每任务连接数",
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("connections-minus")
                            .ghost()
                            .compact()
                            .label("−")
                            .on_click(
                                cx.listener(|this, _, _, cx| this.adjust_connections(-1, cx)),
                            ),
                    )
                    .child(format!("{}", settings.connections))
                    .child(
                        Button::new("connections-plus")
                            .ghost()
                            .compact()
                            .label("+")
                            .on_click(cx.listener(|this, _, _, cx| this.adjust_connections(1, cx))),
                    )
                    .into_any_element(),
                border,
            ))
            .child(setting_row(
                "全局限速",
                Button::new("rate-cycle")
                    .secondary()
                    .label(rate_label)
                    .on_click(cx.listener(|this, _, _, cx| this.cycle_rate(cx)))
                    .into_any_element(),
                border,
            ))
            .child(setting_row(
                "下载目录",
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(320.0))
                            .child(Input::new(&self.dir_input).w_full()),
                    )
                    .child(
                        Button::new("browse-dir")
                            .secondary()
                            .label("浏览")
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(path) =
                                    volant_app_core::native::pick_folder("选择下载目录")
                                {
                                    let value = path.to_string_lossy().to_string();
                                    this.dir_input.update(cx, |input, cx| {
                                        input.set_value(value, window, cx);
                                    });
                                    this.apply_dir(window, cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("apply-dir").secondary().label("应用").on_click(
                            cx.listener(|this, _, window, cx| this.apply_dir(window, cx)),
                        ),
                    )
                    .into_any_element(),
                border,
            ))
            .child(setting_row(
                "剪贴板监视",
                Switch::new("settings-clipboard")
                    .checked(settings.clipboard)
                    .on_change(cx.listener(Self::toggle_clipboard))
                    .into_any_element(),
                border,
            ))
            .child(setting_row(
                "主题",
                div()
                    .flex()
                    .gap_1()
                    .child(
                        Button::new("theme-system")
                            .ghost()
                            .selected(settings.theme == AppThemeMode::System)
                            .label("跟随系统")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_theme(AppThemeMode::System, cx)
                            })),
                    )
                    .child(
                        Button::new("theme-light")
                            .ghost()
                            .selected(settings.theme == AppThemeMode::Light)
                            .label("浅色")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_theme(AppThemeMode::Light, cx)
                            })),
                    )
                    .child(
                        Button::new("theme-dark")
                            .ghost()
                            .selected(settings.theme == AppThemeMode::Dark)
                            .label("深色")
                            .on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.set_theme(AppThemeMode::Dark, cx)
                                }),
                            ),
                    )
                    .into_any_element(),
                border,
            ))
            .when_some(self.notice.clone(), |this, notice| {
                this.child(div().text_size(px(12.0)).text_color(muted).child(notice))
            })
    }
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
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let all_checked = self.checked.iter().all(|checked| *checked);
        let selected_count = self.checked.iter().filter(|checked| **checked).count();
        let selected_size: u64 = self
            .files
            .iter()
            .zip(&self.checked)
            .filter(|(_, checked)| **checked)
            .map(|(file, _)| file.len)
            .sum();
        let mut rows = Vec::new();
        for (index, file) in self.files.iter().enumerate() {
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        Checkbox::new(("torrent-file", index))
                            .label(file.name.clone())
                            .checked(self.checked[index])
                            .on_change(cx.listener(move |this, checked, _, cx| {
                                this.checked[index] = *checked;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(muted)
                            .child(fmt::bytes(file.len)),
                    ),
            );
        }

        div()
            .v_flex()
            .gap_3()
            .w_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(12.0)).text_color(muted).child(format!(
                        "已选 {selected_count}/{} 个文件 · {}",
                        self.files.len(),
                        fmt::bytes(selected_size)
                    )))
                    .child(
                        Button::new("toggle-all-files")
                            .ghost()
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
                    .id("torrent-file-list")
                    .max_h(px(340.0))
                    .overflow_y_scroll()
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

fn setting_row(label: &str, control: AnyElement, border: Hsla) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .pb_3()
        .border_b_1()
        .border_color(border)
        .child(label.to_string())
        .child(control)
}

struct VolantShell {
    home: String,
    torrents_dir: std::path::PathBuf,
    rows: Vec<Row>,
    filter: Filter,
    selected: Option<u64>,
    settings: Settings,
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
    demo: bool,
    _events_task: Option<Task<()>>,
}

impl VolantShell {
    fn new(demo: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let paths = Paths::resolve();
        volant_app_core::log::init(&paths.home);
        let store = Store::new(paths.state.clone());
        let mut saved = store.load();
        let fallback = paths.home.join("downloads");
        let (download_dir, dir_note) = writable_download_dir(&saved.settings.dir, &fallback);
        saved.settings.dir = download_dir;
        saved.settings.normalize();

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
                        this.save_if_due();
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

        let selected = rows.first().map(|row| row.id);
        Self {
            home: paths.home.display().to_string(),
            torrents_dir: paths.torrents,
            rows,
            filter: Filter::All,
            selected,
            settings: saved.settings,
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
            notice: dir_note,
            demo,
            _events_task: events_task,
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

    fn request_preview(&mut self, id: u64, url: String) {
        if let Some(control) = &self.control {
            control.send(Cmd::PreviewHttp {
                id,
                url,
                headers: Vec::new(),
            });
        }
    }

    fn open_add_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let parent = cx.weak_entity();
        let view = cx.new(|cx| AddDownloadView::new(parent, window, cx));
        self.add_dialog = Some(view.clone());
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("新建下载")
                .w(px(560.0))
                .centered()
                .child(view.clone())
        });
    }

    fn open_add_dialog_with_url(
        &mut self,
        url: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.add_dialog.is_none() {
            self.open_add_dialog(window, cx);
        }
        if let Some(view) = &self.add_dialog {
            view.update(cx, |view, cx| view.set_url(url, window, cx));
        }
    }

    fn handle_clipboard_url(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.clipboard {
            self.open_add_dialog_with_url(url, window, cx);
            self.notice = Some("已从剪贴板识别到链接".into());
        }
    }

    fn handle_capture(&mut self, capture: Capture, _window: &mut Window, _cx: &mut Context<Self>) {
        if capture.url.is_empty() {
            return;
        }
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
        if self.queue_url_with_headers(capture.url, headers) {
            self.notice = Some("已接收浏览器下载请求".into());
        }
    }

    fn handle_external_paths(
        &mut self,
        paths: &ExternalPaths,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut accepted = 0;
        for path in paths.paths() {
            if !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("torrent"))
            {
                continue;
            }
            let id = self.next_id;
            self.next_id += 1;
            let target = self.torrents_dir.join(format!("{id}.torrent"));
            if let Some(parent) = target.parent()
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                self.notice = Some(format!("无法保存种子副本：{error}"));
                continue;
            }
            if let Err(error) = std::fs::copy(path, &target) {
                self.notice = Some(format!("无法读取种子文件：{error}"));
                continue;
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
            accepted += 1;
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

    fn open_settings_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let parent = cx.weak_entity();
        let fallback = std::path::PathBuf::from(&self.home).join("downloads");
        let view =
            cx.new(|cx| SettingsView::new(parent, self.settings.clone(), fallback, window, cx));
        self.settings_dialog = Some(view.clone());
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("设置")
                .w(px(680.0))
                .centered()
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
                .w(px(680.0))
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

    fn apply_settings(&mut self, settings: Settings, command: Option<Cmd>) {
        self.settings = settings;
        if let (Some(control), Some(command)) = (&self.control, command) {
            control.send(command);
        }
        self.mark_dirty();
        self.save_now();
    }

    fn set_clipboard_enabled(&mut self, enabled: bool) {
        if let Some(watch) = &self._clip {
            watch.set_enabled(enabled);
        }
    }

    fn confirm_delete(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.iter().find(|row| row.id == id).cloned() else {
            return;
        };
        let parent = cx.weak_entity();
        let name = row.display_name().to_string();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let parent = parent.clone();
            alert
                .title("删除任务？")
                .description(format!("“{name}”将从列表移除，已下载的文件会保留。"))
                .confirm()
                .show_cancel(true)
                .ok_text("删除")
                .cancel_text("取消")
                .on_ok(move |_, _, cx| {
                    let _ = parent.update(cx, |this, cx| {
                        this.delete_row(id);
                        cx.notify();
                    });
                    true
                })
        });
    }

    fn delete_row(&mut self, id: u64) {
        let Some(row) = self.rows.iter().find(|row| row.id == id).cloned() else {
            return;
        };
        if let Some(control) = &self.control {
            if row.kind == Kind::Torrent {
                control.send(Cmd::RemoveTorrent {
                    id,
                    delete_files: false,
                });
            } else {
                control.send(Cmd::Remove(id));
            }
        }
        self.rows.retain(|row| row.id != id);
        if self.selected == Some(id) {
            self.selected = self.rows.first().map(|row| row.id);
        }
        self.notice = Some("任务已从列表移除".into());
        self.mark_dirty();
        self.save_now();
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
                self.rows.retain(|row| row.id != id);
                if self.selected == Some(id) {
                    self.selected = self.rows.first().map(|row| row.id);
                }
                self.mark_dirty();
            }
            UiEvent::TorrentReady {
                id,
                name,
                info_hash,
                total,
                files,
            } => {
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

    fn save_now(&mut self) {
        let Some(store) = &self.store else {
            self.dirty = false;
            return;
        };
        let persisted = Persisted {
            version: 1,
            next_id: self.next_id,
            settings: self.settings.clone(),
            rows: self.rows.clone(),
        };
        if let Err(error) = store.save(&persisted) {
            self.notice = Some(format!("保存状态失败：{error}"));
        }
        self.dirty = false;
        self.last_save = Instant::now();
    }

    fn visible_rows(&self) -> Vec<Row> {
        self.rows
            .iter()
            .filter(|row| self.filter.matches(row))
            .cloned()
            .collect()
    }

    fn filter_button(&self, filter: Filter, cx: &mut Context<Self>) -> Button {
        let count = self.rows.iter().filter(|row| filter.matches(row)).count();
        Button::new(ElementId::Name(format!("filter-{}", filter.key()).into()))
            .ghost()
            .selected(self.filter == filter)
            .label(format!("{}  {}", filter.label(), count))
            .w_full()
            .justify_start()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.filter = filter;
                this.selected = this
                    .rows
                    .iter()
                    .find(|row| filter.matches(row))
                    .map(|row| row.id);
                cx.notify();
            }))
    }

    fn transfer_row(&self, row: Row, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let selected = self.selected == Some(row.id);
        let accent = status_color(row.status, theme);
        let background = if selected {
            theme.secondary
        } else {
            theme.background
        };
        let border = if selected { theme.accent } else { theme.border };
        let muted = theme.muted_foreground;
        let foreground = theme.foreground;
        let icon = match row.status {
            Status::Done => IconName::Check,
            Status::Failed => IconName::CircleX,
            Status::Paused => IconName::CircleCheck,
            _ => IconName::ArrowDown,
        };
        let id = row.id;
        let note = row.note();
        let percent = fmt::percent(row.downloaded, row.size);
        let size = if row.size == 0 {
            "—".to_string()
        } else {
            fmt::bytes(row.size)
        };
        let can_toggle = matches!(
            row.status,
            Status::Active | Status::Seeding | Status::Paused | Status::Queued | Status::Probing
        );
        let action_label = if row.status == Status::Paused {
            "继续"
        } else {
            "暂停"
        };
        let action = can_toggle.then(|| {
            Button::new(ElementId::Name(format!("toggle-{id}").into()))
                .ghost()
                .compact()
                .label(action_label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle(id, cx);
                }))
        });
        let delete = Button::new(ElementId::Name(format!("delete-{id}").into()))
            .ghost()
            .compact()
            .label("删除")
            .on_click(cx.listener(move |this, _, window, cx| {
                this.confirm_delete(id, window, cx);
            }));

        div()
            .id(ElementId::Name(format!("transfer-row-{id}").into()))
            .flex()
            .items_center()
            .gap_3()
            .h(px(78.0))
            .px_4()
            .rounded(px(10.0))
            .border_1()
            .border_color(border)
            .bg(background)
            .cursor_pointer()
            .child(
                div()
                    .size(px(34.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.0))
                    .bg(accent.opacity(0.14))
                    .child(Icon::new(icon).text_color(accent)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(foreground)
                            .child(row.display_name().to_string()),
                    )
                    .child(
                        div()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(12.0))
                            .text_color(muted)
                            .child(note),
                    ),
            )
            .child(
                div()
                    .w(px(118.0))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        Progress::new(ElementId::Name(format!("progress-{id}").into()))
                            .value(row.frac() * 100.0)
                            .color(accent)
                            .w_full(),
                    )
                    .child(div().text_size(px(11.0)).text_color(muted).child(percent)),
            )
            .child(
                div()
                    .w(px(82.0))
                    .text_size(px(12.0))
                    .text_color(foreground)
                    .child(size),
            )
            .child(
                div()
                    .min_w(px(72.0))
                    .px_2()
                    .py_1()
                    .rounded(px(999.0))
                    .bg(accent.opacity(0.12))
                    .text_size(px(11.0))
                    .text_color(accent)
                    .child(row.status.label()),
            )
            .when_some(action, |this, action| this.child(action))
            .child(delete)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(id);
                cx.notify();
            }))
    }
}

impl Render for VolantShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let compact = window.viewport_size().width.as_f32() < 1000.0;
        let sidebar_width = if compact { 190.0 } else { 224.0 };
        let background = cx.theme().background;
        let foreground = cx.theme().foreground;
        let sidebar = cx.theme().sidebar;
        let sidebar_border = cx.theme().sidebar_border;
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let secondary = cx.theme().secondary;

        let mut filters = Vec::new();
        for filter in Filter::ALL {
            filters.push(self.filter_button(filter, cx));
        }

        let rows = self.visible_rows();
        let mut row_elements = Vec::new();
        for row in &rows {
            row_elements.push(self.transfer_row(row.clone(), cx));
        }

        let selected = self
            .selected
            .and_then(|id| self.rows.iter().find(|row| row.id == id))
            .cloned();
        let total_speed = fmt::speed(
            rows.iter()
                .filter(|row| row.status == Status::Active)
                .map(|row| row.speed)
                .sum(),
        );
        let connections = rows.iter().map(|row| row.connections).sum::<usize>();
        let remaining = fmt::bytes(
            rows.iter()
                .map(|row| row.size.saturating_sub(row.downloaded))
                .sum(),
        );
        let notice = self.notice.clone();
        let demo = self.demo;

        div()
            .v_flex()
            .size_full()
            .bg(background)
            .text_color(foreground)
            .on_drop::<ExternalPaths>(cx.listener(|this, paths, window, cx| {
                this.handle_external_paths(paths, window, cx);
            }))
            .child(
                TitleBar::new()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .size(px(20.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(5.0))
                                    .bg(foreground)
                                    .text_color(background)
                                    .child("V"),
                            )
                            .child(div().text_size(px(12.5)).child("Volant")),
                    )
                    .child(
                        Button::new("new-download")
                            .secondary()
                            .compact()
                            .label("新建下载")
                            .icon(IconName::Plus)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_add_dialog(window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(
                        div()
                            .w(px(sidebar_width))
                            .h_full()
                            .flex()
                            .flex_col()
                            .justify_between()
                            .p_3()
                            .bg(sidebar)
                            .border_r_1()
                            .border_color(sidebar_border)
                            .child(
                                div()
                                    .v_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .px_2()
                                            .pb_2()
                                            .text_size(px(11.0))
                                            .text_color(muted)
                                            .child("队列"),
                                    )
                                    .children(filters),
                            )
                            .child(
                                div()
                                    .v_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .p_3()
                                            .rounded(px(8.0))
                                            .bg(secondary)
                                            .text_size(px(11.0))
                                            .text_color(muted)
                                            .child(format!("配置 {}", self.home)),
                                    )
                                    .child(
                                        Button::new("settings")
                                            .ghost()
                                            .label("设置")
                                            .w_full()
                                            .justify_start()
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.open_settings_dialog(window, cx);
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .h_full()
                            .flex()
                            .flex_col()
                            .p_6()
                            .gap_4()
                            .child(
                                div()
                                    .v_flex()
                                    .gap_1()
                                    .child(div().text_size(px(26.0)).child(self.filter.label()))
                                    .child(div().text_size(px(12.0)).text_color(muted).child(
                                        format!(
                                            "{} 个任务 · GPUI Kit{}{}",
                                            rows.len(),
                                            if demo { " · Demo" } else { "" },
                                            " · 真实下载引擎"
                                        ),
                                    )),
                            )
                            .when_some(notice, |this, notice| {
                                this.child(
                                    div()
                                        .px_3()
                                        .py_2()
                                        .rounded(px(8.0))
                                        .bg(secondary)
                                        .text_size(px(12.0))
                                        .text_color(muted)
                                        .child(notice),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .gap_3()
                                    .child(summary_card(
                                        "当前速度",
                                        &total_speed,
                                        background,
                                        border,
                                        foreground,
                                        muted,
                                    ))
                                    .child(summary_card(
                                        "活动连接",
                                        &connections.to_string(),
                                        background,
                                        border,
                                        foreground,
                                        muted,
                                    ))
                                    .child(summary_card(
                                        "剩余", &remaining, background, border, foreground, muted,
                                    )),
                            )
                            .child(
                                div()
                                    .v_flex()
                                    .flex_1()
                                    .min_h(px(0.0))
                                    .gap_2()
                                    .overflow_hidden()
                                    .children(row_elements),
                            ),
                    )
                    .when(!compact, |this| {
                        this.child(detail_panel(
                            selected.as_ref(),
                            secondary,
                            sidebar_border,
                            foreground,
                            muted,
                            cx,
                        ))
                    }),
            )
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

fn summary_card(
    label: &str,
    value: &str,
    background: Hsla,
    border: Hsla,
    foreground: Hsla,
    muted: Hsla,
) -> Div {
    div()
        .flex_1()
        .min_w(px(0.0))
        .overflow_hidden()
        .p_4()
        .rounded(px(10.0))
        .border_1()
        .border_color(border)
        .bg(background)
        .v_flex()
        .gap_2()
        .child(
            div()
                .text_size(px(11.0))
                .text_color(muted)
                .whitespace_nowrap()
                .text_ellipsis()
                .child(label.to_string()),
        )
        .child(
            div()
                .text_size(px(18.0))
                .text_color(foreground)
                .whitespace_nowrap()
                .text_ellipsis()
                .child(value.to_string()),
        )
}

fn detail_panel(
    row: Option<&Row>,
    background: Hsla,
    border: Hsla,
    foreground: Hsla,
    muted: Hsla,
    cx: &mut Context<VolantShell>,
) -> Div {
    let accent = row.map_or(muted, |row| status_color(row.status, cx.theme()));
    let title = row.map_or("选择一个任务", Row::display_name);
    let status = row.map_or("暂无详情", |row| row.status.label());
    let note = row.map_or_else(|| "从左侧列表选择任务。".to_string(), Row::note);
    let size = row.map_or_else(
        || "—".to_string(),
        |row| {
            if row.size == 0 {
                "—".to_string()
            } else {
                fmt::bytes(row.size)
            }
        },
    );

    div()
        .w(px(320.0))
        .h_full()
        .flex()
        .flex_col()
        .p_5()
        .gap_4()
        .bg(background)
        .border_l_1()
        .border_color(border)
        .child(
            div()
                .text_size(px(11.0))
                .text_color(muted)
                .child("任务详情"),
        )
        .child(
            div()
                .v_flex()
                .gap_2()
                .child(div().text_size(px(18.0)).child(title.to_string()))
                .child(
                    div()
                        .self_start()
                        .px_2()
                        .py_1()
                        .rounded(px(999.0))
                        .bg(accent.opacity(0.12))
                        .text_size(px(11.0))
                        .text_color(accent)
                        .child(status),
                )
                .child(div().text_size(px(12.0)).text_color(muted).child(note)),
        )
        .child(
            div()
                .v_flex()
                .gap_2()
                .child(detail_kv("大小", &size, foreground, muted))
                .child(detail_kv(
                    "类型",
                    row.map_or("—", |row| {
                        if row.kind == Kind::Torrent {
                            "BitTorrent"
                        } else {
                            "HTTP 分段下载"
                        }
                    }),
                    foreground,
                    muted,
                )),
        )
        .child(
            Button::new("open-folder")
                .ghost()
                .label("打开下载目录")
                .w_full()
                .justify_start(),
        )
}

fn detail_kv(label: &str, value: &str, foreground: Hsla, muted: Hsla) -> Div {
    div()
        .flex()
        .justify_between()
        .gap_3()
        .child(
            div()
                .text_size(px(12.0))
                .text_color(muted)
                .child(label.to_string()),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(foreground)
                .child(value.to_string()),
        )
}

fn status_color(status: Status, theme: &Theme) -> Hsla {
    match status {
        Status::Active | Status::Probing | Status::Queued => theme.info,
        Status::Done | Status::Seeding => theme.success,
        Status::Paused => theme.warning,
        Status::Failed => theme.danger,
    }
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
        rows.push(row);
    };
    push(
        1,
        "debian-13.2.0-amd64-netinst.iso",
        Status::Active,
        682_000_000,
        421_236_736,
        18_700_000.0,
    );
    push(
        2,
        "openai-paper-collection.zip",
        Status::Queued,
        128_000_000,
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
    push(
        6,
        "debian-13.2.0-amd64-netinst.iso",
        Status::Seeding,
        682_000_000,
        682_000_000,
        0.0,
    );
    rows
}

pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let demo = args.iter().any(|arg| arg == "--demo");
    let initial_add = args
        .iter()
        .position(|arg| arg == "--add")
        .and_then(|index| args.get(index + 1))
        .cloned();

    application().with_assets(assets::Assets).run(move |cx| {
        init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        let (width, height) = crate::platform::startup_size();
        let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(520.0), px(340.0))),
            ..TitleBar::window_options()
        };
        open_window(options, cx, |window, cx| {
            window.set_window_title("Volant");
            let view = cx.new(|cx| VolantShell::new(demo, window, cx));
            if let Some(url) = initial_add.clone() {
                view.update(cx, |this, cx| {
                    this.queue_url(url);
                    cx.notify();
                });
            }
            view
        })
        .expect("open Volant GPUI window");
    });
}
