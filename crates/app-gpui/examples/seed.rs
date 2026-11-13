use std::io::Write;

use volant_torrent::{Options, Source, Torrents};

#[tokio::main]
async fn main() {
    let base = std::env::current_dir()
        .expect("workspace root")
        .join("target")
        .join(format!("p4-torrent-seed-{}", std::process::id()));
    let pack = base.join("pack");
    std::fs::create_dir_all(&pack).expect("create seed directory");
    std::fs::write(pack.join("a.bin"), vec![0x41_u8; 2 * 1024 * 1024]).expect("write a.bin");
    std::fs::write(pack.join("b.bin"), vec![0x42_u8; 1024 * 1024]).expect("write b.bin");

    let spawner = librqbit::spawn_utils::BlockingSpawner::new(2);
    let created = librqbit::create_torrent(
        &pack,
        librqbit::CreateTorrentOptions {
            name: Some("pack"),
            piece_length: Some(16 * 1024),
            ..Default::default()
        },
        &spawner,
    )
    .await
    .expect("create torrent");
    let torrent_bytes = created.as_bytes().expect("serialize torrent").to_vec();
    let torrent_path = base.join("pack.torrent");
    std::fs::write(&torrent_path, &torrent_bytes).expect("write torrent");

    let mut seeder = Torrents::new(Options {
        listen: Some("127.0.0.1:0".parse().expect("listen address")),
        ..Default::default()
    });
    let inspect = seeder
        .inspect(&Source::File(torrent_bytes))
        .await
        .expect("inspect torrent");
    seeder
        .start(&inspect, None, &base)
        .await
        .expect("start seeder");
    let peer = seeder.listen_addr().expect("seed listener");

    println!("torrent={}", torrent_path.display());
    println!("peer={peer}");
    std::io::stdout().flush().ok();
    std::future::pending::<()>().await;
}
