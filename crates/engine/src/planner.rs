//! 动态分段规划器：纯逻辑，无 I/O、无时钟依赖（时间只用在可选的 preempt 上，M1 不用）。
//!
//! 一条连接空出来时 [`Planner::assign`] 做两件事，按顺序：
//! 1. 认领一个没有 worker、还有未认领字节的段（优先接在它刚下完的那一段后面）；
//! 2. 否则把「剩余字节最多的段」对半切，拿走上半，前提是两半都不小于 `min_split`。
//!
//! 切割点从**已认领位置**量起，所以永远不会切进正在写的数据。
//!
//! 写字节前 worker 必须先 [`claim`](Planner::claim)，落盘后再 [`commit`](Planner::commit)：
//! `[start, cursor)` 在盘上，`[cursor, reserved)` 已认领但没提交，`[reserved, end)` 还没人认领。
//! worker 失败时 [`release`](Planner::release) 把 `reserved` 退回 `cursor`——那些字节会重下。
//!
//! **M1 有意没做「抢占还没收到首字节的相邻段」**（MDM 的 assign 第 2 步）：那个机制的
//! `preempt_after` 必须按真实首字节时间标定，而 M1 还没测。延迟设 0 会导致刚分配出去的段
//! 立刻被同伴抢走，白花一次请求。M2 拿到首字节数据后再加，见 docs/m1/REPORT.md。

use serde::{Deserialize, Serialize};

pub type WorkerId = u32;

/// 切割点尽量对齐到 4 KiB：不对齐的区间在磁盘上要读改写整个块。
const ALIGN: u64 = 4096;

/// 从别人手里偷活的下限：剩下的一截至少这么长，才值得去切。
///
/// 切一段的代价是中止一条已经在传的响应，服务端已经写进 socket 缓冲的字节（实测约
/// 1 MB）白发。为了最后那几十 KB 去偷，省下的时间还不够赔进去的：16 MiB / 8 连接的
/// 收尾阶段本来会互相切 17 次，服务端因此多发了 8%、请求数从 9 涨到 25。
const STEAL_MIN: u64 = 4 << 20;

/// 文件里一段连续区间，位置是字节偏移，区间左闭右开。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    /// 段起点（进度条上的一个刻度）。
    pub start: u64,
    /// `[start, cursor)` 已经写到盘上。
    pub cursor: u64,
    /// `[cursor, reserved)` 被某个 worker 认领了，还没提交。
    pub reserved: u64,
    /// 开区间终点；段被切开时这个值往回落。
    pub end: u64,
    pub worker: Option<WorkerId>,
}

impl Segment {
    fn unclaimed(&self) -> u64 {
        self.end - self.reserved
    }
}

/// 落盘用的段表（续传状态）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSegment {
    pub start: u64,
    pub cursor: u64,
    pub end: u64,
}

/// 交给 worker 的一段活：取 `[from, to)`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Assignment {
    pub from: u64,
    pub to: u64,
    /// 这段之前已经有多少字节在盘上（续传或接手段；新切出来的段是 0）。
    pub written: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    /// 允许写这么多字节。可能比请求的少：段在请求期间被切短了。0 表示这一段没地方了。
    Granted(u64),
    /// 这个 worker 在 `pos` 已经不拥有任何段了，应该重新领活。
    Lost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidState;

#[derive(Clone, Debug)]
pub struct Planner {
    size: u64,
    min_split: u64,
    /// 从别人手里偷活，剩的少于这个数就不偷（见 [`STEAL_MIN`]）。
    steal_min: u64,
    /// 按 `start` 排序、首尾相接、正好铺满 `[0, size)`。
    segs: Vec<Segment>,
}

impl Planner {
    /// 从头下载 `size` 字节。`min_split` 是一次对半切之后每半的最小长度。
    pub fn new(size: u64, min_split: u64) -> Self {
        let segs = if size == 0 {
            Vec::new()
        } else {
            vec![fresh(0, size, None)]
        };
        Planner {
            size,
            min_split: min_split.max(1),
            steal_min: STEAL_MIN,
            segs,
        }
    }

