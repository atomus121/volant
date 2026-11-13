//! Volant M0 · egui 路线原型
//!
//! 与 slint-proto 喂同一份 bench_model 数据、同一套视觉规格（颜色/行高/字号），
//! 唯一变量是 GUI 框架。`--bench` 自动滚动 5 秒后打印一行 RESULT 并退出。

use bench_model::{Frames, Kind, Row, Status, bench_secs, report, sample_rows};
use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Stroke, TextFormat, Vec2};
use std::io::Write;
use std::time::Instant;

// ---- 视觉规格（两条路线共用，改这里必须同步改 slint 的 .slint 常量） ----
const BG: Color32 = Color32::from_rgb(0x0B, 0x0C, 0x0E);
const BG_BAR: Color32 = Color32::from_rgb(0x0D, 0x0E, 0x11);
const HOVER: Color32 = Color32::from_rgb(0x16, 0x18, 0x1C);
const SEL: Color32 = Color32::from_rgb(0x1C, 0x20, 0x26);
const TRACK: Color32 = Color32::from_rgb(0x1A, 0x1D, 0x22);
const TEXT: Color32 = Color32::from_rgb(0xF2, 0xF3, 0xF5);
const MUTED: Color32 = Color32::from_rgb(0x8A, 0x8F, 0x98);
const DIM: Color32 = Color32::from_rgb(0x5B, 0x61, 0x6B);
const ACCENT: Color32 = Color32::from_rgb(0x4C, 0x8D, 0xFF);
const OK: Color32 = Color32::from_rgb(0x3D, 0xDC, 0x97);
const WARN: Color32 = Color32::from_rgb(0xFF, 0xB0, 0x20);
const ERR: Color32 = Color32::from_rgb(0xFF, 0x5C, 0x5C);

const ROW_H: f32 = 52.0;
const PAD_X: f32 = 20.0;
const COL_SIZE: f32 = 84.0;
const COL_SPEED: f32 = 96.0;
const F_NAME: f32 = 14.5;
const F_META: f32 = 13.0;

fn status_color(s: Status) -> Color32 {
    match s {
        Status::Active => ACCENT,
        Status::Paused => WARN,
        Status::Done => OK,
        Status::Failed => ERR,
    }
}

fn status_label(s: Status) -> &'static str {
    match s {
        Status::Active => "Active",
        Status::Paused => "Paused",
        Status::Done => "Done",
        Status::Failed => "Failed",
    }
}

struct App {
    rows: Vec<Row>,
    selected: Option<usize>,
    t0: Instant,
    started: bool,
    startup_ms: f64,
    bench: bool,
    bench_secs: f64,
    frames: Frames,
    last_frame: Instant,
    scroll: f32,
}

