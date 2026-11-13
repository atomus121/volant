//! Volant 的故障注入测试服务器（M1 的测试基础设施）。
//!
//! 有意手写 HTTP/1.1 而不是套 hyper：故障注入要精确控制「在哪一字节断开」「每秒发多少」
//! 「什么时候说文件变了」，框架会替我们决定缓冲、超时与连接复用，这些正是要控制的变量。
//!
//! 一条连接一个请求（响应带 `Connection: close`）。代价是每个 Range 请求一次 TCP 握手，
//! 在 localhost 上约 50 µs；如果将来 bench 显示握手成了瓶颈，再加 keep-alive 循环。
//!
//! 内容是可复现的伪随机模式，周期 [`PATTERN_PERIOD`]：客户端用同一个 [`pattern`] 校验下载结果，
//! 所以「下错了字节」能被抓出来，而不只是「字节数对」。
//! ponytail: 周期 16 MiB 意味着整 16 MiB 的错位无法被内容校验发现（计划器的随机模拟测试
//! 另行保证不重叠、不漏字节）；要更细的粒度就把周期改小、按 4 KiB 边界对齐。

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// 内容模式的周期。也是服务端校验缓冲与客户端校验缓冲的大小。
pub const PATTERN_PERIOD: u64 = 16 << 20;

/// 每次写 socket 的字节数。
const CHUNK: usize = 256 << 10;

/// 请求头上限，防住乱发数据把内存吃光。
const MAX_REQUEST: usize = 16 << 10;

/// 可复现的伪随机内容，周期 [`PATTERN_PERIOD`]。
pub fn pattern() -> &'static [u8] {
    static PAT: OnceLock<Vec<u8>> = OnceLock::new();
    PAT.get_or_init(|| {
        let mut v = vec![0u8; PATTERN_PERIOD as usize];
        for (i, b) in v.iter_mut().enumerate() {
            *b = (i.wrapping_mul(2_654_435_761) >> 13) as u8;
        }
        v
    })
}

/// 把 `buf` 填成文件在 `offset` 处应有的字节。
pub fn fill_pattern(buf: &mut [u8], offset: u64) {
    let pat = pattern();
    let period = pat.len();
    let mut done = 0;
    while done < buf.len() {
        let start = (offset + done as u64) as usize % period;
        let n = (buf.len() - done).min(period - start);
        buf[done..done + n].copy_from_slice(&pat[start..start + n]);
        done += n;
    }
}

/// 注入的故障。全部默认关闭 = 一个行为正确的静态文件服务器。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Faults {
    /// 每条连接每秒最多发出的字节。
    pub rate: Option<u64>,
    /// **全局**链路每秒最多发出的字节（所有连接共享）。`rate` 管单连接，`grate` 管链路容量：
    /// 模拟 100 Mbps 宽带要用 `grate=12.5M`，而不是给每条连接 12.5M。
    pub grate: Option<u64>,
    /// 读到请求后先等这么多毫秒再回响应，近似一次往返（本机没有 netem）。
    pub delay: Option<u64>,
    /// 平均每发出这么多正文字节，这条连接卡住 [`Faults::stall`] 毫秒不发。
    ///
    /// 用来近似 TCP 丢包后的重传停顿：0.1% 丢包 × 1460 B 段 ≈ 每 1.46 MB 卡一次，
    /// 所以 `stall_every=1460K,stall=200` 就是「0.1% 丢包 + 200 ms 重传超时」。
    /// 卡顿随连接的字节进度随机错开（不是所有连接同时卡），否则链路会周期性地集体空转。
    pub stall_every: Option<u64>,
    /// 卡多久（毫秒），默认 200。
    pub stall: Option<u64>,
    /// 每条连接发出这么多字节后直接断开（不写完整响应体）。
    pub cut: Option<u64>,
    /// 同时允许打开的连接数；超出的请求得到 503。
    pub conns: Option<usize>,
    /// 忽略 Range：永远 200 全量。
    pub norange: bool,
    /// 全量响应不带 Content-Length（靠关连接表示结束）。
    pub nolength: bool,
    /// 累计发出这么多字节后「文件被改了」：ETag 变 v2，带旧 If-Range 的请求得到 200 全量
    /// （RFC 9110 §13.1.5 的要求，也是我们探测「源已变」的真实路径）。
    pub mutate: Option<u64>,
}

