//! 「这台主机最多让我开几条连接」的记忆。
//!
//! 唯一可信的信号是服务器自己说「连接太多」（503/429）。M1 之前这个信号只被当成一次
//! 普通重试，学过就忘，下一次还是 8 条一起轰上去——T4/T6 那种「服务器只肯给 2 条」的
//! 场景里，这等于每轮都在浪费请求。
//!
//! 记忆的规则是 AIMD：
//! - 被拒绝：上限**减半**（乘性减小，快速退到服务器满意的水平）；
//! - 完整下完一次：上限 **+1**（加性增大，慢慢试探回去）。
//!
//! 键是 `host:port`——同一台机器的不同端口是不同的服务，混在一起会互相污染。
//!
//! 记忆是进程级的（[`cap`] / [`refused`] / [`succeeded`] 三个函数），所以跨任务有效；
//! 给了文件路径就落盘，跨进程也有效。落盘失败不报错：记不住只是慢一点，不该让下载失败。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct Entry {
    limit: usize,
}

/// 落盘的格式：一张 `host:port → 上限` 的表。多出来的字段（将来加的）会被忽略。
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct HostLimits {
    #[serde(default)]
    hosts: BTreeMap<String, Entry>,
}

impl HostLimits {
    pub fn cap(&self, host: &str) -> Option<usize> {
        self.hosts.get(host).map(|e| e.limit.max(1))
    }

    /// 服务器拒绝了这次连接。`attempted` 是当时想开的条数——拒绝说明「比这少才行」，
    /// 直接减半。返回压下来的新上限；没变得更低就返回 `None`。
    pub fn refused(&mut self, host: &str, attempted: usize) -> Option<usize> {
        let new = (attempted / 2).max(1);
        match self.hosts.get(host) {
            Some(e) if e.limit <= new => None,
            _ => {
                self.hosts.insert(host.to_owned(), Entry { limit: new });
                Some(new)
            }
        }
    }

    /// 完整下完一次：上限 +1 试探回去，不会超过 `max`。返回新上限。
    pub fn succeeded(&mut self, host: &str, max: usize) -> Option<usize> {
        let entry = self.hosts.get_mut(host)?;
        let new = entry.limit.saturating_add(1).min(max.max(1));
        if new == entry.limit {
            None
        } else {
            entry.limit = new;
            Some(new)
        }
    }

    /// 文件坏了当没有——PRD 6.3 要求状态文件损坏不能崩。
    pub fn load(path: &Path) -> HostLimits {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(self).unwrap_or_else(|_| b"{}".to_vec());
        std::fs::write(path, bytes)
    }
}

static SHARED: OnceLock<Mutex<HostLimits>> = OnceLock::new();
static FILE: OnceLock<Option<PathBuf>> = OnceLock::new();

/// 绑定记忆文件。进程内第一次调用生效（之后的调用只在 `SHARED` 还没初始化时才有意义）。
pub fn bind(file: Option<PathBuf>) {
    let _ = FILE.set(file);
    let _ = shared();
}

fn shared() -> &'static Mutex<HostLimits> {
    SHARED.get_or_init(|| {
        let loaded = FILE
            .get()
            .and_then(|f| f.as_ref())
            .map(|p| HostLimits::load(p))
            .unwrap_or_default();
        Mutex::new(loaded)
    })
}

/// 锁坏了（某个持锁的线程 panic 过）也要能接着用：记忆丢一点无所谓。
fn lock() -> MutexGuard<'static, HostLimits> {
    shared().lock().unwrap_or_else(|e| e.into_inner())
}

fn persist(limits: &HostLimits) {
    if let Some(path) = FILE.get().and_then(|f| f.as_ref()) {
        let _ = limits.save(path);
    }
}

/// 这台主机上次学到的上限（没学过就是 `None`）。
pub fn cap(host: &str) -> Option<usize> {
    lock().cap(host)
}

/// 服务器拒绝了连接：记住「要更少」。返回压下来的新上限。
pub fn refused(host: &str, attempted: usize) -> Option<usize> {
    let mut limits = lock();
    let new = limits.refused(host, attempted);
    if new.is_some() {
        persist(&limits);
    }
    new
}

/// 完整下完一次：把上限往上试探一格。
pub fn succeeded(host: &str, max: usize) -> Option<usize> {
    let mut limits = lock();
    let new = limits.succeeded(host, max);
    if new.is_some() {
        persist(&limits);
    }
    new
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_halves_and_success_climbs_back() {
        let mut limits = HostLimits::default();
        assert_eq!(limits.cap("a:80"), None);
        assert_eq!(limits.refused("a:80", 8), Some(4));
        assert_eq!(limits.cap("a:80"), Some(4));
        // 已经压到 4 了，再说一次「拒绝 8 条」不该继续往下压。
        assert_eq!(limits.refused("a:80", 8), None);
        assert_eq!(limits.refused("a:80", 4), Some(2));
        assert_eq!(limits.refused("a:80", 2), Some(1));
        // 减到 1 就到底了，不再变。
        assert_eq!(limits.refused("a:80", 1), None);
        assert_eq!(limits.cap("a:80"), Some(1));

        assert_eq!(limits.succeeded("a:80", 8), Some(2));
        assert_eq!(limits.succeeded("a:80", 8), Some(3));
        assert_eq!(limits.cap("a:80"), Some(3));
        // 只学过的没有被「成功」创造出来。
        assert_eq!(limits.succeeded("b:80", 8), None);
        // 键分主机和端口。
        assert_eq!(limits.cap("a:8080"), None);
    }

    #[test]
    fn success_never_exceeds_the_configured_maximum() {
        let mut limits = HostLimits::default();
        limits.refused("a:80", 16);
        assert_eq!(limits.cap("a:80"), Some(8));
        assert_eq!(limits.succeeded("a:80", 8), None);
        assert_eq!(limits.cap("a:80"), Some(8));
    }

    #[test]
    fn a_broken_file_reads_as_empty_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(HostLimits::load(&path).cap("127.0.0.1:1234"), None);

        let mut limits = HostLimits::default();
        limits.refused("127.0.0.1:1234", 8);
        limits.save(&path).unwrap();
        assert_eq!(HostLimits::load(&path).cap("127.0.0.1:1234"), Some(4));
    }
}
