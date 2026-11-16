//! 浏览器捕获的入口：扩展 → native host → **本进程** → GUI 队列。
//!
//! 管道上跑的就是 native messaging 的帧（4 字节小端长度 + JSON），host 只做字节转发，
//! 所以这里读到的字节和浏览器发出的一模一样——host 不需要认识 JSON，扩展加字段也不用改它。
//!
//! 为什么不是 host 直接把 URL 交给引擎：引擎在界面进程里，`Handle`/事件都在那边；
//! 让第二个进程也去碰 `.volant.state` 会打架。管道是多进程唯一的正道。
//!
//! 实测这条链路（含冷启动）见 `docs/m5/REPORT.md`。

// 两个 bin 各编一份：host 只用帧的读写，界面才用 `Listener`。
#![allow(dead_code)]

use std::fmt;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};

use crate::notify::Notifier;

/// Versioned so an orphaned pipe handle from an older app cannot capture the
/// host's next request or make new connections wait forever.
pub const PIPE: &str = r"\\.\pipe\volant-capture-v2";

/// 单帧上限。Chrome 对 host→浏览器 的回复限 1 MB，我们对浏览器→host 也定成 1 MB。
pub const MAX_FRAME: usize = 1 << 20;

/// 浏览器发来的一条捕获。
#[derive(Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct Capture {
    /// `download`, `ping`, `rules`, `videoLookup`, `videoPick`, ...
    #[serde(rename = "type")]
    pub action: String,
    /// One browser request's correlation id.
    #[serde(default, rename = "req")]
    pub request_id: u64,
    pub url: String,
    pub referer: String,
    /// The Origin the page's player used for its stream request.
    pub origin: String,
    pub cookie: String,
    pub user_agent: String,
    /// The video's own page, which may differ from the tab that embedded it.
    pub page: String,
    /// A plain media URL or an HLS/DASH manifest.
    pub src: String,
    pub title: String,
    pub mime: String,
    pub token: String,
    pub choice: usize,
    pub subtitles: bool,
    /// Domain-scoped browser cookies for yt-dlp's Netscape cookie jar.
    pub cookies: Vec<BrowserCookie>,
    /// 浏览器里的 `Date.now()`，用来量「点下载 → 引擎开跑」的真实耗时。
    pub t: u64,
    /// The reply writer for this request. It is attached after deserialization
    /// and never comes from browser JSON.
    #[serde(skip)]
    pub reply: Option<CaptureReply>,
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowserCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    #[serde(rename = "httpOnly")]
    pub http_only: bool,
    #[serde(rename = "hostOnly")]
    pub host_only: bool,
    #[serde(rename = "expirationDate")]
    pub expiration_date: Option<f64>,
}

#[derive(Clone)]
pub struct CaptureReply {
    request_id: u64,
    output: Arc<Mutex<std::fs::File>>,
}

impl CaptureReply {
    fn new(request_id: u64, output: std::fs::File) -> CaptureReply {
        CaptureReply {
            request_id,
            output: Arc::new(Mutex::new(output)),
        }
    }

    pub fn send(&self, value: &serde_json::Value) -> io::Result<()> {
        let mut value = value.clone();
        if let Some(object) = value.as_object_mut() {
            object.insert("req".into(), self.request_id.into());
        }
        let payload = serde_json::to_vec(&value)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let mut output = self
            .output
            .lock()
            .map_err(|_| io::Error::other("capture reply writer poisoned"))?;
        write_frame(&mut *output, &payload)
    }
}

impl fmt::Debug for Capture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Capture")
            .field("action", &self.action)
            .field("request_id", &self.request_id)
            .field("url", &self.url)
            .field("page", &self.page)
            .field("src", &self.src)
            .field("title", &self.title)
            .field("has_reply", &self.reply.is_some())
            .finish()
    }
}

impl Capture {
    /// 给引擎的额外请求头。空的直接不要，避免发一个空的 `Cookie:`。
    pub fn headers(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (name, value) in [
            ("Cookie", &self.cookie),
            ("Referer", &self.referer),
            ("Origin", &self.origin),
            ("User-Agent", &self.user_agent),
        ] {
            if !value.is_empty() {
                out.push((name.to_string(), value.clone()));
            }
        }
        out
    }
}

/// 读一帧（长度前缀 + 负载）。对端正常关闭返回 `UnexpectedEof`。
pub fn read_frame(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("帧长 {len} 不合理"),
        ));
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload)?;
    Ok(payload)
}

