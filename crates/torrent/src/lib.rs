//! torrent 适配层。
//!
//! 这个 crate 的边界就是 librqbit 的边界：外面（控制器、界面）只看见
//! [`Torrents`]/[`Source`]/[`Inspect`]/[`Snapshot`]，升级 librqbit 只改这里。
//!
//! 会话是「按需存在」的：第一次添加才建 [`Session`]，最后一个任务删掉后由调用方
//! 调 [`Torrents::shutdown`] 把整个会话丢掉。`SessionOptions` 里 DHT、监听端口、
//! 本地服务发现全部关掉，所以空闲时既不开端口，也不会触发 Windows 防火墙弹窗。
//!
//! 添加分两步：先 `inspect`（`list_only`）拿文件名/大小/文件列表，用户选完再
//! `start`。两步之间只传 `.torrent` 字节，不重新解析、也不重复下载元数据。

#![allow(clippy::too_many_arguments)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use librqbit::api::TorrentIdOrHash;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ByteBufOwned, ListenerOptions, Session,
    SessionOptions, TorrentMetaV1Info, TorrentStatsState,
};

/// 解析磁力链接时要等 metadata，给足时间；超过就是对面没有 peer。
const INSPECT_TIMEOUT: Duration = Duration::from_secs(90);

/// 磁力链接自己没带 `tr=` 时补的公共 tracker。
///
/// 关掉 DHT 之后，没有 tracker 的 magnet 根本拿不到 metadata（librqbit 会直接
/// 报 "no known way to resolve peers"）。只在确实没写 tracker 时才用，不会把
/// 每个种子的 info-hash 都广播一遍。
const FALLBACK_TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
];

/// 添加的来源。`.torrent` 走字节，磁力链接走字符串。
pub enum Source {
    Magnet(String),
    File(Vec<u8>),
}

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub name: String,
    pub len: u64,
}

/// `inspect` 的结果：足够画一个选文件列表，也足够直接 `start`。
pub struct Inspect {
    pub name: String,
    pub info_hash: String,
    pub total: u64,
    /// 多文件时的扁平列表（相对路径）；单文件只有一项。
    pub files: Vec<FileEntry>,
    pub multi: bool,
    /// 规范化后的 `.torrent` 字节，`start` 直接用它。
    pub torrent: Vec<u8>,
    /// 元数据没有 tracker；start 时补公共 tracker，DHT 也保持开启。
    pub trackerless: bool,
}

/// 运行中任务的一帧状态。
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub downloaded: u64,
    pub total: u64,
    pub uploaded: u64,
    pub peers: usize,
    pub seeders: usize,
    /// 下载完成（之后仍在做种，直到被暂停）。
    pub finished: bool,
    pub paused: bool,
    pub error: Option<String>,
    /// 字节/秒。
    pub down_speed: f64,
    pub up_speed: f64,
}

pub struct Started {
    pub id: usize,
    /// 文件真正落在哪个目录（多文件是 `<下载目录>/<种子名>`）。
    pub dest: PathBuf,
}

/// 全局设置；界面里没有旋钮的用默认值。
#[derive(Clone)]
pub struct Options {
    /// 每个种子最多连多少 peer（PRD 6.2 的上限 100）。
    pub peer_limit: usize,
    /// 只给本机测试用：直接连这些 peer，不依赖 tracker/DHT。
    pub initial_peers: Vec<SocketAddr>,
    /// 只给本机测试/做种用：监听地址；`None` = 不监听（不触发防火墙）。
    pub listen: Option<SocketAddr>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            peer_limit: 100,
            initial_peers: Vec::new(),
            listen: None,
        }
    }
}

pub struct Torrents {
    opts: Options,
    session: Option<Arc<Session>>,
}

impl Torrents {
    pub fn new(opts: Options) -> Torrents {
        Torrents {
            opts,
            session: None,
        }
    }

    /// 会话是否还活着（测试与内存账用）。
    pub fn alive(&self) -> bool {
        self.session.is_some()
    }

    /// 更新后续会话使用的 peer 上限；已经建立的 librqbit Session 不重建。
    pub fn set_peer_limit(&mut self, limit: usize) {
        self.opts.peer_limit = limit.max(1);
    }

