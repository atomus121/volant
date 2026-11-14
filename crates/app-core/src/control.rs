//! 后台控制器：界面线程只发 `Cmd`、只收 `UiEvent`，两边没有共享状态。
//!
//! 这样界面不会因为引擎的锁卡帧，引擎也不用知道窗口存在（PRD 的模块边界）。
//! 唯一的共享对象是 UI 通知器——只用来在事件到来时叫醒重绘。

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use volant_engine::{Config, Download, Error, Handle, probe_with};
use volant_torrent::Torrents;

use crate::model::{
    MAX_CONNECTIONS_PER_TASK, MAX_PARALLEL_DOWNLOADS, ProxyMode, Settings, TorrentFile,
    writable_download_dir,
};
use crate::notify::Notifier;
use crate::video::{self, VideoContext, VideoLookup, VideoTask};

/// torrent 的来源。`.torrent` 传磁盘上的副本路径，控制器自己去读。
#[derive(Clone, Debug)]
pub enum TorSource {
    Magnet(String),
    File(PathBuf),
}

/// 界面发给控制器的指令。
#[derive(Debug)]
pub enum Cmd {
    /// 只读取链接的文件信息，不创建任务。用于新建下载弹窗的预览。
    PreviewHttp {
        id: u64,
        url: String,
        headers: Vec<(String, String)>,
    },
    /// 新任务：解析、入队、开跑。`headers` 是浏览器捕获带下来的身份（可为空）。
    Add {
        id: u64,
        url: String,
        headers: Vec<(String, String)>,
    },
    /// 继续一个已暂停的任务（必须给回原来的目标路径，才能接上 `.volant.state`）。
    Resume {
        id: u64,
        url: String,
        dest: PathBuf,
        headers: Vec<(String, String)>,
    },
    Pause(u64),
    Remove(u64),
    SetLimit(usize),
    SetConnections(usize),
    SetRate(Option<u64>),
    SetProxy {
        mode: ProxyMode,
        url: String,
    },
    SetDir(PathBuf),
    /// Resolve a page/stream with yt-dlp. `id` is the browser request id.
    VideoLookup {
        id: u64,
        context: VideoContext,
    },
    /// Download one resolved video choice through the normal task lifecycle.
    AddVideo {
        id: u64,
        task: VideoTask,
        destination: PathBuf,
    },

    /// 解析种子（`list_only`）：拿到文件名/大小/文件列表，等界面选完文件。
    InspectTorrent {
        id: u64,
        source: TorSource,
    },
    /// Drop metadata held only for the add-download preview.
    ForgetTorrentPreview(u64),
    /// 用户选定文件后真正开始下。`only` 为 `None` = 全部。
    StartTorrent {
        id: u64,
        only: Option<Vec<usize>>,
    },
    PauseTorrent(u64),
    RemoveTorrent {
        id: u64,
        delete_files: bool,
    },
    /// 做种策略：`seed` 为假时下完就停。
    SetSeed {
        seed: bool,
        ratio: f64,
    },
    Shutdown,
}

/// 控制器发给界面的事件。
#[derive(Debug)]
pub enum UiEvent {
    /// HTTP 预览完成：界面先让用户确认，再发送 [`Cmd::Add`] 真正入队。
    HttpPreview {
        id: u64,
        name: String,
        size: Option<u64>,
        content_type: Option<String>,
        final_url: String,
    },
    HttpPreviewFailed {
        id: u64,
        reason: String,
    },
    Started {
        id: u64,
        dest: PathBuf,
        name: String,
        size: u64,
        resumed_from: u64,
    },
    Progress {
        id: u64,
        downloaded: u64,
        total: u64,
        connections: usize,
        elapsed_ms: u64,
    },
    Done {
        id: u64,
        path: PathBuf,
        size: u64,
        elapsed_ms: u64,
    },
    Failed {
        id: u64,
        reason: String,
    },
    /// 种子元数据解析完了：界面拿这些字段建行、画选文件列表。
    TorrentReady {
        id: u64,
        name: String,
        info_hash: String,
        total: u64,
        files: Vec<TorrentFile>,
    },
    /// torrent 行的一帧状态。
    TorrentProgress {
        id: u64,
        dest: PathBuf,
        downloaded: u64,
        total: u64,
        uploaded: u64,
        peers: usize,
        seeders: usize,
        finished: bool,
    },
    VideoLookup {
        id: u64,
        result: Result<VideoLookup, String>,
    },
    Paused {
        id: u64,
    },
    Removed {
        id: u64,
    },
    Notice(String),
}

const IDLE: u8 = 0;
const PAUSE: u8 = 1;
const REMOVE: u8 = 2;

struct Pending {
    id: u64,
    source: PendingSource,
}

enum PendingSource {
    Http {
        url: String,
        dest: Option<PathBuf>,
        headers: Vec<(String, String)>,
    },
    Video {
        task: VideoTask,
        destination: PathBuf,
    },
}

struct Running {
    intent: Arc<AtomicU8>,
    handle: Arc<Mutex<Option<Handle>>>,
    video_cancel: Option<Arc<AtomicBool>>,
}

