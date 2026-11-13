//! 下载主循环：probe 之后的所有事——分段、多连接、稀疏写盘、状态落盘、续传、收尾。
//!
//! 结构：一个 supervisor（本文件的 [`run`]）+ N 个 worker + 一条写盘任务（[`Writer`]）。
//! - worker 之间分享三样东西：`Arc<Mutex<Planner>>`（谁写哪一段）、`PartFile`（同一个
//!   句柄的偏移写）、一个 `AtomicBool`（停）。
//! - worker 不直接写盘，只把批次交给写盘队列；进度与收尾只有 supervisor 做，worker 不
//!   碰事件通道，避免锁顺序问题。
//!
//! 磁盘写从 M2 起走队列（`writer.rs`）：慢盘只让队列变长，不会把 worker 卡在网络读取
//! 上，这是 T6「慢盘不等于慢连接」的前提。
//!
//! 进度每 `progress_every` 报一次。状态文件（`.volant.state`）每涨 [`STATE_STEP`] 字节
//! 就原子落一次盘，顺序是「取段表 → 写状态」——段表里只有 `commit` 过的字节，而 `commit`
//! 只在 `write_at` 成功之后发生，所以状态文件永远不会说「有」而盘上「没有」。排空写队列
//! 只在要 `sync_data` 时才做（每 `sync_every` 秒），因为排空一次要等 2 MiB 写完，按 1 MiB
//! 记账的节奏排空等于把吞吐砍掉一半。数据本身只每 `sync_every` 秒 `sync_data` 一次。这
//! 不是偷懒，是量出来的：10 GiB 下载里每秒 `sync_data` 一次要吃掉 40% 吞吐
//! （1052 → 1726 MB/s）。PRD 6.3 只要求 `kill -9` 后不重下、丢失 ≤1 MiB，而 kill -9 不
//! 影响页缓存，状态跟上就够了；代价是断电/蓝屏可能留下「状态说有、盘上没有」的洞，M2 用
//! 分段 CRC 兜（见 `docs/m1/REPORT.md`）。

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::header::{CONTENT_RANGE, IF_RANGE, RANGE};
use reqwest::{Client, StatusCode, Url};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};

use fs2::FileExt;

use crate::Error;
use crate::disk;
use crate::hosts;
use crate::limiter::Limiter;
use crate::part::PartFile;
use crate::planner::{Assignment, Claim, Planner, WorkerId};
use crate::probe::{self, Probe};
use crate::state::{self, ResumeState, StateWriter};
use crate::trace::Trace;
use crate::writer::{Ledger, Writer};

/// 连续失败多少次就放弃（不是每个故障都值得无限重试）。
const MAX_RETRIES: u32 = 8;

/// 状态文件记录的进度最多落后这么多——也就是 `kill -9` 之后最多重下这么多字节。
const STATE_STEP: u64 = 1 << 20;

/// 分段模式下最多重起几轮 worker。每轮按「学到的连接上限」起，所以 8→4→2 正好三轮收敛。
const MAX_ROUNDS: usize = 3;
const MAX_CONNECTIONS: usize = 32;

pub struct Config {
    pub url: Url,
    /// 下载目录。
    pub dir: PathBuf,
    /// 指定的最终文件路径；`None` = 用服务器给的名字，重名自动加 `(1)`。
    pub dest: Option<PathBuf>,
    /// 并发连接数。
    pub connections: usize,
    /// 对半切之后每一半的最小长度。
    pub min_split: u64,
    /// 每个 worker 攒多少字节写一次盘。
    pub batch: usize,
    /// 状态文件最长的落盘间隔。真正决定它的是 `state_step`：进度每涨这么多
    /// 就落一次，这个时间只是兜底。
    pub flush_every: Duration,
    /// 状态文件允许落后的最大字节数。默认 `STATE_STEP`（1 MiB），也就是 PRD 6.3 说的
    /// 「kill -9 后丢失 ≤1 MiB」；调大换吞吐，调小换安全。
    pub state_step: u64,
    /// 多久 `sync_data` 一次。比状态落盘稀疏得多，理由见模块文档。
    pub sync_every: Duration,
    /// 多久报一次进度。
    pub progress_every: Duration,
    /// 全局下载限速（字节/秒），`None` = 不限。所有连接共享这一个额度。
    pub rate_limit: Option<u64>,
    /// 落盘吞吐上限（字节/秒），`None` = 不限。**只用来仿真慢盘**（T6），真实磁盘上开
    /// 它等于无故拖慢。
    pub disk_rate: Option<u64>,
    /// 多久拿不到响应头就认为这条连接死了。
    pub first_byte_timeout: Duration,
    /// 下载中多久收不到一个字节就认为这条连接卡住了（让出它的段，不算失败）。
    pub stall_timeout: Duration,
    /// 一条连接至少跑了这么久，才允许判它「慢」（太快下结论会把 TLS 握手当慢连接）。
    pub shed_after: Duration,
    /// 比最快的那条连接慢几倍才算「慢」。取值理由见 [`Shared::shed_reason`]。
    pub shed_ratio: u32,
    /// 主机连接上限的记忆文件；`None` = 只在内存里记（跨任务有效，跨进程无效）。
    pub host_file: Option<PathBuf>,
    /// 打开 `--trace`：跑完往 stderr 打一份「时间花在哪」的账。默认关。
    pub trace: bool,
    /// 每个请求额外带的头（浏览器捕获来的 `Cookie` / `Referer` / `User-Agent`）。
    /// 探测和分段请求都用这一份；名字/值不合法的会被丢掉，不会 panic。
    pub headers: Vec<(String, String)>,
}