    /// 实际监听的地址；没开会话或没监听时返回 `None`。只给本机做种测试用。
    pub fn listen_addr(&self) -> Option<SocketAddr> {
        self.session.as_ref().and_then(|s| s.listen_addr())
    }

    async fn ensure(&mut self) -> Result<Arc<Session>, String> {
        if let Some(s) = &self.session {
            return Ok(s.clone());
        }
        // 不监听入站端口、不做局域网发现；DHT 只在活跃会话期间存在，用来发现
        // trackerless 种子和 magnet 的 peer。最后一个任务结束仍会丢掉整个 Session。
        let o = SessionOptions {
            dht: Some(librqbit::DhtSessionConfig {
                persistence: None,
                ..Default::default()
            }),
            listen: self.opts.listen.map(|addr| ListenerOptions {
                listen_addr: addr,
                ..Default::default()
            }),
            disable_local_service_discovery: true,
            disable_trackers: false,
            // 不落 librqbit 自己的持久化目录；进度靠我们自己的 state.json 重建。
            persistence: None,
            fastresume: false,
            peer_limit: Some(self.opts.peer_limit.max(1)),
            client_name_and_version: Some(concat!("Volant/", env!("CARGO_PKG_VERSION")).into()),
            ..Default::default()
        };
        let session = Session::new_with_opts(PathBuf::from("."), o)
            .await
            .map_err(|e| format!("torrent 会话启动失败：{e:#}"))?;
        self.session = Some(session.clone());
        Ok(session)
    }

    /// 只解析元数据，不开下载。磁力链接会走 tracker 拿 metadata（最多等 90 秒）。
    pub async fn inspect(&mut self, source: &Source) -> Result<Inspect, String> {
        let session = self.ensure().await?;
        let (add, extra_trackers) = match source {
            Source::Magnet(m) => {
                let extra = if m.contains("tr=") {
                    Vec::new()
                } else {
                    FALLBACK_TRACKERS.iter().map(|s| s.to_string()).collect()
                };
                (AddTorrent::from_url(m.clone()), extra)
            }
            Source::File(bytes) => (AddTorrent::from_bytes(bytes.clone()), Vec::new()),
        };

        let mut opt = AddTorrentOptions {
            list_only: true,
            ..Default::default()
        };
        if !extra_trackers.is_empty() {
            opt.trackers = Some(extra_trackers);
        }
        if !self.opts.initial_peers.is_empty() {
            opt.initial_peers = Some(self.opts.initial_peers.clone());
        }

        let resp = tokio::time::timeout(INSPECT_TIMEOUT, session.add_torrent(add, Some(opt)))
            .await
            .map_err(|_| {
                "解析种子超时：90 秒内没拿到 metadata，可能没人做种或 tracker 不通".to_string()
            })?
            .map_err(friendly)?;

        let (info, info_hash, torrent) = match resp {
            AddTorrentResponse::ListOnly(r) => (r.info, r.info_hash, r.torrent_bytes),
            // list_only 时不会开下载；能变成 Added/AlreadyManaged 只可能是重复添加。
            _ => return Err("这个种子已经在下载列表里".into()),
        };

        let files = files_of(info.info())?;
        let total = files.iter().map(|f| f.len).sum();
        let trackerless = librqbit::torrent_from_bytes(torrent.as_ref())
            .map(|m| m.iter_announce().next().is_none())
            .unwrap_or(false);
        let name = info
            .name()
            .map(|n| n.into_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "torrent".to_string());
        Ok(Inspect {
            name,
            info_hash: info_hash.as_string(),
            total,
            // 只要种子里带文件列表（多文件种子，或从一个目录造的种子），
            // 目录布局就是 `<下载目录>/<种子名>/...`，单个文件才是直接落在下载目录。
            multi: info.info().files.is_some(),
            files,
            torrent: torrent.to_vec(),
            trackerless,
        })
    }