impl Faults {
    /// `rate=2M,grate=12.5M,delay=20,stall_every=1460K,stall=200,cut=2M,conns=2,norange,nolength,mutate=1M`
    pub fn parse(s: &str) -> Result<Self, String> {
        let mut f = Faults::default();
        for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (key, val) = match part.split_once('=') {
                Some((k, v)) => (k.trim(), Some(v.trim())),
                None => (part, None),
            };
            match (key, val) {
                ("rate", Some(v)) => {
                    f.rate = Some(parse_size(v).map_err(|e| format!("rate: {e}"))?)
                }
                ("grate", Some(v)) => {
                    f.grate = Some(parse_size(v).map_err(|e| format!("grate: {e}"))?)
                }
                ("delay", Some(v)) => f.delay = Some(v.parse().map_err(|_| format!("delay: {v}"))?),
                ("stall_every", Some(v)) => {
                    f.stall_every = Some(parse_size(v).map_err(|e| format!("stall_every: {e}"))?)
                }
                ("stall", Some(v)) => f.stall = Some(v.parse().map_err(|_| format!("stall: {v}"))?),
                ("cut", Some(v)) => f.cut = Some(parse_size(v).map_err(|e| format!("cut: {e}"))?),
                ("mutate", Some(v)) => {
                    f.mutate = Some(parse_size(v).map_err(|e| format!("mutate: {e}"))?)
                }
                ("conns", Some(v)) => f.conns = Some(v.parse().map_err(|_| format!("conns: {v}"))?),
                ("norange", None) => f.norange = true,
                ("nolength", None) => f.nolength = true,
                _ => return Err(format!("未知故障 `{part}`")),
            }
        }
        Ok(f)
    }

    /// 给人看的键值对，`GET /` 用它自报家门。
    pub fn describe(&self) -> String {
        format!(
            "rate={:?} grate={:?} delay={:?} stall_every={:?} stall={:?} cut={:?} conns={:?} norange={} nolength={} mutate={:?}",
            self.rate,
            self.grate,
            self.delay,
            self.stall_every,
            self.stall,
            self.cut,
            self.conns,
            self.norange,
            self.nolength,
            self.mutate
        )
    }
}

fn parse_size(s: &str) -> Result<u64, String> {
    let (num, mult) = match s.as_bytes().last().map(u8::to_ascii_uppercase) {
        Some(b'K') => (&s[..s.len() - 1], 1u64 << 10),
        Some(b'M') => (&s[..s.len() - 1], 1u64 << 20),
        Some(b'G') => (&s[..s.len() - 1], 1u64 << 30),
        _ => (s, 1),
    };
    // 小数是给链路速率用的：100 Mbps = 12.5M 字节/秒。
    let n: f64 = num.trim().parse().map_err(|_| format!("不是数字: {s}"))?;
    if !n.is_finite() || n < 0.0 {
        return Err(format!("不是正数: {s}"));
    }
    Ok((n * mult as f64) as u64)
}

/// 全局链路整形：所有连接共享一条 `rate` 字节/秒的管道。
///
/// 虚拟时钟预约：每次发送前预约一个时间片，谁先到谁先占，于是聚合速率正好是 `rate`，
/// 而单条连接能占到多少取决于它有多少数据可发（和真实链路抢带宽一样）。
struct Shaper {
    next: std::sync::Mutex<Instant>,
}

impl Shaper {
    fn new() -> Self {
        Self {
            next: std::sync::Mutex::new(Instant::now()),
        }
    }

    /// 等到 `n` 字节轮到发送的时刻。
    ///
    /// `next` 记的是理论排程而不是「上次真发出去的时刻」：定时器只会醒晚，拿「现在」当
    /// 新起点就把晚醒的时间白丢了，量出来的链路会比设定的快。排程落后时不睡，后面几块
    /// 把欠的时间还回来；真的空闲了一秒以上才重置，免得闲置之后一次性突刺。
    async fn spend(&self, n: u64, rate: u64) {
        if rate == 0 || n == 0 {
            return;
        }
        let per = Duration::from_secs_f64(n as f64 / rate as f64);
        let now = Instant::now();
        let slot = {
            let mut next = self.next.lock().unwrap_or_else(|e| e.into_inner());
            if *next + Duration::from_secs(1) < now {
                *next = now;
            }
            let slot = *next;
            *next = slot + per;
            slot
        };
        if slot > now {
            tokio::time::sleep(slot - now).await;
        }
    }
}