    /// 从段表恢复。段表必须正好铺满 `[0, size)` 且每段的 `start ≤ cursor ≤ end`。
    pub fn resume(size: u64, min_split: u64, saved: &[SavedSegment]) -> Result<Self, InvalidState> {
        let mut expect = 0;
        for s in saved {
            if s.start != expect || s.cursor < s.start || s.cursor > s.end || s.end <= s.start {
                return Err(InvalidState);
            }
            expect = s.end;
        }
        if expect != size || (size > 0 && saved.is_empty()) {
            return Err(InvalidState);
        }
        let segs = saved
            .iter()
            .map(|s| Segment {
                start: s.start,
                cursor: s.cursor,
                reserved: s.cursor,
                end: s.end,
                worker: None,
            })
            .collect();
        Ok(Planner {
            size,
            min_split: min_split.max(1),
            steal_min: STEAL_MIN,
            segs,
        })
    }

    pub fn save(&self) -> Vec<SavedSegment> {
        self.segs
            .iter()
            .map(|s| SavedSegment {
                start: s.start,
                cursor: s.cursor,
                end: s.end,
            })
            .collect()
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segs
    }

    /// 已经在盘上的字节数。
    pub fn downloaded(&self) -> u64 {
        self.segs.iter().map(|s| s.cursor - s.start).sum()
    }

    pub fn is_complete(&self) -> bool {
        self.segs.iter().all(|s| s.cursor == s.end)
    }

    /// 正在干活的 worker 数。
    pub fn busy_workers(&self) -> usize {
        self.segs.iter().filter(|s| s.worker.is_some()).count()
    }

    pub fn holds(&self, w: WorkerId) -> bool {
        self.segs.iter().any(|s| s.worker == Some(w))
    }

    pub fn segment_of(&self, w: WorkerId) -> Option<Segment> {
        self.segs.iter().copied().find(|s| s.worker == Some(w))
    }

    /// 给空闲的 worker `w` 派活。`after` 是它刚下完那一段的终点（没有就传 `None`）。
    /// 返回 `None` 表示没有值得再切的活，这个 worker 可以退出了。
    pub fn assign(&mut self, w: WorkerId, after: Option<u64>) -> Option<Assignment> {
        self.release(w);

        // 1. 有没人认领的剩余字节的段。
        let open = |s: &Segment| s.worker.is_none() && s.unclaimed() > 0;
        let pick = self
            .segs
            .iter()
            .position(|s| open(s) && Some(s.start) == after)
            .or_else(|| self.segs.iter().position(open));
        if let Some(i) = pick {
            return Some(self.give(i, w));
        }

        // 2. 剩得够多的话，把最大的一段对半切（平手取靠前的）。
        let i = self.largest_splittable(self.steal_min)?;
        let j = self.split_at(i);
        let seg = self.segs[j];
        self.segs[j].worker = Some(w);
        Some(Assignment {
            from: seg.start,
            to: seg.end,
            written: 0,
        })
    }

    /// 开局先切成 `parts` 段，让每台 worker 一上来就有一整段自己的活。
    ///
    /// 不这么做的代价是量出来的：8 个 worker 抢同一段再互相切，每切一次都要中止一条
    /// 已经在传的响应，服务端已经写进 socket 缓冲的字节全白费——16 MiB / 8 连接时
    /// 服务端发了 140%（见 docs/m3/REPORT.md）。切分规则与 [`assign`](Self::assign)
    /// 的第 2 步相同（最大未认领段对半），所以段大小不一定相等。
    pub fn presplit(&mut self, parts: usize) {
        while self.segs.len() < parts.max(1) {
            // 开局还没有任何响应在传，切多小都不浪费，只要两半都 ≥ min_split。
            let Some(i) = self.largest_splittable(2 * self.min_split) else {
                break;
            };
            self.split_at(i);
        }
    }

    /// 剩余最多、切得动（两半都 ≥ `min_split`）、且至少还剩 `least` 字节的那一段。
    fn largest_splittable(&self, least: u64) -> Option<usize> {
        let (i, s) = self
            .segs
            .iter()
            .enumerate()
            .max_by_key(|(i, s)| (s.unclaimed(), std::cmp::Reverse(*i)))?;
        (s.unclaimed() / 2 >= self.min_split && s.unclaimed() >= least).then_some(i)
    }

    /// 把第 `i` 段从「已认领位置 + 一半」处切开，返回新段的索引（新段无主）。
    fn split_at(&mut self, i: usize) -> usize {
        let seg = self.segs[i];
        let half = seg.unclaimed() / 2;
        let mut mid = seg.reserved + half;
        let aligned = mid - mid % ALIGN;
        if aligned >= seg.reserved + self.min_split {
            mid = aligned;
        }
        self.segs[i].end = mid;
        self.segs.insert(i + 1, fresh(mid, seg.end, None));
        i + 1
    }

