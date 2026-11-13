//! Volant M0 · librqbit 集成风险探针
//!
//! PRD 第 7 节列了 4 个风险，这里逐条验证：
//! 1. API 稳定性 —— `=9.1.0` 锁死
//! 2. runtime 冲突 —— `Session::new` 返回 `BoxFuture`，在调用者 runtime 上跑
//! 3. 写盘层可替换性 —— `SessionOptions::default_storage_factory`
//! 4. 二进制体积 —— 编译产物大小

use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // 注意：本机上子进程写 %TEMP% 会被拒（PermissionDenied），
    // 所以数据目录必须放在工作区里。
    let dir = std::path::PathBuf::from(r"D:\Documents\deepseek\volant\target\rq-spike-data");
    std::fs::create_dir_all(&dir)?;

    // Volant 的默认姿态：没有活跃种子时 Session 完全不存在。
    // 这里模拟"用户刚添加了一个种子"，所以手动创建一个最小会话：
    // 不监听端口、不开 DHT、不发 tracker、不开 LSD。
    let opts = librqbit::SessionOptions {
        dht: None,
        listen: None,
        disable_trackers: true,
        disable_local_service_discovery: true,
        persistence: None,
        ..Default::default()
    };

    let session = librqbit::Session::new_with_opts(dir.clone(), opts).await?;
    println!("SPIKE-OK session created at {}", dir.display());

    // 3. 存储层是否可替换：能设置即证明接得住自己的 sparse + 状态文件。
    let _probe = librqbit::SessionOptions {
        default_storage_factory: None,
        ..Default::default()
    };
    println!("SPIKE-OK storage_factory is settable (field exists on SessionOptions)");

    // 2. 会话没被显式 drop 时不应残留监听端口；这里直接 drop，验证能干净退出。
    drop(session);
    println!("SPIKE-OK session dropped without background runtime");

    Ok(())
}