    /// 真正开始下载。`only` 是选中的文件下标；`None`/全选 = 下全部。
    pub async fn start(
        &mut self,
        insp: &Inspect,
        only: Option<Vec<usize>>,
        dir: &Path,
    ) -> Result<Started, String> {
        let session = self.ensure().await?;
        let dest = if insp.multi {
            dir.join(safe_name(&insp.name))
        } else {
            dir.to_path_buf()
        };
        let mut opt = AddTorrentOptions {
            // 续传要写已经存在的文件；不给这个开关 librqbit 会拒绝重新打开。
            overwrite: true,
            output_folder: Some(dest.to_string_lossy().into_owned()),
            ..Default::default()
        };
        // 全选时传 None：留部分选择会让种子不完整（BEP-53），做种就没意义了。
        if only.as_ref().is_some_and(|v| v.is_empty()) {
            return Err("至少要选一个文件".into());
        }
        opt.only_files = only.filter(|v| v.len() < insp.files.len());
        if insp.trackerless {
            opt.trackers = Some(FALLBACK_TRACKERS.iter().map(|s| s.to_string()).collect());
        }
        if !self.opts.initial_peers.is_empty() {
            opt.initial_peers = Some(self.opts.initial_peers.clone());
        }

        let resp = session
            .add_torrent(AddTorrent::from_bytes(insp.torrent.clone()), Some(opt))
            .await
            .map_err(friendly)?;
        match resp {
            AddTorrentResponse::Added(id, _) => Ok(Started { id, dest }),
            AddTorrentResponse::AlreadyManaged(_, _) => Err("这个种子已经在下载列表里".into()),
            AddTorrentResponse::ListOnly(_) => Err("种子信息不完整，无法开始下载".into()),
        }
    }

    /// 读一帧状态；任务已经没了就返回 `None`。
    pub fn snapshot(&self, id: usize) -> Option<Snapshot> {
        let session = self.session.as_ref()?;
        let handle = session.get(TorrentIdOrHash::Id(id))?;
        let st = handle.stats();
        let mut peers = 0;
        let mut seeders = 0;
        let mut down_speed = 0.0;
        let mut up_speed = 0.0;
        if let Some(l) = &st.live {
            peers = l.snapshot.peer_stats.live as usize;
            seeders = l.snapshot.peer_stats.live_seeders as usize;
            // librqbit 的 Speed 单位是 MiB/s。
            down_speed = l.download_speed.mbps * 1024.0 * 1024.0;
            up_speed = l.upload_speed.mbps * 1024.0 * 1024.0;
        }
        let paused = matches!(st.state, TorrentStatsState::Paused)
            || matches!(st.state, TorrentStatsState::Initializing { paused: true });
        Some(Snapshot {
            downloaded: st.progress_bytes,
            total: st.total_bytes,
            uploaded: st.uploaded_bytes,
            peers,
            seeders,
            finished: st.finished,
            paused,
            error: st.error.clone(),
            down_speed,
            up_speed,
        })
    }

    pub async fn pause(&mut self, id: usize) -> Result<(), String> {
        let session = self.session.clone().ok_or("torrent 会话已经关闭")?;
        let handle = session
            .get(TorrentIdOrHash::Id(id))
            .ok_or("任务已经不在会话里")?;
        session.pause(&handle).await.map_err(friendly)
    }

    pub async fn resume(&mut self, id: usize) -> Result<(), String> {
        let session = self.session.clone().ok_or("torrent 会话已经关闭")?;
        let handle = session
            .get(TorrentIdOrHash::Id(id))
            .ok_or("任务已经不在会话里")?;
        session.unpause(&handle).await.map_err(friendly)
    }

    /// 从会话里删掉；`delete_files` 为 false 时已经下到的文件保留。
    pub async fn remove(&mut self, id: usize, delete_files: bool) -> Result<(), String> {
        let session = self.session.clone().ok_or("torrent 会话已经关闭")?;
        session
            .delete(TorrentIdOrHash::Id(id), delete_files)
            .await
            .map_err(friendly)
    }

    /// 停掉并丢掉整个会话（最后一个任务结束时调）。
    pub async fn shutdown(&mut self) {
        if let Some(s) = self.session.take() {
            s.stop().await;
        }
    }
}

fn friendly(e: impl std::fmt::Display) -> String {
    let msg = format!("{e:#}");
    if msg.contains("no known way to resolve peers") {
        return "这个磁力链接没有 tracker，关掉 DHT 后找不到 peer——换 .torrent 文件或带 tracker 的链接"
            .into();
    }
    msg
}