/// 一条连接的随机卡顿计划：错开各连接的停顿，免得整条链路周期性地一起空转。
struct Stalls {
    every: u64,
    dur: Duration,
    due: u64,
    rng: u64,
}

impl Stalls {
    fn new(every: u64, dur: Duration, seed: u64) -> Self {
        let mut s = Self {
            every,
            dur,
            due: 0,
            rng: seed | 1,
        };
        s.due = s.next_due(0);
        s
    }

    fn rand(&mut self) -> u64 {
        // xorshift64：只为错开，不需要统计学质量。
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }

    fn next_due(&mut self, sent: u64) -> u64 {
        // 期望间隔 = `every`，实际落在 [0.5×, 1.5×) 之间。
        sent + self.every / 2 + self.rand() % self.every.max(1)
    }

    /// 发到 `sent` 了，该不该卡一下。
    async fn maybe(&mut self, sent: u64) {
        if sent >= self.due {
            tokio::time::sleep(self.dur).await;
            self.due = self.next_due(sent);
        }
    }
}

/// 跑起来的服务器。drop 即停（监听端口关闭）。
pub struct Server {
    pub addr: SocketAddr,
    pub faults: Faults,
    served: Arc<AtomicU64>,
    requests: Arc<AtomicU64>,
    active: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    /// 绑定 `127.0.0.1:0` 并开始服务。
    pub async fn start(faults: Faults) -> io::Result<Self> {
        Self::start_on("127.0.0.1:0", faults).await
    }

    pub async fn start_on(addr: &str, faults: Faults) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let served = Arc::new(AtomicU64::new(0));
        let requests = Arc::new(AtomicU64::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let shared = Arc::new(Shared {
            faults: faults.clone(),
            served: served.clone(),
            requests: requests.clone(),
            active: active.clone(),
            shaper: faults
                .grate
                .is_some_and(|g| g > 0)
                .then(|| Arc::new(Shaper::new())),
        });
        let task = tokio::spawn(async move {
            let mut conns: u64 = 0;
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                let _ = sock.set_nodelay(true);
                let shared = shared.clone();
                conns += 1;
                tokio::spawn(async move {
                    let _ = handle(sock, shared, conns).await;
                });
            }
        });
        Ok(Server {
            addr,
            faults,
            served,
            requests,
            active,
            task,
        })
    }

    /// `http://127.0.0.1:port/file/<size>`
    pub fn url(&self, size: u64) -> String {
        format!("http://{}/file/{}", self.addr, size)
    }

    /// 已发出的字节总数（`mutate` 用它当阈值）。
    pub fn served(&self) -> u64 {
        self.served.load(Ordering::Relaxed)
    }

    /// `/file/` 请求数：引擎「有没有额外重连」的硬证据。
    pub fn requests(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }

    /// 当前打开的连接数。
    pub fn active(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }

    /// 等到累计发出 `n` 字节，或超时。
    pub async fn wait_served(&self, n: u64, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while self.served() < n {
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        true
    }
}

struct Shared {
    faults: Faults,
    served: Arc<AtomicU64>,
    requests: Arc<AtomicU64>,
    active: Arc<AtomicUsize>,
    shaper: Option<Arc<Shaper>>,
}

