//! 端到端：真 tokio runtime + 真 HTTP + 真文件，全部打到 `volant-labserver`。
//!
//! 这里只断言正确性（字节对、状态文件收拾干净、续传没从头下），速度留给人跑的
//! `examples/bench`——测试里放时间断言会在别人机器上随机红。

use std::path::Path;

use volant_engine::{
    Client, Config, Download, Error, Event, Summary, Url, client, probe, probe_with_timeout,
};
use volant_labserver::{Faults, Server, fill_pattern};

async fn lab(faults: Faults) -> (Server, tempfile::TempDir) {
    let server = Server::start(faults).await.expect("起 labserver");
    (server, tempfile::tempdir().expect("临时目录"))
}

fn faults(spec: &str) -> Faults {
    Faults::parse(spec).expect("故障串")
}

async fn fetch(client: &Client, url: &Url, dir: &Path) -> Result<Summary, Error> {
    let done = probe(client, url).await?;
    Download::start(client.clone(), Config::new(url.clone(), dir), done)
        .wait()
        .await
}

/// 下载回来的字节必须和服务器生成的字节一模一样。
fn assert_bytes(path: &Path, size: u64) {
    let got = std::fs::read(path).expect("读回下载的文件");
    assert_eq!(got.len() as u64, size, "长度不对");
    let mut want = vec![0u8; size as usize];
    fill_pattern(&mut want, 0);
    if got != want {
        let at = got.iter().zip(&want).position(|(a, b)| a != b);
        panic!("内容不符，第一个不同的字节在 {at:?}");
    }
}

fn part_and_state(dir: &Path, name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    (
        dir.join(format!("{name}.volant.part")),
        dir.join(format!("{name}.volant.state")),
    )
}

#[tokio::test]
async fn a_paged_download_is_byte_exact_and_leaves_no_debris() {
    let (server, dir) = lab(Faults::default()).await;
    let size = (3 << 20) + 12_345; // 故意不是 4 KiB 的整数倍
    let url: Url = server.url(size).parse().unwrap();
    let summary = fetch(&client().unwrap(), &url, dir.path()).await.unwrap();

    assert_eq!(summary.size, size);
    assert_eq!(summary.resumed_from, 0);
    let name = summary
        .dest
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        name,
        format!("blob-{size}.bin"),
        "文件名该听 Content-Disposition"
    );
    assert_bytes(&summary.dest, size);

    let (part, state) = part_and_state(dir.path(), &name);
    assert!(!part.exists(), "收尾该删掉 {}", part.display());
    assert!(!state.exists(), "收尾该删掉 {}", state.display());
}

#[tokio::test]
async fn a_server_that_ignores_range_still_downloads() {
    let (server, dir) = lab(faults("norange")).await;
    let size = (2 << 20) + 7;
    let url: Url = server.url(size).parse().unwrap();
    let summary = fetch(&client().unwrap(), &url, dir.path()).await.unwrap();
    assert_eq!(summary.size, size);
    assert_bytes(&summary.dest, size);
}

#[tokio::test]
async fn an_unknown_length_streams_in_order() {
    let (server, dir) = lab(faults("norange,nolength")).await;
    let size = 2 << 20;
    let url: Url = server.url(size).parse().unwrap();
    let summary = fetch(&client().unwrap(), &url, dir.path()).await.unwrap();
    assert_eq!(summary.size, size);
    assert_bytes(&summary.dest, size);
}

#[tokio::test]
async fn connections_dropped_mid_response_are_resumed_not_restarted() {
    let (server, dir) = lab(faults("cut=1M")).await;
    let size = 16 << 20;
    let url: Url = server.url(size).parse().unwrap();
    let client = client().unwrap();
    let first = probe(&client, &url).await.unwrap();
    let mut cfg = Config::new(url, dir.path());
    cfg.connections = 4;
    let summary = Download::start(client, cfg, first).wait().await.unwrap();

    assert_bytes(&summary.dest, size);
    // 一条连接每次只能发 1 MiB，16 MiB 至少要十六次请求，所以服务器发出去的总量
    // 必然多于一个文件——多出来的那部分正是被切断后从断点续的。
    assert!(
        server.served() > size,
        "断连后该续传，服务器只发了 {} 字节",
        server.served()
    );
}

