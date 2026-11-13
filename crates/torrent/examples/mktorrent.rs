//! 造一个 .torrent：给离线冒烟测试和本机做种/下载基准用。
//!
//! `cargo run --release -p volant-torrent --example mktorrent -- <文件或目录> <输出.torrent> [name]`

use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let src: PathBuf = match args.next() {
        Some(s) => s.into(),
        None => {
            eprintln!("用法: mktorrent <文件或目录> <输出.torrent> [name]");
            std::process::exit(2);
        }
    };
    let out: PathBuf = match args.next() {
        Some(s) => s.into(),
        None => {
            eprintln!("缺输出路径");
            std::process::exit(2);
        }
    };
    let name = args.next();

    let spawner = librqbit::spawn_utils::BlockingSpawner::new(2);
    let created = librqbit::create_torrent(
        &src,
        librqbit::CreateTorrentOptions {
            name: name.as_deref(),
            piece_length: Some(16 * 1024),
            ..Default::default()
        },
        &spawner,
    )
    .await
    .expect("造种子失败");

    let bytes = created.as_bytes().expect("生成 .torrent 字节失败");
    std::fs::write(&out, &bytes).expect("写 .torrent 失败");
    println!("{}", created.as_info().info_hash.as_string());
    println!("{}", created.as_magnet());
    println!("{} bytes -> {}", bytes.len(), out.display());
}