/// 写一帧。`flush` 跟着写，否则字节会卡在缓冲里，浏览器那边看起来像没反应。
pub fn write_frame(w: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    w.write_all(&(payload.len() as u32).to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

#[cfg(windows)]
mod imp {
    use std::fs::File;
    use std::os::windows::io::FromRawHandle;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::Receiver;
    use std::thread::JoinHandle;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    use super::{Capture, CaptureReply, PIPE, read_frame};

    /// `windows-sys` of this workspace does not export the access-mode constants.
    /// `PIPE_ACCESS_DUPLEX` is `FILE_READ_DATA | FILE_WRITE_DATA`.
    const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;

    /// 管道上的 JSON → `Capture`。坏消息只丢那一条，不让整条管道停摆。
    fn parse(payload: &[u8], output: std::fs::File) -> Option<Capture> {
        let mut c: Capture = serde_json::from_slice(payload).ok()?;
        let action = if c.action.is_empty() {
            "download"
        } else {
            c.action.as_str()
        };
        let target = if c.url.trim().is_empty() {
            c.page.trim()
        } else {
            c.url.trim()
        };
        let web_target = target.starts_with("http://") || target.starts_with("https://");
        let known = matches!(
            action,
            "download"
                | "ping"
                | "rules"
                | "videoLookup"
                | "videoPoll"
                | "videoPick"
                | "video"
                | "media"
                | "mediaDownload"
                | "open"
                | "openVideoSettings"
        );
        let targetless = matches!(
            action,
            "ping" | "rules" | "videoPoll" | "videoPick" | "open" | "openVideoSettings"
        );
        if !known || (!web_target && !targetless) {
            return None;
        }
        c.reply = Some(CaptureReply::new(c.request_id, output));
        Some(c)
    }

    pub struct Listener {
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Listener {
        /// 后台线程守着管道，来一条捕获就往 `Receiver` 里塞一条。
        pub fn start(notify: super::Notifier) -> (Listener, Receiver<Capture>) {
            let (tx, rx) = std::sync::mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));
            let t_stop = stop.clone();
            let thread = std::thread::Builder::new()
                .name("volant-capture".into())
                .spawn(move || {
                    while !t_stop.load(Ordering::Relaxed) {
                        let handle = unsafe {
                            CreateNamedPipeW(
                                wide(PIPE).as_ptr(),
                                PIPE_ACCESS_DUPLEX,
                                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                                PIPE_UNLIMITED_INSTANCES,
                                4096,
                                4096,
                                0,
                                std::ptr::null(),
                            )
                        };
                        if handle == INVALID_HANDLE_VALUE {
                            break; // 建不出管道：没有捕获功能也不该拖垮下载
                        }
                        // 阻塞等 host 连上来；连上之后这条句柄交给 File 管生命周期。
                        let ok = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };
                        if ok == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
                            unsafe { CloseHandle(handle) };
                            continue;
                        }
                        let mut conn = unsafe { File::from_raw_handle(handle as _) };
                        let tx = tx.clone();
                        let notify = notify.clone();
                        // A browser or host can disappear while holding a pipe.
                        // Serve each connection separately so it cannot block the
                        // next popup or tab from reaching the app.
                        std::thread::Builder::new()
                            .name("volant-capture-conn".into())
                            .spawn(move || {
                                while let Ok(frame) = read_frame(&mut conn) {
                                    let writer = match conn.try_clone() {
                                        Ok(writer) => writer,
                                        Err(_) => break,
                                    };
                                    let Some(c) = parse(&frame, writer) else {
                                        continue;
                                    };
                                    crate::log::line(&format!("browser capture {}", c.action));
                                    if tx.send(c).is_ok() {
                                        notify.notify();
                                    }
                                }
                            })
                            .ok();
                    }
                })
                .expect("spawn capture thread");
            (
                Listener {
                    stop,
                    thread: Some(thread),
                },
                rx,
            )
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            // 自己连一下把阻塞的 ConnectNamedPipe 叫醒；线程没人 join，进程退出时自然结束
            // （它只在等连接时阻塞，不会拿锁、不会写文件）。
            let _ = std::fs::File::open(PIPE);
            self.thread.take();
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(windows)]
#[allow(unused_imports)]
pub use imp::Listener;

#[cfg(not(windows))]
mod stub {
    use std::sync::mpsc::Receiver;

    use super::Capture;

    pub struct Listener;

    impl Listener {
        pub fn start(_notify: Notifier) -> (Listener, Receiver<Capture>) {
            let (_, rx) = std::sync::mpsc::channel();
            (Listener, rx)
        }
    }
}

#[cfg(not(windows))]
#[allow(unused_imports)]
pub use stub::Listener;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"{\"url\":\"http://a\"}").unwrap();
        let mut cur = &buf[..];
        assert_eq!(read_frame(&mut cur).unwrap(), b"{\"url\":\"http://a\"}");
    }

    #[test]
    fn absurd_lengths_are_rejected() {
        let mut bad = u32::MAX.to_le_bytes().to_vec();
        bad.extend_from_slice(b"x");
        assert!(read_frame(&mut &bad[..]).is_err());
        assert!(read_frame(&mut &0u32.to_le_bytes()[..]).is_err());
    }

    #[test]
    fn headers_are_built_from_the_message() {
        let c: Capture = serde_json::from_str(r#"{"url":" http://a ","cookie":"k=v"}"#).unwrap();
        assert_eq!(c.url.trim(), "http://a");
        assert_eq!(c.headers(), vec![("Cookie".to_string(), "k=v".to_string())]);
        // 空的不发：`Cookie: ` 比没有更难查。
        let c: Capture =
            serde_json::from_str(r#"{"url":"http://a","referer":"http://b/"}"#).unwrap();
        assert_eq!(
            c.headers(),
            vec![("Referer".to_string(), "http://b/".to_string())]
        );
    }

    #[test]
    fn video_requests_keep_domain_scoped_cookies() {
        let c: Capture = serde_json::from_str(
            r#"{
                "req":7,
                "type":"videoLookup",
                "page":"https://videos.test/watch",
                "src":"https://cdn.test/master.m3u8",
                "cookies":[{"name":"sid","value":"a","domain":".videos.test","path":"/"}]
            }"#,
        )
        .unwrap();
        assert_eq!(c.request_id, 7);
        assert_eq!(c.action, "videoLookup");
        assert_eq!(c.cookies[0].domain, ".videos.test");
    }
}