impl App {
    fn new(rows: Vec<Row>, t0: Instant, bench: bool) -> Self {
        Self {
            rows,
            selected: Some(1),
            t0,
            started: false,
            startup_ms: 0.0,
            bench,
            bench_secs: bench_secs(),
            frames: Frames::new(),
            last_frame: Instant::now(),
            scroll: 0.0,
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f64() * 1000.0;
        self.last_frame = now;
        if self.started {
            self.frames.record(dt);
        } else {
            self.started = true;
            self.startup_ms = self.t0.elapsed().as_secs_f64() * 1000.0;
        }

        if self.bench {
            self.scroll += 100.0;
            let max = (self.rows.len() as f32 * ROW_H) - 600.0;
            if self.scroll > max {
                self.scroll = 0.0;
            }
            if self.frames.elapsed_secs() >= self.bench_secs {
                report("egui", &self.frames, self.startup_ms);
                let _ = std::io::stdout().flush();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                std::process::exit(0);
            }
            ctx.request_repaint();
        }

        self.top_bar(ui);
        self.detail_panel(ui);
        self.list(ui);
    }
}

impl App {
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top")
            .exact_size(56.0)
            .frame(
                egui::Frame::NONE
                    .fill(BG_BAR)
                    .inner_margin(egui::Margin::symmetric(20, 0)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(
                        egui::RichText::new("Volant")
                            .size(16.5)
                            .strong()
                            .color(TEXT),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        for glyph in ["⚙", "＋"] {
                            let b = ui.add(
                                egui::Button::new(
                                    egui::RichText::new(glyph).size(17.0).color(MUTED),
                                )
                                .frame(false)
                                .min_size(Vec2::new(34.0, 34.0)),
                            );
                            if b.hovered() {
                                ui.painter().rect_filled(b.rect, 8.0, HOVER);
                            }
                        }
                        // 未读提示点（装饰性最小化：只是一个圆点）
                        let (r, _) = ui.allocate_exact_size(Vec2::new(8.0, 8.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 3.0, ACCENT);
                    });
                });
            });
    }

    fn list(&mut self, ui: &mut egui::Ui) {
        let bench = self.bench;
        let scroll = self.scroll;
        let rows_len = self.rows.len();
        let selected = self.selected;
        let rows = &self.rows;
        let mut new_sel = selected;

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG))
            .show(ui, |ui| {
                let mut sa = egui::ScrollArea::vertical().auto_shrink([false, false]);
                if bench {
                    sa = sa.vertical_scroll_offset(scroll);
                }
                sa.show_rows(ui, ROW_H, rows_len, |ui, range| {
                    for i in range {
                        draw_row(ui, i, &rows[i], selected == Some(i), &mut new_sel);
                    }
                });
            });

        if new_sel != selected {
            self.selected = new_sel;
        }
    }

    fn detail_panel(&mut self, ui: &mut egui::Ui) {
        let sel = self.selected.and_then(|i| self.rows.get(i));
        egui::Panel::bottom("detail")
            .exact_size(88.0)
            .frame(
                egui::Frame::NONE
                    .fill(BG_BAR)
                    .inner_margin(egui::Margin::symmetric(20, 14)),
            )
            .show(ui, |ui| match sel {
                Some(r) => {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&r.name).size(15.0).color(TEXT));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(status_label(r.status))
                                    .size(F_META)
                                    .color(status_color(r.status)),
                            );
                        });
                    });
                    ui.add_space(6.0);
                    let link = if r.kind == Kind::Torrent {
                        format!("{} peers", r.links)
                    } else {
                        format!("{} 连接", r.links)
                    };
                    let eta = r
                        .eta_secs()
                        .map(|s| format!("剩余 {}", bench_model::fmt_eta(s)))
                        .unwrap_or_else(|| "—".into());
                    let meta = format!(
                        "{} · {} · {} · {} · {}",
                        bench_model::fmt_bytes(r.size),
                        link,
                        r.country,
                        eta,
                        bench_model::fmt_speed(r.speed)
                    );
                    ui.label(egui::RichText::new(meta).size(F_META).color(MUTED));
                }
                None => {
                    ui.label(
                        egui::RichText::new("选择一项查看详情")
                            .size(F_META)
                            .color(DIM),
                    );
                }
            });
    }
}