enum Msg {
    /// 任务线程结束（正常结束或已收到取消）。
    Finished(u64),
    /// Torrent 释放了一个全局并发额度，让 HTTP 队列有机会补位。
    TorrentSlotAvailable,
}

/// HTTP 与 torrent 共用的并发额度。两类任务合计不能超过用户设置的同时下载数。
#[derive(Clone)]
struct SlotPool {
    active: Arc<AtomicUsize>,
    limit: Arc<AtomicUsize>,
}

impl SlotPool {
    fn new(limit: usize) -> SlotPool {
        SlotPool {
            active: Arc::new(AtomicUsize::new(0)),
            limit: Arc::new(AtomicUsize::new(limit.clamp(1, MAX_PARALLEL_DOWNLOADS))),
        }
    }

    fn try_acquire(&self) -> bool {
        loop {
            let limit = self.limit.load(Ordering::Acquire);
            let active = self.active.load(Ordering::Acquire);
            if active >= limit {
                return false;
            }
            if self
                .active
                .compare_exchange_weak(active, active + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    fn release(&self) {
        let previous = self.active.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "released an unacquired download slot");
    }

    fn set_limit(&self, limit: usize) {
        self.limit
            .store(limit.clamp(1, MAX_PARALLEL_DOWNLOADS), Ordering::Release);
    }
}

pub struct Control {
    cmds: UnboundedSender<Cmd>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Control {
    pub fn start(
        notify: Notifier,
        mut settings: Settings,
        hosts: PathBuf,
        fallback_dir: PathBuf,
    ) -> (Control, Receiver<UiEvent>) {
        settings.normalize();
        let (cmd_tx, cmd_rx) = unbounded_channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("volant-control".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .thread_name("volant-net")
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ev_tx.send(UiEvent::Notice(format!("网络运行时启动失败：{e}")));
                        notify.notify();
                        return;
                    }
                };
                let (msg_tx, msg_rx) = unbounded_channel();
                let mut ctl = Controller::new(
                    rt.handle().clone(),
                    notify.clone(),
                    settings,
                    hosts,
                    fallback_dir,
                    ev_tx,
                    msg_tx,
                );
                rt.block_on(ctl.run(cmd_rx, msg_rx));
                // 退出一律先把在跑的任务取消掉：引擎会在 cancel 后把进度写进
                // `.volant.state`，下次启动点继续就是续传。
                ctl.cancel_all();
                // 给引擎 400 ms 把进度写进 `.volant.state`（sleep 必须在 runtime 里造）。
                rt.block_on(async {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                });
                rt.shutdown_timeout(Duration::from_secs(2));
            })
            .expect("spawn controller thread");
        (
            Control {
                cmds: cmd_tx,
                thread: Some(thread),
            },
            ev_rx,
        )
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.cmds.send(cmd);
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        let _ = self.cmds.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Controller {
    rt: tokio::runtime::Handle,
    notify: Notifier,
    emit: Sender<UiEvent>,
    msg: UnboundedSender<Msg>,
    settings: Settings,
    hosts: PathBuf,
    fallback_dir: PathBuf,
    client: Option<volant_engine::Client>,
    running: HashMap<u64, Running>,
    queue: VecDeque<Pending>,
    slots: SlotPool,
    /// torrent 子系统（独立任务，独占 `Torrents`）；没有活跃任务时它会自己丢掉会话。
    tor: Option<UnboundedSender<TorCmd>>,
}

impl Controller {
    fn new(
        rt: tokio::runtime::Handle,
        notify: Notifier,
        settings: Settings,
        hosts: PathBuf,
        fallback_dir: PathBuf,
        emit: Sender<UiEvent>,
        msg: UnboundedSender<Msg>,
    ) -> Controller {
        let proxy = match settings.proxy_mode {
            ProxyMode::Direct => volant_engine::HttpProxy::Direct,
            ProxyMode::System => volant_engine::HttpProxy::System,
            ProxyMode::Manual => volant_engine::HttpProxy::Manual(settings.proxy_url.clone()),
        };
        let client = match volant_engine::client_with_proxy(proxy) {
            Ok(c) => Some(c),
            Err(e) => {
                let _ = emit.send(UiEvent::Notice(format!("网络初始化失败：{e}")));
                notify.notify();
                None
            }
        };
        let slots = SlotPool::new(settings.limit);
        Controller {
            rt,
            notify,
            emit,
            msg,
            settings,
            hosts,
            fallback_dir,
            client,
            running: HashMap::new(),
            queue: VecDeque::new(),
            slots,
            tor: None,
        }
    }

    fn send(&self, ev: UiEvent) {
        if self.emit.send(ev).is_ok() {
            self.notify.notify();
        }
    }

    /// 转发给 torrent 子系统；还没起来就当没这回事。
    fn tor_send(&self, cmd: TorCmd) {
        if let Some(t) = &self.tor {
            let _ = t.send(cmd);
        }
    }

    async fn run(&mut self, mut cmds: UnboundedReceiver<Cmd>, mut msgs: UnboundedReceiver<Msg>) {
        let (tor_tx, tor_rx) = unbounded_channel();
        let actor = self.rt.spawn(torrent_actor(
            tor_rx,
            self.settings.clone(),
            self.emit.clone(),
            self.notify.clone(),
            self.fallback_dir.clone(),
            self.slots.clone(),
            self.msg.clone(),
        ));
        self.tor = Some(tor_tx.clone());
        loop {
            enum In {
                Cmd(Cmd),
                Msg(Msg),
            }
            // 两个通道都关了就退出：界面已经没了。
            let incoming = tokio::select! {
                c = cmds.recv() => c.map(In::Cmd),
                m = msgs.recv() => m.map(In::Msg),
            };
            match incoming {
                None => break,
                Some(In::Cmd(Cmd::Shutdown)) => break,
                Some(In::Cmd(cmd)) => self.on_cmd(cmd),
                Some(In::Msg(Msg::Finished(id))) => self.on_finished(id),
                Some(In::Msg(Msg::TorrentSlotAvailable)) => self.pump(),
            }
        }
        self.tor = None;
        let _ = tor_tx.send(TorCmd::Stop);
        let _ = actor.await;
    }

    fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::PreviewHttp { id, url, headers } => {
                let Some(client) = self.client.clone() else {
                    self.send(UiEvent::HttpPreviewFailed {
                        id,
                        reason: "网络客户端未初始化".into(),
                    });
                    return;
                };
                let emit = self.emit.clone();
                let notify = self.notify.clone();
                self.rt.spawn(async move {
                    let ev = match volant_engine::Url::parse(&url) {
                        Ok(parsed) => match probe_with(&client, &parsed, &headers).await {
                            Ok(p) => UiEvent::HttpPreview {
                                id,
                                name: p.filename.unwrap_or_else(|| "下载文件".into()),
                                size: p.size,
                                content_type: p.content_type,
                                final_url: p.final_url.to_string(),
                            },
                            Err(e) => UiEvent::HttpPreviewFailed {
                                id,
                                reason: e.to_string(),
                            },
                        },
                        Err(e) => UiEvent::HttpPreviewFailed {
                            id,
                            reason: format!("链接无效：{e}"),
                        },
                    };
                    if emit.send(ev).is_ok() {
                        notify.notify();
                    }
                });
            }
            Cmd::Add { id, url, headers } => {
                self.queue.push_back(Pending {
                    id,
                    source: PendingSource::Http {
                        url,
                        dest: None,
                        headers,
                    },
                });
                self.pump();
            }
            Cmd::Resume {
                id,
                url,
                dest,
                headers,
            } => {
                self.queue.push_back(Pending {
                    id,
                    source: PendingSource::Http {
                        url,
                        dest: Some(dest),
                        headers,
                    },
                });
                self.pump();
            }
            Cmd::VideoLookup { id, context } => {
                let emit = self.emit.clone();
                let notify = self.notify.clone();
                self.rt.spawn(async move {
                    let result = video::lookup(id, &context).await;
                    if emit.send(UiEvent::VideoLookup { id, result }).is_ok() {
                        notify.notify();
                    }
                });
            }
            Cmd::AddVideo {
                id,
                task,
                destination,
            } => {
                self.queue.push_back(Pending {
                    id,
                    source: PendingSource::Video { task, destination },
                });
                self.pump();
            }
            Cmd::Pause(id) => {
                if self.cancel_running(id, PAUSE) {
                    return;
                }
                if self.drop_queued(id).is_some() {
                    self.send(UiEvent::Paused { id });
                }
            }
            Cmd::Remove(id) => {
                if self.cancel_running(id, REMOVE) {
                    return;
                }
                if self.drop_queued(id).is_some() {
                    self.send(UiEvent::Removed { id });
                }
            }
            Cmd::SetLimit(n) => {
                self.settings.limit = n.clamp(1, MAX_PARALLEL_DOWNLOADS);
                self.slots.set_limit(self.settings.limit);
                self.tor_send(TorCmd::SetLimit(self.settings.limit));
                self.pump();
            }
            Cmd::SetConnections(n) => {
                self.settings.connections = n.clamp(1, MAX_CONNECTIONS_PER_TASK);
                self.tor_send(TorCmd::SetConnections(self.settings.connections));
            }
            Cmd::SetRate(r) => self.settings.rate = r,
            Cmd::SetProxy { mode, url } => {
                let proxy = match mode {
                    ProxyMode::Direct => volant_engine::HttpProxy::Direct,
                    ProxyMode::System => volant_engine::HttpProxy::System,
                    ProxyMode::Manual => volant_engine::HttpProxy::Manual(url.clone()),
                };
                match volant_engine::client_with_proxy(proxy) {
                    Ok(client) => {
                        self.client = Some(client);
                        self.settings.proxy_mode = mode;
                        self.settings.proxy_url = url;
                    }
                    Err(error) => {
                        let _ = error;
                        self.send(UiEvent::Notice(
                            "代理地址无效，已保留之前的代理设置。".into(),
                        ));
                    }
                }
            }
            Cmd::SetDir(d) => {
                let (dir, note) = writable_download_dir(&d, &self.fallback_dir);
                if let Some(note) = note {
                    self.send(UiEvent::Notice(note));
                }
                self.settings.dir = dir.clone();
                self.tor_send(TorCmd::Dir(dir));
            }
            Cmd::InspectTorrent { id, source } => self.tor_send(TorCmd::Inspect { id, source }),
            Cmd::ForgetTorrentPreview(id) => self.tor_send(TorCmd::Forget(id)),
            Cmd::StartTorrent { id, only } => self.tor_send(TorCmd::Start { id, only }),
            Cmd::PauseTorrent(id) => self.tor_send(TorCmd::Pause(id)),
            Cmd::RemoveTorrent { id, delete_files } => {
                self.tor_send(TorCmd::Remove { id, delete_files });
            }
            Cmd::SetSeed { seed, ratio } => {
                self.settings.seed = seed;
                self.settings.seed_ratio = ratio;
                self.tor_send(TorCmd::SetSeed { seed, ratio });
            }
            Cmd::Shutdown => {}
        }
    }