#[tokio::test]
async fn too_many_connections_are_retried() {
    let (server, dir) = lab(faults("conns=2")).await;
    let size = 2 << 20;
    let url: Url = server.url(size).parse().unwrap();
    let client = client().unwrap();
    let first = probe(&client, &url).await.unwrap();
    let mut cfg = Config::new(url, dir.path());
    cfg.connections = 4; // 比服务器允许的多，多出来的会吃 503
    let summary = Download::start(client, cfg, first).wait().await.unwrap();
    assert_bytes(&summary.dest, size);
}

#[tokio::test]
async fn cancel_keeps_progress_and_resume_finishes_it() {
    let (server, dir) = lab(faults("rate=4M")).await;
    let size = 16 << 20;
    let url: Url = server.url(size).parse().unwrap();
    let client = client().unwrap();
    let name = format!("blob-{size}.bin");
    let (_, state) = part_and_state(dir.path(), &name);

    let first = probe(&client, &url).await.unwrap();
    let mut cfg = Config::new(url.clone(), dir.path());
    cfg.connections = 2;
    let mut download = Download::start(client.clone(), cfg, first);
    let mut cancelled = false;
    while let Some(event) = download.events.recv().await {
        if let Event::Progress { downloaded, .. } = event
            && downloaded >= (1 << 20)
            && !cancelled
        {
            download.handle.cancel();
            cancelled = true;
        }
    }
    let err = download.wait().await.unwrap_err();
    assert!(
        matches!(err, Error::Cancelled),
        "取消该返回 Cancelled，实际 {err}"
    );

    assert!(state.exists(), "取消后必须留下状态文件才能续传");
    let again = probe(&client, &url).await.unwrap();
    let summary = Download::start(client, Config::new(url, dir.path()), again)
        .wait()
        .await
        .unwrap();
    assert!(
        summary.resumed_from >= 1 << 20,
        "续传从 {} 开始，已经下好的 1 MiB 被重下了",
        summary.resumed_from
    );
    assert_bytes(&summary.dest, size);
    assert!(!state.exists());
}

#[tokio::test]
async fn a_corrupt_state_file_means_start_over_not_a_crash() {
    let (server, dir) = lab(faults("rate=8M")).await;
    let size = 8 << 20;
    let url: Url = server.url(size).parse().unwrap();
    let client = client().unwrap();
    let name = format!("blob-{size}.bin");
    let (part, state) = part_and_state(dir.path(), &name);

    let first = probe(&client, &url).await.unwrap();
    let mut cfg = Config::new(url.clone(), dir.path());
    cfg.connections = 2;
    let mut download = Download::start(client.clone(), cfg, first);
    let mut cancelled = false;
    while let Some(event) = download.events.recv().await {
        if let Event::Progress { downloaded, .. } = event
            && downloaded >= (1 << 20)
            && !cancelled
        {
            download.handle.cancel();
            cancelled = true;
        }
    }
    let _ = download.wait().await;

    assert!(part.exists() && state.exists());
    std::fs::write(&state, b"{ this is not json").unwrap();

    let again = probe(&client, &url).await.unwrap();
    let summary = Download::start(client, Config::new(url, dir.path()), again)
        .wait()
        .await
        .unwrap();
    assert_eq!(summary.resumed_from, 0, "状态坏了就该从头下");
    assert_bytes(&summary.dest, size);
}

#[tokio::test]
async fn a_source_that_changes_midway_is_refused() {
    let (server, dir) = lab(faults("cut=512K,mutate=1M")).await;
    let size = 8 << 20;
    let url: Url = server.url(size).parse().unwrap();
    let client = client().unwrap();
    let first = probe(&client, &url).await.unwrap();
    let mut cfg = Config::new(url, dir.path());
    cfg.connections = 4;
    let err = Download::start(client, cfg, first)
        .wait()
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::SourceChanged),
        "文件变了该报 SourceChanged，实际 {err}"
    );
}

#[tokio::test]
async fn an_existing_file_is_never_overwritten() {
    let (server, dir) = lab(Faults::default()).await;
    let size = 1 << 20;
    let url: Url = server.url(size).parse().unwrap();
    let client = client().unwrap();
    let name = format!("blob-{size}.bin");
    // 先占住这个名字，内容故意不同。
    std::fs::write(dir.path().join(&name), b"do not touch").unwrap();

    let summary = fetch(&client, &url, dir.path()).await.unwrap();
    assert_eq!(
        summary.dest.file_name().unwrap().to_string_lossy(),
        format!("blob-{size} (1).bin")
    );
    assert_bytes(&summary.dest, size);
    assert_eq!(
        std::fs::read(dir.path().join(&name)).unwrap(),
        b"do not touch"
    );
}

