//! 资源指标采样（PRD 6.2）。只在 `--metrics-out` 模式下用。
//!
//! 采样放在独立线程里，不往界面线程要重绘——否则「空闲 CPU」量出来的就是
//! 我们自己为了采样而画的那几帧。

use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub struct Sampler {
    last: Option<(Instant, u64)>,
}

impl Sampler {
    pub fn new() -> Sampler {
        Sampler { last: None }
    }

    /// 返回 (工作集 MB, 距上次采样的单核占用百分比)。
    pub fn sample(&mut self) -> (f64, f64) {
        let rss = rss_mb();
        let now = Instant::now();
        let cpu = cpu_ticks();
        let pct = match self.last {
            Some((t, c)) => {
                let dt = now.duration_since(t).as_secs_f64();
                if dt > 0.05 {
                    (cpu.saturating_sub(c)) as f64 / (dt * 1e7) * 100.0
                } else {
                    0.0
                }
            }
            None => 0.0,
        };
        self.last = Some((now, cpu));
        (rss, pct)
    }
}

/// 采样若干秒，把结果写进 `out`，然后关窗。
pub fn watch(
    ctx: egui::Context,
    startup_ms: Arc<AtomicU64>,
    frames: Arc<AtomicU64>,
    secs: f64,
    out: PathBuf,
) {
    let mut s = Sampler::new();
    let t0 = Instant::now();
    // 启动瞬间的分配和首帧布局不算「空闲」，前 3 秒只用来取峰值。
    let warmup = 3.0;
    let (mut idle_rss, mut cpu_max, mut peak_rss) = (0.0f64, 0.0f64, 0.0f64);
    let (mut cpu_sum, mut cpu_n) = (0.0f64, 0u32);
    while t0.elapsed().as_secs_f64() < secs {
        std::thread::sleep(Duration::from_millis(250));
        let (rss, pct) = s.sample();
        peak_rss = peak_rss.max(rss);
        if t0.elapsed().as_secs_f64() > warmup {
            idle_rss = idle_rss.max(rss);
            cpu_max = cpu_max.max(pct);
            cpu_sum += pct;
            cpu_n += 1;
        }
    }
    let frames_drawn = frames.load(Ordering::SeqCst);
    let fps = frames_drawn as f64 / t0.elapsed().as_secs_f64();
    let cpu_avg = if cpu_n == 0 {
        0.0
    } else {
        cpu_sum / cpu_n as f64
    };
    let json = format!(
        "{{\"startup_ms\":{},\"idle_rss_mb\":{idle_rss:.1},\"peak_rss_mb\":{peak_rss:.1},\"cpu_max_pct\":{cpu_max:.3},\"cpu_avg_pct\":{cpu_avg:.3},\"frames\":{frames_drawn},\"fps\":{fps:.2},\"seconds\":{secs:.1}}}",
        startup_ms.load(Ordering::SeqCst)
    );
    if let Some(dir) = out.parent()
        && !dir.as_os_str().is_empty()
    {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&out, &json);
    ctx.request_repaint();
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
}

#[cfg(windows)]
fn rss_mb() -> f64 {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let mut c: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) == 0 {
            return 0.0;
        }
        c.WorkingSetSize as f64 / (1024.0 * 1024.0)
    }
}

#[cfg(windows)]
fn cpu_ticks() -> u64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    unsafe {
        let mut created: FILETIME = std::mem::zeroed();
        let mut exited: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        if GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        ) == 0
        {
            return 0;
        }
        let k = ((kernel.dwHighDateTime as u64) << 32) | kernel.dwLowDateTime as u64;
        let u = ((user.dwHighDateTime as u64) << 32) | user.dwLowDateTime as u64;
        k + u
    }
}

#[cfg(not(windows))]
fn rss_mb() -> f64 {
    0.0
}

#[cfg(not(windows))]
fn cpu_ticks() -> u64 {
    0
}