    /// 给正在跑的任务打意图并取消；没有这个任务返回 false。
    fn cancel_running(&mut self, id: u64, intent: u8) -> bool {
        let Some(r) = self.running.get(&id) else {
            return false;
        };
        r.intent.store(intent, Ordering::SeqCst);
        // 任务还在 probe 阶段时 handle 是 None——run_one 会在 probe 回来后自查意图。
        if let Some(h) = r.handle.lock().unwrap().as_ref() {
            h.cancel();
        }
        if let Some(cancel) = &r.video_cancel {
            cancel.store(true, Ordering::SeqCst);
        }
        true
    }

    fn drop_queued(&mut self, id: u64) -> Option<Pending> {
        let pos = self.queue.iter().position(|p| p.id == id)?;
        self.queue.remove(pos)
    }

    fn on_finished(&mut self, id: u64) {
        let Some(r) = self.running.remove(&id) else {
            return;
        };
        self.slots.release();
        match r.intent.load(Ordering::SeqCst) {
            PAUSE => self.send(UiEvent::Paused { id }),
            REMOVE => self.send(UiEvent::Removed { id }),
            _ => {}
        }
        self.tor_send(TorCmd::Pump);
        self.pump();
    }

    fn pump(&mut self) {
        let Some(client) = self.client.clone() else {
            while let Some(p) = self.queue.pop_front() {
                if let PendingSource::Http { .. } = p.source {
                    self.send(UiEvent::Failed {
                        id: p.id,
                        reason: "网络客户端未初始化".into(),
                    });
                } else {
                    self.queue.push_front(p);
                    break;
                }
            }
            return;
        };
        while !self.queue.is_empty() {
            if !self.slots.try_acquire() {
                break;
            }
            let p = self.queue.pop_front().expect("queue checked above");
            self.spawn(client.clone(), p);
        }
    }

