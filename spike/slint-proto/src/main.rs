//! Volant M0 · Slint 路线原型
//!
//! 与 egui-proto 喂同一份 bench_model 数据、同一套视觉规格，唯一变量是 GUI 框架。

slint::include_modules!();

use bench_model::{Frames, Kind, Row, Status, bench_secs, report, sample_rows};
use slint::{Color, ModelRc, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::time::{Duration, Instant};

fn col(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb_u8(r, g, b)
}

fn status_color(s: Status) -> Color {
    match s {
        Status::Active => col(0x4C, 0x8D, 0xFF),
        Status::Paused => col(0xFF, 0xB0, 0x20),
        Status::Done => col(0x3D, 0xDC, 0x97),
        Status::Failed => col(0xFF, 0x5C, 0x5C),
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

fn to_item(r: &Row) -> RowItem {
    let speed = if r.status == Status::Active {
        bench_model::fmt_speed(r.speed)
    } else {
        status_label(r.status).to_string()
    };
    RowItem {
        name: r.name.clone().into(),
        size_text: bench_model::fmt_bytes(r.size).into(),
        speed_text: speed.into(),
        speed_color: status_color(r.status),
        progress: r.frac(),
        bar_color: status_color(r.status),
    }
}

fn set_detail(ui: &MainWindow, rows: &[Row], idx: i32) {
    match rows.get(idx.max(0) as usize) {
        Some(r) => {
            let link = if r.kind == Kind::Torrent {
                format!("{} peers", r.links)
            } else {
                format!("{} conns", r.links)
            };
            let eta = r
                .eta_secs()
                .map(|s| format!("ETA {}", bench_model::fmt_eta(s)))
                .unwrap_or_else(|| "—".into());
            ui.set_detail_name(r.name.clone().into());
            ui.set_detail_meta(
                format!(
                    "{} · {} · {} · {} · {}",
                    bench_model::fmt_bytes(r.size),
                    link,
                    r.country,
                    eta,
                    bench_model::fmt_speed(r.speed)
                )
                .into(),
            );
            ui.set_detail_status(status_label(r.status).into());
            ui.set_detail_status_color(status_color(r.status));
        }
        None => {
            ui.set_detail_name("".into());
            ui.set_detail_meta("Select an item".into());
            ui.set_detail_status("".into());
        }
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let t0 = Instant::now();
    let bench = std::env::args().any(|a| a == "--bench");
    let shot = std::env::args().any(|a| a == "--shot");
    let rows = sample_rows(1000);

    let ui = MainWindow::new()?;
    if shot {
        // 截图模式：物理像素 1560x940，保证在 1600x1000 的屏上一屏放下
        ui.window().set_size(slint::PhysicalSize::new(1560, 940));
        ui.window().set_position(slint::PhysicalPosition::new(0, 0));
    }
    let items: Vec<RowItem> = rows.iter().map(to_item).collect();
    ui.set_rows(ModelRc::new(VecModel::from(items)));
    ui.set_selected(1);
    set_detail(&ui, &rows, 1);

    let weak = ui.as_weak();
    let rows_for_cb = rows.clone();
    ui.on_pick(move |idx| {
        if let Some(ui) = weak.upgrade() {
            set_detail(&ui, &rows_for_cb, idx);
        }
    });

    let startup_ms = t0.elapsed().as_secs_f64() * 1000.0;

    if !bench {
        return ui.run();
    }

    // ---- bench：每 16ms 滚 100px，5 秒后报一行 RESULT ----
    let frames = Rc::new(RefCell::new(Frames::new()));
    let last = Rc::new(RefCell::new(Instant::now()));
    let scroll = Rc::new(RefCell::new(0.0f32));
    let secs = bench_secs();
    let weak = ui.as_weak();
    let timer = Timer::default();

    {
        let frames = frames.clone();
        let scroll = scroll.clone();
        timer.start(TimerMode::Repeated, Duration::from_millis(16), move || {
            let now = Instant::now();
            let dt = now.duration_since(*last.borrow()).as_secs_f64() * 1000.0;
            *last.borrow_mut() = now;
            let mut f = frames.borrow_mut();
            f.record(dt);

            let mut s = scroll.borrow_mut();
            *s += 100.0;
            if *s > 52_000.0 {
                *s = 0.0;
            }
            if let Some(ui) = weak.upgrade() {
                ui.set_scroll_y(*s);
            }

            if f.elapsed_secs() >= secs {
                report("slint", &f, startup_ms);
                let _ = std::io::stdout().flush();
                std::process::exit(0);
            }
        });
    }

    ui.run()?;
    drop(timer);
    Ok(())
}
