//! 写盘队列：worker 只管收字节，落盘和提交由一条专职任务按 FIFO 做。
//!
//! M1 里 `write_at` 是同步调用，慢盘会把 worker 一起卡住：网络在等、盘也在等，两边
//! 互相拖住（T6 的病根）。拆开以后，worker 把批次交给队列就回去读网络，磁盘慢只会让
//! 队列变长；只有队列真的满了，反压才会传回网络侧——那时确实该慢下来。
//!
//! 写盘任务独占 `PartFile` 的写端，所以「写完才 `commit`」这个不变量在同一个任务里
//! 顺序成立，不需要额外的锁。`commit` 之后进度才算数，因此进度反映的永远是盘上真有的字节。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};

use crate::Error;
use crate::limiter::Pacer;
use crate::part::PartFile;
use crate::planner::{Planner, SavedSegment, WorkerId};
use crate::state::{ResumeState, StateWriter};
use crate::trace::Trace;

/// 队列深度。8 批 × 256 KiB = 2 MiB 的缓冲：够吃掉一次写卡顿，又不至于让进度记账
/// 落后太多（`kill -9` 的损失由 `Ledger` 的步长管）。
const QUEUE: usize = 8;

/// 记账本：状态文件和「已经记了多少字节」都由写盘任务拿着。
///
/// 记账必须贴着 `commit` 做。放在监督者那边做就得多跳一次任务调度：8 条连接跑满时，
/// 几微秒的调度延迟就是几百 KiB，`kill -9` 的损失按 PRD 6.3 要压在 1 MiB 以内，
/// 这一跳省不掉。监督者只在自己要 `fsync`、取消或收尾时用它。
pub struct Ledger {
    /// 攒够这么多字节就记一次。
    step: u64,
    /// 已经写进状态文件的字节数。
    pub saved: Arc<AtomicU64>,
    state: Mutex<StateWriter>,
    /// 模板：URL、ETag、大小这些开工时定死，每次只换段表。
    resume: Mutex<ResumeState>,
}

impl Ledger {
    pub fn new(
        step: u64,
        saved: Arc<AtomicU64>,
        state: StateWriter,
        resume: ResumeState,
    ) -> Ledger {
        Ledger {
            step,
            saved,
            state: Mutex::new(state),
            resume: Mutex::new(resume),
        }
    }

    /// 把段表拍进状态文件。`sync` 时才 `sync_data`——那是给断电准备的，`kill -9` 用不着
    /// （进程死了页缓存还活着）。
    pub fn record(&self, segments: Vec<SavedSegment>, sync: bool) -> Result<(), Error> {
        let mut resume = self.resume.lock().unwrap();
        resume.segments = segments;
        let mut state = self.state.lock().unwrap();
        state.write(&resume, sync).map_err(Error::Io)?;
        self.saved.store(resume.downloaded(), Ordering::Relaxed);
        Ok(())
    }
}

enum Cmd {
    Batch {
        worker: WorkerId,
        offset: u64,
        end: u64,
        buf: Vec<u8>,
    },
    /// 排在所有已入队的批次之后：做一次「之前的都落盘了」的确认。
    Barrier {
        sync: bool,
        ack: oneshot::Sender<Result<(), Error>>,
    },
}

pub struct Writer {
    tx: mpsc::Sender<Cmd>,
    ledger: Option<Arc<Ledger>>,
}

impl Writer {
    /// 起一条写盘任务。`rate` 只给慢盘仿真用（见 [`Pacer`]）；`ledger` 见 [`Ledger`]。
    pub fn spawn(
        part: Arc<PartFile>,
        planner: Arc<Mutex<Planner>>,
        rate: Option<u64>,
        ledger: Option<Arc<Ledger>>,
        trace: Option<Arc<Trace>>,
    ) -> Writer {
        let (tx, mut rx) = mpsc::channel::<Cmd>(QUEUE);
        let shared = ledger.clone();
        tokio::spawn(async move {
            let mut pacer = rate.map(Pacer::new);
            // 第一次写失败后就不再写了：继续写只会把坏状态扩散成更多坏字节。
            let mut broken: Option<(std::io::ErrorKind, String)> = None;
            while let Some(cmd) = rx.recv().await {
                match cmd {
                    Cmd::Batch {
                        worker,
                        offset,
                        end,
                        buf,
                    } => {
                        if broken.is_some() {
                            continue;
                        }
                        if let Some(p) = pacer.as_mut() {
                            p.spend(buf.len() as u64).await;
                        }
                        let writing = std::time::Instant::now();
                        let cpu0 = trace.as_ref().map_or(0, |_| crate::trace::cpu_ns());
                        let wrote = part.write_at(offset, &buf);
                        if let Some(t) = &trace {
                            t.write(
                                buf.len(),
                                writing.elapsed(),
                                crate::trace::cpu_ns().saturating_sub(cpu0),
                            );
                        }
                        match wrote {
                            Ok(()) => {
                                let mut p = planner.lock().unwrap();
                                p.commit(worker, end);
                                // 记账贴着提交做：攒够一个步长就把段表拍下来，放锁之后
                                // 再写文件，别让监督者等这把锁。
                                let segments = shared.as_ref().and_then(|l| {
                                    let ahead = p
                                        .downloaded()
                                        .saturating_sub(l.saved.load(Ordering::Relaxed));
                                    (ahead >= l.step).then(|| p.save())
                                });
                                drop(p);
                                if let (Some(l), Some(segs)) = (&shared, segments) {
                                    let cpu0 = trace.as_ref().map_or(0, |_| crate::trace::cpu_ns());
                                    let wrote_state = l.record(segs, false);
                                    if let Some(t) = &trace {
                                        t.state(crate::trace::cpu_ns().saturating_sub(cpu0));
                                    }
                                    if let Err(e) = wrote_state {
                                        broken = Some((std::io::ErrorKind::Other, e.to_string()));
                                    }
                                }
                            }
                            Err(e) => broken = Some((e.kind(), e.to_string())),
                        }
                    }
                    Cmd::Barrier { sync, ack } => {
                        let result = match &broken {
                            Some((kind, msg)) => {
                                Err(Error::Io(std::io::Error::new(*kind, msg.clone())))
                            }
                            None if sync => part.sync().map_err(Error::Io),
                            None => Ok(()),
                        };
                        let _ = ack.send(result);
                    }
                }
            }
        });
        Writer { tx, ledger }
    }

