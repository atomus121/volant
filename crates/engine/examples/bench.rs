//! M1/M2 验收用的手工基准（T1–T6）：
//!
//! ```text
//! cargo run --release -p volant-engine --example bench            # 全部
//! cargo run --release -p volant-engine --example bench -- t1      # 只跑一个
//! ```
//!
//! 每个用例自己起一个 labserver，跑完把文件删掉。数字打到 stdout，达不到 PRD 的
//! 目标就以非零退出——这样它既能给人看，也能进 CI 当门禁。
//!
//! **服务端跑在单独进程里**（`target/release/volant-labserver.exe`，先 `cargo build
//! --release -p volant-labserver --bins`）。同一个进程里服务端和客户端抢同一个
//! runtime，T1 会从 1.7 GB/s 掉到 1.3 GB/s——那量的是两个东西互相踩脚，不是客户端。
//!
//! 为什么不在 `tests/` 里：这些用例要按 release 跑、要写几十 GB、单次要十几秒，
//! 和「一秒钟跑完的正确性测试」是两种东西。

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use volant_engine::{Config, Download, Url, client, probe};

/// 一次下载的实测量。
struct Measured {
    secs: f64,
    size: u64,
}

impl Measured {
    fn mb_per_s(&self) -> f64 {
        self.size as f64 / 1e6 / self.secs
    }
}

fn gib(bytes: u64) -> f64 {
    bytes as f64 / (1u64 << 30) as f64
}

/// 子进程里的 labserver。`Drop` 里杀掉，用例之间不会互相干扰。
struct Lab {
    child: Child,
    base: String,
    /// 必须一直留着：管道读端一关，子进程下一次 `println!` 就 panic 退出，
    /// 于是「起好了又突然连不上」——第一版就是这么偶发失败的。
    _out: std::io::BufReader<std::process::ChildStdout>,
}