async fn handle(mut sock: TcpStream, shared: Arc<Shared>, seq: u64) -> io::Result<()> {
    let n = shared.active.fetch_add(1, Ordering::SeqCst) + 1;
    struct Done<'a>(&'a AtomicUsize);
    impl Drop for Done<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let _done = Done(&shared.active);

    let Some(req) = read_request(&mut sock).await? else {
        return Ok(());
    };

    // 一次往返的近似：真实链路上请求与响应之间至少隔一个 RTT。
    if let Some(ms) = shared.faults.delay {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }

    if let Some(limit) = shared.faults.conns
        && n > limit
    {
        let body = b"too many connections\n";
        write_headers(
            &mut sock,
            "503 Service Unavailable",
            &[
                ("Content-Length", body.len().to_string()),
                ("Retry-After", "1".into()),
                ("Connection", "close".into()),
            ],
        )
        .await?;
        sock.write_all(body).await?;
        sock.flush().await?;
        return Ok(());
    }

    let (path, _) = match req.path.split_once('?') {
        Some((p, q)) => (p, q),
        None => (req.path.as_str(), ""),
    };

    if path == "/" || path == "/index" {
        let body = format!(
            "volant-labserver\nfaults: {}\nfile: /file/<bytes>  (内容 = fill_pattern 生成的模式)\n",
            shared.faults.describe()
        );
        return reply(
            &mut sock,
            "200 OK",
            &[("Content-Type", "text/plain".into())],
            Some(body.into_bytes()),
            &shared,
        )
        .await;
    }

    // 只数 `/file/` 请求，`/stats` 自己不算——bench 用「请求数」判断引擎有没有额外重连。
    if path == "/stats" {
        let body = format!(
            "requests {}\nserved {}\nactive {}\n",
            shared.requests.load(Ordering::Relaxed),
            shared.served.load(Ordering::Relaxed),
            shared.active.load(Ordering::Relaxed)
        );
        return reply(
            &mut sock,
            "200 OK",
            &[("Content-Type", "text/plain".into())],
            Some(body.into_bytes()),
            &shared,
        )
        .await;
    }

    let Some(size_str) = path.strip_prefix("/file/") else {
        let body = b"not found\n".to_vec();
        return reply(&mut sock, "404 Not Found", &[], Some(body), &shared).await;
    };
    let Ok(size) = size_str.parse::<u64>() else {
        let body = b"bad size\n".to_vec();
        return reply(&mut sock, "400 Bad Request", &[], Some(body), &shared).await;
    };

    serve_file(&mut sock, size, &req, &shared, seq).await
}

async fn serve_file(
    sock: &mut TcpStream,
    size: u64,
    req: &Request,
    shared: &Shared,
    seq: u64,
) -> io::Result<()> {
    shared.requests.fetch_add(1, Ordering::Relaxed);
    let f = &shared.faults;
    // 文件是否已经「被改」：累计发出量达到阈值之后，ETag 变 v2。
    let mutated = f
        .mutate
        .is_some_and(|m| shared.served.load(Ordering::Relaxed) >= m);
    let etag = if mutated { "\"v2\"" } else { "\"v1\"" };
    let last_modified = "Wed, 21 Oct 2015 07:28:00 GMT";

    // If-Range 不匹配 → 必须回 200 全量（RFC 9110 §13.1.5），这正是「下载中文件被改」的路径。
    let if_range = req.header("if-range");
    let range_ok = !f.norange && if_range.is_none_or(|v| v.trim() == etag);

    let range = req.header("range").and_then(|v| parse_range(v, size));
    if req.header("range").is_some() && range.is_none() && range_ok {
        let body = b"range not satisfiable\n".to_vec();
        return reply(
            sock,
            "416 Range Not Satisfiable",
            &[("Content-Range", format!("bytes */{size}"))],
            Some(body),
            shared,
        )
        .await;
    }

    let (status, from, to) = match range {
        Some((a, b)) if range_ok => ("206 Partial Content".to_string(), a, b),
        _ => ("200 OK".to_string(), 0, size.saturating_sub(1)),
    };
    let len = if size == 0 { 0 } else { to - from + 1 };

    let mut headers: Vec<(&str, String)> = vec![
        ("Content-Type", "application/octet-stream".into()),
        ("ETag", etag.into()),
        ("Last-Modified", last_modified.into()),
        ("Connection", "close".into()),
        (
            "Content-Disposition",
            format!("attachment; filename=\"blob-{size}.bin\"; filename*=UTF-8''blob-{size}.bin"),
        ),
    ];
    if range_ok {
        headers.push(("Accept-Ranges", "bytes".into()));
    }
    if from > 0 || range.is_some_and(|_| range_ok) {
        headers.push(("Content-Range", format!("bytes {from}-{to}/{size}")));
    }
    // nolength 只对全量 200 有意义：206 的长度由 Content-Range 决定，去掉会让客户端无法校验。
    if !(f.nolength && status.starts_with("200")) {
        headers.push(("Content-Length", len.to_string()));
    }

    write_headers(sock, &status, &headers).await?;

    // 正文：按块生成、按速率限流、按 cut 截断。
    // cut 是「这条连接最多发这么多字节」，所以把它当成本次响应的上限，而不是发完
    // 一整块之后才发现已经超了——客户端要看到的是精确的短读。
    let limit = match f.cut {
        Some(cut) => len.min(cut),
        None => len,
    };
    let mut buf = vec![0u8; CHUNK.min(limit.max(1) as usize)];
    let start = Instant::now();
    let mut sent: u64 = 0;
    let mut stalls = f.stall_every.filter(|e| *e > 0).map(|every| {
        Stalls::new(
            every,
            Duration::from_millis(f.stall.unwrap_or(200)),
            seq.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(from),
        )
    });
    while sent < limit {
        let n = (limit - sent).min(buf.len() as u64) as usize;
        fill_pattern(&mut buf[..n], from + sent);
        // 先等链路让出这一块的发送时刻，再真的发出去。反过来（先发后等）等于让每条
        // 连接先白拿一块，8 条连接就是 2 MiB 的头部空转，量 100 Mbps 会量出 105%。
        if let (Some(shaper), Some(grate)) = (&shared.shaper, f.grate) {
            shaper.spend(n as u64, grate).await;
        }
        sock.write_all(&buf[..n]).await?;
        sent += n as u64;
        shared.served.fetch_add(n as u64, Ordering::Relaxed);
        if let Some(rate) = f.rate
            && rate > 0
        {
            let target = start + Duration::from_secs_f64(sent as f64 / rate as f64);
            let now = Instant::now();
            if target > now {
                tokio::time::sleep(target - now).await;
            }
        }
        if let Some(stalls) = &mut stalls {
            stalls.maybe(sent).await;
        }
    }
    // 故意不写完整：客户端会收到「响应体短于 Content-Length」。
    sock.flush().await?;
    Ok(())
}