fn draw_row(ui: &mut egui::Ui, idx: usize, r: &Row, selected: bool, new_sel: &mut Option<usize>) {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, ROW_H), Sense::click());
    let hovered = resp.hovered();

    let size_r = rect.right() - PAD_X;
    let speed_r = size_r - COL_SIZE;
    let name_l = rect.left() + PAD_X;
    let name_max = (speed_r - COL_SPEED) - name_l;
    let mid_y = rect.center().y - 1.0;

    // 背景
    {
        let p = ui.painter();
        if selected {
            p.rect_filled(rect, 0.0, SEL);
        } else if hovered {
            p.rect_filled(rect, 0.0, HOVER);
        }
        if selected {
            p.rect_filled(
                Rect::from_min_size(rect.left_top(), Vec2::new(2.0, ROW_H)),
                0.0,
                ACCENT,
            );
        }
    }

    // 文件名（超宽截断加省略号）
    let mut job = egui::text::LayoutJob::single_section(
        r.name.clone(),
        TextFormat {
            font_id: FontId::proportional(F_NAME),
            color: TEXT,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width: name_max,
        max_rows: 1,
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);

    let name_pos = egui::pos2(name_l, mid_y - galley.size().y / 2.0);
    ui.painter().galley(name_pos, galley, TEXT);

    // 速度
    let sp = if r.status == Status::Active {
        bench_model::fmt_speed(r.speed)
    } else {
        status_label(r.status).to_string()
    };
    ui.painter().text(
        egui::pos2(speed_r, mid_y),
        Align2::RIGHT_CENTER,
        sp,
        FontId::proportional(F_META),
        status_color(r.status),
    );

    // 大小
    ui.painter().text(
        egui::pos2(size_r, mid_y),
        Align2::RIGHT_CENTER,
        bench_model::fmt_bytes(r.size),
        FontId::proportional(F_META),
        MUTED,
    );

    // 底部 1.5px 进度线
    let bar_y = rect.bottom() - 1.0;
    let bar = Rect::from_min_max(egui::pos2(name_l, bar_y - 1.5), egui::pos2(size_r, bar_y));
    {
        let p = ui.painter();
        p.rect_filled(bar, 0.75, TRACK);
        let mut done = bar;
        done.set_width(bar.width() * r.frac());
        p.rect_filled(done, 0.75, status_color(r.status));
    }

    if resp.clicked() {
        *new_sel = Some(idx);
    }
}

fn main() -> eframe::Result<()> {
    let t0 = Instant::now();
    let bench = std::env::args().any(|a| a == "--bench");
    let shot = std::env::args().any(|a| a == "--shot");
    let rows = sample_rows(1000);
    // 截图/bench 模式：窗口小到能在 1600x1000@200% 屏上一屏放下，并置顶。
    let size = if shot || bench {
        [780.0, 470.0]
    } else {
        [980.0, 680.0]
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(size)
            .with_min_inner_size([720.0, 480.0])
            .with_position(egui::pos2(0.0, 0.0))
            .with_always_on_top()
            .with_title("Volant"),
        ..Default::default()
    };

    eframe::run_native(
        "Volant",
        options,
        Box::new(move |cc| {
            // 中文字形：egui 自带字体不含 CJK，必须挂系统字体，否则中文全是豆腐块。
            let mut fonts = egui::FontDefinitions::default();
            for path in [r"C:\Windows\Fonts\msyh.ttc", r"C:\Windows\Fonts\simhei.ttf"] {
                if let Ok(bytes) = std::fs::read(path) {
                    fonts.font_data.insert(
                        "cjk".to_owned(),
                        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
                    );
                    fonts
                        .families
                        .entry(egui::FontFamily::Proportional)
                        .or_default()
                        .insert(0, "cjk".to_owned());
                    fonts
                        .families
                        .entry(egui::FontFamily::Monospace)
                        .or_default()
                        .push("cjk".to_owned());
                    break;
                }
            }
            cc.egui_ctx.set_fonts(fonts);

            let mut v = egui::Visuals::dark();
            v.panel_fill = BG;
            v.window_fill = BG;
            v.extreme_bg_color = BG;
            v.override_text_color = Some(TEXT);
            v.widgets.noninteractive.bg_stroke = Stroke::NONE;
            v.widgets.inactive.bg_stroke = Stroke::NONE;
            v.widgets.hovered.bg_stroke = Stroke::NONE;
            v.selection.bg_fill = ACCENT.gamma_multiply(0.25);
            v.selection.stroke = Stroke::new(1.0, ACCENT);
            let mut style = (*cc.egui_ctx.style_of(egui::Theme::Dark)).clone();
            style.visuals = v;
            style.spacing.item_spacing = Vec2::new(8.0, 6.0);
            style.spacing.scroll.bar_width = 6.0;
            cc.egui_ctx.set_style_of(egui::Theme::Dark, style);
            cc.egui_ctx.set_theme(egui::Theme::Dark);

            Ok(Box::new(App::new(rows, t0, bench)))
        }),
    )
}