/// 种子里可能有 `..`、绝对路径或 Windows 不允许的字符；librqbit 不清洗，
/// 写盘前必须自己拦。宁可拒绝这个种子，也不能写到下载目录外面。
fn files_of(info: &TorrentMetaV1Info<ByteBufOwned>) -> Result<Vec<FileEntry>, String> {
    match &info.files {
        Some(files) if !files.is_empty() => {
            let mut out = Vec::with_capacity(files.len());
            for f in files {
                let parts: Vec<String> = f
                    .path
                    .iter()
                    .map(|p| String::from_utf8_lossy(p.as_ref()).into_owned())
                    .collect();
                for part in &parts {
                    check_component(part)?;
                }
                out.push(FileEntry {
                    name: parts.join("/"),
                    len: f.length,
                });
            }
            Ok(out)
        }
        _ => {
            let name = info
                .name
                .as_ref()
                .map(|n| String::from_utf8_lossy(n.as_ref()).into_owned())
                .unwrap_or_default();
            check_component(&name)?;
            Ok(vec![FileEntry {
                name: name.clone(),
                len: info.length.unwrap_or(0),
            }])
        }
    }
}

fn check_component(part: &str) -> Result<(), String> {
    if part.is_empty() || part == ".." || part.starts_with('/') || part.starts_with('\\') {
        return Err(format!("种子里的文件路径不安全：{part:?}，已拒绝"));
    }
    if part.contains(['\\', ':', '*', '?', '"', '<', '>', '|'])
        || part.chars().any(|c| c.is_control())
    {
        return Err(format!(
            "种子里的文件名在 Windows 上不能用：{part:?}，已拒绝"
        ));
    }
    if part.ends_with('.') || part.ends_with(' ') {
        return Err(format!("种子里的文件名以点或空格结尾：{part:?}，已拒绝"));
    }
    Ok(())
}

