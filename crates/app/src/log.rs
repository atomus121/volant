//! 自写滚动日志：`<home>/volant.log`，超过 1 MiB 就把旧文件挪成 `volant.log.1`。
//!
//! 只记「发生过什么」：命令、事件、失败原因、启动时间。崩溃转储不做（PRD 第 31 项）。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

const MAX_BYTES: u64 = 1 << 20;

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

struct State {
    path: PathBuf,
    started: Instant,
    failed: bool,
}

/// 打开日志文件；同一个进程只开一次，之后的调用直接返回。
pub fn init(home: &Path) {
    let path = home.join("volant.log");
    STATE.get_or_init(|| {
        rotate(&path);
        let ok = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .is_ok();
        Mutex::new(State {
            path,
            started: Instant::now(),
            failed: !ok,
        })
    });
    line(&format!("volant {} 启动", env!("CARGO_PKG_VERSION")));
}

/// 写一行；写不进去就静默放弃（日志不该拖垮下载）。
pub fn line(msg: &str) {
    let Some(state) = STATE.get() else { return };
    let Ok(mut s) = state.lock() else { return };
    if s.failed {
        return;
    }
    let secs = s.started.elapsed().as_secs_f64();
    let mut file = match OpenOptions::new().create(true).append(true).open(&s.path) {
        Ok(f) => f,
        Err(_) => {
            s.failed = true;
            return;
        }
    };
    let _ = writeln!(file, "[{secs:>9.3}s] {msg}");
    if file
        .metadata()
        .map(|m| m.len() > MAX_BYTES)
        .unwrap_or(false)
    {
        drop(file);
        rotate(&s.path);
    }
}

fn rotate(path: &Path) -> bool {
    let big = fs::metadata(path)
        .map(|m| m.len() > MAX_BYTES)
        .unwrap_or(false);
    if !big {
        return false;
    }
    let old = path.with_extension("log.1");
    let _ = fs::remove_file(&old);
    let _ = fs::rename(path, old);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_when_too_big() {
        let dir = std::env::temp_dir().join(format!("volant-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("volant.log");
        fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        assert!(rotate(&path));
        assert!(!path.exists());
        assert!(dir.join("volant.log.1").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