    fn spawn(&mut self, client: volant_engine::Client, p: Pending) {
        let id = p.id;
        let intent = Arc::new(AtomicU8::new(IDLE));
        let emit = self.emit.clone();
        let msg = self.msg.clone();
        let notify = self.notify.clone();
        let settings = self.settings.clone();
        let hosts = self.hosts.clone();
        let fallback_dir = self.fallback_dir.clone();
        match p.source {
            PendingSource::Http { url, dest, headers } => {
                let slot: Arc<Mutex<Option<Handle>>> = Arc::new(Mutex::new(None));
                self.running.insert(
                    id,
                    Running {
                        intent: intent.clone(),
                        handle: slot.clone(),
                        video_cancel: None,
                    },
                );
                self.rt.spawn(async move {
                    run_one(
                        client,
                        id,
                        url,
                        dest,
                        headers,
                        settings,
                        hosts,
                        fallback_dir,
                        intent,
                        slot,
                        emit,
                        notify,
                    )
                    .await;
                    let _ = msg.send(Msg::Finished(id));
                });
            }
            PendingSource::Video { task, destination } => {
                let cancel = Arc::new(AtomicBool::new(false));
                self.running.insert(
                    id,
                    Running {
                        intent: intent.clone(),
                        handle: Arc::new(Mutex::new(None)),
                        video_cancel: Some(cancel.clone()),
                    },
                );
                self.rt.spawn(async move {
                    run_video(id, task, destination, intent, cancel, emit, notify).await;
                    let _ = msg.send(Msg::Finished(id));
                });
            }
        }
    }

    fn cancel_all(&mut self) {
        for r in self.running.values() {
            r.intent.store(PAUSE, Ordering::SeqCst);
            if let Some(h) = r.handle.lock().unwrap().as_ref() {
                h.cancel();
            }
            if let Some(cancel) = &r.video_cancel {
                cancel.store(true, Ordering::SeqCst);
            }
        }
    }
}