/// 种子名当目录名用，非法字符换掉；空名用 info-hash 兜底。
fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_string();
    if trimmed.is_empty() {
        "torrent".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_escaping_paths() {
        assert!(check_component("..").is_err());
        assert!(check_component("/etc/passwd").is_err());
        assert!(check_component(r"C:\x").is_err());
        assert!(check_component("a:b").is_err());
        assert!(check_component("ok.iso").is_ok());
        assert!(check_component("子目录").is_ok());
    }

    /// 本机种一个文件再下回来：走完 inspect/start/snapshot/shutdown，
    /// 也验证关掉 DHT 后 initial_peers 能把两个会话连起来。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_seed_and_download() {
        let root = std::env::temp_dir().join(format!("volant-torrent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let seed_dir = root.join("seed");
        let out_dir = root.join("out");
        std::fs::create_dir_all(&seed_dir).unwrap();
        std::fs::create_dir_all(&out_dir).unwrap();

        let data: Vec<u8> = (0..600_000u32).map(|i| (i % 251) as u8).collect();
        let file = seed_dir.join("blob.bin");
        std::fs::write(&file, &data).unwrap();

        let spawner = librqbit::spawn_utils::BlockingSpawner::new(2);
        let created = librqbit::create_torrent(
            &file,
            librqbit::CreateTorrentOptions {
                name: Some("blob.bin"),
                ..Default::default()
            },
            &spawner,
        )
        .await
        .unwrap();

        let mut seeder = Torrents::new(Options {
            listen: Some("127.0.0.1:0".parse().unwrap()),
            ..Default::default()
        });
        let insp = seeder
            .inspect(&Source::File(created.as_bytes().unwrap().to_vec()))
            .await
            .unwrap();
        assert_eq!(insp.name, "blob.bin");
        assert_eq!(insp.total, data.len() as u64);
        assert!(!insp.multi);
        assert!(insp.trackerless, "测试种子没有 tracker，应走 DHT");
        seeder.start(&insp, None, &seed_dir).await.unwrap();
        let addr = seeder.listen_addr().expect("做种会话应该在监听");

        let mut down = Torrents::new(Options {
            initial_peers: vec![addr],
            ..Default::default()
        });
        let insp2 = down
            .inspect(&Source::File(created.as_bytes().unwrap().to_vec()))
            .await
            .unwrap();
        let st = down.start(&insp2, None, &out_dir).await.unwrap();
        assert_eq!(st.dest, out_dir);

        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let snap = down.snapshot(st.id).expect("任务应该在会话里");
            if snap.finished && snap.downloaded >= data.len() as u64 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "等下载超时：downloaded={} total={} peers={} err={:?}",
                snap.downloaded,
                snap.total,
                snap.peers,
                snap.error
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert_eq!(std::fs::read(out_dir.join("blob.bin")).unwrap(), data);

        down.shutdown().await;
        seeder.shutdown().await;
        assert!(!down.alive() && !seeder.alive());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 多文件种子只勾一个：只要选中的那个文件，未选的不落盘。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subset_download_leaves_other_files_out() {
        let root = std::env::temp_dir().join(format!("volant-subset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let seed_dir = root.join("seed").join("pack");
        let out_dir = root.join("out");
        std::fs::create_dir_all(&seed_dir).unwrap();
        std::fs::create_dir_all(&out_dir).unwrap();
        // 文件大小取 16 KiB 的整数倍、片段也钉成 16 KiB：这样没有跨文件的片段，
        // “只下这个文件”才是干净的。否则一条片段横跨两个文件，两个文件都会被写。
        let a: Vec<u8> = (0..393_216u32).map(|i| (i % 239) as u8).collect();
        let b: Vec<u8> = (0..294_912u32).map(|i| (i % 241) as u8).collect();
        std::fs::write(seed_dir.join("a.bin"), &a).unwrap();
        std::fs::write(seed_dir.join("b.bin"), &b).unwrap();

        let spawner = librqbit::spawn_utils::BlockingSpawner::new(2);
        let created = librqbit::create_torrent(
            &seed_dir,
            librqbit::CreateTorrentOptions {
                name: Some("pack"),
                piece_length: Some(16_384),
                ..Default::default()
            },
            &spawner,
        )
        .await
        .unwrap();
        let bytes = created.as_bytes().unwrap().to_vec();

        let mut seeder = Torrents::new(Options {
            listen: Some("127.0.0.1:0".parse().unwrap()),
            ..Default::default()
        });
        let insp = seeder.inspect(&Source::File(bytes.clone())).await.unwrap();
        assert!(insp.multi);
        assert_eq!(insp.files.len(), 2);
        assert_eq!(insp.total, 688_128);
        assert!(insp.trackerless);
        seeder.start(&insp, None, &root.join("seed")).await.unwrap();
        let addr = seeder.listen_addr().expect("做种会话应该在监听");

        let mut down = Torrents::new(Options {
            initial_peers: vec![addr],
            ..Default::default()
        });
        let insp2 = down.inspect(&Source::File(bytes)).await.unwrap();
        let st = down.start(&insp2, Some(vec![0]), &out_dir).await.unwrap();
        assert_eq!(st.dest, out_dir.join("pack"));

        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let snap = down.snapshot(st.id).expect("任务应该在会话里");
            if snap.finished {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "等下载超时：downloaded={} total={} err={:?}",
                snap.downloaded,
                snap.total,
                snap.error
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let first = st.dest.join(&insp2.files[0].name);
        assert_eq!(std::fs::read(&first).unwrap(), a, "选中的文件要下对");
        // librqbit 的 filesystem storage 会给种子里每个文件都 create(true)，
        // 没勾的只留 0 字节占位，长度和内容都不会写。
        let second = st.dest.join(&insp2.files[1].name);
        let len = std::fs::metadata(&second).map(|m| m.len()).unwrap_or(0);
        assert_eq!(len, 0, "没勾的文件不该有数据：{second:?}");

        down.shutdown().await;
        seeder.shutdown().await;
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn safe_name_strips_path_separators() {
        assert_eq!(safe_name("a/b:c"), "a_b_c");
        assert_eq!(safe_name("  ..  "), "torrent");
        assert_eq!(safe_name("ubuntu-24.04.iso"), "ubuntu-24.04.iso");
    }
}
