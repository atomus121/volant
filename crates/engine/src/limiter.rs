//! 限速：网络侧的令牌桶，以及写盘侧的节奏器。
//!
//! 两边用的是同一套算术——**不按秒切窗口**，而是给每一块字节在一条虚拟时间轴上预约
//! 一个时刻，谁先到谁先发。按秒切窗口的问题是窗口边界会突刺（一秒内前 10 ms 把额度
//! 打满），长时间看平均对、短时间看抖动大；预约时刻法把字节摊平成一条直线。
//!
//! 网络侧的等待发生在**读下一个 chunk 之前**，所以服务端会因为 TCP 接收窗口满而自己
//! 减速——限速靠反压实现，不丢字节，也不需要服务端配合。

use std::time::{Duration, Instant};

use tokio::sync::Mutex;

/// 在虚拟时间轴上给 `n` 字节预约一个发送时刻，返回还要等多久。
///
/// `next` 记的是**理论排程**，不是「上一次真的发出去的时刻」。这台机器的定时器粒度是
/// 15.6 ms（`sleep(1ms)` 实测也要 15 ms），一批 256 KiB 在 50 MB/s 下只该睡 5 ms；
/// 要是每次都拿「现在」当新起点，每一次晚醒就白丢十几毫秒，800 批攒下来一条 50 MB/s
/// 的慢盘会被算成 39 MB/s。排程落后时干脆不睡，让后面几批把欠的时间还回来——误差就
/// 被压在一次滴答之内，而不是随批次数量累积。
///
/// 唯一重置排程的情况是真空闲：超过 [`IDLE_RESET`] 没有字节经过，说明前面欠的时间不是
/// 定时器造成的，不必再补（否则闲置之后会一次性突刺）。
fn reserve(next: &mut Instant, rate: u64, n: u64) -> Duration {
    let per = Duration::from_secs_f64(n as f64 / rate as f64);
    let now = Instant::now();
    if *next + IDLE_RESET < now {
        *next = now;
    }
    let slot = *next;
    *next = slot + per;
    slot.saturating_duration_since(now)
}

/// 排程落后超过这么久就当作「空转」，不再补欠账。
const IDLE_RESET: Duration = Duration::from_secs(1);

/// 小于这么长的额度不单独睡，攒着跟下一块一起还。
///
/// 这台机器上 `sleep` 的粒度就是 15.6 ms（见 [`reserve`] 的注释）：为 3 ms 的额度进一次
/// 定时器，回来的时间已经晚了五倍。M5 实测（125 MB/s、8 连接、2 GiB、外部采样 CPU）：
///
/// | 量子 | 真睡次数 | 单核占用 |
/// |------|---------|---------|
/// | 每块都睡 | 5205 | 50.7% |
/// | 16 ms | 4256 | 44.9% |
/// | 128 ms | 874 | 36.8% |
/// | 512 ms | 224 | 33.2% |
/// | 2048 ms | 56 | 27.8% |
///
/// 越粗越省——省的不是定时器本身，是「醒来后 TCP 只送来一个窗口」这件事：粗到 2048 ms 时
/// 限速器几乎不睡，改由接收窗口决定节奏，跟不限速一样按大块搬。但 2048 ms 意味着「两秒
/// 不出字节，然后涌 256 MB」，带宽图像一格一格跳，已经不像限速了。512 ms（1 Gbps 下
/// 64 MB 一波）是拐点附近还能接受的那个，取它。字节账不受影响：欠的时间记在 `next` 里，
/// 下一块自然问得更久。
const QUANTUM: Duration = Duration::from_millis(512);

/// 一条链路共享的令牌桶。`rate` 是字节/秒，`0` 表示不限。
pub struct Limiter {
    rate: u64,
    next: Mutex<Instant>,
}

impl Limiter {
    pub fn new(rate: u64) -> Self {
        Limiter {
            rate,
            next: Mutex::new(Instant::now()),
        }
    }

    /// 等 `n` 字节的额度。所有 worker 共用同一个桶，所以限的是总量。
    ///
    /// 返回真正睡了的时长（不足 [`QUANTUM`] 的额度不睡，返回 0），只给 `--trace` 用。
    pub async fn take(&self, n: u64) -> Duration {
        if self.rate == 0 || n == 0 {
            return Duration::ZERO;
        }
        let wait = {
            let mut next = self.next.lock().await;
            reserve(&mut next, self.rate, n)
        };
        if wait >= QUANTUM {
            tokio::time::sleep(wait).await;
            return wait;
        }
        Duration::ZERO
    }
}

