//! `volant-cli get` 的端到端检查：真的起一个进程、真的下完整、字节逐一对上。
//!
//! M3 的出口是「不靠 UI 就把文件下完」，所以这里不用 engine 的 API，只敲命令行。

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use volant_labserver::{Faults, Server, fill_pattern};

const BIN: &str = env!("CARGO_BIN_EXE_volant-cli");

/// 跑一次 CLI，等它退出。用 spawn_blocking 是为了别把 tokio 的唯一 worker 占住——
/// 占住了 labserver 就没线程服务，测试会自己卡死。
async fn run_cli(args: Vec<String>) -> std::process::Output {
    tokio::task::spawn_blocking(move || {
        Command::new(BIN)
            .args(args)
            .output()
            .expect("启动 volant-cli 失败")
    })
    .await
    .expect("CLI 任务崩了")
}

fn pattern(len: u64) -> Vec<u8> {
    let mut v = vec![0u8; len as usize];
    fill_pattern(&mut v, 0);
    v
}

#[tokio::test]
async fn get_downloads_the_whole_file() {
    let srv = Server::start(Faults::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let size = 3 * 1024 * 1024 + 4096;

    let out = run_cli(vec![
        "get".into(),
        srv.url(size),
        "-d".into(),
        dir.path().display().to_string(),
        "-q".into(),
    ])
    .await;

    assert!(
        out.status.success(),
        "退出码 {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let path = dir.path().join(format!("blob-{size}.bin"));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        pattern(size),
        "落盘的字节必须和服务器的一模一样"
    );
    // 下完就该把 .part / .state 收干净，不留垃圾。
    assert!(
        !dir.path()
            .join(format!("blob-{size}.bin.volant.part"))
            .exists()
    );
    assert!(
        !dir.path()
            .join(format!("blob-{size}.bin.volant.state"))
            .exists()
    );
    // 拿掉进度文件之后没有残留请求重试（8 段 + 1 次探测）。
    assert!(srv.requests() <= 10, "请求数 {}", srv.requests());
}

#[tokio::test]
async fn killed_download_resumes_and_ends_byte_identical() {
    let srv = Server::start(Faults::default()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let size = 4 * 1024 * 1024;
    let url = srv.url(size);

    // 限到 1 MB/s，1.2 秒后掐死——这时写队列里一定还有积压。
    let mut child = Command::new(BIN)
        .args(["get", &url, "-d", &dir.path().display().to_string(), "-q"])
        .args(["--rate", "1M"])
        .spawn()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    child.kill().unwrap();
    tokio::task::spawn_blocking(move || child.wait())
        .await
        .unwrap()
        .unwrap();

    let state = dir.path().join(format!("blob-{size}.bin.volant.state"));
    let part = dir.path().join(format!("blob-{size}.bin.volant.part"));
    assert!(part.exists(), "杀进程后 .part 必须还在");
    assert!(state.exists(), "杀进程后 .state 必须还在（否则等于重下）");
    assert!(
        !dir.path().join(format!("blob-{size}.bin")).exists(),
        "没下完不该有正式文件"
    );

    // 同一个 URL 再跑一次：不该重新开头。
    let out = run_cli(vec![
        "get".into(),
        url.clone(),
        "-d".into(),
        dir.path().display().to_string(),
        "-q".into(),
    ])
    .await;
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "退出码 {:?}\nstderr: {stderr}",
        out.status
    );
    assert!(
        stderr.contains("续传"),
        "第二次应该走续传，实际 stderr：{stderr}"
    );
    assert_eq!(
        std::fs::read(dir.path().join(format!("blob-{size}.bin"))).unwrap(),
        pattern(size),
        "续传后的字节必须逐一对上"
    );
    assert!(!Path::new(&state).exists(), "下完要删掉进度文件");
}