    /// worker `w` 想在 `pos`（它下一个要写的字节）写 `len` 字节。
    pub fn claim(&mut self, w: WorkerId, pos: u64, len: u64) -> Claim {
        let Some(s) = self.segs.iter_mut().find(|s| s.worker == Some(w)) else {
            return Claim::Lost;
        };
        if pos != s.reserved {
            return Claim::Lost;
        }
        let n = len.min(s.unclaimed());
        s.reserved += n;
        Claim::Granted(n)
    }

    /// worker `w` 已经把 `upto` 之前的字节写进盘了。返回 `Some(end)` 表示这一段下完了
    /// （worker 已被释放，应该带着 `after = Some(end)` 重新 [`assign`](Self::assign)）。
    pub fn commit(&mut self, w: WorkerId, upto: u64) -> Option<u64> {
        let s = self.segs.iter_mut().find(|s| s.worker == Some(w))?;
        s.cursor = upto.clamp(s.cursor, s.reserved);
        if s.cursor == s.end {
            s.worker = None;
            return Some(s.end);
        }
        None
    }

    /// worker `w` 失败或退出：它这段变回无主，没提交的认领作废（那些字节会重下）。
    pub fn release(&mut self, w: WorkerId) {
        if let Some(s) = self.segs.iter_mut().find(|s| s.worker == Some(w)) {
            s.worker = None;
            s.reserved = s.cursor;
        }
    }

    fn give(&mut self, i: usize, w: WorkerId) -> Assignment {
        let s = &mut self.segs[i];
        s.worker = Some(w);
        s.reserved = s.cursor;
        Assignment {
            from: s.cursor,
            to: s.end,
            written: s.cursor - s.start,
        }
    }
}