impl Lab {
    /// `spec` 是 `Faults::parse` 认的故障串，空串表示无故障。
    fn start(spec: &str) -> Lab {
        let exe = labserver_exe();
        let mut cmd = Command::new(&exe);
        cmd.arg("--addr").arg("127.0.0.1:0");
        if !spec.is_empty() {
            cmd.arg("--faults").arg(spec);
        }
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("起 {} 失败: {e}", exe.display()));
        // 它绑好端口才打 "listening http://<addr>"，看到这一行再连就不会撞上没起好。
        let stdout = child.stdout.take().expect("stdout");
        let mut out = std::io::BufReader::new(stdout);
        let mut base = None;
        for line in out.by_ref().lines() {
            let Ok(line) = line else { break };
            if let Some(rest) = line.strip_prefix("listening ") {
                base = Some(rest.to_string());
                break;
            }
        }
        let Some(base) = base else {
            let _ = child.kill();
            panic!("labserver 没打出 listening 就退了");
        };
        Lab {
            child,
            base,
            _out: out,
        }
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn labserver_exe() -> PathBuf {
    // bench.exe 在 target/<profile>/examples/ 下，labserver 在 target/<profile>/ 下。
    let path = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .and_then(Path::parent)
        .expect("target 目录")
        .join("volant-labserver.exe");
    assert!(
        path.exists(),
        "找不到 {}，先跑 cargo build --release -p volant-labserver --bins",
        path.display()
    );
    path
}

/// 一次下载的可调项。默认值就是引擎的默认值，用例只改自己关心的那几个。
#[derive(Clone, Copy)]
struct Tune {
    connections: usize,
    rate_limit: Option<u64>,
    disk_rate: Option<u64>,
    stall_timeout: Option<Duration>,
}

impl Default for Tune {
    fn default() -> Self {
        Tune {
            connections: 8,
            rate_limit: None,
            disk_rate: None,
            stall_timeout: None,
        }
    }
}

async fn once(dir: &Path, size: u64, conns: usize, fault_spec: &str) -> Measured {
    let server = Lab::start(fault_spec);
    let url = format!("{}/file/{size}", server.base);
    once_at(
        &url,
        dir,
        Tune {
            connections: conns,
            ..Default::default()
        },
    )
    .await
}

/// 对任意 URL 跑一次下载。和 [`once`] 分开，是为了能对着**别的进程**里的
/// labserver 量客户端——同一个进程里服务端和客户端抢 runtime，量出来的数字
/// 说不清是谁的瓶颈。
async fn once_at(url: &str, dir: &Path, tune: Tune) -> Measured {
    let url: Url = url.parse().expect("URL");
    let http = client().expect("http client");
    let probed = probe(&http, &url).await.expect("probe");
    let mut cfg = Config::new(url, dir);
    cfg.connections = tune.connections;
    cfg.rate_limit = tune.rate_limit;
    cfg.disk_rate = tune.disk_rate;
    cfg.stall_timeout = tune.stall_timeout.unwrap_or(cfg.stall_timeout);
    if let Ok(v) = std::env::var("VOLANT_CONNS") {
        cfg.connections = v.parse().expect("VOLANT_CONNS");
    }
    // 调参用：这几个环境变量不改代码就能试。`VOLANT_FLUSH_MS` 给得很大就等于不落状态。
    if let Ok(v) = std::env::var("VOLANT_BATCH") {
        cfg.batch = v.parse().expect("VOLANT_BATCH");
    }
    if let Ok(v) = std::env::var("VOLANT_STATE_STEP") {
        cfg.state_step = v.parse().expect("VOLANT_STATE_STEP");
    }
    if let Ok(v) = std::env::var("VOLANT_FLUSH_MS") {
        cfg.flush_every = std::time::Duration::from_millis(v.parse().expect("VOLANT_FLUSH_MS"));
    }
    if let Ok(v) = std::env::var("VOLANT_SYNC_MS") {
        cfg.sync_every = std::time::Duration::from_millis(v.parse().expect("VOLANT_SYNC_MS"));
    }
    if let Ok(v) = std::env::var("VOLANT_RATE") {
        cfg.rate_limit = Some(v.parse().expect("VOLANT_RATE"));
    }
    if let Ok(v) = std::env::var("VOLANT_DISK_RATE") {
        cfg.disk_rate = Some(v.parse().expect("VOLANT_DISK_RATE"));
    }
    if let Ok(v) = std::env::var("VOLANT_STALL_MS") {
        cfg.stall_timeout = std::time::Duration::from_millis(v.parse().expect("VOLANT_STALL_MS"));
    }
    let started = Instant::now();
    let summary = Download::start(http, cfg, probed)
        .wait()
        .await
        .expect("下载失败");
    let secs = started.elapsed().as_secs_f64();
    std::fs::remove_file(&summary.dest).ok();
    Measured {
        secs,
        size: summary.size,
    }
}

/// 服务端的累计计数：`(请求数, 发出的正文字节数)`。裸 HTTP/1.0 问一次 `/stats`。
fn stats(base: &str) -> (u64, u64) {
    let addr = base.trim_start_matches("http://");
    let mut sock = std::net::TcpStream::connect(addr).expect("连 labserver /stats");
    sock.write_all(b"GET /stats HTTP/1.0\r\n\r\n")
        .expect("写请求");
    let mut text = String::new();
    sock.read_to_string(&mut text).expect("读响应");
    let body = text.split("\r\n\r\n").nth(1).unwrap_or_default();
    let field = |name: &str| -> u64 {
        body.lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.trim().parse().ok())
            .unwrap_or(0)
    };
    (field("requests "), field("served "))
}

fn report(name: &str, what: String, ok: bool) -> bool {
    println!("{name:<4} {what}  {}", if ok { "达标" } else { "未达标" });
    ok
}