/// 同名任务即使同时开跑，也必须各自拿到不同的目标名。
///
/// 最终文件名不存在不代表这个名字空闲：正在下载的任务只留下 `.volant.part` /
/// `.volant.state`。这里先让第一个任务真正开工，再启动第二个同名任务。
#[tokio::test]
async fn simultaneous_downloads_with_the_same_name_get_distinct_destinations() {
    let (server, dir) = lab(faults("rate=4M")).await;
    let size = 8 << 20;
    let url: Url = server.url(size).parse().unwrap();
    let http = client().unwrap();

    let first = probe(&http, &url).await.unwrap();
    let mut a = Download::start(http.clone(), Config::new(url.clone(), dir.path()), first);
    let Event::Started { .. } = a.events.recv().await.expect("第一个任务该开工") else {
        panic!("第一个任务没收到 Started");
    };

    let second = probe(&http, &url).await.unwrap();
    let b = Download::start(http, Config::new(url, dir.path()), second);
    let (a, b) = tokio::join!(a.wait(), b.wait());
    let a = a.unwrap_or_else(|e| panic!("第一个任务失败：{e:?}"));
    let b = b.unwrap_or_else(|e| panic!("第二个任务失败：{e:?}"));

    assert_ne!(
        a.dest,
        b.dest,
        "两个同名任务选到了同一个目标：{}",
        a.dest.display()
    );
    assert_bytes(&a.dest, size);
    assert_bytes(&b.dest, size);
    assert!(
        std::fs::read_dir(dir.path()).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".volant.lock")),
        "下载完成后不该留下锁文件"
    );
}

#[tokio::test]
async fn progress_reaches_the_total_and_done_carries_the_path() {
    let (server, dir) = lab(Faults::default()).await;
    let size = (1 << 20) + 3;
    let url: Url = server.url(size).parse().unwrap();
    let client = client().unwrap();
    let first = probe(&client, &url).await.unwrap();
    let mut download = Download::start(client, Config::new(url, dir.path()), first);

    let mut last = 0;
    let mut started = None;
    let mut done = None;
    while let Some(event) = download.events.recv().await {
        match event {
            Event::Started {
                size, resumed_from, ..
            } => started = Some((size, resumed_from)),
            Event::Progress {
                downloaded, total, ..
            } => {
                assert!(downloaded >= last, "进度倒退了：{last} → {downloaded}");
                assert_eq!(total, size);
                last = downloaded;
            }
            Event::Done { path, .. } => done = Some(path),
            Event::Failed { reason } => panic!("不该失败：{reason}"),
        }
    }
    let summary = download.wait().await.unwrap();
    assert_eq!(started, Some((size, 0)));
    assert_eq!(done.as_deref(), Some(summary.dest.as_path()));
    assert_eq!(last, size, "最后一次进度该等于总长");
}

#[tokio::test]
async fn a_not_found_url_is_an_error_not_a_panic() {
    let (server, dir) = lab(Faults::default()).await;
    let url: Url = format!("http://{}/missing", server.addr).parse().unwrap();
    let err = fetch(&client().unwrap(), &url, dir.path())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Status(_) | Error::Http(_)), "{err}");
    // 失败不该在目录里留下空文件。
    let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(leftovers, 0, "留下了 {leftovers} 个文件");
}

#[tokio::test]
async fn probe_times_out_when_the_server_never_sends_headers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    });
    let url: Url = format!("http://{addr}/stalled").parse().unwrap();
    let err = probe_with_timeout(
        &client().unwrap(),
        &url,
        &[],
        std::time::Duration::from_millis(100),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("超时"), "{err}");
}

// ------------------------------------------------------------ M2：限速与解耦

/// 引擎侧令牌桶：4 MiB 走 2 MB/s 的管道，耗时应该接近 2 秒而不是 20 毫秒。
#[tokio::test]
async fn the_engine_rate_limit_caps_the_whole_download() {
    let (server, dir) = lab(Faults::default()).await;
    let size = 4 << 20;
    let url: Url = server.url(size).parse().unwrap();
    let http = client().unwrap();
    let first = probe(&http, &url).await.unwrap();
    let mut cfg = Config::new(url, dir.path());
    cfg.connections = 4;
    cfg.rate_limit = Some(2_000_000);
    let started = std::time::Instant::now();
    let summary = Download::start(http, cfg, first).wait().await.unwrap();
    let took = started.elapsed().as_secs_f64();

    assert_bytes(&summary.dest, size);
    assert!(took >= 1.7, "限速 2 MB/s 下 4 MiB 只用了 {took:.2} s");
    assert!(took <= 3.5, "限速 2 MB/s 下 4 MiB 用了 {took:.2} s");
}

