//! 剪贴板监视（P1）：复制一个链接就打开新建下载预览。
//!
//! 轮询 `GetClipboardSequenceNumber`，只有序列号变了才去读文本——比定时读
//! 剪贴板省事，也不会和别的程序抢剪贴板锁。不引入 arboard，直接用 windows-sys。

#[cfg(windows)]
mod imp {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::Receiver;
    use std::thread::JoinHandle;
    use std::time::Duration;

    use eframe::egui;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};

    const CF_UNICODETEXT: u32 = 13;
    const POLL: Duration = Duration::from_millis(400);
    const MAX_CHARS: usize = 1 << 20;

    pub struct Watch {
        stop: Arc<AtomicBool>,
        on: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Watch {
        /// `enabled` 随时可以通过 `set_enabled` 改，不用重启线程。
        pub fn start(ctx: egui::Context, enabled: bool) -> (Watch, Receiver<String>) {
            let (tx, rx) = std::sync::mpsc::channel();
            let (stop, on) = (
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(enabled)),
            );
            let (t_stop, t_on) = (stop.clone(), on.clone());
            let thread = std::thread::Builder::new()
                .name("volant-clip".into())
                .spawn(move || {
                    let mut last = unsafe { GetClipboardSequenceNumber() };
                    while !t_stop.load(Ordering::Relaxed) {
                        std::thread::sleep(POLL);
                        if t_stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let seq = unsafe { GetClipboardSequenceNumber() };
                        if seq == last {
                            continue;
                        }
                        last = seq;
                        if !t_on.load(Ordering::Relaxed) {
                            continue;
                        }
                        let text = match unsafe { read_text() } {
                            Some(t) => t,
                            None => continue,
                        };
                        let Some(url) = pick_url(&text) else { continue };
                        if tx.send(url).is_ok() {
                            ctx.request_repaint();
                        }
                    }
                })
                .expect("spawn clipboard thread");
            (
                Watch {
                    stop,
                    on,
                    thread: Some(thread),
                },
                rx,
            )
        }

        pub fn set_enabled(&self, enabled: bool) {
            self.on.store(enabled, Ordering::Relaxed);
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    pub(super) unsafe fn read_text() -> Option<String> {
        if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
            return None; // 别的程序正占着，下一轮再试
        }
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT) };
        if handle.is_null() {
            unsafe { CloseClipboard() };
            return None;
        }
        let ptr = unsafe { GlobalLock(handle) };
        if ptr.is_null() {
            unsafe { CloseClipboard() };
            return None;
        }
        let utf16 = ptr as *const u16;
        let mut len = 0usize;
        // 剪贴板内容是别的进程写的，长度只能自己扫，加个上限防止卡死。
        while len < MAX_CHARS && unsafe { *utf16.add(len) } != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(utf16, len) });
        unsafe {
            GlobalUnlock(handle);
            CloseClipboard();
        }
        Some(text)
    }

    /// 文本里唯一一个 http(s) 链接才算数：复制一段带链接的正文不该自动加任务。
    fn pick_url(text: &str) -> Option<String> {
        if text.len() > 8192 {
            return None;
        }
        let mut found: Option<&str> = None;
        for raw in text.split_whitespace() {
            let t = raw.trim_matches(|c: char| {
                matches!(
                    c,
                    '"' | '\''
                        | '<'
                        | '>'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '，'
                        | '。'
                        | '、'
                        | ','
                        | '.'
                        | ';'
                        | '；'
                        | '!'
                        | '！'
                        | '?'
                        | '？'
                )
            });
            if !(t.starts_with("http://")
                || t.starts_with("https://")
                || t.get(..7)
                    .is_some_and(|p| p.eq_ignore_ascii_case("magnet:")))
            {
                continue;
            }
            if found.is_some() {
                return None;
            }
            found = Some(t);
        }
        found.filter(|u| u.len() > 8).map(|u| u.to_string())
    }

    #[cfg(test)]
    mod tests {
        use super::pick_url;

        #[test]
        fn only_single_links_are_taken() {
            assert_eq!(pick_url("https://a.b/c").as_deref(), Some("https://a.b/c"));
            assert_eq!(
                pick_url("magnet:?xt=urn:btih:abc").as_deref(),
                Some("magnet:?xt=urn:btih:abc")
            );
            assert_eq!(pick_url("看这个：https://a.b/c。"), None); // 中文句号粘在链接上，不是独立 token
            assert_eq!(pick_url("https://a.b/c, 顺便 https://d.e/f"), None);
            assert_eq!(pick_url("不是链接"), None);
        }
    }
}

#[cfg(windows)]
pub use imp::Watch;

#[cfg(not(windows))]
mod stub {
    use std::sync::mpsc::Receiver;

    pub struct Watch;

    impl Watch {
        pub fn start(_ctx: egui::Context, _enabled: bool) -> (Watch, Receiver<String>) {
            let (_, rx) = std::sync::mpsc::channel();
            (Watch, rx)
        }

        pub fn set_enabled(&self, _enabled: bool) {}
    }
}

#[cfg(not(windows))]
pub use stub::Watch;

/// 按需同步读一次剪贴板文本。TextEdit 使用的通用剪贴板后端在受限进程里可能失败，
/// 顶部添加栏用这条已经验证可用的 Win32 路径作为兜底。
pub fn read_now() -> Option<String> {
    #[cfg(windows)]
    {
        unsafe { imp::read_text() }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// 添加栏打开时直接查询 Windows 的原生按键状态。
///
/// egui-winit 会先尝试从它自己的剪贴板后端取文本；后端失败时可能不留下
/// `Key::V` 或 `Event::Paste`。Win32 的异步按键状态仍能告诉我们用户刚按了 Ctrl+V，
/// 随后由项目自己的 Win32 剪贴板读取兜底。
pub fn ctrl_v_down() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_V};
        let down = |key: u16| unsafe { GetAsyncKeyState(key as i32) as u16 & 0x8000 != 0 };
        down(VK_CONTROL) && down(VK_V)
    }
    #[cfg(not(windows))]
    {
        false
    }
}
