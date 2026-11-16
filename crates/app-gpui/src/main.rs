#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod platform;
mod theme;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // The installer asks the GUI binary to register native messaging. Handle
    // this before GPUI starts so the command never flashes a window or needs GL.
    if args_request_install(&args) {
        let lines = match volant_app_core::native::install() {
            Ok(lines) => lines,
            Err(error) => {
                eprintln!("安装失败：{error}");
                std::process::exit(2);
            }
        };
        for line in lines {
            println!("{line}");
        }
        return;
    }

    if args.iter().any(|arg| arg == "--prepare-video") {
        match volant_app_core::video::install_dependencies_blocking() {
            Ok(status) => {
                println!(
                    "视频组件已就绪：yt-dlp={} ffmpeg={}",
                    status.ytdlp.ready, status.ffmpeg.ready
                );
            }
            Err(error) => {
                eprintln!("视频组件安装失败：{error}");
                std::process::exit(2);
            }
        }
        return;
    }

    if !single_instance() {
        crate::platform::signal_existing_instance();
        return;
    }

    let background = args.iter().any(|arg| arg == "--background");
    app::run(background);
}

#[cfg(windows)]
fn single_instance() -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = std::ffi::OsStr::new(r"Local\Volant.SingleInstance")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return true;
    }
    (unsafe { GetLastError() }) != ERROR_ALREADY_EXISTS
}

#[cfg(not(windows))]
fn single_instance() -> bool {
    true
}

fn args_request_install(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "--install-host")
}

#[cfg(test)]
mod tests {
    #[test]
    fn recognizes_install_host_argument() {
        assert!(super::args_request_install(&[
            "--demo".to_string(),
            "--install-host".to_string(),
        ]));
        assert!(!super::args_request_install(&["--demo".to_string()]));
    }
}
