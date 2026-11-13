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

use std::io::{self, Read, Write};

use crate::notify::Notifier;

/// 管道名。单用户桌面应用，固定名字就够；多用户同时开同一台机器才会撞。
pub const PIPE: &str = r"\\.\pipe\volant-capture";

/// 单帧上限。Chrome 对 host→浏览器 的回复限 1 MB，我们对浏览器→host 也定成 1 MB。
pub const MAX_FRAME: usize = 1 << 20;

/// 浏览器发来的一条捕获。
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default)]
pub struct Capture {
    pub url: String,
    pub referer: String,
    pub cookie: String,
    pub user_agent: String,
    /// 浏览器里的 `Date.now()`，用来量「点下载 → 引擎开跑」的真实耗时。
    pub t: u64,
}

impl Capture {
    /// 给引擎的额外请求头。空的直接不要，避免发一个空的 `Cookie:`。
    pub fn headers(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (name, value) in [
            ("Cookie", &self.cookie),
            ("Referer", &self.referer),
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

    use super::{Capture, PIPE, read_frame};

    /// `PIPE_ACCESS_INBOUND`：windows-sys 0.61 没导出这个常量，它就是 `FILE_READ_DATA`。
    const PIPE_ACCESS_INBOUND: u32 = 0x0000_0001;

    /// 管道上的 JSON → `Capture`。坏消息只丢那一条，不让整条管道停摆。
    fn parse(payload: &[u8]) -> Option<Capture> {
        let c: Capture = serde_json::from_slice(payload).ok()?;
        let url = c.url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return None;
        }
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
                                PIPE_ACCESS_INBOUND,
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
                        while let Ok(frame) = read_frame(&mut conn) {
                            let Some(c) = parse(&frame) else { continue };
                            if tx.send(c).is_ok() {
                                notify.notify();
                            }
                        }
                        // 对端关掉了：File 析构时 CloseHandle，下一轮再建一个实例。
                        drop(conn);
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
}
