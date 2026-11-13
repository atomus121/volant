//! `volant-labserver --faults "rate=2M,cut=2M" --addr 127.0.0.1:8080`
//!
//! 不带参数时起一个行为正确的静态文件服务器（随机端口，地址打到 stdout）。

use std::process::ExitCode;

use volant_labserver::{Faults, Server};

#[tokio::main]
async fn main() -> ExitCode {
    let mut faults = Faults::default();
    let mut addr = "127.0.0.1:0".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--faults" => {
                let Some(v) = args.next() else {
                    eprintln!("--faults 需要一个值");
                    return ExitCode::FAILURE;
                };
                match Faults::parse(&v) {
                    Ok(f) => faults = f,
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--addr" => {
                let Some(v) = args.next() else {
                    eprintln!("--addr 需要一个值");
                    return ExitCode::FAILURE;
                };
                addr = v;
            }
            other => {
                eprintln!("未知参数 `{other}`");
                eprintln!(
                    "用法: volant-labserver [--addr 127.0.0.1:0] [--faults \"grate=12.5M,delay=20,stall_every=1460K,stall=200,rate=2M,cut=2M,conns=2,norange,nolength,mutate=1M\"]"
                );
                return ExitCode::FAILURE;
            }
        }
    }

    let server = match Server::start_on(&addr, faults).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("绑定 {addr} 失败: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("listening http://{}", server.addr);
    println!("faults: {}", server.faults.describe());
    println!("file:   {}", server.url(10 << 20));
    // 一直服务到进程被杀。
    std::future::pending::<()>().await;
    ExitCode::SUCCESS
}