async fn reply(
    sock: &mut TcpStream,
    status: &str,
    extra: &[(&str, String)],
    body: Option<Vec<u8>>,
    shared: &Shared,
) -> io::Result<()> {
    let mut headers: Vec<(&str, String)> = vec![("Connection", "close".into())];
    for (k, v) in extra {
        headers.push((k, (*v).to_string()));
    }
    if let Some(b) = &body {
        headers.push(("Content-Length", b.len().to_string()));
    }
    write_headers(sock, status, &headers).await?;
    if let Some(b) = &body {
        sock.write_all(b).await?;
        shared.served.fetch_add(b.len() as u64, Ordering::Relaxed);
    }
    sock.flush().await?;
    Ok(())
}

async fn write_headers(
    sock: &mut TcpStream,
    status: &str,
    headers: &[(&str, String)],
) -> io::Result<()> {
    let mut head = format!("HTTP/1.1 {status}\r\n");
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    sock.write_all(head.as_bytes()).await
}

struct Request {
    path: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// 读到空行为止；只关心请求行与头。
async fn read_request(sock: &mut TcpStream) -> io::Result<Option<Request>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(end) = find_headers_end(&buf) {
            let head = String::from_utf8_lossy(&buf[..end]).into_owned();
            let mut lines = head.split("\r\n");
            let Some(line) = lines.next() else {
                return Ok(None);
            };
            let mut parts = line.split_whitespace();
            let (Some(_method), Some(path)) = (parts.next(), parts.next()) else {
                return Ok(None);
            };
            if !path.starts_with('/') {
                return Ok(None);
            }
            let mut headers = Vec::new();
            for l in lines {
                if let Some((k, v)) = l.split_once(':') {
                    headers.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
            return Ok(Some(Request {
                path: path.to_string(),
                headers,
            }));
        }
        if buf.len() > MAX_REQUEST {
            return Ok(None);
        }
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// `bytes=from-to` / `bytes=from-` / `bytes=-suffix`，返回闭区间。
pub fn parse_range(v: &str, size: u64) -> Option<(u64, u64)> {
    let v = v.trim();
    let rest = v.strip_prefix("bytes=")?;
    if size == 0 {
        return None;
    }
    let (a, b) = rest.split_once('-')?;
    let (from, to) = if a.trim().is_empty() {
        let suffix: u64 = b.trim().parse().ok()?;
        let n = suffix.min(size);
        (size - n, size - 1)
    } else {
        let from: u64 = a.trim().parse().ok()?;
        let to = if b.trim().is_empty() {
            size - 1
        } else {
            b.trim().parse::<u64>().ok()?.min(size - 1)
        };
        (from, to)
    };
    (from <= to && from < size).then_some((from, to))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个最小客户端：发一个 GET，读回状态行、头与正文。
    async fn get(addr: SocketAddr, path: &str, headers: &[(&str, &str)]) -> (String, Vec<u8>) {
        use tokio::io::AsyncReadExt;
        let mut s = TcpStream::connect(addr).await.unwrap();
        let mut req = format!("GET {path} HTTP/1.1\r\nHost: x\r\n");
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).await.unwrap();
        let split = find_headers_end(&out).unwrap() + 4;
        (
            String::from_utf8_lossy(&out[..split]).into_owned(),
            out[split..].to_vec(),
        )
    }

    #[tokio::test]
    async fn serves_ranges_with_the_matching_bytes() {
        let srv = Server::start(Faults::default()).await.unwrap();
        let size = 4096;
        let (head, body) = get(
            srv.addr,
            &format!("/file/{size}"),
            &[("Range", "bytes=1000-1099")],
        )
        .await;
        assert!(head.starts_with("HTTP/1.1 206"), "{head}");
        assert!(
            head.contains("Content-Range: bytes 1000-1099/4096"),
            "{head}"
        );
        assert!(head.contains("Content-Length: 100"), "{head}");
        let mut want = vec![0u8; 100];
        fill_pattern(&mut want, 1000);
        assert_eq!(body, want);
    }

    #[tokio::test]
    async fn full_file_and_the_last_byte() {
        let srv = Server::start(Faults::default()).await.unwrap();
        let size = 1000;
        let (head, body) = get(srv.addr, &format!("/file/{size}"), &[]).await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert_eq!(body.len() as u64, size);
        let mut want = vec![0u8; size as usize];
        fill_pattern(&mut want, 0);
        assert_eq!(body, want);
    }

    #[tokio::test]
    async fn norange_ignores_the_header() {
        let srv = Server::start(Faults {
            norange: true,
            ..Faults::default()
        })
        .await
        .unwrap();
        let (head, body) = get(srv.addr, "/file/100", &[("Range", "bytes=0-0")]).await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(!head.contains("Content-Range"), "{head}");
        assert_eq!(body.len(), 100);
    }

    #[tokio::test]
    async fn nolength_gives_no_length() {
        let srv = Server::start(Faults {
            nolength: true,
            ..Faults::default()
        })
        .await
        .unwrap();
        let (head, body) = get(srv.addr, "/file/64", &[]).await;
        assert!(!head.contains("Content-Length"), "{head}");
        assert_eq!(body.len(), 64);
    }

    #[tokio::test]
    async fn cut_truncates_the_body() {
        let srv = Server::start(Faults {
            cut: Some(10),
            ..Faults::default()
        })
        .await
        .unwrap();
        let (head, body) = get(srv.addr, "/file/1000", &[]).await;
        assert!(head.contains("Content-Length: 1000"), "{head}");
        assert_eq!(body.len(), 10, "只发出 10 字节就断开");
    }

    #[tokio::test]
    async fn a_stale_if_range_gets_the_whole_file() {
        let srv = Server::start(Faults {
            mutate: Some(1),
            ..Faults::default()
        })
        .await
        .unwrap();
        // 先发一点字节，让 mutate 阈值达成。
        let _ = get(srv.addr, "/file/8", &[]).await;
        assert!(srv.wait_served(1, Duration::from_secs(1)).await);
        let (head, body) = get(
            srv.addr,
            "/file/100",
            &[("Range", "bytes=10-19"), ("If-Range", "\"v1\"")],
        )
        .await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert_eq!(body.len(), 100, "源已变，回完整文件");
    }

    #[tokio::test]
    async fn connection_limit_refuses_the_extra_one() {
        let srv = Server::start(Faults {
            conns: Some(0),
            ..Faults::default()
        })
        .await
        .unwrap();
        let (head, _) = get(srv.addr, "/file/10", &[]).await;
        assert!(head.starts_with("HTTP/1.1 503"), "{head}");
    }

    #[test]
    fn ranges_parse() {
        assert_eq!(parse_range("bytes=0-0", 100), Some((0, 0)));
        assert_eq!(parse_range("bytes=10-", 100), Some((10, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=0-999", 100), Some((0, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("items=0-1", 100), None);
        assert_eq!(parse_range("bytes=0-0", 0), None);
    }

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("2M").unwrap(), 2 << 20);
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("1G").unwrap(), 1 << 30);
        // 100 Mbps = 12.5 MB/s，链路速率必须能吃小数。
        assert_eq!(parse_size("12.5M").unwrap(), 13_107_200);
    }

    #[test]
    fn faults_parse_every_knob() {
        let f = Faults::parse("grate=12.5M,delay=20,stall_every=1460K,stall=200").unwrap();
        assert_eq!(f.grate, Some(13_107_200));
        assert_eq!(f.delay, Some(20));
        assert_eq!(f.stall_every, Some(1_495_040));
        assert_eq!(f.stall, Some(200));
        assert!(Faults::parse("grate=1M").is_ok());
        assert!(Faults::parse("grate=abc").is_err());
    }

    /// `grate` 管的是**聚合**带宽：两条连接分 1 MiB 也要花满 1 MiB 的时间。
    #[tokio::test]
    async fn grate_caps_the_whole_link() {
        let srv = Server::start(Faults {
            grate: Some(2 << 20),
            ..Faults::default()
        })
        .await
        .unwrap();
        let half = 512 << 10;
        let path = format!("/file/{half}");
        let t = Instant::now();
        let (a, b) = tokio::join!(get(srv.addr, &path, &[]), get(srv.addr, &path, &[]));
        let secs = t.elapsed().as_secs_f64();
        assert_eq!(a.1.len() + b.1.len(), 1 << 20);
        // 1 MiB / 2 MB/s ≈ 0.52 s。不整形的话本机是几毫秒。
        //
        // 少算一块：整形是按「这一块轮到什么时候发」预约的（先把 256 KiB 的额度等够再
        // 写），所以最后一块的 131 ms 传输时间落在最后一次写之后，不在这段耗时里。下限
        // 按「三块」留，别按「四块」卡死在边界上。
        assert!(secs > 0.33, "只用了 {secs:.3}s，全局限速没生效");
        assert!(secs < 3.0, "用了 {secs:.3}s，限速过头了");
    }

    #[tokio::test]
    async fn delay_waits_before_the_response() {
        let srv = Server::start(Faults {
            delay: Some(120),
            ..Faults::default()
        })
        .await
        .unwrap();
        let t = Instant::now();
        let (_, body) = get(srv.addr, "/file/16", &[]).await;
        assert_eq!(body.len(), 16);
        assert!(t.elapsed().as_millis() >= 100, "{:?}", t.elapsed());
    }

    #[tokio::test]
    async fn stall_pauses_mid_stream() {
        let srv = Server::start(Faults {
            stall_every: Some(1000),
            stall: Some(300),
            ..Faults::default()
        })
        .await
        .unwrap();
        let t = Instant::now();
        let (_, body) = get(srv.addr, "/file/4000", &[]).await;
        assert_eq!(body.len(), 4000, "卡顿不该丢字节");
        // 4 KB 的响应体只有一次 `write_all`（`CHUNK` 是 256 KB），卡顿计划在每块之后
        // 才检查一次，所以这事只能卡一下；`due` 落在 [500,1500)，4 KB 必然越过它。
        // 断言就卡这一下的时长，别按「4000 / 1000 = 4 次」算——那是 `CHUNK` 还小的时候。
        assert!(t.elapsed().as_millis() >= 280, "{:?}", t.elapsed());
    }

    #[tokio::test]
    async fn stats_counts_file_requests_only() {
        let srv = Server::start(Faults::default()).await.unwrap();
        get(srv.addr, "/file/8", &[]).await;
        get(srv.addr, "/file/8", &[]).await;
        let (_, body) = get(srv.addr, "/stats", &[]).await;
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("requests 2\n"), "{text}");
        assert!(text.contains("served 16\n"), "{text}");
    }
}
