//! `--trace` 的诊断计数：只回答「CPU 花在哪」，默认关闭。
//!
//! M4 测出 1 Gbps 下载要 46% 的单核，比 curl 高一倍多，但引擎里每一段代码看上去都
//! 没有问题。猜不下去的时候就把事实量出来：收块多大、每块收多久、限速真的睡了多少次、
//! 写盘占多少。全部是 `Relaxed` 原子加，开销是一两次 predict 得住的分支。
//!
//! 这不是常驻功能，别把它接到 UI 上；`volant-cli get --trace` 是唯一的入口。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// 收块大小的分桶上界（含）。最后一桶是「其余」。
const EDGES: [u64; 8] = [
    4 << 10,
    8 << 10,
    16 << 10,
    32 << 10,
    64 << 10,
    128 << 10,
    256 << 10,
    u64::MAX,
];

pub struct Trace {
    chunks: AtomicU64,
    bytes: AtomicU64,
    /// 初值 `u64::MAX` = 还没记过，报告时当 0。
    min_chunk: AtomicU64,
    max_chunk: AtomicU64,
    hist: [AtomicU64; EDGES.len()],
    /// 一次 `resp.chunk()` 花了多久（墙钟）。
    read_ns: AtomicU64,
    /// 同上，但只算这块线程真正吃掉的 CPU（含 park/wake 与内核拷贝）。
    read_cpu_ns: AtomicU64,
    takes: AtomicU64,
    slept: AtomicU64,
    slept_ns: AtomicU64,
    take_cpu_ns: AtomicU64,
    queues: AtomicU64,
    queue_wait_ns: AtomicU64,
    writes: AtomicU64,
    write_bytes: AtomicU64,
    write_ns: AtomicU64,
    write_cpu_ns: AtomicU64,
    state_cpu_ns: AtomicU64,
}

/// 当前线程已消耗的 CPU（内核 + 用户），单位 ns。只有 Windows 有；别处恒 0。
///
/// 用 `GetThreadTimes` 而不是墙钟：墙钟里睡着的时间不算账，而这次要查的正是
/// 「明明在等 socket，CPU 却被谁吃掉了」。
#[cfg(windows)]
pub fn cpu_ns() -> u64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};

    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited) = (zero, zero);
    let (mut kernel, mut user) = (zero, zero);
    // SAFETY: `GetCurrentThread` 返回伪句柄，不需要也不能关闭；四个 FILETIME 都是
    // 有效的可写局部量，没有输出缓冲。
    let ok = unsafe {
        GetThreadTimes(
            GetCurrentThread(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    if ok == 0 {
        return 0;
    }
    let ticks = |t: FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) * 100;
    ticks(kernel) + ticks(user)
}

#[cfg(not(windows))]
pub fn cpu_ns() -> u64 {
    0
}

/// 整个进程已消耗的 CPU（内核 + 用户），单位 ns。收尾时用来核对线程账加起来对不对。
#[cfg(windows)]
fn proc_cpu_ns() -> u64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited) = (zero, zero);
    let (mut kernel, mut user) = (zero, zero);
    // SAFETY: 同 `cpu_ns`，伪句柄 + 栈上可写结构。
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    if ok == 0 {
        return 0;
    }
    let ticks = |t: FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) * 100;
    ticks(kernel) + ticks(user)
}

#[cfg(not(windows))]
fn proc_cpu_ns() -> u64 {
    0
}

impl Trace {
    pub fn new() -> Trace {
        Trace {
            chunks: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            min_chunk: AtomicU64::new(u64::MAX),
            max_chunk: AtomicU64::new(0),
            hist: std::array::from_fn(|_| AtomicU64::new(0)),
            read_ns: AtomicU64::new(0),
            read_cpu_ns: AtomicU64::new(0),
            takes: AtomicU64::new(0),
            slept: AtomicU64::new(0),
            slept_ns: AtomicU64::new(0),
            take_cpu_ns: AtomicU64::new(0),
            queues: AtomicU64::new(0),
            queue_wait_ns: AtomicU64::new(0),
            writes: AtomicU64::new(0),
            write_bytes: AtomicU64::new(0),
            write_ns: AtomicU64::new(0),
            write_cpu_ns: AtomicU64::new(0),
            state_cpu_ns: AtomicU64::new(0),
        }
    }

    pub fn chunk(&self, len: usize, read: Duration, cpu: u64) {
        let n = len as u64;
        self.chunks.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(n, Ordering::Relaxed);
        self.min_chunk.fetch_min(n, Ordering::Relaxed);
        self.max_chunk.fetch_max(n, Ordering::Relaxed);
        let i = EDGES
            .iter()
            .position(|e| n <= *e)
            .unwrap_or(EDGES.len() - 1);
        self.hist[i].fetch_add(1, Ordering::Relaxed);
        self.read_ns
            .fetch_add(read.as_nanos() as u64, Ordering::Relaxed);
        self.read_cpu_ns.fetch_add(cpu, Ordering::Relaxed);
    }

