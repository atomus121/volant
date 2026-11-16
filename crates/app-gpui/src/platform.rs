use gpui_kit::Window;

/// Logical window size that fits the current display without exceeding it.
pub fn startup_size() -> (f32, f32) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN,
        };

        let dpi = unsafe { GetDpiForSystem() }.max(96) as f32;
        let scale = dpi / 96.0;
        let width = unsafe { GetSystemMetrics(SM_CXSCREEN) } as f32 / scale;
        let height = unsafe { GetSystemMetrics(SM_CYSCREEN) } as f32 / scale;
        (
            1180.0_f32.min((width - 32.0).max(960.0)),
            760.0_f32.min((height - 64.0).max(560.0)),
        )
    }
    #[cfg(not(windows))]
    {
        (1180.0, 760.0)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum TrayEvent {
    Open,
    Quit,
}

#[cfg(windows)]
pub fn show_window(window: &mut Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_RESTORE, SW_SHOW, ShowWindow};

    if let Ok(handle) = HasWindowHandle::window_handle(window)
        && let RawWindowHandle::Win32(handle) = handle.as_raw()
    {
        let hwnd = handle.hwnd.get() as _;
        unsafe {
            ShowWindow(hwnd, SW_SHOW);
            ShowWindow(hwnd, SW_RESTORE);
        }
    }
    window.activate_window();
}

#[cfg(not(windows))]
pub fn show_window(window: &mut Window) {
    window.activate_window();
}

#[cfg(windows)]
pub fn hide_window(window: &mut Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};

    if let Ok(handle) = HasWindowHandle::window_handle(window)
        && let RawWindowHandle::Win32(handle) = handle.as_raw()
    {
        unsafe {
            ShowWindow(handle.hwnd.get() as _, SW_HIDE);
        }
    }
}

#[cfg(not(windows))]
pub fn hide_window(window: &mut Window) {
    window.minimize_window();
}

