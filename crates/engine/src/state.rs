//! 续传状态：`<文件>.volant.state`，放在 part 文件旁边，内容还是 JSON（人可读）。
//!
//! **不存请求头**（Cookie、Authorization）：谁续传谁自己再提供，凭据不落在下载目录里。
//!
//! # 为什么是两个槽位，而不是「临时文件 + rename」
//!
//! 第一版是 `create(.tmp)` → 写 → `rename`。正确，但贵得离谱：10 GiB 的下载里每秒
//! 一次这样的写入，吞吐从 1530 MB/s 掉到 1150 MB/s。这不是 `fsync` 的锅，是**元数据**
//! 的锅——create/rename 要让 NTFS 提交日志，而这个卷上同时有几 GB 脏页在排队。
//!
//! 现在是固定两个槽位，裸 `pwrite` 覆盖，一个元数据操作都没有：每槽
//! `[seq: u64][len: u32][fnv: u32][json…]`，写的时候挑序号大的另一个槽，读的时候取
//! 两个槽里校验通过、序号最大的那个。写到一半崩了只会坏一个槽，另一个还是上一个状态，
//! 所以最差退一个落盘间隔（`state_step`），正好是 PRD 6.3 要的那 1 MiB 量级。
//!
//! 磁盘顺序仍然是「先 `sync` part，再写状态」：状态的 `cursor` 绝不能指向还没交给
//! 操作系统的字节。反过来多写几个字节进状态是安全的（顶多重下）。

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::planner::SavedSegment;

const VERSION: u32 = 1;

/// 一个槽位的大小。32 段 × 每段约 60 字节 JSON，4 KiB 就够；给到 8 KiB 是为了
/// 连接数上限 32 段也放得下，且远超一行 4 KiB 的页大小对齐。
const SLOT: u64 = 8 << 10;

/// 槽位头：seq + len + fnv。
const HEAD: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeState {
    pub version: u32,
    /// 用户给的原始 URL。
    pub url: String,
    /// 跟随跳转之后的 URL，分段请求打这里。
    pub final_url: String,
    /// 最终文件路径（part 文件是它的 `.volant.part` 兄弟）。
    pub dest: PathBuf,
    pub size: u64,
    /// 用来判断「源文件还是不是同一个」。弱校验器（`W/"..."`）不记。
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
    /// 每段 `[start, cursor)` 在盘上。
    pub segments: Vec<SavedSegment>,
}

impl ResumeState {
    pub fn new(
        url: &str,
        final_url: &str,
        dest: PathBuf,
        size: u64,
        etag: Option<String>,
        last_modified: Option<String>,
        content_type: Option<String>,
    ) -> Self {
        ResumeState {
            version: VERSION,
            url: url.to_owned(),
            final_url: final_url.to_owned(),
            dest,
            size,
            etag,
            last_modified,
            content_type,
            segments: Vec::new(),
        }
    }

    /// 读回来校验版本；内容坏掉不是错误路径的终点——调用者会降级成「从头下载」。
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut raw = Vec::new();
        file.read_to_end(&mut raw)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let json =
            read_slot(&raw).ok_or_else(|| format!("{}: 两个槽位都没有完整状态", path.display()))?;
        let s: ResumeState =
            serde_json::from_slice(json).map_err(|e| format!("{}: {e}", path.display()))?;
        if s.version != VERSION {
            return Err(format!(
                "{}: 状态版本 {} 不认识（当前 {VERSION}）",
                path.display(),
                s.version
            ));
        }
        Ok(s)
    }

    /// 这份状态能不能接着用：URL 指同一个东西、长度一样、校验器没冲突。
    pub fn matches(
        &self,
        url: &str,
        final_url: &str,
        size: u64,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> bool {
        if self.size != size {
            return false;
        }
        if self.url != url || self.final_url != final_url {
            return false;
        }
        // 只要双方都给了同一类校验器，就必须一致。服务器这次没给校验器时仍允许续传，
        // 但绝不接受两个明确冲突的值。
        if let (Some(saved), Some(current)) = (self.etag.as_deref(), etag)
            && saved != current
        {
            return false;
        }
        if let (Some(saved), Some(current)) = (self.last_modified.as_deref(), last_modified)
            && saved != current
        {
            return false;
        }
        true
    }

    /// 状态 + part 都在盘上，才算「有进度可续」。
    pub fn downloaded(&self) -> u64 {
        self.segments.iter().map(|s| s.cursor - s.start).sum()
    }
}

