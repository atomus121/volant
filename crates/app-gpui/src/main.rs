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

    app::run();
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
