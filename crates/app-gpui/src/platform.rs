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
            1180.0_f32.min((width - 32.0).max(520.0)),
            760.0_f32.min((height - 64.0).max(340.0)),
        )
    }
    #[cfg(not(windows))]
    {
        (1180.0, 760.0)
    }
}