/// 写盘的节奏器：给写队列的每一批字节预约落盘时刻。
///
/// 只给 T6 的慢盘仿真用——真实磁盘不需要它，加上去就是无故拖慢。它住在写盘任务里，
/// 所以不需要锁。
pub(crate) struct Pacer {
    rate: u64,
    next: Instant,
}

impl Pacer {
    pub(crate) fn new(rate: u64) -> Self {
        Pacer {
            rate,
            next: Instant::now(),
        }
    }

    pub(crate) async fn spend(&mut self, n: u64) {
        if self.rate == 0 || n == 0 {
            return;
        }
        let wait = reserve(&mut self.next, self.rate, n);
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三块各 1 MiB、桶 10 MB/s：300 ms 的额度要记在同一个桶上，谁拿都是这个账。
    ///
    /// 这里量排程本身（`next` 走了多远），不等墙钟——300 ms 还不到一个 [`QUANTUM`]，
    /// 量墙钟只能看到「没睡」，那正是下面那条测试的事。
    #[tokio::test]
    async fn credit_is_shared_by_the_whole_bucket() {
        let limiter = Limiter::new(10_000_000);
        let before = *limiter.next.lock().await;
        for _ in 0..3 {
            limiter.take(1_000_000).await;
        }
        let spent = *limiter.next.lock().await - before;
        assert!(
            (290..=310).contains(&spent.as_millis()),
            "3 MiB 在 10 MB/s 下该记 300 ms，实际 {spent:?}"
        );
    }

    /// 小块（每块额度远不足一个 [`QUANTUM`]）也不能白拿：攒着的额度要在后面几块里一起
    /// 还。200 × 64 KiB 在 10 MB/s 下是 1.31 s，够睡两个量子；账要一分不差地记在 `next`
    /// 上，墙钟只用来确认它真的睡了。
    #[tokio::test]
    async fn small_credits_are_paid_back_in_whole_ticks() {
        let limiter = Limiter::new(10_000_000);
        let before = *limiter.next.lock().await;
        let started = Instant::now();
        for _ in 0..200 {
            limiter.take(64 << 10).await;
        }
        let took = started.elapsed();
        let ideal = Duration::from_secs_f64((200 * (64 << 10)) as f64 / 10_000_000.0);

        let spent = *limiter.next.lock().await - before;
        assert!(
            spent.abs_diff(ideal) <= Duration::from_millis(2),
            "排程走了 {spent:?}，该是 {ideal:?}"
        );
        // 没睡的话墙钟会接近 0；睡过头超过一个量子则是排程被重置了。
        assert!(
            took + QUANTUM >= ideal && took <= ideal + QUANTUM,
            "{took:?} 对理想的 {ideal:?}"
        );
    }

    #[tokio::test]
    async fn zero_rate_means_no_wait() {
        let limiter = Limiter::new(0);
        let started = Instant::now();
        limiter.take(1 << 30).await;
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    /// 写盘节奏器要按字节收费：50 MB/s 下 8 批 × 256 KiB = 2 MiB，正好 42 ms。
    ///
    /// 这条测试盯的是「晚醒的时间要还回来」。这台机器的定时器粒度是 15.6 ms（实测
    /// `sleep(1ms)` 也要 15 ms），一批只该睡 5 ms，晚醒一次就多花 3 个批次的时间——
    /// 每次重新计时的话，一条 50 MB/s 的慢盘会被算成 39 MB/s。反过来，只要把欠的时间
    /// 还回来，总误差就被压在一次定时器滴答之内，而不是随批次数量累积。
    #[tokio::test]
    async fn the_pacer_pays_back_what_the_timer_overslept() {
        let mut pacer = Pacer::new(50_000_000);
        let started = Instant::now();
        for _ in 0..40 {
            pacer.spend(256 << 10).await;
        }
        let took = started.elapsed();
        let ideal = Duration::from_secs_f64((10u64 << 20) as f64 / 50_000_000.0);
        // 10 MiB 在 50 MB/s 下是 210 ms，约 13 个滴答，所以「按滴答数累积」的偏差会
        // 是 3 倍左右；这里要求 1 个滴答（16 ms）之内。
        let slack = Duration::from_millis(16);
        assert!(
            took >= ideal.mul_f64(0.95) && took <= ideal + slack,
            "{took:?} 对理想的 {ideal:?}（+{slack:?}）"
        );
    }
}