#[cfg(windows)]
mod windows {
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::{OnceLock, mpsc};
    use std::thread::JoinHandle;

    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
    use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegDeleteValueW, RegSetValueExW,
    };
    use windows_sys::Win32::UI::Shell::{
        NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
        DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW, IMAGE_ICON, LR_DEFAULTCOLOR,
        LR_DEFAULTSIZE, LR_SHARED, LoadImageW, MF_SEPARATOR, MF_STRING, MSG, PostMessageW,
        PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow,
        TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE,
        WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
    };

    use super::TrayEvent;

    const WINDOW_CLASS: &str = "VolantTrayWindow";
    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN_VALUE: &str = "Volant";
    const TRAY_MESSAGE: u32 = WM_APP + 1;
    const SHOW_MESSAGE: u32 = WM_APP + 2;

    static EVENT_SENDER: OnceLock<UnboundedSender<TrayEvent>> = OnceLock::new();
    static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();
    static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn icon() -> windows_sys::Win32::UI::WindowsAndMessaging::HICON {
        unsafe {
            let module = GetModuleHandleW(std::ptr::null());
            let resource: *const u16 = std::ptr::without_provenance(1);
            LoadImageW(
                module as HINSTANCE,
                resource,
                IMAGE_ICON,
                0,
                0,
                LR_DEFAULTSIZE | LR_SHARED | LR_DEFAULTCOLOR,
            ) as _
        }
    }

    fn notify_data(hwnd: HWND) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: TRAY_MESSAGE,
            hIcon: icon(),
            ..Default::default()
        };
        for (slot, ch) in data.szTip.iter_mut().zip("Volant · 下载器".encode_utf16()) {
            *slot = ch;
        }
        data
    }

    fn add_icon(hwnd: HWND) -> bool {
        let data = notify_data(hwnd);
        let added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) };
        if added == 0 {
            volant_app_core::log::line(&format!(
                "tray NIM_ADD failed hwnd={hwnd:p} icon={:p} size={}",
                data.hIcon, data.cbSize
            ));
        }
        added != 0
    }

    fn remove_icon(hwnd: HWND) {
        let data = notify_data(hwnd);
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
    }

    fn send(event: TrayEvent) {
        if let Some(sender) = EVENT_SENDER.get() {
            let _ = sender.send(event);
        }
    }

    fn menu(hwnd: HWND) {
        use windows_sys::Win32::Foundation::POINT;

        unsafe {
            let popup = CreatePopupMenu();
            if popup.is_null() {
                return;
            }
            let open = wide("打开 Volant");
            let exit = wide("退出 Volant");
            let empty = wide("");
            AppendMenuW(popup, MF_STRING, 1, open.as_ptr());
            AppendMenuW(popup, MF_SEPARATOR, 0, empty.as_ptr());
            AppendMenuW(popup, MF_STRING, 2, exit.as_ptr());
            let mut point = POINT::default();
            GetCursorPos(&mut point);
            SetForegroundWindow(hwnd);
            let command = TrackPopupMenu(
                popup,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                hwnd,
                std::ptr::null::<RECT>(),
            );
            DestroyMenu(popup);
            PostMessageW(hwnd, WM_NULL, 0, 0);
            match command {
                1 => send(TrayEvent::Open),
                2 => send(TrayEvent::Quit),
                _ => {}
            }
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == TRAY_MESSAGE {
            match lparam as u32 & 0xffff {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => send(TrayEvent::Open),
                WM_RBUTTONUP => menu(hwnd),
                _ => {}
            }
        } else if message == SHOW_MESSAGE {
            send(TrayEvent::Open);
        } else if TASKBAR_CREATED
            .get()
            .is_some_and(|registered| message == *registered)
        {
            let _ = add_icon(hwnd);
        } else if message == WM_CLOSE {
            remove_icon(hwnd);
            unsafe {
                DestroyWindow(hwnd);
            }
        } else if message == WM_DESTROY {
            TRAY_HWND.store(0, Ordering::Release);
            unsafe {
                PostQuitMessage(0);
            }
        } else {
            return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
        }
        0
    }

    pub struct TrayIcon {
        hwnd: HWND,
        thread: Option<JoinHandle<()>>,
    }

    impl TrayIcon {
        pub fn start() -> Option<(TrayIcon, UnboundedReceiver<TrayEvent>)> {
            let (sender, receiver) = unbounded_channel();
            EVENT_SENDER.get_or_init(|| sender);
            let (ready_tx, ready_rx) = mpsc::channel();
            let thread = std::thread::Builder::new()
                .name("volant-tray".into())
                .spawn(move || {
                    let class = wide(WINDOW_CLASS);
                    let title = wide("Volant");
                    unsafe {
                        let module = GetModuleHandleW(std::ptr::null());
                        let window_class = WNDCLASSW {
                            lpfnWndProc: Some(wnd_proc),
                            hInstance: module as HINSTANCE,
                            lpszClassName: class.as_ptr(),
                            ..Default::default()
                        };
                        RegisterClassW(&window_class);
                        let taskbar = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
                        if taskbar != 0 {
                            let _ = TASKBAR_CREATED.set(taskbar);
                        }
                        let hwnd = CreateWindowExW(
                            0,
                            class.as_ptr(),
                            title.as_ptr(),
                            0,
                            0,
                            0,
                            0,
                            0,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            module as HINSTANCE,
                            std::ptr::null(),
                        );
                        if hwnd.is_null() {
                            volant_app_core::log::line("tray CreateWindowExW failed");
                            let _ = ready_tx.send(false);
                            return;
                        }
                        TRAY_HWND.store(hwnd as isize, Ordering::Release);
                        if !add_icon(hwnd) {
                            let _ = ready_tx.send(false);
                            DestroyWindow(hwnd);
                            return;
                        }
                        let _ = ready_tx.send(true);

                        let mut msg = MSG::default();
                        loop {
                            let result = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                            if result <= 0 {
                                break;
                            }
                            TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                })
                .ok()?;
            if !ready_rx
                .recv_timeout(std::time::Duration::from_secs(3))
                .ok()?
            {
                return None;
            }
            let hwnd = TRAY_HWND.load(Ordering::Acquire) as HWND;
            Some((
                TrayIcon {
                    hwnd,
                    thread: Some(thread),
                },
                receiver,
            ))
        }
    }

    impl Drop for TrayIcon {
        fn drop(&mut self) {
            if !self.hwnd.is_null() {
                let _ = unsafe { PostMessageW(self.hwnd, WM_CLOSE, 0, 0) };
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    pub fn signal_existing_instance() -> bool {
        let class = wide(WINDOW_CLASS);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let hwnd = unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) };
            if !hwnd.is_null() {
                let _ = unsafe { PostMessageW(hwnd, SHOW_MESSAGE, 0, 0) };
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
    }

    pub fn set_autostart(enabled: bool) -> Result<(), String> {
        let command = if enabled {
            let exe = std::env::current_exe()
                .map_err(|error| format!("无法定位 Volant：{error}"))?
                .display()
                .to_string();
            Some(wide(&format!("\"{exe}\" --background")))
        } else {
            None
        };
        let subkey = wide(RUN_KEY);
        let value_name = wide(RUN_VALUE);
        let mut key = std::ptr::null_mut();
        let opened = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                std::ptr::null_mut(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        if opened != 0 {
            return Err(format!("无法打开 Windows 启动项（错误 {opened}）"));
        }
        let result = if let Some(command) = command {
            unsafe {
                RegSetValueExW(
                    key,
                    value_name.as_ptr(),
                    0,
                    REG_SZ,
                    command.as_ptr() as *const u8,
                    (command.len() * std::mem::size_of::<u16>()) as u32,
                )
            }
        } else {
            unsafe { RegDeleteValueW(key, value_name.as_ptr()) }
        };
        unsafe {
            RegCloseKey(key);
        }
        if result == 0 || (!enabled && result == 2) {
            Ok(())
        } else {
            Err(format!("无法更新 Windows 启动项（错误 {result}）"))
        }
    }
}

#[cfg(windows)]
pub use windows::{TrayIcon, set_autostart, signal_existing_instance};

#[cfg(not(windows))]
pub fn signal_existing_instance() -> bool {
    false
}

#[cfg(not(windows))]
pub fn set_autostart(_enabled: bool) -> Result<(), String> {
    Err("开机启动目前仅支持 Windows".into())
}

#[cfg(not(windows))]
pub struct TrayIcon;

#[cfg(not(windows))]
impl TrayIcon {
    pub fn start() -> Option<(TrayIcon, tokio::sync::mpsc::UnboundedReceiver<TrayEvent>)> {
        None
    }
}