impl Config {
    pub fn new(url: Url, dir: impl Into<PathBuf>) -> Self {
        Config {
            url,
            dir: dir.into(),
            dest: None,
            connections: 8,
            min_split: 12 << 10,
            batch: 256 << 10,
            flush_every: Duration::from_secs(1),
            state_step: STATE_STEP,
            sync_every: Duration::from_secs(30),
            progress_every: Duration::from_millis(250),
            rate_limit: None,
            disk_rate: None,
            first_byte_timeout: Duration::from_secs(10),
            stall_timeout: Duration::from_secs(20),
            shed_after: Duration::from_secs(2),
            shed_ratio: 4,
            host_file: None,
            trace: false,
            headers: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Event {
    Started {
        dest: PathBuf,
        size: u64,
        filename: String,
        ranges: bool,
        resumed_from: u64,
    },
    Progress {
        downloaded: u64,
        total: u64,
        connections: usize,
        elapsed: Duration,
    },
    Done {
        path: PathBuf,
        elapsed: Duration,
    },
    Failed {
        reason: String,
    },
}

#[derive(Clone, Debug)]
pub struct Summary {
    pub dest: PathBuf,
    pub size: u64,
    pub resumed_from: u64,
    pub elapsed: Duration,
}

/// 控制一个在跑的下载。
#[derive(Clone)]
pub struct Handle {
    stop: Arc<AtomicBool>,
}

impl Handle {
    /// 停下并保住已完成的进度（`.volant.state` 会写最后一次）。之后可以用同一个 URL 续传。
    pub fn cancel(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub struct Download {
    pub handle: Handle,
    pub events: mpsc::Receiver<Event>,
    task: Option<JoinHandle<Result<Summary, Error>>>,
}

impl Download {
    /// 启动。probe 由调用者先做——它决定文件名与大小，调用者通常要先显示出来。
    pub fn start(client: Client, cfg: Config, probe: Probe) -> Download {
        let (tx, events) = mpsc::channel(64);
        let stop = Arc::new(AtomicBool::new(false));
        let handle = Handle { stop: stop.clone() };
        let task = tokio::spawn(async move { run(client, cfg, probe, tx, stop).await });
        Download {
            handle,
            events,
            task: Some(task),
        }
    }

    /// 等它跑完（顺带把进度事件排掉，免得通道满了把 supervisor 堵住）。
    pub async fn wait(mut self) -> Result<Summary, Error> {
        let task = self.task.take().expect("wait 只能调用一次");
        let drain = async move { while self.events.recv().await.is_some() {} };
        let (joined, ()) = tokio::join!(task, drain);
        match joined {
            Ok(result) => result,
            Err(e) => Err(Error::Internal(format!("下载任务崩了: {e}"))),
        }
    }
}

impl Drop for Download {
    fn drop(&mut self) {
        // 调用者把 Download 丢了（或提前退出），别让任务自己跑下去。
        if self.task.is_some() {
            self.handle.cancel();
        }
    }
}

struct PreparedDestination {
    dest: PathBuf,
    part: PartFile,
    planner: Option<Planner>,
    resumed_from: u64,
    _lock: DestinationLock,
}

/// 进程存活期间锁住一个目标名。OS 会在进程退出时释放锁，崩溃不会留下永久占位。
struct DestinationLock {
    file: Option<File>,
    path: PathBuf,
}

impl Drop for DestinationLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = FileExt::unlock(&file);
            drop(file);
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

fn try_lock_destination(dest: &Path) -> Result<Option<DestinationLock>, Error> {
    let path = state::lock_path(dest);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(Error::Io)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(DestinationLock {
            file: Some(file),
            path,
        })),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.raw_os_error() == Some(33) => {
            Ok(None)
        }
        Err(e) => Err(Error::Io(e)),
    }
}

/// 为一次下载确定目标，并原子占住对应的 `.volant.part`。
///
/// 只检查最终文件名不够：暂停或正在下载的任务还没有最终文件，但 `.volant.part`
/// 和 `.volant.state` 已经存在。这里同时检查三项，并用 `create_new` 竞争占位，
/// 保证两个同名任务不会在 `exists()` 与创建之间同时选中同一个名字。
fn prepare_destination(
    cfg: &Config,
    probe: &Probe,
    paged: bool,
    size: Option<u64>,
) -> Result<PreparedDestination, Error> {
    if let Some(dest) = &cfg.dest {
        return prepare_explicit_destination(cfg, probe, paged, size, dest.clone());
    }

    let preferred = probe.filename.as_deref().unwrap_or("download");
    for n in 0..10_000u32 {
        let dest = cfg.dir.join(destination_name(preferred, n));
        if dest.exists() {
            continue;
        }
        let Some(lock) = try_lock_destination(&dest)? else {
            // 同名任务正在运行，即使 URL 相同也不能把它当成自己的续传状态。
            continue;
        };

        let part_path = state::part_path(&dest);
        let state_path = state::state_path(&dest);
        if paged
            && state_path.exists()
            && let Some(planner) =
                resume_planner(cfg, probe, size.unwrap_or(0), &part_path, &state_path)
        {
            let resumed_from = planner.downloaded();
            check_space_for(&dest, cfg.dir.as_path(), size, resumed_from)?;
            let part = PartFile::open(&part_path, size, false).map_err(Error::Io)?;
            return Ok(PreparedDestination {
                dest,
                part,
                planner: Some(planner),
                resumed_from,
                _lock: lock,
            });
        }

        // 有 part 或 state 却没有一份可安全续传的状态：这是另一个任务的占位，
        // 也可能是坏状态留下的残骸。两种情况下都不应覆盖它。
        if part_path.exists() || state_path.exists() {
            continue;
        }

        check_space_for(&dest, cfg.dir.as_path(), size, 0)?;
        match PartFile::create_new(&part_path, size) {
            Ok(part) => {
                return Ok(PreparedDestination {
                    dest,
                    part,
                    planner: None,
                    resumed_from: 0,
                    _lock: lock,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Error::Io(e)),
        }
    }

    Err(Error::Internal(format!(
        "目录 {} 里找不到可用的目标文件名",
        cfg.dir.display()
    )))
}

/// 续传（或失败重试）已经有明确目标，只能复用这一个名字。
fn prepare_explicit_destination(
    cfg: &Config,
    probe: &Probe,
    paged: bool,
    size: Option<u64>,
    dest: PathBuf,
) -> Result<PreparedDestination, Error> {
    let lock = try_lock_destination(&dest)?
        .ok_or_else(|| Error::Internal(format!("目标 {} 正在被另一个任务使用", dest.display())))?;
    let part_path = state::part_path(&dest);
    let state_path = state::state_path(&dest);
    let planner = paged
        .then(|| resume_planner(cfg, probe, size.unwrap_or(0), &part_path, &state_path))
        .flatten();
    let resumed_from = planner.as_ref().map_or(0, Planner::downloaded);
    check_space_for(&dest, cfg.dir.as_path(), size, resumed_from)?;
    let truncate = planner.is_none();
    let part = PartFile::open(&part_path, size, truncate).map_err(Error::Io)?;
    Ok(PreparedDestination {
        dest,
        part,
        planner,
        resumed_from,
        _lock: lock,
    })
}

fn resume_planner(
    cfg: &Config,
    probe: &Probe,
    size: u64,
    part_path: &Path,
    state_path: &Path,
) -> Option<Planner> {
    let saved = ResumeState::load(state_path).ok()?;
    if !saved.matches(
        cfg.url.as_str(),
        probe.final_url.as_str(),
        size,
        probe.etag.as_deref(),
        probe.last_modified.as_deref(),
    ) || !part_is_whole(part_path, size)
    {
        return None;
    }
    Planner::resume(size, cfg.min_split, &saved.segments).ok()
}

fn check_space_for(
    dest: &Path,
    fallback_dir: &Path,
    size: Option<u64>,
    resumed_from: u64,
) -> Result<(), Error> {
    let Some(size) = size else {
        return Ok(());
    };
    let dir = dest.parent().unwrap_or(fallback_dir);
    disk::check_space(dir, size - resumed_from).map_err(Error::NoSpace)
}

async fn run(
    client: Client,
    cfg: Config,
    probe: Probe,
    tx: mpsc::Sender<Event>,
    stop: Arc<AtomicBool>,
) -> Result<Summary, Error> {
    hosts::bind(cfg.host_file.clone());
    let started = Instant::now();
    let size = probe.size;
    let paged = probe.ranges && size.is_some_and(|s| s > 0);

    tokio::fs::create_dir_all(&cfg.dir)
        .await
        .map_err(Error::Io)?;

    // ---- 目标路径、原子占用与续传判定 ----
    let PreparedDestination {
        dest,
        part,
        planner,
        resumed_from,
        _lock,
    } = prepare_destination(&cfg, &probe, paged, size)?;
    let state_path = state::state_path(&dest);
    let part = Arc::new(part);

    let filename = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".into());
    let _ = tx
        .send(Event::Started {
            dest: dest.clone(),
            size: size.unwrap_or(0),
            filename,
            ranges: probe.ranges,
            resumed_from,
        })
        .await;

    // ---- 干活 ----
    let trace = cfg.trace.then(|| Arc::new(Trace::new()));
    let result = if paged {
        let state_writer = StateWriter::create(&state_path).map_err(Error::Io)?;
        let size = size.unwrap_or(0);
        let host = host_key(&probe.final_url);
        // 学到的上限不能超过用户要的，也不该低于 1。
        let connection_budget = bounded_connections(cfg.connections);
        let conns = hosts::cap(&host)
            .unwrap_or(connection_budget)
            .clamp(1, connection_budget);
        let mut planner = planner.unwrap_or_else(|| Planner::new(size, cfg.min_split));
        // 一上来就按连接数切好：让每个 worker 各自有一段，而不是 8 条连接抢同一段再互相
        // 切——每切一次都要中止一条已经在传的响应，已经进了 socket 缓冲的字节就白发了。
        planner.presplit(conns);
        let planner = Arc::new(Mutex::new(planner));
        let saved = Arc::new(AtomicU64::new(planner.lock().unwrap().downloaded()));
        let state = ResumeState::new(
            cfg.url.as_str(),
            probe.final_url.as_str(),
            dest.clone(),
            size,
            probe.etag.clone(),
            probe.last_modified.clone(),
            probe.content_type.clone(),
        );
        // 写盘任务每攒够四分之一的 `state_step` 就自己记一次账。四分之一而不是一半：
        // 状态文件是双槽轮换的，进程恰好死在写槽中间时只能回退到上一个槽，实际损失会是
        // 「两个记账步长 + 一个批次」，四分之一才把最坏情况压在 `state_step` 以内。
        let ledger = Arc::new(Ledger::new(
            (cfg.state_step / 4).max(1),
            saved.clone(),
            state_writer,
            state,
        ));
        let writer = Writer::spawn(
            part.clone(),
            planner.clone(),
            cfg.disk_rate,
            Some(ledger),
            trace.clone(),
        );
        let shared = Arc::new(Shared {
            client: client.clone(),
            url: probe.final_url.clone(),
            if_range: probe.etag.clone().or_else(|| probe.last_modified.clone()),
            planner,
            writer,
            stop: stop.clone(),
            fatal: Mutex::new(None),
            host,
            conns: AtomicUsize::new(conns),
            best_rate: AtomicU64::new(0),
            limiter: cfg.rate_limit.map(|r| Arc::new(Limiter::new(r))),
            batch: cfg.batch,
            first_byte_timeout: cfg.first_byte_timeout,
            stall_timeout: cfg.stall_timeout,
            shed_after: cfg.shed_after,
            shed_ratio: cfg.shed_ratio,
            trace: trace.clone(),
            headers: cfg.headers.clone(),
        });
        let finished = run_paged(&cfg, &probe, &shared, &tx, started).await;
        if finished.is_ok() {
            // 完整下完一次：把这台主机的上限 +1 试探回去（下次可能能多开几条）。
            hosts::succeeded(&shared.host, cfg.connections);
        }
        finished
    } else {
        run_stream(&client, &cfg, &probe, &part, &tx, &stop, started).await
    };

    // ---- 收尾 ----
    if let Some(trace) = &trace {
        trace.report(started.elapsed());
    }
    if let Err(e) = result {
        part.sync().ok();
        let _ = tx
            .send(Event::Failed {
                reason: e.to_string(),
            })
            .await;
        return Err(e);
    }

    part.sync().map_err(Error::Io)?;
    part.finish();
    let from = part.path().to_path_buf();
    drop(part);
    // 万一在我们下载期间有人往这个路径写了文件：改名而不是覆盖（PRD 6.3「绝不覆盖」）。
    let dest = if dest.exists() {
        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        pick_free_name(&cfg.dir, &name)
    } else {
        dest
    };
    std::fs::rename(&from, &dest).map_err(Error::Io)?;
    let _ = std::fs::remove_file(&state_path);

    let elapsed = started.elapsed();
    // 顺序模式下 probe 拿不到长度，真正的长度就是落盘文件的大小。
    let size = size.unwrap_or_else(|| std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0));
    // 收尾补一次 100% 进度：本地这种毫秒级完成的下载根本等不到 ticker。
    let _ = tx.try_send(Event::Progress {
        downloaded: size,
        total: size,
        connections: 0,
        elapsed,
    });
    let summary = Summary {
        dest: dest.clone(),
        size,
        resumed_from,
        elapsed,
    };
    let _ = tx
        .send(Event::Done {
            path: dest,
            elapsed,
        })
        .await;
    Ok(summary)
}

/// part 文件的长度必须和状态里说的一致，否则盘上的东西不可信。
fn part_is_whole(part: &Path, size: u64) -> bool {
    std::fs::metadata(part).is_ok_and(|m| m.len() == size)
}

/// 主机连接上限记忆的键。带上端口——同一台机器的不同端口是不同的服务，混在一起会让
/// 一个服务的 503 影响另一个服务（测试里尤其明显）。
fn host_key(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port_or_known_default() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
}

// ---------------------------------------------------------------- 分段模式

struct Shared {
    client: Client,
    url: Url,
    if_range: Option<String>,
    planner: Arc<Mutex<Planner>>,
    writer: Writer,
    stop: Arc<AtomicBool>,
    fatal: Mutex<Option<Error>>,
    /// `host:port`，主机连接上限记忆的键。
    host: String,
    /// 这一轮允许开的连接数：先按学到的上限给，被 503 拒绝后再降。worker 会按它决定
    /// 是不是该让位。
    conns: AtomicUsize,
    /// 观测到的最快连接速率（字节/秒），慢连接判定的分母。
    best_rate: AtomicU64,
    limiter: Option<Arc<Limiter>>,
    batch: usize,
    first_byte_timeout: Duration,
    stall_timeout: Duration,
    shed_after: Duration,
    shed_ratio: u32,
    trace: Option<Arc<Trace>>,
    headers: Vec<(String, String)>,
}

impl Shared {
    fn fail(&self, e: Error) {
        self.fatal.lock().unwrap().get_or_insert(e);
        self.stop.store(true, Ordering::Relaxed);
    }

    /// 这条连接该不该把它的段让出来？返回给人看的理由。
    ///
    /// 只看**网络侧**速率：`net` 是等 chunk 的时间总和，写队列的等待一刻都没算进来。
    /// 这一条是 T6 的全部要害——慢盘会让每秒落盘的字节变少，但完全不改变网络侧速率，
    /// 所以慢盘永远不会被误判成慢连接。
    ///
    /// 三个门槛：
    /// 1. 至少跑满 `shed_after`（默认 2 秒）才下结论；
    /// 2. 比观察到的最快连接还慢 `shed_ratio` 倍（默认 4 倍）以上。自己就是最快的 → 比值
    ///    ≤ 1，永远不让；
    /// 3. 剩下的活按自己的速度 2 秒内能干完 → 不值得再花一次请求。
    ///
    /// 4 倍这个数抄的是「慢连接让位」的常见量级（MDM 用 4），但我们只在网络侧测速，
    /// 而且加了「自己最快就不让」这条，慢盘和单连接场景天然不会触发。
    fn shed_reason(&self, got: u64, net: Duration, a: &Assignment, pos: u64) -> Option<String> {
        if net < self.shed_after || got == 0 {
            return None;
        }
        let rate = got as f64 / net.as_secs_f64();
        let best = self
            .best_rate
            .fetch_max(rate as u64, Ordering::Relaxed)
            .max(rate as u64) as f64;
        if rate * self.shed_ratio as f64 >= best {
            return None;
        }
        if a.to.saturating_sub(pos) as f64 / rate < 2.0 {
            return None;
        }
        Some(format!(
            "这条连接 {:.2} MB/s，最快的那条 {:.2} MB/s",
            rate / 1e6,
            best / 1e6
        ))
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_paged(
    cfg: &Config,
    probe: &Probe,
    shared: &Arc<Shared>,
    tx: &mpsc::Sender<Event>,
    started: Instant,
) -> Result<(), Error> {
    let size = probe.size.unwrap_or(0);

    // worker 全退出但还没下完，说明是「没人愿意再切」而不是「干完了」，再起一轮；
    // 每轮按最新的连接上限起（503 会把上限压下来，见 `Shared::conns`），三轮还不动
    // 就是真卡住了。
    for _round in 0..MAX_ROUNDS {
        let conns = shared
            .conns
            .load(Ordering::Relaxed)
            .clamp(1, bounded_connections(cfg.connections));
        let mut set = JoinSet::new();
        for id in 0..conns as u32 {
            let shared = shared.clone();
            set.spawn(async move { worker(id, shared).await });
        }

        let mut ticker = tokio::time::interval(cfg.progress_every);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_flush = Instant::now();
        let mut last_sync = Instant::now();
        loop {
            tokio::select! {
                joined = set.join_next() => match joined {
                    None => break,
                    Some(Err(e)) => {
                        // worker 崩了：先把队列里的字节收干净再走，别丢已经收到的东西。
                        flush(shared, true).await.ok();
                        return Err(Error::Internal(format!("worker 崩了: {e}")));
                    }
                    Some(Ok(())) => {}
                },
                _ = ticker.tick() => {
                    let (downloaded, busy) = {
                        let p = shared.planner.lock().unwrap();
                        (p.downloaded(), p.busy_workers())
                    };
                    let _ = tx.try_send(Event::Progress {
                        downloaded,
                        total: size,
                        connections: busy,
                        elapsed: started.elapsed(),
                    });
                    // 周期记账已经由写盘任务按半拍做掉了（见 `Ledger`），这里只管
                    // 定时落一次盘：每秒记一次，每 `sync_every` 再 `fsync` 一次。
                    if last_flush.elapsed() >= cfg.flush_every {
                        let sync = last_sync.elapsed() >= cfg.sync_every;
                        flush(shared, sync).await?;
                        last_flush = Instant::now();
                        if sync {
                            last_sync = Instant::now();
                        }
                    }
                }
            }
        }

        // 先把 fatal 取出来再 await：MutexGuard 不能跨 await（不然整个任务不是 Send）。
        let fatal = shared.fatal.lock().unwrap().take();
        if let Some(e) = fatal {
            flush(shared, true).await.ok();
            return Err(e);
        }
        if shared.stop.load(Ordering::Relaxed) {
            flush(shared, true).await?;
            return Err(Error::Cancelled);
        }
        if shared.planner.lock().unwrap().is_complete() {
            break;
        }
    }

    if !shared.planner.lock().unwrap().is_complete() {
        return Err(Error::Stalled);
    }
    // 收尾还有一次 sync（见 run 的收尾段），这里不用重复。
    flush(shared, false).await?;
    Ok(())
}

/// 把段表拍一次写进状态文件，顺带把写队列排空（`sync` 时）。
///
/// 顺序不能颠倒：状态文件说「这些字节在盘上」，就必须真的在——`commit` 只在 `write_at`
/// 成功之后发生，段表里的字节一定已经交给操作系统了。`sync` 时才先排空写队列再
/// `sync_data`，那是给断电准备的，不是给 `kill -9` 准备的（kill -9 之后页缓存还在）。
async fn flush(shared: &Shared, sync: bool) -> Result<(), Error> {
    if sync {
        shared.writer.barrier(true).await?;
    }
    shared.writer.record(&shared.planner, sync)
}

/// 一个 worker 的一生：领活 → 取一段 → 走完或失败 → 再领活。
async fn worker(id: WorkerId, shared: Arc<Shared>) {
    let mut after: Option<u64> = None;
    let mut failures = 0u32;
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }
        let assignment = shared.planner.lock().unwrap().assign(id, after);
        let Some(a) = assignment else {
            return;
        };
        match fetch(id, a, &shared).await {
            Ok(pos) => {
                if pos == a.from && a.to > a.from {
                    // 一个字节都没拿到，却也不算错：当作一次失败，防止空转。
                    failures += 1;
                } else {
                    failures = 0;
                }
                after = if pos >= a.to { Some(a.to) } else { None };
                if failures > MAX_RETRIES {
                    shared.fail(Error::Stalled);
                    return;
                }
            }
            Err(FetchError::Retry { reason, progress }) => {
                shared.planner.lock().unwrap().release(id);
                // 有进展就不算「连续失败」：T5 那种「每 2 MiB 被服务器掐一次」的下载，
                // 如果按次数记，8 段没走完就先放弃了。只有一点都没拿到才退避，免得空转。
                if progress > 0 {
                    failures = 0;
                } else {
                    failures += 1;
                    tokio::time::sleep(backoff(failures)).await;
                }
                if failures > MAX_RETRIES {
                    shared.fail(Error::Http(format!("连续 {MAX_RETRIES} 次失败: {reason}")));
                    return;
                }
                after = None;
            }
            Err(FetchError::Busy(reason)) => {
                shared.planner.lock().unwrap().release(id);
                // 上限已经被 `hosts::refused` 压下来了。现在还超着就本轮退出，让
                // supervisor 按新上限重新起 worker，而不是继续拿被拒的连接空转。
                let cap = shared.conns.load(Ordering::Relaxed);
                let over = shared.planner.lock().unwrap().busy_workers() >= cap;
                if over {
                    return;
                }
                failures += 1;
                if failures > MAX_RETRIES {
                    shared.fail(Error::Http(format!(
                        "连续 {MAX_RETRIES} 次被服务器拒绝: {reason}"
                    )));
                    return;
                }
                after = None;
                tokio::time::sleep(backoff(failures)).await;
            }
            Err(FetchError::Shed(_why)) => {
                // 慢连接让位：段交回 planner（变成「无人认领」），别的连接会更早拿到它。
                // 不算连续失败——让位是主动决策，不是故障。
                shared.planner.lock().unwrap().release(id);
                after = None;
            }
            Err(FetchError::Fatal(e)) => {
                shared.fail(e);
                return;
            }
        }
    }
}

fn backoff(failures: u32) -> Duration {
    let ms = 50u64.saturating_mul(1 << failures.min(6));
    Duration::from_millis(ms.min(2000))
}

enum FetchError {
    /// 可重试：`progress` 是这次已经提交的字节数，用来区分「真卡住」和「被掐了一下」。
    Retry {
        reason: String,
        progress: u64,
    },
    /// 服务器说连接太多（503/429）。上限已经被压下来了，worker 可能要让位退出这一轮。
    Busy(String),
    /// 这条连接太慢或者卡住了：段让回去，不算失败，也不算让位次数。
    Shed(String),
    Fatal(Error),
}

impl FetchError {
    /// 还没开始就失败（连不上、超时），一个字节都没拿到。
    fn retry(reason: impl std::fmt::Display) -> Self {
        FetchError::Retry {
            reason: reason.to_string(),
            progress: 0,
        }
    }
}

/// 取 `[a.from, a.to)`，边收边写边提交。返回**已经提交到盘上**的位置。
async fn fetch(id: WorkerId, a: Assignment, shared: &Shared) -> Result<u64, FetchError> {
    let mut req = crate::with_headers(shared.client.get(shared.url.clone()), &shared.headers)
        .header(RANGE, format!("bytes={}-{}", a.from, a.to - 1));
    if let Some(validator) = &shared.if_range {
        req = req.header(IF_RANGE, validator);
    }
    // 连响应头都不给，多半是这条连接已经死了：让段出来比干等强。
    let mut resp = match tokio::time::timeout(shared.first_byte_timeout, req.send()).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => return Err(FetchError::retry(e)),
        Err(_) => {
            return Err(FetchError::retry(format!(
                "{} 秒没有响应头",
                shared.first_byte_timeout.as_secs_f64()
            )));
        }
    };

    match resp.status() {
        StatusCode::PARTIAL_CONTENT => {
            // 起点不对 = 服务器没按我们要求的位置给数据，写下去就是错位。
            if let Some(cr) = resp
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                && let Some(start) = probe::start_of_content_range(cr)
                && start != a.from
            {
                return Err(FetchError::Fatal(Error::SourceChanged));
            }
        }
        StatusCode::OK => {
            // probe 说支持分段，真的发了 Range 却回整个文件：源文件变了（If-Range 不匹配
            // 时服务器必须这样回）。即使请求的是 0 起点也不能用，因为新版本的字节和已经
            // 落盘的部分未必一致。
            return Err(FetchError::Fatal(Error::SourceChanged));
        }
        StatusCode::SERVICE_UNAVAILABLE | StatusCode::TOO_MANY_REQUESTS => {
            // 服务器说连接太多：记住「要更少」，这一轮可能要按新上限重来。
            let attempted = shared.conns.load(Ordering::Relaxed);
            if let Some(fewer) = hosts::refused(&shared.host, attempted) {
                shared.conns.store(fewer, Ordering::Relaxed);
            }
            return Err(FetchError::Busy(format!("服务器拒绝: {}", resp.status())));
        }
        StatusCode::RANGE_NOT_SATISFIABLE => {
            return Err(FetchError::Fatal(Error::Http(
                "服务器拒绝了这个字节区间".into(),
            )));
        }
        other => return Err(FetchError::Fatal(Error::Status(other.as_u16()))),
    }

    let mut pos = a.from;
    let mut buf: Vec<u8> = Vec::with_capacity(shared.batch);
    let mut lost = false;
    // 只累加「等 chunk」的时间：写队列的等待一刻都不算，否则慢盘会被当成慢连接（T6）。
    let mut net = Duration::ZERO;
    let mut got = 0u64;
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            break;
        }
        let waiting = Instant::now();
        let cpu0 = shared.trace.as_ref().map_or(0, |_| crate::trace::cpu_ns());
        let chunk = match tokio::time::timeout(shared.stall_timeout, resp.chunk()).await {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(e)) => {
                // 已经收到的字节先落盘，再从 pos 接着要（T5 的断连就走这条路）。
                flush_partial(id, shared, &mut buf, pos).await?;
                return Err(FetchError::Retry {
                    reason: format!("读取中断: {e}"),
                    progress: pos - a.from,
                });
            }
            Err(_) => {
                // 卡住了：不是错，只是这条连接不值得再等。让段出来给别的连接。
                flush_partial(id, shared, &mut buf, pos).await?;
                return Err(FetchError::Shed(format!(
                    "{} 秒没收到字节",
                    shared.stall_timeout.as_secs_f64()
                )));
            }
        };
        net += waiting.elapsed();
        if let Some(trace) = &shared.trace {
            trace.chunk(
                chunk.len(),
                waiting.elapsed(),
                crate::trace::cpu_ns().saturating_sub(cpu0),
            );
        }
        got += chunk.len() as u64;
        let granted = {
            let mut planner = shared.planner.lock().unwrap();
            match planner.claim(id, pos, chunk.len() as u64) {
                Claim::Granted(n) => n,
                Claim::Lost => {
                    lost = true;
                    0
                }
            }
        };
        if lost || granted == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..granted as usize]);
        pos += granted;
        let cut = granted < chunk.len() as u64;
        if cut || buf.len() >= shared.batch {
            write_and_queue(id, shared, &mut buf, pos).await?;
        }
        if cut {
            break;
        }
        // 限速：拿到下一块的额度之后才去读它。等待期间服务端因为接收窗口满而自己减速，
        // 所以限速是「反压」而不是「丢弃」。
        if let Some(limiter) = &shared.limiter {
            let cpu0 = shared.trace.as_ref().map_or(0, |_| crate::trace::cpu_ns());
            let asked = limiter.take(chunk.len() as u64).await;
            if let Some(trace) = &shared.trace {
                trace.take(asked, crate::trace::cpu_ns().saturating_sub(cpu0));
            }
        }
        if let Some(why) = shared.shed_reason(got, net, &a, pos) {
            flush_partial(id, shared, &mut buf, pos).await?;
            return Err(FetchError::Shed(why));
        }
    }
    // 收尾：缓冲里剩下的字节（可能一个都没有），然后等它们真的到盘上。
    flush_partial(id, shared, &mut buf, pos).await?;
    Ok(pos)
}