#[tokio::main]
async fn main() {
    let only: Vec<String> = std::env::args().skip(1).collect();
    let picks = |name: &str| only.is_empty() || only.iter().any(|o| o == name);

    let dir = std::path::Path::new("target/bench");
    std::fs::create_dir_all(dir).expect("建 target/bench");

    let mut failures = 0;
    println!("结果目录 {}（每次跑完都会删掉文件）\n", dir.display());

    // `bench url=<url>`：只量客户端，服务端在别的进程里（诊断用，不算达标）。
    if let Some(url) = only.iter().find_map(|o| o.strip_prefix("url=")) {
        let m = once_at(url, dir, Tune::default()).await;
        println!(
            "客户端单独跑: {:.2} s, {:.0} MB/s ({:.2} GiB/s)",
            m.secs,
            m.mb_per_s(),
            gib(m.size) / m.secs
        );
        return;
    }

    // T1：本地 10 GiB，8 连接 → ≥ 同机同条件裸写基线 × 80%（PRD 6.1）。
    // 基线由外面量好传进来（`VOLANT_RAW_BASELINE=<MB/s>`，见 tools/t1.ps1）：基准要用
    // 独立于被测代码的东西量，bench 自己算会把「引擎跑得快」混进分母里。
    if picks("t1") {
        // 和 `tools/t1.ps1` 的裸写基线用同一个尺寸，否则两边量的不是一件事。
        let size: u64 = std::env::var("VOLANT_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10 << 30);
        let m = once(dir, size, 8, "").await;
        let got = m.mb_per_s();
        let baseline: Option<f64> = std::env::var("VOLANT_RAW_BASELINE")
            .ok()
            .and_then(|v| v.parse().ok());
        let ok = match baseline {
            Some(base) => report(
                "T1",
                format!(
                    "{:.1} GiB / 8 连接: {:.2} s, {:.0} MB/s ({:.2} GiB/s)，裸写基线 {base:.0} MB/s，{:.0}%（目标 ≥ 80%）",
                    gib(m.size),
                    m.secs,
                    got,
                    gib(m.size) / m.secs,
                    got / base * 100.0
                ),
                got >= base * 0.8,
            ),
            None => {
                println!(
                    "T1   {:.1} GiB / 8 连接: {:.2} s, {:.0} MB/s ({:.2} GiB/s)——没设 VOLANT_RAW_BASELINE，不作判据",
                    gib(m.size),
                    m.secs,
                    got,
                    gib(m.size) / m.secs
                );
                true
            }
        };
        failures += usize::from(!ok);
    }

    // T3：单连接对照，多连接收益 ≥ 3×。
    if picks("t3") {
        let size = 512 << 20;
        let one = once(dir, size, 1, "").await;
        let many = once(dir, size, 8, "").await;
        let ratio = one.secs / many.secs;
        let ok = report(
            "T3",
            format!(
                "512 MiB: 1 连接 {:.2} s ({:.0} MB/s) vs 8 连接 {:.2} s ({:.0} MB/s) = {ratio:.2}×，目标 ≥ 3×",
                one.secs,
                one.mb_per_s(),
                many.secs,
                many.mb_per_s()
            ),
            ratio >= 3.0,
        );
        failures += usize::from(!ok);
    }

    // T4：2 条连接各 2 MiB/s，必须两条都用上，耗时 ≤ 理论值 110%。
    if picks("t4") {
        let size = 64 << 20;
        let m = once(dir, size, 2, "rate=2M").await;
        let ideal = size as f64 / (2.0 * 2.0 * (1u64 << 20) as f64);
        let ok = report(
            "T4",
            format!(
                "64 MiB / 2 连接 × 2 MiB/s: {:.2} s，理论 {ideal:.2} s = {:.0}%，目标 ≤ 110%",
                m.secs,
                m.secs / ideal * 100.0
            ),
            m.secs <= ideal * 1.10,
        );
        failures += usize::from(!ok);
    }

    // T5：每 2 MiB 断一次，耗时 ≤ 无故障场景 1.8×。
    if picks("t5") {
        let size = 512 << 20;
        let clean = once(dir, size, 8, "").await;
        let cut = once(dir, size, 8, "cut=2M").await;
        let ratio = cut.secs / clean.secs;
        let ok = report(
            "T5",
            format!(
                "512 MiB: 无故障 {:.2} s，每 2 MiB 切断 {:.2} s = {ratio:.2}×，目标 ≤ 1.8×",
                clean.secs, cut.secs
            ),
            ratio <= 1.8,
        );
        failures += usize::from(!ok);
    }

    // T2：模拟 100 Mbps 宽带（20 ms RTT、0.1% 丢包）→ ≥ 90 Mbps（链路 90%）。
    //
    // 服务端仿真，不是真宽带（Windows 没有 netem）：
    // - `grate=12.5M`：全链路共享 12.5 MB/s = 100 Mbps 的容量上限；
    // - `delay=20`：每个请求先等一个 RTT 再回响应；
    // - `stall_every=1460K,stall=20`：0.1% 丢包 × 1460 B 段 ≈ 每 1.46 MB 一次；
    //   一次丢包的代价按「快速重传」算是一个 RTT（20 ms），不是 RTO 的 200 ms。
    // 没仿真的是 TCP 拥塞窗口：这里假设 cwnd 已经足够大，属于**偏乐观**的仿真。
    if picks("t2") {
        let size = 64 << 20;
        let m = once(
            dir,
            size,
            8,
            "grate=12.5M,delay=20,stall_every=1460K,stall=20",
        )
        .await;
        let got = m.mb_per_s();
        let link = 12.5; // MB/s，即 100 Mbps
        let ok = report(
            "T2",
            format!(
                "64 MiB / 模拟 100 Mbps（20 ms RTT、0.1% 丢包）: {:.2} s = {:.2} MB/s（{:.1} Mbps），链路 {link} MB/s 的 {:.1}%，目标 ≥ 90%",
                m.secs,
                got,
                got * 8.0,
                got / link * 100.0
            ),
            got >= link * 0.9,
        );
        failures += usize::from(!ok);
    }

    // T6：慢盘 50 MB/s → 吞吐就是 50 MB/s、不额外重连、不误判慢连接。
    //
    // 机器上只有一块 NVMe，没有 50 MB/s 的慢盘：慢盘用引擎侧的写盘节奏器
    // （`disk_rate`）仿真，效果等价——写队列被限速，网络侧照旧。
    //
    // 「不额外重连」拿**对比**做硬证据：同一份文件先全速下、再限速下，慢盘那次的
    // 请求数不能多于全速那次。让位/重连都会多出请求。
    if picks("t6") {
        let size = 200 << 20;
        let disk = 50_000_000; // 50 MB/s
        let server = Lab::start("");
        let url = format!("{}/file/{size}", server.base);

        let (r0, s0) = stats(&server.base);
        let fast = once_at(&url, dir, Tune::default()).await;
        let (r1, s1) = stats(&server.base);
        let slow = once_at(
            &url,
            dir,
            Tune {
                disk_rate: Some(disk),
                ..Default::default()
            },
        )
        .await;
        let (r2, s2) = stats(&server.base);

        let ideal = size as f64 / disk as f64;
        let fast_requests = r1 - r0;
        let slow_requests = r2 - r1;
        let fast_served = s1 - s0;
        let slow_served = s2 - s1;
        let ok = report(
            "T6",
            format!(
                "200 MB / 慢盘 {:.0} MB/s: {:.2} s = {:.1} MB/s = 理想的 {:.0}%，目标 ≥ 95% 且 ≤ 105%；\
                 请求 全速 {fast_requests}（{:.0} MB/s）→ 慢盘 {slow_requests}（不求更多）；\
                 重发 全速 {:.1}% / 慢盘 {:.1}%（planner 分段代价，与慢盘无关）",
                disk as f64 / 1e6,
                slow.secs,
                slow.mb_per_s(),
                slow.secs / ideal * 100.0,
                fast.mb_per_s(),
                fast_served as f64 / size as f64 * 100.0,
                slow_served as f64 / size as f64 * 100.0
            ),
            slow.secs <= ideal * 1.05 && slow_requests <= fast_requests,
        );
        failures += usize::from(!ok);
    }

    if failures > 0 {
        eprintln!("\n{failures} 项未达标");
        std::process::exit(1);
    }
    println!("\n全部达标");
}
