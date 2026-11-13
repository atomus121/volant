#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Volant 的窗口：eframe/glow 主线程 + 后台控制器线程。

mod capture;
mod clipboard;
mod control;
mod fmt;
mod log;
mod metrics;
mod model;
mod native;
mod selftest;
mod theme;
mod ui;

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

fn main() -> eframe::Result<()> {
    // 冷启动时间是「进程起来 → 第一帧画完」，所以从进程第一行就开始计时。
    let started = Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();

    // 这个分支必须在 eframe 之前 return：它没有窗口，也不该初始化 GL。
    if args.iter().any(|a| a == "--install-host") {
        let lines = match native::install() {
            Ok(lines) => lines,
            Err(e) => {
                eprintln!("安装失败：{e}");
                std::process::exit(2);
            }
        };
        for line in &lines {
            println!("{line}");
        }
        return Ok(());
    }

    let launch = ui::Launch::parse(args.into_iter());

    if let Some(url) = launch.selftest.clone() {
        let result = selftest::run(&url, launch.selftest_dir.clone());
        let text = match &result {
            Ok(log) => format!("OK\n{log}"),
            Err(e) => format!("FAILED\n{e}\n"),
        };
        let out = launch
            .selftest_out
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("target/selftest-result.txt"));
        if let Some(dir) = out.parent()
            && !dir.as_os_str().is_empty()
        {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&out, &text);
        if result.is_err() {
            std::process::exit(3);
        }
        return Ok(());
    }

    // 默认 1180×760 逻辑像素（IMPLEMENT §1.2）：本机工作区 1600×970，三栏齐活且居中舒服。
    // 早前那句「780×480，200% DPI 也不超出屏幕」是按逻辑 800×500 的误读写的。
    // 不要系统标题栏（白色，和近黑界面割裂），顶栏自己画。
    let size = launch.size.unwrap_or([1180.0, 760.0]);
    let viewport = viewport(size);

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let startup = Arc::new(AtomicU64::new(0));
    let ms = startup.clone();
    eframe::run_native(
        "Volant",
        options,
        Box::new(move |cc| Ok(Box::new(ui::App::new(cc, launch, started, ms)))),
    )
}

fn viewport(size: [f32; 2]) -> eframe::egui::ViewportBuilder {
    eframe::egui::ViewportBuilder::default()
        .with_title("Volant")
        .with_decorations(false)
        .with_inner_size(size)
        .with_min_inner_size([520.0, 340.0])
        .with_clamp_size_to_monitor_size(true)
        // 自绘窗口也必须显式接收 Explorer 的 OLE 文件投放；不要依赖后端默认值。
        .with_drag_and_drop(true)
}

#[cfg(test)]
mod tests {
    #[test]
    fn window_explicitly_accepts_file_drops() {
        assert_eq!(super::viewport([1180.0, 760.0]).drag_and_drop, Some(true));
    }
}