/// 一个任务的一生：解析 → 开跑 → 转发进度。
#[allow(clippy::too_many_arguments)]
async fn run_one(
    client: volant_engine::Client,
    id: u64,
    url: String,
    dest: Option<PathBuf>,
    headers: Vec<(String, String)>,
    settings: Settings,
    hosts: PathBuf,
    fallback_dir: PathBuf,
    intent: Arc<AtomicU8>,
    slot: Arc<Mutex<Option<Handle>>>,
    emit: Sender<UiEvent>,
    notify: Notifier,
) {
    let send = |ev: UiEvent| {
        if emit.send(ev).is_ok() {
            notify.notify();
        }
    };

    let parsed = match volant_engine::Url::parse(&url) {
        Ok(u) => u,
        Err(e) => {
            send(UiEvent::Failed {
                id,
                reason: format!("链接无效：{e}"),
            });
            return;
        }
    };
    if intent.load(Ordering::SeqCst) != IDLE {
        return;
    }

    let p = tokio::select! {
        biased;
        // 探测阶段还没有 Download::Handle 可以取消；在这里直接观察用户意图，
        // 避免暂停/删除一个卡住的服务器时还要等 15 秒探测超时。
        _ = wait_for_cancel(&intent) => return,
        result = probe_with(&client, &parsed, &headers) => match result {
            Ok(p) => p,
            Err(e) => {
                send(UiEvent::Failed {
                    id,
                    reason: e.to_string(),
                });
                return;
            }
        },
    };
    if intent.load(Ordering::SeqCst) != IDLE {
        return;
    }

    let name = p.filename.clone().unwrap_or_else(|| "download".into());
    let size = p.size.unwrap_or(0);
    let (download_dir, dir_note) = writable_download_dir(&settings.dir, &fallback_dir);
    if let Some(note) = dir_note {
        send(UiEvent::Notice(note));
    }
    let mut cfg = Config::new(parsed, download_dir.clone());
    cfg.connections = settings.connections;
    cfg.rate_limit = settings.rate;
    cfg.host_file = Some(hosts);
    cfg.headers = headers;
    cfg.dest = dest.clone();

    let mut dl = Download::start(client, cfg, p);
    *slot.lock().unwrap() = Some(dl.handle.clone());
    if intent.load(Ordering::SeqCst) != IDLE {
        dl.handle.cancel();
    }

    // 续传字节数只能用引擎给的 `resumed_from`：part 文件是按总长度预分配的，
    // 拿文件长度估算会得到满值，把「续传 1.2 MB」说成「续传 8 MB」。
    let mut announced = false;
    while let Some(ev) = dl.events.recv().await {
        match ev {
            volant_engine::Event::Started {
                dest,
                size: total,
                filename,
                resumed_from,
                ..
            } => {
                announced = true;
                send(UiEvent::Started {
                    id,
                    dest,
                    name: if filename.is_empty() {
                        name.clone()
                    } else {
                        filename
                    },
                    size: total,
                    resumed_from,
                });
            }
            volant_engine::Event::Progress {
                downloaded,
                total,
                connections,
                elapsed,
            } => {
                send(UiEvent::Progress {
                    id,
                    downloaded,
                    total,
                    connections,
                    elapsed_ms: elapsed.as_millis() as u64,
                });
            }
            _ => {}
        }
    }

    if !announced {
        // 还没开跑就被叫停：至少让界面知道文件名和落点。
        send(UiEvent::Started {
            id,
            dest: dest.unwrap_or_else(|| download_dir.join(&name)),
            name,
            size,
            resumed_from: 0,
        });
    }

    match dl.wait().await {
        Ok(s) => send(UiEvent::Done {
            id,
            path: s.dest,
            size: s.size,
            elapsed_ms: s.elapsed.as_millis() as u64,
        }),
        // 用户按了暂停/删除：由控制器按意图发 Paused 或 Removed。
        Err(Error::Cancelled) => {}
        Err(e) => send(UiEvent::Failed {
            id,
            reason: e.to_string(),
        }),
    }
    *slot.lock().unwrap() = None;
}

