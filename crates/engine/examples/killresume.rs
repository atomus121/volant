//! `kill -9` 续传验证用的小程序。
//!
//! 自己起不了能让两次运行共用同一个 URL 的服务器（端口会变、文件名会变），所以 URL
//! 由外面给：
//!
//! ```text
//! cargo run --release -p volant-labserver -- --addr 127.0.0.1:47777
//! cargo run --release -p volant-engine --example killresume -- \
//!     http://127.0.0.1:47777/file/1073741824 target/bench
//! ```
//!
//! 第一遍看它打印 `pid`，直接 `Stop-Process -Id <pid> -Force`（约等于 kill -9），
//! 然后原样再跑一遍：`resumed_from` 就是它接着下时已经有的字节数。最后加 `--verify`
//! 再跑一次，确认字节和服务器生成的完全一致。

use std::path::PathBuf;

use volant_engine::{Config, Download, Event, Url, client, probe};

/// 压测要能调旋钮：`VOLANT_CONNS`、`VOLANT_DISK_RATE`（调慢制造写队列积压）、
/// `VOLANT_RATE`、`VOLANT_STATE_STEP`、`VOLANT_BATCH`。带 `k`/`M`/`G` 后缀的数字也认。
fn tune(mut cfg: Config) -> Config {
    let num = |name: &str| -> Option<u64> {
        let raw = std::env::var(name).ok()?;
        let text = raw.trim();
        let (digits, mult) = match text.chars().last()? {
            'k' | 'K' => (&text[..text.len() - 1], 1_000),
            'm' | 'M' => (&text[..text.len() - 1], 1_000_000),
            'g' | 'G' => (&text[..text.len() - 1], 1_000_000_000),
            _ => (text, 1),
        };
        Some(digits.trim().parse::<f64>().ok()? as u64 * mult)
    };
    if let Some(n) = num("VOLANT_CONNS") {
        cfg.connections = (n as usize).max(1);
    }
    if let Some(r) = num("VOLANT_DISK_RATE") {
        cfg.disk_rate = Some(r);
    }
    if let Some(r) = num("VOLANT_RATE") {
        cfg.rate_limit = Some(r);
    }
    if let Some(s) = num("VOLANT_STATE_STEP") {
        cfg.state_step = s.max(1);
    }
    if let Some(b) = num("VOLANT_BATCH") {
        cfg.batch = (b as usize).max(4096);
    }
    cfg
}

#[tokio::main]
async fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // 只看状态文件里记了多少，不下载：压测需要拿它和盘上真正有的字节对账。
    if argv.first().map(String::as_str) == Some("--show") {
        let path = argv
            .get(1)
            .expect("用法: killresume --show <x.volant.state>");
        match volant_engine::ResumeState::load(std::path::Path::new(path)) {
            Ok(s) => println!(
                "downloaded={} segments={}",
                s.downloaded(),
                s.segments.len()
            ),
            Err(e) => {
                eprintln!("读不出状态文件: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    let mut args = argv.into_iter();
    let usage = "用法: killresume <url> <dir> [--verify]";
    let url: Url = args
        .next()
        .unwrap_or_else(|| usage.into())
        .parse()
        .expect("URL");
    let dir = PathBuf::from(args.next().unwrap_or_else(|| usage.into()));
    let verify = args.any(|a| a == "--verify");

    let http = client().expect("http client");
    let probed = probe(&http, &url).await.expect("probe");
    println!(
        "pid {} 大小 {:?} 支持分段 {} 文件名 {:?}",
        std::process::id(),
        probed.size,
        probed.ranges,
        probed.filename
    );

    let mut download = Download::start(http, tune(Config::new(url, &dir)), probed);
    let mut last = 0;
    while let Some(event) = download.events.recv().await {
        match event {
            Event::Started { resumed_from, .. } => {
                println!("resumed_from={resumed_from}");
            }
            Event::Progress { downloaded, .. } => last = downloaded,
            Event::Done { .. } => {}
            Event::Failed { reason } => println!("failed={reason}"),
        }
        println!("{last}");
    }
    match download.wait().await {
        Err(e) => {
            eprintln!("下载失败: {e}");
            std::process::exit(1);
        }
        Ok(summary) => {
            println!(
                "完成 {} 字节，耗时 {:.2} s，续传自 {}",
                summary.size,
                summary.elapsed.as_secs_f64(),
                summary.resumed_from
            );
            if verify {
                let got = std::fs::read(&summary.dest).expect("读回文件");
                let mut want = vec![0u8; summary.size as usize];
                volant_labserver::fill_pattern(&mut want, 0);
                assert_eq!(got.len(), want.len(), "长度不对");
                let bad = got.iter().zip(&want).position(|(a, b)| a != b);
                match bad {
                    None => println!("校验通过：{} 字节逐字节相同", got.len()),
                    Some(at) => {
                        eprintln!("校验失败：第 {at} 个字节不同");
                        std::process::exit(1);
                    }
                }
            }
        }
    }
}
