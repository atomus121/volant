use gpui_kit::base::StyledExt;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Selectable, Theme, ThemeMode,
    button::{Button, ButtonVariants},
    progress::Progress,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use volant_app_core::fmt;
use volant_app_core::model::{Kind, Paths, Row, Status};

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

struct VolantShell {
    home: String,
    rows: Vec<Row>,
    filter: Filter,
    selected: Option<u64>,
}

impl VolantShell {
    fn new(home: String) -> Self {
        let rows = sample_rows();
        let selected = rows.first().map(|row| row.id);
        Self {
            home,
            rows,
            filter: Filter::All,
            selected,
        }
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

        div()
            .v_flex()
            .size_full()
            .bg(background)
            .text_color(foreground)
            .child(
                div()
                    .h(px(56.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_5()
                    .bg(sidebar)
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .size(px(22.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(6.0))
                                    .bg(foreground)
                                    .text_color(background)
                                    .child("V"),
                            )
                            .child("Volant"),
                    )
                    .child(
                        Button::new("new-download")
                            .primary()
                            .label("新建下载")
                            .icon(IconName::Plus)
                            .on_click(|_, _, _| {}),
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
                                            .justify_start(),
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
                                        format!("{} 个任务 · GPUI Kit 组件风格", rows.len()),
                                    )),
                            )
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

fn main() {
    let home = Paths::resolve().home.display().to_string();

    application().with_assets(assets::Assets).run(move |cx| {
        init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        // This development display runs at 200% scaling. A 760x460 logical
        // window fits the physical viewport; platform-aware DPI sizing lands later.
        let bounds = Bounds::centered(None, size(px(760.0), px(460.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(520.0), px(340.0))),
            ..Default::default()
        };
        open_window(options, cx, |window, cx| {
            window.set_window_title("Volant");
            cx.new(|_| VolantShell::new(home))
        })
        .expect("open Volant GPUI window");
    });
}