/// T6 的核心：慢盘只该让速度变慢，不该让引擎以为「连接太慢」而去重连。
///
/// 判据是**对比**而不是绝对值：同一份文件先全速下、再限到 8 MB/s 下，慢盘那次的
/// 请求数不能比全速那次多。让位/重连都会多出请求，所以「没有更多请求」就是
/// 「慢盘没被当成慢连接」的硬证据。（少量字节被重发是 planner 分段的固有代价，
/// 两次都有，和慢盘无关——见 docs/m2/REPORT.md。）
#[tokio::test]
async fn a_slow_disk_costs_time_but_never_a_reconnect() {
    let (server, dir) = lab(Faults::default()).await;
    let size = 16 << 20;
    let url: Url = server.url(size).parse().unwrap();
    let http = client().unwrap();

    // 先全速跑一次，拿到「正常情况下的请求数」。
    let first = probe(&http, &url).await.unwrap();
    let fast_requests = server.requests();
    let mut cfg = Config::new(url.clone(), dir.path());
    cfg.connections = 8;
    cfg.dest = Some(dir.path().join("fast.bin"));
    Download::start(http.clone(), cfg, first)
        .wait()
        .await
        .unwrap();
    let fast_requests = server.requests() - fast_requests;

    // 再限到 8 MB/s：耗时该接近 16 MiB ÷ 8 MB/s = 2 秒。
    let first = probe(&http, &url).await.unwrap();
    let slow_requests = server.requests();
    let mut cfg = Config::new(url, dir.path());
    cfg.connections = 8;
    cfg.dest = Some(dir.path().join("slow.bin"));
    cfg.disk_rate = Some(8_000_000);
    let started = std::time::Instant::now();
    let summary = Download::start(http, cfg, first).wait().await.unwrap();
    let took = started.elapsed().as_secs_f64();

    assert_bytes(&summary.dest, size);
    assert!(took >= 1.85, "8 MB/s 下 16 MiB 只用了 {took:.2} s");
    assert!(took <= 2.6, "8 MB/s 下 16 MiB 用了 {took:.2} s");
    let slow_requests = server.requests() - slow_requests;
    assert!(
        slow_requests <= fast_requests,
        "慢盘那次用了 {slow_requests} 次请求，全速那次只有 {fast_requests} 次：多出来的就是重连"
    );
}

/// 卡住的连接要把段交回去，而且交回去之后一个字节都不能丢。
#[tokio::test]
async fn a_stalled_connection_gives_its_segment_back_intact() {
    let (server, dir) = lab(faults("stall_every=256K,stall=1500")).await;
    let size = 8 << 20;
    let url: Url = server.url(size).parse().unwrap();
    let http = client().unwrap();
    let first = probe(&http, &url).await.unwrap();
    let mut cfg = Config::new(url, dir.path());
    cfg.connections = 4;
    // 比卡顿本身短得多：卡 1.5 s，0.4 s 就判「这条不行」。
    cfg.stall_timeout = std::time::Duration::from_millis(400);
    let summary = Download::start(http, cfg, first).wait().await.unwrap();
    assert_bytes(&summary.dest, size);
}

/// 主机连接上限要跨任务记住：第一次撞了 503，第二次就不该再用那么多连接。
#[tokio::test]
async fn a_refusal_is_remembered_across_two_downloads() {
    let (server, dir) = lab(faults("conns=2")).await;
    let size = 2 << 20;
    let url: Url = server.url(size).parse().unwrap();
    let http = client().unwrap();

    let first = probe(&http, &url).await.unwrap();
    let mut cfg = Config::new(url.clone(), dir.path());
    cfg.connections = 4; // 比服务器允许的多，必然吃 503
    cfg.dest = Some(dir.path().join("a.bin"));
    Download::start(http.clone(), cfg, first)
        .wait()
        .await
        .unwrap();

    // 第二次：上限已经被学到（≤ 4），请求数应该接近「每条连接一次」而不是「8 条探一遍」。
    let before = server.requests();
    let again = probe(&http, &url).await.unwrap();
    let mut cfg = Config::new(url, dir.path());
    cfg.connections = 8;
    cfg.dest = Some(dir.path().join("b.bin"));
    Download::start(http, cfg, again).wait().await.unwrap();

    let requests = server.requests() - before;
    assert!(
        requests <= 6,
        "第二次下了 {requests} 次请求，说明上限没记住（2 条连接下完 2 MiB 只需要 2-4 次）"
    );
}