/// 状态文件的写入端：一个常开的句柄，两个槽位轮流盖。
pub struct StateWriter {
    file: File,
    seq: u64,
    buf: Vec<u8>,
}

impl StateWriter {
    /// 截断重建。续传时旧的段表由调用者读进来再写回，不需要这里读。
    pub fn create(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        Ok(StateWriter {
            file,
            seq: 0,
            buf: vec![0; SLOT as usize],
        })
    }

    /// 写一个槽。`durable` 为真时才 `sync_data`——`kill -9` 用不着它（页缓存还在），
    /// 只有断电才需要。
    pub fn write(&mut self, s: &ResumeState, durable: bool) -> io::Result<()> {
        let json = serde_json::to_vec(s).map_err(io::Error::other)?;
        if json.len() + HEAD > SLOT as usize {
            return Err(io::Error::other(format!(
                "状态 {} 字节，超过一个槽位（{} 段太多了）",
                json.len(),
                s.segments.len()
            )));
        }
        self.seq += 1;
        let buf = &mut self.buf;
        buf[..HEAD].fill(0);
        buf[HEAD..].fill(0);
        buf[..8].copy_from_slice(&self.seq.to_le_bytes());
        buf[8..12].copy_from_slice(&(json.len() as u32).to_le_bytes());
        buf[12..16].copy_from_slice(&fnv(&json).to_le_bytes());
        buf[HEAD..HEAD + json.len()].copy_from_slice(&json);
        // 序号从 1 开始：第 n 次写落在第 (n-1) % 2 个槽。
        let at = (self.seq - 1) % 2 * SLOT;
        self.file.seek(SeekFrom::Start(at))?;
        self.file.write_all(buf)?;
        if durable {
            self.file.sync_data()?;
        }
        Ok(())
    }
}

/// 两个槽位里挑一个：校验通过且序号最大。`None` = 都不完整（截断、损坏）。
fn read_slot(raw: &[u8]) -> Option<&[u8]> {
    let mut best: Option<(u64, &[u8])> = None;
    for i in 0..2u64 {
        let at = (i * SLOT) as usize;
        let Some(slot) = raw.get(at..at + SLOT as usize) else {
            continue;
        };
        let seq = u64::from_le_bytes(slot[..8].try_into().ok()?);
        let len = u32::from_le_bytes(slot[8..12].try_into().ok()?) as usize;
        let sum = u32::from_le_bytes(slot[12..16].try_into().ok()?);
        if seq == 0 || len > SLOT as usize - HEAD {
            continue;
        }
        let payload = &slot[HEAD..HEAD + len];
        if fnv(payload) != sum {
            continue;
        }
        if best.is_none_or(|(s, _)| seq > s) {
            best = Some((seq, payload));
        }
    }
    best.map(|(_, p)| p)
}

