//! 手工验收用的小台架：本机多任务并发（T8 口径）+ 公网种子实测（T7 口径）。
//!
//! ```text
//! cargo run --release -p volant-torrent --example bench -- local 64
//! cargo run --release -p volant-torrent --example bench -- swarm <magnet-or-.torrent-url> 60
//! ```
//!
//! `local` 在本机起 seeder 再用同一个适配层下载，测的是协议栈上限；
//! `swarm` 连真实 swarm，测到的是本机链路能给多少（报告里要注明）。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use librqbit::CreateTorrentOptions;
use librqbit::create_torrent;
use librqbit::spawn_utils::BlockingSpawner;
use volant_torrent::{Options, Source, Torrents};

fn scratch(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/t6/bench")
        .join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn payload(n: usize, salt: u8) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(37).wrapping_add(salt))
        .collect()
}

#[tokio::main]
async fn main() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("VOLANT_TRACE")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("local") => {
            let mib: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(64);
            local(mib).await;
        }
        Some("swarm") => {
            let src = args
                .get(1)
                .cloned()
                .expect("swarm 模式要给 magnet 或 .torrent 的 URL");
            let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(60);
            swarm(&src, secs).await;
        }
        _ => eprintln!("用法：bench local <MiB> | bench swarm <magnet|url> [秒]"),
    }
}

/// 同一份载荷，先单任务跑一次，再三任务并发跑一次；比对总吞吐。
async fn local(mib: usize) {
    let single = run_local(mib, 1).await;
    let triple = run_local(mib, 3).await;
    println!("\n== 汇总（每份 {mib} MiB，loopback）==");
    println!("单任务 : {:>7.2} MiB/s", single);
    println!("三任务 : {:>7.2} MiB/s", triple);
    println!("三/单  : {:>7.0}%", triple / single * 100.0);
}

async fn run_local(mib: usize, n: usize) -> f64 {
    let size = mib * 1024 * 1024;
    let mut seeders = Vec::new();
    let mut peers = Vec::new();
    let mut torrents = Vec::new();
    for i in 0..n {
        let root = scratch(&format!("seed-{n}-{i}"));
        // 种子名要和落盘目录一致，否则做种端在空目录上校验，have 会是 0。
        let name = format!("pack-{n}-{i}");
        let pack = root.join(&name);
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join("data.bin"), payload(size, i as u8)).unwrap();
        eprintln!("[bench] 造种子 {i}…");
        let bytes = create_torrent(
            &pack,
            CreateTorrentOptions {
                // 每个种子单独命名，否则三个任务会往同一个 out/pack 目录里写。
                name: Some(&name),
                piece_length: Some(1024 * 1024),
                ..Default::default()
            },
            &BlockingSpawner::new(2),
        )
        .await
        .unwrap()
        .as_bytes()
        .unwrap()
        .to_vec();

        let mut seeder = Torrents::new(Options {
            listen: Some("127.0.0.1:0".parse().unwrap()),
            ..Default::default()
        });
        eprintln!("[bench] 起 seeder {i}…");
        let insp = seeder.inspect(&Source::File(bytes.clone())).await.unwrap();
        let seeder_id = seeder.start(&insp, None, &root).await.unwrap().id;
        eprintln!(
            "[bench] seeder {i} 已启动 id={seeder_id} addr={:?}",
            seeder.listen_addr()
        );
        // librqbit 在把已有数据校验完之前不会应答请求；等它进入做种状态再测，
        // 否则下载端能连上但拿不到块（30 秒后被 peer 踢掉）。
        let wait = Instant::now();
        let mut last = String::new();
        while !seeder.snapshot(seeder_id).is_some_and(|s| s.finished) {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let now = format!("{:?}", seeder.snapshot(seeder_id));
            if now != last {
                eprintln!("[bench] seeder 状态 {:?}: {now}", wait.elapsed());
                last = now;
            }
            if wait.elapsed() > Duration::from_secs(20) {
                eprintln!("[bench] seeder 20 秒还没校验完，继续");
                break;
            }
        }
        peers.push(seeder.listen_addr().unwrap());
        seeders.push(seeder);
        torrents.push(bytes);
    }

    let out = scratch(&format!("out-{n}"));
    let mut dl = Torrents::new(Options {
        initial_peers: peers,
        ..Default::default()
    });
    let mut ids = Vec::new();
    let started = Instant::now();
    eprintln!("[bench] 开始下载 {n} 个…");
    for bytes in &torrents {
        let insp = dl.inspect(&Source::File(bytes.clone())).await.unwrap();
        ids.push(dl.start(&insp, None, &out).await.unwrap().id);
    }
    let mut done = 0;
    let total = (size * n) as f64;
    let mut tick = 0;
    while done < n && started.elapsed() < Duration::from_secs(600) {
        tokio::time::sleep(Duration::from_millis(100)).await;
        done = ids
            .iter()
            .filter(|id| dl.snapshot(**id).is_some_and(|s| s.finished))
            .count();
        tick += 1;
        if tick % 10 == 0 {
            let f = ids.first().and_then(|id| dl.snapshot(*id));
            eprintln!(
                "[bench] {:>5.1}s done={done}/{n} first={:?}",
                started.elapsed().as_secs_f64(),
                f.map(|s| (s.downloaded, s.total, s.peers, s.seeders, s.error))
            );
        }
    }
    let secs = started.elapsed().as_secs_f64();
    let mbps = total / secs / (1024.0 * 1024.0);
    println!("{n} 个种子 × {mib} MiB：{:.2} s，{mbps:.2} MiB/s", secs);
    for mut s in seeders {
        s.shutdown().await;
    }
    dl.shutdown().await;
    mbps
}