async fn wait_for_cancel(intent: &AtomicU8) {
    while intent.load(Ordering::SeqCst) == IDLE {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn run_video(
    id: u64,
    task: VideoTask,
    destination: PathBuf,
    intent: Arc<AtomicU8>,
    cancel: Arc<AtomicBool>,
    emit: Sender<UiEvent>,
    notify: Notifier,
) {
    let event_emit = emit.clone();
    let event_notify = notify.clone();
    let send = move |event: UiEvent| {
        if event_emit.send(event).is_ok() {
            event_notify.notify();
        }
    };
    if intent.load(Ordering::SeqCst) != IDLE || cancel.load(Ordering::SeqCst) {
        return;
    }
    let expected = video::expected_path(&task, &destination);
    send(UiEvent::Started {
        id,
        dest: expected,
        name: video::safe_stem(&task.title),
        size: task.choice.size.unwrap_or(0),
        resumed_from: 0,
    });
    let elapsed = std::time::Instant::now();
    let progress_emit = emit.clone();
    let progress_notify = notify.clone();
    let result = video::download(&task, &destination, cancel.clone(), move |progress| {
        if progress_emit
            .send(UiEvent::Progress {
                id,
                downloaded: progress.downloaded,
                total: progress.total,
                connections: 1,
                elapsed_ms: elapsed.elapsed().as_millis() as u64,
            })
            .is_ok()
        {
            progress_notify.notify();
        }
    })
    .await;
    if intent.load(Ordering::SeqCst) != IDLE || cancel.load(Ordering::SeqCst) {
        return;
    }
    match result {
        Ok(path) => {
            let size = std::fs::metadata(&path).map_or(0, |meta| meta.len());
            send(UiEvent::Done {
                id,
                path,
                size,
                elapsed_ms: elapsed.elapsed().as_millis() as u64,
            });
        }
        Err(reason) => send(UiEvent::Failed { id, reason }),
    }
}

// ---------------------------------------------------------------------------
// torrent 子系统
// ---------------------------------------------------------------------------

enum TorCmd {
    Inspect { id: u64, source: TorSource },
    Forget(u64),
    Start { id: u64, only: Option<Vec<usize>> },
    Pause(u64),
    Remove { id: u64, delete_files: bool },
    SetSeed { seed: bool, ratio: f64 },
    SetLimit(usize),
    SetConnections(usize),
    Pump,
    Dir(PathBuf),
    Stop,
}

struct ActiveTor {
    tid: usize,
    dest: PathBuf,
}

/// 独占 [`Torrents`] 的任务：串行处理 torrent 指令，每 500 ms 采一帧状态。
///
/// 会话按需存在：没有任务在跑、也没有种子等着选文件时，把整个 `Session` 丢掉，
/// 端口和 DHT 一起关掉（PRD 5.3）。解析磁力链接是阻塞的，但它在同一个任务里
/// 顺序执行，不需要共享 `Session` 的锁。
async fn torrent_actor(
    mut rx: UnboundedReceiver<TorCmd>,
    mut settings: Settings,
    emit: Sender<UiEvent>,
    notify: Notifier,
    fallback_dir: PathBuf,
    slots: SlotPool,
    msg: UnboundedSender<Msg>,
) {
    let send = |ev: UiEvent| {
        if emit.send(ev).is_ok() {
            notify.notify();
        }
    };
    settings.normalize();
    let mut tor = Torrents::new(volant_torrent::Options {
        peer_limit: settings.connections,
        initial_peers: test_peers(),
        ..Default::default()
    });
    let mut parked: HashMap<u64, volant_torrent::Inspect> = HashMap::new();
    let mut active: HashMap<u64, ActiveTor> = HashMap::new();
    let mut queued: VecDeque<(u64, Option<Vec<usize>>)> = VecDeque::new();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        // 没有会话、没有任务时只等命令：空闲期不做任何定时唤醒（PRD 6.2 空闲 CPU ≤0.1%）。
        let idle = !tor.alive() && active.is_empty() && parked.is_empty() && queued.is_empty();
        let cmd = if idle {
            match rx.recv().await {
                Some(c) => c,
                None => break,
            }
        } else {
            tokio::select! {
                c = rx.recv() => match c {
                    Some(c) => c,
                    None => break,
                },
                _ = tick.tick() => {
                    let released = reap(&mut tor, &mut active, &settings, &send).await;
                    release_torrent_slots(&slots, &msg, released);
                    pump_torrent(
                        &mut tor,
                        &mut active,
                        &mut parked,
                        &mut queued,
                        &settings,
                        &fallback_dir,
                        &slots,
                        &msg,
                        &send,
                    ).await;
                    if tor.alive() && active.is_empty() && parked.is_empty() && queued.is_empty() {
                        tor.shutdown().await;
                    }
                    continue;
                }
            }
        };
        match cmd {
            TorCmd::Stop => break,
            TorCmd::Inspect { id, source } => {
                let src = match source {
                    TorSource::Magnet(m) => volant_torrent::Source::Magnet(m),
                    TorSource::File(p) => match std::fs::read(&p) {
                        Ok(bytes) => volant_torrent::Source::File(bytes),
                        Err(e) => {
                            send(UiEvent::Failed {
                                id,
                                reason: format!("读不了 .torrent 文件：{e}"),
                            });
                            continue;
                        }
                    },
                };
                match tor.inspect(&src).await {
                    Ok(insp) => {
                        send(UiEvent::TorrentReady {
                            id,
                            name: insp.name.clone(),
                            info_hash: insp.info_hash.clone(),
                            total: insp.total,
                            files: insp
                                .files
                                .iter()
                                .map(|f| TorrentFile {
                                    name: f.name.clone(),
                                    len: f.len,
                                })
                                .collect(),
                        });
                        parked.insert(id, insp);
                    }
                    Err(e) => send(UiEvent::Failed { id, reason: e }),
                }
            }
            TorCmd::Forget(id) => {
                parked.remove(&id);
            }
            TorCmd::Start { id, only } => {
                queued.push_back((id, only));
                pump_torrent(
                    &mut tor,
                    &mut active,
                    &mut parked,
                    &mut queued,
                    &settings,
                    &fallback_dir,
                    &slots,
                    &msg,
                    &send,
                )
                .await;
            }
            TorCmd::Pause(id) => {
                queued.retain(|(queued_id, _)| *queued_id != id);
                parked.remove(&id);
                // 暂停就把它从会话里摘掉：这样「最后一个任务停下」时整个 Session 能干净 drop，
                // 恢复时再走一遍解析 + 校验已有数据（librqbit 会自己核对已下载的块）。
                if let Some(a) = active.remove(&id) {
                    let _ = tor.pause(a.tid).await;
                    let _ = tor.remove(a.tid, false).await;
                    release_torrent_slots(&slots, &msg, 1);
                }
                send(UiEvent::Paused { id });
            }
            TorCmd::Remove { id, delete_files } => {
                queued.retain(|(queued_id, _)| *queued_id != id);
                parked.remove(&id);
                if let Some(a) = active.remove(&id) {
                    let _ = tor.remove(a.tid, delete_files).await;
                    release_torrent_slots(&slots, &msg, 1);
                }
                send(UiEvent::Removed { id });
            }
            TorCmd::SetSeed { seed, ratio } => {
                settings.seed = seed;
                settings.seed_ratio = ratio;
            }
            TorCmd::SetLimit(n) => {
                settings.limit = n.clamp(1, MAX_PARALLEL_DOWNLOADS);
                pump_torrent(
                    &mut tor,
                    &mut active,
                    &mut parked,
                    &mut queued,
                    &settings,
                    &fallback_dir,
                    &slots,
                    &msg,
                    &send,
                )
                .await;
            }
            TorCmd::SetConnections(n) => {
                settings.connections = n.clamp(1, MAX_CONNECTIONS_PER_TASK);
                tor.set_peer_limit(settings.connections);
            }
            TorCmd::Pump => {}
            TorCmd::Dir(d) => settings.dir = d,
        }
        if !queued.is_empty() {
            pump_torrent(
                &mut tor,
                &mut active,
                &mut parked,
                &mut queued,
                &settings,
                &fallback_dir,
                &slots,
                &msg,
                &send,
            )
            .await;
        }
        if active.is_empty() && parked.is_empty() && queued.is_empty() {
            tor.shutdown().await;
        }
    }

    for a in active.values() {
        let _ = tor.pause(a.tid).await;
    }
    tor.shutdown().await;
}

#[allow(clippy::too_many_arguments)]
async fn pump_torrent(
    tor: &mut Torrents,
    active: &mut HashMap<u64, ActiveTor>,
    parked: &mut HashMap<u64, volant_torrent::Inspect>,
    queued: &mut VecDeque<(u64, Option<Vec<usize>>)>,
    settings: &Settings,
    fallback_dir: &Path,
    slots: &SlotPool,
    msg: &UnboundedSender<Msg>,
    send: &impl Fn(UiEvent),
) {
    while !queued.is_empty() {
        if !slots.try_acquire() {
            break;
        }
        let Some((id, only)) = queued.pop_front() else {
            slots.release();
            break;
        };
        let Some(insp) = parked.remove(&id) else {
            slots.release();
            continue;
        };
        let (dir, note) = writable_download_dir(&settings.dir, fallback_dir);
        if let Some(note) = note {
            send(UiEvent::Notice(note));
        }
        match tor.start(&insp, only, &dir).await {
            Ok(st) => {
                send(UiEvent::Started {
                    id,
                    dest: st.dest.clone(),
                    name: insp.name,
                    size: insp.total,
                    resumed_from: 0,
                });
                active.insert(
                    id,
                    ActiveTor {
                        tid: st.id,
                        dest: st.dest,
                    },
                );
            }
            Err(e) => {
                slots.release();
                send(UiEvent::Failed { id, reason: e });
                let _ = msg.send(Msg::TorrentSlotAvailable);
            }
        }
    }
}

fn release_torrent_slots(slots: &SlotPool, msg: &UnboundedSender<Msg>, count: usize) {
    for _ in 0..count {
        slots.release();
    }
    if count > 0 {
        let _ = msg.send(Msg::TorrentSlotAvailable);
    }
}

/// 本机测试用：`VOLANT_TEST_PEERS=127.0.0.1:6881` 直接连这些 peer，
/// 不依赖 tracker/DHT（T7/T8 的 loopback 场景和端到端测试都走这条）。
fn test_peers() -> Vec<SocketAddr> {
    match std::env::var("VOLANT_TEST_PEERS") {
        Ok(v) => v.split(',').filter_map(|s| s.trim().parse().ok()).collect(),
        Err(_) => Vec::new(),
    }
}

/// 采一帧：转发进度；下完（且按设置停止）时暂停并报完成；到分享率上限同理。
async fn reap(
    tor: &mut Torrents,
    active: &mut HashMap<u64, ActiveTor>,
    settings: &Settings,
    send: &impl Fn(UiEvent),
) -> usize {
    let mut stop: Vec<(u64, PathBuf, u64)> = Vec::new();
    let mut lost: Vec<u64> = Vec::new();
    let mut released = 0;
    for (id, a) in active.iter() {
        let Some(s) = tor.snapshot(a.tid) else {
            lost.push(*id);
            continue;
        };
        send(UiEvent::TorrentProgress {
            id: *id,
            dest: a.dest.clone(),
            downloaded: s.downloaded,
            total: s.total,
            uploaded: s.uploaded,
            peers: s.peers,
            seeders: s.seeders,
            finished: s.finished,
        });
        if let Some(e) = s.error {
            send(UiEvent::Failed { id: *id, reason: e });
            stop.push((*id, a.dest.clone(), s.total));
            continue;
        }
        let ratio = if s.downloaded > 0 {
            s.uploaded as f64 / s.downloaded as f64
        } else {
            0.0
        };
        let at_limit = settings.seed && settings.seed_ratio > 0.0 && ratio >= settings.seed_ratio;
        // 「下完即停」是默认；打开做种后要等分享率到上限才停。
        if s.finished && (!settings.seed || at_limit) {
            stop.push((*id, a.dest.clone(), s.total));
        }
    }
    for (id, dest, size) in stop {
        if let Some(a) = active.remove(&id) {
            let _ = tor.pause(a.tid).await;
            released += 1;
            send(UiEvent::Done {
                id,
                path: dest,
                size,
                elapsed_ms: 0,
            });
        }
    }
    for id in lost {
        if active.remove(&id).is_some() {
            released += 1;
            send(UiEvent::Failed {
                id,
                reason: "会话里找不到这个种子了".into(),
            });
        }
    }
    released
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Instant;

    use librqbit::CreateTorrentOptions;
    use librqbit::create_torrent;
    use librqbit::spawn_utils::BlockingSpawner;

    fn scratch(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/t6/flow")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn payload(n: usize, salt: u8) -> Vec<u8> {
        (0..n)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(salt))
            .collect()
    }

    #[test]
    fn download_slots_enforce_one_shared_limit() {
        let http = SlotPool::new(2);
        let torrent = http.clone();

        assert!(http.try_acquire());
        assert!(torrent.try_acquire());
        assert!(!http.try_acquire(), "HTTP 与 torrent 应共用同一个额度");

        http.release();
        assert!(torrent.try_acquire());

        http.set_limit(1);
        torrent.release();
        torrent.release();
        assert!(http.try_acquire());
        assert!(!torrent.try_acquire());
    }

    /// 本机做种 → 应用里添加 .torrent → 只选一个文件 → 下完。
    /// 覆盖 InspectTorrent/StartTorrent/Started/Progress/Done 整条链路。
    #[test]
    fn torrent_flow_from_file_to_done() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let seed_root = scratch("seed");
        let pack = seed_root.join("pack");
        std::fs::create_dir_all(&pack).unwrap();
        let a = payload(262_144, 7);
        let b = payload(131_072, 11);
        std::fs::write(pack.join("a.bin"), &a).unwrap();
        std::fs::write(pack.join("b.bin"), &b).unwrap();

        let bytes = rt.block_on(async {
            let created = create_torrent(
                &pack,
                CreateTorrentOptions {
                    name: Some("pack"),
                    piece_length: Some(16 * 1024),
                    ..Default::default()
                },
                &BlockingSpawner::new(2),
            )
            .await
            .unwrap();
            created.as_bytes().unwrap().to_vec()
        });

        let torrent_path = scratch("files").join("pack.torrent");
        std::fs::write(&torrent_path, &bytes).unwrap();

        let mut seeder = volant_torrent::Torrents::new(volant_torrent::Options {
            listen: Some("127.0.0.1:0".parse().unwrap()),
            ..Default::default()
        });
        let addr = rt.block_on(async {
            let insp = seeder
                .inspect(&volant_torrent::Source::File(bytes.clone()))
                .await
                .unwrap();
            seeder.start(&insp, None, &seed_root).await.unwrap();
            seeder.listen_addr().expect("seeder listens")
        });
        // SAFETY: 测试进程里没有别的线程读这个变量，actor 在自己的线程里读一次。
        unsafe { std::env::set_var("VOLANT_TEST_PEERS", addr.to_string()) };

        let out = scratch("out");
        let (ctl, events) = Control::start(
            crate::notify::noop(),
            Settings {
                dir: out.clone(),
                ..Default::default()
            },
            scratch("hosts.json").join("hosts.json"),
            out.join("fallback"),
        );
        ctl.send(Cmd::InspectTorrent {
            id: 1,
            source: TorSource::File(torrent_path),
        });

        let mut started = false;
        let mut done = None;
        let deadline = Instant::now() + Duration::from_secs(90);
        while Instant::now() < deadline && done.is_none() {
            let Ok(ev) = events.recv_timeout(Duration::from_secs(5)) else {
                continue;
            };
            match ev {
                UiEvent::TorrentReady {
                    id: 1,
                    files,
                    total,
                    ..
                } => {
                    assert_eq!(files.len(), 2);
                    assert_eq!(total, (a.len() + b.len()) as u64);
                    ctl.send(Cmd::StartTorrent {
                        id: 1,
                        only: Some(vec![0]),
                    });
                }
                UiEvent::Started { id: 1, dest, .. } => {
                    started = true;
                    assert_eq!(dest, out.join("pack"));
                }
                UiEvent::Done { id: 1, path, .. } => done = Some(path),
                UiEvent::Failed { id: 1, reason } => panic!("torrent failed: {reason}"),
                _ => {}
            }
        }
        assert!(started, "应该先收到 Started");
        assert_eq!(done.expect("应该在 90 秒内下完"), out.join("pack"));
        assert_eq!(std::fs::read(out.join("pack").join("a.bin")).unwrap(), a);
        // 没勾的文件只会留下 0 字节占位（BEP-53 只挑片段）。
        let b_out = out.join("pack").join("b.bin");
        let blen = std::fs::metadata(&b_out).map(|m| m.len()).unwrap_or(0);
        assert_eq!(blen, 0, "没选中的文件不该有内容");

        drop(ctl);
        rt.block_on(async { seeder.shutdown().await });
    }
}