/// FNV-1a 32：只为发现「写到一半」，不是防篡改。自带实现是为了不引依赖。
fn fnv(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in bytes {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// `<dest>.volant.part`
pub fn part_path(dest: &Path) -> PathBuf {
    sibling(dest, ".volant.part")
}

/// `<dest>.volant.state`
pub fn state_path(dest: &Path) -> PathBuf {
    sibling(dest, ".volant.state")
}

/// `<dest>.volant.lock`：只用来在进程/任务存活期间持有排他锁，内容始终为空。
pub fn lock_path(dest: &Path) -> PathBuf {
    sibling(dest, ".volant.lock")
}

fn sibling(dest: &Path, suffix: &str) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(dest: PathBuf) -> ResumeState {
        let mut s = ResumeState::new(
            "http://x/a.zip",
            "http://cdn/a.zip",
            dest,
            100,
            Some("\"e\"".into()),
            None,
            Some("application/zip".into()),
        );
        s.segments = vec![
            SavedSegment {
                start: 0,
                cursor: 10,
                end: 50,
            },
            SavedSegment {
                start: 50,
                cursor: 60,
                end: 100,
            },
        ];
        s
    }

    #[test]
    fn save_load_round_trip_and_progress() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.zip");
        let path = state_path(&dest);
        let s = sample(dest);
        let mut w = StateWriter::create(&path).unwrap();
        w.write(&s, true).unwrap();
        let back = ResumeState::load(&path).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.downloaded(), 20);
    }

    #[test]
    fn the_newest_slot_wins_and_a_torn_one_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.state");
        let mut w = StateWriter::create(&path).unwrap();
        let mut first = sample(dir.path().join("a"));
        w.write(&first, false).unwrap();
        first.segments[0].cursor = 30;
        w.write(&first, false).unwrap();
        // 第三次写进的是第 0 槽：把它的校验和改坏，读回来必须是上一次（第 1 槽）。
        first.segments[0].cursor = 40;
        w.write(&first, false).unwrap();
        let mut raw = std::fs::read(&path).unwrap();
        raw[12] ^= 0xff;
        std::fs::write(&path, &raw).unwrap();
        let back = ResumeState::load(&path).unwrap();
        assert_eq!(back.segments[0].cursor, 30, "坏掉的槽要被跳过");
    }

    #[test]
    fn a_broken_or_future_state_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.json");
        std::fs::write(&p, b"{not json").unwrap();
        assert!(ResumeState::load(&p).is_err());
        // 合法 JSON 但版本不认识：槽位校验过，版本检查拦下来。
        let bad = ResumeState {
            version: 99,
            ..sample(dir.path().join("a"))
        };
        let mut w = StateWriter::create(&p).unwrap();
        w.write(&bad, false).unwrap();
        assert!(ResumeState::load(&p).is_err());
        assert!(ResumeState::load(&dir.path().join("missing.json")).is_err());
    }

    #[test]
    fn matches_rejects_a_changed_source() {
        let dest = PathBuf::from("a");
        let s = ResumeState::new(
            "u",
            "f",
            dest,
            100,
            Some("\"v1\"".into()),
            Some("Tue, 11 Nov 2026 00:00:00 GMT".into()),
            None,
        );
        assert!(s.matches(
            "u",
            "f",
            100,
            Some("\"v1\""),
            Some("Tue, 11 Nov 2026 00:00:00 GMT")
        ));
        assert!(
            s.matches("u", "f", 100, None, None),
            "服务器这次没给 ETag，不能让用户从头下"
        );
        assert!(!s.matches("u", "f", 101, Some("\"v1\""), None), "长度变了");
        assert!(!s.matches("u", "f", 100, Some("\"v2\""), None), "ETag 变了");
        assert!(
            !s.matches("other", "other", 100, Some("\"v1\""), None),
            "URL 变了"
        );
        assert!(
            !s.matches("u", "cdn-v2", 100, Some("\"v1\""), None),
            "同一个原始 URL 重定向到了不同最终 URL"
        );
        assert!(
            !s.matches("u", "f", 100, None, Some("Wed, 12 Nov 2026 00:00:00 GMT")),
            "没有 ETag 时必须校验 Last-Modified"
        );
    }

    #[test]
    fn paths_are_siblings_of_the_destination() {
        let d = PathBuf::from(r"D:\dl\a.zip");
        assert_eq!(part_path(&d), PathBuf::from(r"D:\dl\a.zip.volant.part"));
        assert_eq!(state_path(&d), PathBuf::from(r"D:\dl\a.zip.volant.state"));
        assert_eq!(lock_path(&d), PathBuf::from(r"D:\dl\a.zip.volant.lock"));
    }
}