    pub fn take(&self, asked: Duration, cpu: u64) {
        self.takes.fetch_add(1, Ordering::Relaxed);
        self.take_cpu_ns.fetch_add(cpu, Ordering::Relaxed);
        if !asked.is_zero() {
            self.slept.fetch_add(1, Ordering::Relaxed);
            self.slept_ns
                .fetch_add(asked.as_nanos() as u64, Ordering::Relaxed);
        }
    }

    pub fn queue(&self, waited: Duration) {
        self.queues.fetch_add(1, Ordering::Relaxed);
        self.queue_wait_ns
            .fetch_add(waited.as_nanos() as u64, Ordering::Relaxed);
    }

    pub fn write(&self, len: usize, took: Duration, cpu: u64) {
        self.writes.fetch_add(1, Ordering::Relaxed);
        self.write_bytes.fetch_add(len as u64, Ordering::Relaxed);
        self.write_ns
            .fetch_add(took.as_nanos() as u64, Ordering::Relaxed);
        self.write_cpu_ns.fetch_add(cpu, Ordering::Relaxed);
    }

    /// 把段表拍进状态文件那一下的 CPU——它按 `state_step` 触发，跟吞吐走。
    pub fn state(&self, cpu: u64) {
        self.state_cpu_ns.fetch_add(cpu, Ordering::Relaxed);
    }

    /// 打到 stderr。`elapsed` 是整个下载的墙钟，用来算「这些时间是不是已经占满了」。
    pub fn report(&self, elapsed: Duration) {
        let secs = |ns: u64| ns as f64 / 1e9;
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let chunks = get(&self.chunks);
        let bytes = get(&self.bytes);
        let mean = bytes.checked_div(chunks).unwrap_or(0);
        let min = get(&self.min_chunk);
        let min = if min == u64::MAX { 0 } else { min };
        eprintln!(
            "[trace] {:.1} MB / {:.2} s，收块 {} 个（均 {}，{}–{}），收块共 {:.2} s",
            bytes as f64 / 1e6,
            elapsed.as_secs_f64(),
            chunks,
            size(mean),
            size(min),
            size(get(&self.max_chunk)),
            secs(get(&self.read_ns)),
        );
        let mut hist = String::new();
        for (i, n) in self.hist.iter().enumerate() {
            let n = n.load(Ordering::Relaxed);
            if n == 0 {
                continue;
            }
            let edge = if EDGES[i] == u64::MAX {
                "其余".to_string()
            } else {
                format!("<={}", size(EDGES[i]))
            };
            hist.push_str(&format!(" {edge}:{n}"));
        }
        eprintln!("[trace] 块大小分布{hist}");
        eprintln!(
            "[trace] 限速 take {} 次，真睡 {} 次，共 {:.2} s；写盘队列 {} 次，排队 {:.3} s",
            get(&self.takes),
            get(&self.slept),
            secs(get(&self.slept_ns)),
            get(&self.queues),
            secs(get(&self.queue_wait_ns)),
        );
        eprintln!(
            "[trace] pwrite {} 次 {:.1} MB，共 {:.2} s",
            get(&self.writes),
            get(&self.write_bytes) as f64 / 1e6,
            secs(get(&self.write_ns)),
        );
        let read_cpu = get(&self.read_cpu_ns);
        let take_cpu = get(&self.take_cpu_ns);
        let write_cpu = get(&self.write_cpu_ns);
        let state_cpu = get(&self.state_cpu_ns);
        let total = proc_cpu_ns();
        let per = |ns: u64| {
            if chunks == 0 {
                0.0
            } else {
                ns as f64 / chunks as f64 / 1e3
            }
        };
        let pct = if total == 0 {
            0.0
        } else {
            total as f64 / elapsed.as_nanos() as f64 * 100.0
        };
        eprintln!(
            "[trace] CPU：收块 {:.2} s（均 {:.0} µs/块），限速 {:.2} s，pwrite {:.2} s，\
             记账 {:.2} s，未记账 {:.2} s，进程共 {:.2} s = {pct:.1}% 单核",
            secs(read_cpu),
            per(read_cpu),
            secs(take_cpu),
            secs(write_cpu),
            secs(state_cpu),
            secs(total.saturating_sub(read_cpu + take_cpu + write_cpu + state_cpu)),
            secs(total),
        );
    }
}

fn size(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MiB", n as f64 / (1 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{:.1} KiB", n as f64 / (1 << 10) as f64)
    } else {
        format!("{n} B")
    }
}