    /// 拍一次段表写进状态文件（监督者在 `fsync`、取消、收尾时用）。
    ///
    /// 周期性记账不走这里——那是写盘任务自己在 `commit` 之后顺手做的。
    pub fn record(&self, planner: &Mutex<Planner>, sync: bool) -> Result<(), Error> {
        let Some(l) = &self.ledger else {
            return Ok(());
        };
        let segments = planner.lock().unwrap().save();
        l.record(segments, sync)
    }

    /// 追加一批写入。队列满时在这里等——这就是磁盘对网络的反压。
    pub async fn queue(
        &self,
        worker: WorkerId,
        offset: u64,
        end: u64,
        buf: Vec<u8>,
    ) -> Result<(), Error> {
        self.tx
            .send(Cmd::Batch {
                worker,
                offset,
                end,
                buf,
            })
            .await
            .map_err(|_| Error::Internal("写盘任务提前结束了".into()))
    }

    /// 等已入队的批次都写完（`sync` 时再 `sync_data`），返回第一个写错误的副本。
    ///
    /// 调用者拿到 `Ok` 之后才能相信「盘上已经有这些字节」，也才能写状态文件。
    pub async fn barrier(&self, sync: bool) -> Result<(), Error> {
        let (ack, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Barrier { sync, ack })
            .await
            .map_err(|_| gone())?;
        rx.await.map_err(|_| gone())?
    }
}

fn gone() -> Error {
    Error::Internal("写盘任务提前结束了".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::Planner;

    /// 队列写下的字节要真的在文件里，进度要等写完之后才对得上。
    #[tokio::test]
    async fn batches_land_before_the_barrier_returns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.part");
        let part = Arc::new(PartFile::open(&path, Some(64), true).unwrap());
        let planner = Arc::new(Mutex::new(Planner::new(64, 1024)));
        let writer = Writer::spawn(part.clone(), planner.clone(), None, None, None);

        let id: WorkerId = 0;
        planner.lock().unwrap().assign(id, None).unwrap();
        // 认领之后 `reserved` 才是 64；`commit` 会 clamp 到 reserved，不 claim 就提交不进去。
        assert!(matches!(
            planner.lock().unwrap().claim(id, 0, 64),
            crate::planner::Claim::Granted(64)
        ));
        writer.queue(id, 0, 32, vec![7u8; 32]).await.unwrap();
        writer.queue(id, 32, 64, vec![9u8; 32]).await.unwrap();

        writer.barrier(true).await.unwrap();
        assert_eq!(planner.lock().unwrap().downloaded(), 64);

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 64);
        assert!(bytes[..32].iter().all(|b| *b == 7));
        assert!(bytes[32..].iter().all(|b| *b == 9));
    }

    /// 记账贴着提交做：攒够一个步长才落一次状态文件，落完 `saved` 就跟上盘上的字节。
    #[tokio::test]
    async fn ledger_records_once_per_step() {
        let dir = tempfile::tempdir().unwrap();
        let part = Arc::new(PartFile::open(&dir.path().join("b.part"), Some(4096), true).unwrap());
        let state_path = dir.path().join("b.volant.state");
        let planner = Arc::new(Mutex::new(Planner::new(4096, 1024)));
        let saved = Arc::new(AtomicU64::new(0));
        let resume = ResumeState::new("u", "u", dir.path().join("b"), 4096, None, None, None);
        let ledger = Arc::new(Ledger::new(
            2048,
            saved.clone(),
            StateWriter::create(&state_path).unwrap(),
            resume,
        ));
        let writer = Writer::spawn(part.clone(), planner.clone(), None, Some(ledger), None);

        let id: WorkerId = 0;
        planner.lock().unwrap().assign(id, None).unwrap();
        assert!(matches!(
            planner.lock().unwrap().claim(id, 0, 4096),
            crate::planner::Claim::Granted(4096)
        ));

        // 1024 < 2048：先不记。
        writer.queue(id, 0, 1024, vec![1u8; 1024]).await.unwrap();
        writer.barrier(false).await.unwrap();
        assert_eq!(saved.load(Ordering::Relaxed), 0);

        // 攒到 2048：记一次，状态文件里就是这两个批次。
        writer.queue(id, 1024, 2048, vec![2u8; 1024]).await.unwrap();
        writer.barrier(false).await.unwrap();
        assert_eq!(saved.load(Ordering::Relaxed), 2048);
        assert_eq!(ResumeState::load(&state_path).unwrap().downloaded(), 2048);

        // 再攒 1024，还不够一个步长：不该再记。
        let before = std::fs::metadata(&state_path).unwrap().modified().unwrap();
        writer.queue(id, 2048, 3072, vec![3u8; 1024]).await.unwrap();
        writer.barrier(false).await.unwrap();
        assert_eq!(saved.load(Ordering::Relaxed), 2048);
        assert_eq!(
            std::fs::metadata(&state_path).unwrap().modified().unwrap(),
            before
        );
    }
}
