//! M0 spike 共用件：假数据、格式化、内存与帧统计。
//!
//! 这个 crate 只服务于 Slint / egui 两条路线的同条件对比，**不进正式产品**。
//! 两条路线必须喂同一份数据、用同一套视觉规格，否则比较无效。

use std::time::Instant;

// ---------------------------------------------------------------- 数据模型

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Http,
    Torrent,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Active,
    Paused,
    Done,
    Failed,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub name: String,
    pub size: u64,
    pub done: u64,
    /// 字节/秒；暂停、完成、失败时为 0。
    pub speed: f64,
    pub kind: Kind,
    pub status: Status,
    /// HTTP: 连接数；Torrent: peer 数。
    pub links: u32,
    pub country: &'static str,
}

impl Row {
    pub fn frac(&self) -> f32 {
        if self.size == 0 {
            0.0
        } else {
            (self.done as f64 / self.size as f64).clamp(0.0, 1.0) as f32
        }
    }

    pub fn eta_secs(&self) -> Option<f64> {
        if self.speed <= 1.0 || self.status != Status::Active {
            return None;
        }
        let left = self.size.saturating_sub(self.done) as f64;
        Some(left / self.speed)
    }
}

// ---------------------------------------------------------------- 格式化

pub fn fmt_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

pub fn fmt_speed(bps: f64) -> String {
    if bps <= 1.0 {
        return "—".into();
    }
    format!("{}/s", fmt_bytes(bps as u64))
}

pub fn fmt_eta(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

// ---------------------------------------------------------------- 假数据

/// 确定性 LCG：不引入 rand，保证两条路线拿到完全一样的 1000 行。
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn pick(&mut self, n: usize) -> usize {
        (self.next() as usize) % n
    }
}

const NAMES: &[(&str, &str)] = &[
    ("ubuntu-24.04.1-desktop-amd64.iso", "GB"),
    ("debian-13.0.0-netinst-amd64.iso", "MB"),
    ("holiday-photos-2026.zip", "MB"),
    ("project-report-q4-final-v3.pdf", "MB"),
    ("rust-1.99.1-x86_64-pc-windows-gnu.msi", "MB"),
    ("open-data-2026.csv", "MB"),
    ("podcast-episode-142-44k.mp3", "MB"),
    ("big-buck-bunny-1080p.mkv", "GB"),
    ("blender-4.5.0-windows-x64.zip", "MB"),
    ("node-v24.21.0-win-x64.zip", "MB"),
    ("fedora-42-workstation.iso", "GB"),
    ("dataset-imagenet-mini.tar.gz", "GB"),
    ("libreoffice-25.8.0.msi", "MB"),
    ("screenshot-2026-11-10.png", "KB"),
    ("contract-signed.pdf", "KB"),
    ("archlinux-2026.11.01-x86_64.iso", "MB"),
];

const COUNTRIES: &[&str] = &["JP", "SG", "US", "DE", "NL", "KR"];