fn fresh(start: u64, end: u64, worker: Option<WorkerId>) -> Segment {
    Segment {
        start,
        cursor: start,
        reserved: start,
        end,
        worker,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 确定性伪随机（xorshift64*），不引第三方依赖。
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: u64) -> u64 {
            if n == 0 { 0 } else { self.next() % n }
        }
    }

    /// 每次操作后都要成立的结构不变量。
    fn check_shape(p: &Planner) {
        let segs = p.segments();
        if p.size() == 0 {
            assert!(segs.is_empty());
            return;
        }
        assert_eq!(segs[0].start, 0, "第一段必须从 0 开始");
        assert_eq!(segs.last().unwrap().end, p.size(), "最后一段必须到文件尾");
        for (i, s) in segs.iter().enumerate() {
            assert!(s.start <= s.cursor, "cursor 跑到 start 前面: {s:?}");
            assert!(s.cursor <= s.reserved, "reserved 跑到 cursor 前面: {s:?}");
            assert!(s.reserved <= s.end, "reserved 越过 end: {s:?}");
            assert!(s.end > s.start, "空段: {s:?}");
            if i + 1 < segs.len() {
                assert_eq!(s.end, segs[i + 1].start, "段之间有缝或重叠: {s:?}");
            }
            if let Some(w) = s.worker {
                assert!(
                    segs[i + 1..].iter().all(|o| o.worker != Some(w)),
                    "worker {w} 占了两段"
                );
            }
        }
        assert!(p.downloaded() <= p.size(), "下载量超过文件长度");
    }

    /// 一万次随机的领取 / 认领 / 提交 / 释放：不重叠、不丢字节、总长度守恒。
    #[test]
    fn random_ops_never_overlap_and_never_lose_bytes() {
        const SIZE: u64 = 256 << 10;
        const MIN_SPLIT: u64 = 4096;
        const WORKERS: u64 = 8;

        let mut p = Planner::new(SIZE, MIN_SPLIT);
        // 模拟要覆盖切段路径，所以把「剩多少才值得偷」的门槛关掉。
        p.steal_min = 0;
        let mut rng = Rng(0x1234_5678_9abc_def0);
        // written[b] = 字节 b 被写下的次数。
        let mut written = vec![0u8; SIZE as usize];
        let mut wrote_any = false;

        for step in 0..10_000 {
            check_shape(&p);
            let w = rng.below(WORKERS) as WorkerId;

            if let Some(seg) = p.segment_of(w) {
                // 上一步必须要么提交要么释放，所以这里 cursor 一定等于 reserved。
                assert_eq!(seg.cursor, seg.reserved, "step {step}: 有未提交的认领");
                let pos = seg.reserved;
                let room = seg.end - pos;
                if room == 0 {
                    p.release(w);
                    continue;
                }
                let want = 1 + rng.below(room);
                let Claim::Granted(n) = p.claim(w, pos, want) else {
                    panic!("step {step}: 自己占着的段却 claim 不到");
                };
                assert_eq!(n, want.min(room), "step {step}");
                if rng.below(4) == 0 {
                    // 释放：这些字节还没写，应该被别人重新领走。
                    p.release(w);
                } else {
                    for b in (seg.cursor..pos + n).map(|b| b as usize) {
                        assert_eq!(written[b], 0, "step {step}: 字节 {b} 被写了两次");
                        written[b] = 1;
                    }
                    wrote_any = true;
                    p.commit(w, pos + n);
                }
            } else {
                let after = if rng.below(3) == 0 {
                    let segs = p.segments();
                    Some(segs[rng.below(segs.len().max(1) as u64) as usize].start)
                } else {
                    None
                };
                p.assign(w, after);
            }
        }

        // 加速跑完剩下的：谁空着就派活，拿到活就整段写完。
        let mut guard = 0;
        while !p.is_complete() {
            guard += 1;
            assert!(guard < 10_000, "跑不完");
            let mut progressed = false;
            for w in 0..WORKERS as WorkerId {
                let Some(seg) = p.segment_of(w) else { continue };
                let pos = seg.reserved;
                let room = seg.end - pos;
                if room == 0 {
                    p.release(w);
                    continue;
                }
                let n = match p.claim(w, pos, room) {
                    Claim::Granted(n) => n,
                    Claim::Lost => continue,
                };
                if n == 0 {
                    continue;
                }
                for b in (seg.cursor..pos + n).map(|b| b as usize) {
                    assert_eq!(written[b], 0, "收尾阶段字节 {b} 被写了两次");
                    written[b] = 1;
                }
                p.commit(w, pos + n);
                progressed = true;
            }
            if !progressed {
                for w in 0..WORKERS as WorkerId {
                    if !p.holds(w) {
                        p.assign(w, None);
                    }
                }
            }
            check_shape(&p);
        }

        assert_eq!(p.downloaded(), SIZE, "总长度必须守恒");
        assert!(
            written.iter().all(|&b| b == 1),
            "每个字节都得写上，且只写一次"
        );
        assert!(wrote_any, "随机阶段应该真的写过东西");

        // 段表落盘再读回来，进度不能变。
        let saved = p.save();
        let restored = Planner::resume(SIZE, MIN_SPLIT, &saved).unwrap();
        assert_eq!(restored.downloaded(), SIZE);
        assert!(restored.is_complete());
    }

    #[test]
    fn presplit_gives_every_worker_its_own_piece() {
        const SIZE: u64 = 16 << 20;
        let mut p = Planner::new(SIZE, 12 << 10);
        p.presplit(8);
        assert_eq!(p.segments().len(), 8, "8 个 worker 就该有 8 段");
        check_shape(&p);
        assert!(p.segments().iter().all(|s| s.worker.is_none()));

        // 谁来拿就整段给谁，接下来不该再切（这才是「不中止在传的响应」）。
        let mut got = Vec::new();
        for w in 0..8 {
            let a = p.assign(w, None).unwrap();
            let seg = p.segment_of(w).unwrap();
            assert_eq!((a.from, a.to), (seg.start, seg.end), "worker {w}");
            got.push(a);
        }
        assert_eq!(p.segments().len(), 8, "8 个 worker 正好 8 段，不该再切");
        got.sort_by_key(|a| a.from);
        assert_eq!(got[0].from, 0);
        assert_eq!(got[7].to, SIZE);
    }

    #[test]
    fn presplit_never_goes_below_min_split() {
        // 8 KiB 的文件、4 KiB 的 min_split：最多切成 2 段。
        let mut p = Planner::new(8192, 4096);
        p.presplit(8);
        assert_eq!(p.segments().len(), 2);
        check_shape(&p);
        // 切不动就保持原样，不能造出空段或小于 min_split 的段。
        let mut p = Planner::new(4096, 4096);
        p.presplit(8);
        assert_eq!(p.segments().len(), 1);
    }

    #[test]
    fn a_tail_too_short_to_pay_for_itself_is_left_alone() {
        // 16 MiB / 8 段 = 每段 2 MiB，比偷活门槛短：第 9 个 worker 宁可空手回去，
        // 也不该把别人正在传的段切一刀（那一刀要白扔服务端约 1 MB 的在途字节）。
        let mut p = Planner::new(16 << 20, 12 << 10);
        p.presplit(8);
        for w in 0..8 {
            assert!(p.assign(w, None).is_some(), "worker {w}");
        }
        assert_eq!(p.assign(8, None), None);
        assert_eq!(p.segments().len(), 8, "不该有人被切");
    }

    #[test]
    fn a_free_worker_splits_the_largest_segment() {
        let mut p = Planner::new(100 << 10, 4096);
        p.steal_min = 0;
        let a = p.assign(0, None).unwrap();
        assert_eq!((a.from, a.to), (0, 100 << 10), "第一个 worker 整段拿走");
        let b = p.assign(1, None).unwrap();
        assert_eq!(b.to, 100 << 10);
        // 中间点 51200 会往回对齐到 4 KiB 边界。
        assert_eq!(b.from, 49_152, "对半切，上半给新 worker");
        assert_eq!(b.from % ALIGN, 0);
        assert_eq!(p.segments().len(), 2);
        // 3 个 worker 时继续切最大的那一半。
        let c = p.assign(2, None).unwrap();
        assert_eq!(c.from % ALIGN, 0, "切割点对齐");
        assert_eq!(p.segments().len(), 3);
        check_shape(&p);
    }

    #[test]
    fn nothing_is_split_below_min_split() {
        // 两半都正好等于 min_split：切。
        let mut p = Planner::new(4096 * 2, 4096);
        p.steal_min = 0;
        assert!(p.assign(0, None).is_some());
        assert!(p.assign(1, None).is_some(), "半段等于 min_split 仍然值得切");
        assert_eq!(p.segments().len(), 2);
        // 切出来的一半不够 min_split：不切。
        let mut p = Planner::new(4096 + 100, 4096);
        p.steal_min = 0;
        assert!(p.assign(0, None).is_some());
        assert!(p.assign(1, None).is_none(), "切出来的一半不够 min_split");
    }

    #[test]
    fn a_resumed_segment_is_handed_back_at_its_cursor() {
        let mut p = Planner::new(10_000, 4096);
        let a = p.assign(0, None).unwrap();
        p.claim(0, a.from, 3000);
        p.commit(0, 3000);
        let saved = p.save();
        let mut p = Planner::resume(10_000, 4096, &saved).unwrap();
        let a = p.assign(7, None).unwrap();
        assert_eq!(a.from, 3000, "从盘上已有的位置接着下");
        assert_eq!(a.written, 3000);
        assert_eq!(a.to, 10_000);
    }

    #[test]
    fn resume_rejects_a_segment_table_that_does_not_tile_the_file() {
        let bad = [
            // 有缝
            vec![
                SavedSegment {
                    start: 0,
                    cursor: 0,
                    end: 5,
                },
                SavedSegment {
                    start: 6,
                    cursor: 6,
                    end: 10,
                },
            ],
            // 长度对不上
            vec![SavedSegment {
                start: 0,
                cursor: 0,
                end: 9,
            }],
            // cursor 越界
            vec![SavedSegment {
                start: 0,
                cursor: 11,
                end: 10,
            }],
            // 空表但文件非空
            vec![],
        ];
        for saved in bad {
            assert_eq!(
                Planner::resume(10, 1, &saved).err(),
                Some(InvalidState),
                "{saved:?}"
            );
        }
        assert!(Planner::resume(0, 1, &[]).is_ok());
    }

    #[test]
    fn a_stolen_tail_makes_the_owner_lose_its_claim() {
        let size = 100 << 10;
        let mut p = Planner::new(size, 4096);
        p.steal_min = 0;
        p.assign(0, None);
        // worker 1 把 worker 0 的段从中间切走。
        let own = p.assign(1, None).unwrap();
        // worker 0 还想照着旧范围 claim，它现在只拥有 [0, own.from)。
        let Claim::Granted(n) = p.claim(0, 0, own.from) else {
            panic!("分段内的位置应该还能写");
        };
        assert_eq!(n, own.from);
        // 越过新的 end 就一个字节也拿不到。
        assert_eq!(p.claim(0, own.from, 4096), Claim::Granted(0));
        let end = p.commit(0, own.from);
        assert_eq!(end, Some(own.from), "段写满后 worker 被释放");
        assert!(!p.holds(0));
    }

    #[test]
    fn an_empty_file_is_complete_at_once() {
        let p = Planner::new(0, 1);
        assert!(p.is_complete());
        assert_eq!(p.downloaded(), 0);
        assert!(p.save().is_empty());
    }
}
