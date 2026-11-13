//! 一次性诊断：只做「收包」，不带 engine 的分段/写盘队列/记账。
//!
//! 用途是把 M5 的 CPU 账拆开：同一条限速链路上，`curl -o NUL` 只要 9%，而引擎 8 连接
//! 要 36%。这个程序逐个加回引擎里的一层，看哪一层吃掉那 27 点：
//!
//! ```text
//! cargo run --release -p volant-engine --example recvonly -- <url> [--write <path>]
//! ```
//!
//! `--write` 时用同步 `pwrite` 顺序落盘（单线程，不做 256 KiB 攒批）。
//! 不是产品代码，也不进测试；量完就可以删。

use std::time::Instant;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(url) = args.first() else {
        eprintln!("用法: recvonly <url> [--write <path>]");
        std::process::exit(2);
    };
    let write = args
        .iter()
        .position(|a| a == "--write")
        .and_then(|i| args.get(i + 1))
        .map(std::path::PathBuf::from);

    let started = Instant::now();
    let resp = reqwest::get(url).await.expect("请求失败");
    let status = resp.status();
    let mut resp = resp;
    let mut file = write.as_ref().map(|p| {
        std::fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(p)
            .expect("打不开")
    });
    let mut pos = 0u64;
    let mut chunks = 0u64;
    while let Some(chunk) = resp.chunk().await.expect("读失败") {
        chunks += 1;
        if let Some(f) = file.as_mut() {
            use std::os::windows::fs::FileExt;
            let mut off = pos;
            let mut rest = &chunk[..];
            while !rest.is_empty() {
                let n = f.seek_write(rest, off).expect("写失败");
                off += n as u64;
                rest = &rest[n..];
            }
        }
        pos += chunk.len() as u64;
    }
    eprintln!(
        "recvonly: {status} {} B / {:.2} s，{} 块，均 {:.0} KiB，写盘={}",
        pos,
        started.elapsed().as_secs_f64(),
        chunks,
        pos as f64 / chunks.max(1) as f64 / 1024.0,
        write.is_some(),
    );
}