/// 把缓冲里的字节交给写盘队列，并等队列排空——返回之后 `pos` 之前的字节才真的在盘上。
async fn flush_partial(
    id: WorkerId,
    shared: &Shared,
    buf: &mut Vec<u8>,
    pos: u64,
) -> Result<(), FetchError> {
    write_and_queue(id, shared, buf, pos).await?;
    shared
        .writer
        .barrier(false)
        .await
        .map_err(FetchError::Fatal)
}

/// 把缓冲交给写盘队列就回来（不等待）。队列满时在这里等——那是磁盘对网络的反压。
async fn write_and_queue(
    id: WorkerId,
    shared: &Shared,
    buf: &mut Vec<u8>,
    pos: u64,
) -> Result<(), FetchError> {
    if buf.is_empty() {
        return Ok(());
    }
    let at = pos - buf.len() as u64;
    let batch = std::mem::replace(buf, Vec::with_capacity(shared.batch));
    let queued = Instant::now();
    shared
        .writer
        .queue(id, at, pos, batch)
        .await
        .map_err(FetchError::Fatal)?;
    if let Some(trace) = &shared.trace {
        trace.queue(queued.elapsed());
    }
    Ok(())
}

// ---------------------------------------------------------------- 顺序模式

/// 服务器不支持 Range，或没给长度：只能从头到尾一条连接顺着写，没有续传。
async fn run_stream(
    client: &Client,
    cfg: &Config,
    probe: &Probe,
    part: &PartFile,
    tx: &mpsc::Sender<Event>,
    stop: &Arc<AtomicBool>,
    started: Instant,
) -> Result<(), Error> {
    let mut resp = crate::with_headers(client.get(probe.final_url.clone()), &cfg.headers)
        .send()
        .await
        .map_err(|e| Error::Http(e.to_string()))?;
    if resp.status() != StatusCode::OK {
        return Err(Error::Status(resp.status().as_u16()));
    }
    let total = probe.size.unwrap_or(0);
    let mut pos = 0u64;
    let mut buf: Vec<u8> = Vec::with_capacity(cfg.batch);
    let mut ticker = tokio::time::interval(cfg.progress_every);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // 第一次 tick 会立刻返回，别让它把第一次真正上报挤掉。
    ticker.tick().await;

    loop {
        if stop.load(Ordering::Relaxed) {
            part.sync().map_err(Error::Io)?;
            return Err(Error::Cancelled);
        }
        tokio::select! {
            chunk = resp.chunk() => match chunk {
                Ok(Some(c)) => {
                    buf.extend_from_slice(&c);
                    pos += c.len() as u64;
                    if buf.len() >= cfg.batch {
                        part.write_at(pos - buf.len() as u64, &buf).map_err(Error::Io)?;
                        buf.clear();
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    if !buf.is_empty() {
                        part.write_at(pos - buf.len() as u64, &buf).map_err(Error::Io)?;
                        buf.clear();
                    }
                    part.sync().map_err(Error::Io)?;
                    return Err(Error::Http(format!(
                        "连接中断: {e}（服务器不支持分段，这一份没法续传）"
                    )));
                }
            },
            _ = ticker.tick() => {
                let _ = tx.try_send(Event::Progress {
                    downloaded: pos,
                    total,
                    connections: 1,
                    elapsed: started.elapsed(),
                });
            }
        }
    }

    if !buf.is_empty() {
        part.write_at(pos - buf.len() as u64, &buf)
            .map_err(Error::Io)?;
    }
    part.sync().map_err(Error::Io)?;
    let _ = tx.try_send(Event::Progress {
        downloaded: pos,
        total: total.max(pos),
        connections: 1,
        elapsed: started.elapsed(),
    });
    Ok(())
}

/// 重名不覆盖：`a.zip` 已存在就试 `a (1).zip`、`a (2).zip`……
pub fn pick_free_name(dir: &Path, name: &str) -> PathBuf {
    for n in 0..10_000u32 {
        let candidate = dir.join(destination_name(name, n));
        if !destination_is_taken(&candidate) {
            return candidate;
        }
    }
    dir.join(destination_name(
        name,
        (std::process::id() as u32).wrapping_add(10_000),
    ))
}

fn destination_name(name: &str, n: u32) -> String {
    if n == 0 {
        return name.to_owned();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    format!("{stem} ({n}){ext}")
}

fn bounded_connections(requested: usize) -> usize {
    requested.clamp(1, MAX_CONNECTIONS)
}

fn destination_is_taken(dest: &Path) -> bool {
    dest.exists() || state::part_path(dest).exists() || state::state_path(dest).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_names_go_around_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            pick_free_name(dir.path(), "a.zip"),
            dir.path().join("a.zip")
        );
        std::fs::write(dir.path().join("a.zip"), b"x").unwrap();
        assert_eq!(
            pick_free_name(dir.path(), "a.zip"),
            dir.path().join("a (1).zip")
        );
        std::fs::write(dir.path().join("a (1).zip"), b"x").unwrap();
        assert_eq!(
            pick_free_name(dir.path(), "a.zip"),
            dir.path().join("a (2).zip")
        );
    }

    #[test]
    fn a_name_without_extension_still_gets_a_suffix() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README"), b"x").unwrap();
        assert_eq!(
            pick_free_name(dir.path(), "README"),
            dir.path().join("README (1)")
        );
    }

    #[test]
    fn backoff_grows_then_stops_at_two_seconds() {
        assert_eq!(backoff(1), Duration::from_millis(100));
        assert_eq!(backoff(2), Duration::from_millis(200));
        assert_eq!(backoff(30), Duration::from_millis(2000));
    }

    #[test]
    fn connections_are_capped_at_32() {
        assert_eq!(bounded_connections(0), 1);
        assert_eq!(bounded_connections(8), 8);
        assert_eq!(bounded_connections(32), 32);
        assert_eq!(bounded_connections(100), 32);
    }
}