pub fn sample_rows(n: usize) -> Vec<Row> {
    let mut r = Lcg(0x5eed_1234_abcd_0001);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let (base, unit) = NAMES[i % NAMES.len()];
        // 同名文件加序号，模拟真实列表里的大量条目
        let name = if i < NAMES.len() {
            base.to_string()
        } else {
            let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
            let ext = base.rsplit_once('.').map(|(_, e)| e).unwrap_or("bin");
            format!("{stem}-{:02}.{ext}", i / NAMES.len())
        };
        let size = match unit {
            "KB" => (100 + r.pick(900)) as u64 * 1024,
            "MB" => (2 + r.pick(400)) as u64 * 1024 * 1024,
            _ => (1 + r.pick(8)) as u64 * 1024 * 1024 * 1024,
        };

        // 状态分布：15% 完成、65% 进行中、10% 暂停、10% 失败
        let roll = r.pick(100);
        let status = if roll < 15 {
            Status::Done
        } else if roll < 80 {
            Status::Active
        } else if roll < 90 {
            Status::Paused
        } else {
            Status::Failed
        };
        let frac = match status {
            Status::Done => 1.0,
            Status::Active => 0.02 + (r.pick(90) as f64) / 100.0,
            _ => (r.pick(80) as f64) / 100.0,
        };
        let done = (size as f64 * frac) as u64;
        let speed = if status == Status::Active {
            (0.3 + (r.pick(4_000) as f64) / 100.0) * 1024.0 * 1024.0
        } else {
            0.0
        };
        let kind = if i % 9 == 0 {
            Kind::Torrent
        } else {
            Kind::Http
        };
        out.push(Row {
            name,
            size,
            done,
            speed,
            kind,
            status,
            links: if kind == Kind::Torrent {
                6 + r.pick(26) as u32
            } else {
                1 + r.pick(16) as u32
            },
            country: COUNTRIES[r.pick(COUNTRIES.len())],
        });
    }
    out
}

// ---------------------------------------------------------------- 内存

#[derive(Debug, Clone, Copy, Default)]
pub struct Mem {
    pub working_set: u64,
    pub peak_working_set: u64,
    pub private: u64,
}

#[cfg(windows)]
pub fn mem() -> Mem {
    use core::mem::size_of;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut c: PROCESS_MEMORY_COUNTERS = unsafe { core::mem::zeroed() };
    c.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    if ok == 0 {
        return Mem::default();
    }
    Mem {
        working_set: c.WorkingSetSize as u64,
        peak_working_set: c.PeakWorkingSetSize as u64,
        private: c.PagefileUsage as u64,
    }
}

#[cfg(not(windows))]
pub fn mem() -> Mem {
    Mem::default()
}

/// 峰值 RSS，单位 MB，保留一位小数。
pub fn peak_mb() -> f64 {
    mem().peak_working_set as f64 / (1024.0 * 1024.0)
}

pub fn rss_mb() -> f64 {
    mem().working_set as f64 / (1024.0 * 1024.0)
}

// ---------------------------------------------------------------- 帧统计

pub struct Frames {
    started: Instant,
    pub count: u64,
    pub worst_ms: f64,
    pub sum_ms: f64,
    pub slow_frames: u64, // > 20 ms
}

impl Default for Frames {
    fn default() -> Self {
        Self::new()
    }
}

impl Frames {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            count: 0,
            worst_ms: 0.0,
            sum_ms: 0.0,
            slow_frames: 0,
        }
    }

    pub fn record(&mut self, frame: f64) {
        self.count += 1;
        self.sum_ms += frame;
        if frame > self.worst_ms {
            self.worst_ms = frame;
        }
        if frame > 20.0 {
            self.slow_frames += 1;
        }
    }

    pub fn avg_ms(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum_ms / self.count as f64
        }
    }

    pub fn elapsed_secs(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }
}

pub fn bench_secs() -> f64 {
    std::env::var("VOLANT_BENCH_SECS")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(5.0)
}

/// `--bench` 时打印一行可被脚本解析的结论然后退出。
pub fn report(ui: &str, frames: &Frames, startup_ms: f64) {
    let m = mem();
    let fps = if frames.avg_ms() > 0.0 {
        1000.0 / frames.avg_ms()
    } else {
        0.0
    };
    println!(
        "RESULT ui={ui} startup_ms={startup_ms:.1} frames={} avg_ms={:.2} fps={fps:.1} p_worst_ms={:.1} slow={} rss_mb={:.1} peak_mb={:.1} private_mb={:.1} rows=1000",
        frames.count,
        frames.avg_ms(),
        frames.worst_ms,
        frames.slow_frames,
        m.working_set as f64 / 1048576.0,
        m.peak_working_set as f64 / 1048576.0,
        m.private as f64 / 1048576.0,
    );
}