/// 连真实 swarm 跑一段时间，只报能看到的东西：下了多少、多少 peer、多快。
async fn swarm(src: &str, secs: u64) {
    let source = if src.starts_with("magnet:") {
        Source::Magnet(src.to_string())
    } else {
        Source::File(std::fs::read(src).expect("读 .torrent 文件"))
    };
    let mut tor = Torrents::new(Options::default());
    let insp = match tokio::time::timeout(Duration::from_secs(90), tor.inspect(&source)).await {
        Ok(Ok(i)) => i,
        Ok(Err(e)) => {
            println!("解析失败：{e}");
            return;
        }
        Err(_) => {
            println!("解析超时（90 s，多半是连不上 tracker/DHT）");
            return;
        }
    };
    println!(
        "种子：{} · {:.1} MiB · {} 个文件 · {}",
        insp.name,
        insp.total as f64 / 1048576.0,
        insp.files.len(),
        insp.info_hash
    );
    let out = scratch("swarm-out");
    let st = match tor.start(&insp, None, &out).await {
        Ok(s) => s,
        Err(e) => {
            println!("开始失败：{e}");
            return;
        }
    };
    let t0 = Instant::now();
    let mut last = 0u64;
    let mut peak = 0.0f64;
    let mut peak_peers = 0usize;
    while t0.elapsed() < Duration::from_secs(secs) {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let Some(s) = tor.snapshot(st.id) else { break };
        let dt = 2.0;
        let inst = (s.downloaded.saturating_sub(last)) as f64 / dt / 1048576.0;
        last = s.downloaded;
        peak = peak.max(inst);
        peak_peers = peak_peers.max(s.peers);
        println!(
            "{:>5.0}s  下载 {:>8.1} MiB / {:>8.1} MiB  peer {:<3} 做种 {:<3} 即时 {:>6.2} MiB/s{}",
            t0.elapsed().as_secs_f64(),
            s.downloaded as f64 / 1048576.0,
            s.total as f64 / 1048576.0,
            s.peers,
            s.seeders,
            inst,
            if s.finished { "  已完成" } else { "" }
        );
        if s.finished {
            break;
        }
    }
    let secs = t0.elapsed().as_secs_f64();
    println!(
        "\n共 {:.1} MiB / {:.1} s，平均 {:.2} MiB/s，峰值 {:.2} MiB/s，最多 {} peer",
        last as f64 / 1048576.0,
        secs,
        last as f64 / secs / 1048576.0,
        peak,
        peak_peers
    );
    tor.shutdown().await;
}
