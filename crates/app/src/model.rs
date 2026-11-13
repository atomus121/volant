//! 列表里的数据与落盘格式。
//!
//! 这里只有「界面要知道的东西」：一行一个下载任务、一个全局设置。引擎的
//! `Config`/`Event` 不进来，控制器负责翻译。
//!
//! 落盘是 `%LOCALAPPDATA%\Volant\state.json`，先写 `state.json.tmp` 再改名——
//! 断电时最多丢最后一次保存，不会留下半个 JSON。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

pub const MAX_PARALLEL_DOWNLOADS: usize = 20;
pub const MAX_CONNECTIONS_PER_TASK: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum Status {
    #[default]
    Queued,
    Probing,
    Active,
    Paused,
    Done,
    Failed,
    /// 下完了但还在做种（设置里打开了做种）。
    Seeding,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Queued => "排队中",
            Status::Probing => "解析中",
            Status::Active => "下载中",
            Status::Paused => "已暂停",
            Status::Done => "已完成",
            Status::Failed => "失败",
            Status::Seeding => "做种中",
        }
    }

    /// 进程重启后这些状态没法继续：引擎的任务已经不在了。
    pub fn needs_restart(self) -> bool {
        matches!(self, Status::Queued | Status::Probing | Status::Active)
    }
}

/// 任务类型。同一个列表里两种行，只有详情面板和 note 的字段不同。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum Kind {
    #[default]
    Http,
    Torrent,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Row {
    pub id: u64,
    pub url: String,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub dest: Option<PathBuf>,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub downloaded: u64,
    pub status: Status,
    #[serde(default)]
    pub connections: usize,
    #[serde(default)]
    pub resumed_from: u64,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub elapsed_ms: u64,
    /// 从浏览器带下来的请求头（Cookie/Referer/UA）。落盘是为了续传时还能用原来的身份。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<(String, String)>,

    /// `.torrent` 字节的副本（`home/torrents/<id>.torrent`）；磁力链接是 `None`。
    /// 存副本而不是原路径：拖进来的文件被删了也能续传。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub torrent: Option<PathBuf>,
    /// 多文件的扁平列表；单文件或 HTTP 是空的。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<TorrentFile>,
    /// 选中的文件下标；`None` = 全部。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<usize>>,
    #[serde(default)]
    pub uploaded: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub info_hash: String,

    /// 下面几个是运行时算的瞬时值，不落盘。
    #[serde(skip)]
    pub speed: f64,
    #[serde(skip)]
    pub sample: Option<(Instant, u64)>,
    /// 最近 60 秒的速度采样；只用于运行时折线图，不写 `state.json`。
    #[serde(skip)]
    pub speed_history: VecDeque<(Instant, f64)>,
    #[serde(skip)]
    pub peers: usize,
    #[serde(skip)]
    pub seeders: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TorrentFile {
    pub name: String,
    pub len: u64,
}

impl Row {
    pub fn new(id: u64, url: String) -> Row {
        Row {
            id,
            kind: Kind::Http,
            name: String::new(),
            url,
            dest: None,
            size: 0,
            downloaded: 0,
            status: Status::Queued,
            connections: 0,
            resumed_from: 0,
            error: None,
            elapsed_ms: 0,
            headers: Vec::new(),
            torrent: None,
            files: Vec::new(),
            only: None,
            uploaded: 0,
            info_hash: String::new(),
            speed: 0.0,
            sample: None,
            speed_history: VecDeque::new(),
            peers: 0,
            seeders: 0,
        }
    }

    /// 一行 torrent 任务。`url` 可以是磁力链接，也可以是 `.torrent` 的显示名。
    pub fn torrent(id: u64, url: String) -> Row {
        Row {
            kind: Kind::Torrent,
            ..Row::new(id, url)
        }
    }

    /// 上传/下载比；还没下到东西时是 0。
    pub fn ratio(&self) -> f64 {
        if self.downloaded == 0 {
            0.0
        } else {
            self.uploaded as f64 / self.downloaded as f64
        }
    }

    /// 记录一次当前速度，并裁掉 60 秒之前的点。
    pub fn push_speed_sample(&mut self, now: Instant) {
        self.speed_history.push_back((now, self.speed));
        while self
            .speed_history
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > std::time::Duration::from_secs(60))
        {
            self.speed_history.pop_front();
        }
    }

    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.url
        } else {
            &self.name
        }
    }

    pub fn frac(&self) -> f32 {
        if self.size == 0 {
            0.0
        } else {
            (self.downloaded as f64 / self.size as f64).clamp(0.0, 1.0) as f32
        }
    }

    /// 剩余秒数；速度还没测出来时为 `None`。
    pub fn eta_secs(&self) -> Option<f64> {
        if self.size == 0 || self.speed < 1024.0 {
            return None;
        }
        Some((self.size.saturating_sub(self.downloaded)) as f64 / self.speed)
    }

    /// 行内第二行（灰色说明）。
    pub fn note(&self) -> String {
        match self.status {
            Status::Active => {
                let mut parts = Vec::new();
                if let Some(eta) = self.eta_secs() {
                    parts.push(format!("剩余 {}", crate::fmt::eta(eta)));
                }
                if self.connections > 0 {
                    parts.push(format!("{} 连接", self.connections));
                }
                if let Some(picked) = self.picked_note() {
                    parts.push(picked);
                }
                if parts.is_empty() {
                    "正在传输…".into()
                } else {
                    parts.join(" · ")
                }
            }
            Status::Probing => "正在解析链接…".into(),
            Status::Queued => "等待前面的任务".into(),
            Status::Paused => {
                let mut parts = vec![if self.downloaded > 0 {
                    "可续传".to_string()
                } else {
                    "等待继续".to_string()
                }];
                if let Some(picked) = self.picked_note() {
                    parts.push(picked);
                }
                parts.join(" · ")
            }
            Status::Done => self.done_note(),
            Status::Seeding => {
                let mut parts = vec![
                    format!("上传 {}", crate::fmt::bytes(self.uploaded)),
                    format!("分享率 {:.2}", self.ratio()),
                ];
                if self.connections > 0 {
                    parts.push(format!("{} 连接", self.connections));
                }
                if let Some(picked) = self.picked_note() {
                    parts.push(picked);
                }
                parts.join(" · ")
            }
            Status::Failed => friendly_error(self.error.as_deref().unwrap_or("")),
        }
    }

    fn picked_note(&self) -> Option<String> {
        match &self.only {
            Some(v) if self.files.len() > 1 => {
                Some(format!("已选 {}/{} 个文件", v.len(), self.files.len()))
            }
            _ => None,
        }
    }

    fn done_note(&self) -> String {
        if self.kind != Kind::Torrent {
            return "HTTP 分段下载".into();
        }
        let mut parts = Vec::new();
        if self.files.len() > 1 {
            let selected = self.only.as_ref().map_or(self.files.len(), Vec::len);
            parts.push(format!("已选 {selected}/{} 个文件", self.files.len()));
        } else {
            parts.push("BitTorrent".into());
        }
        if self.uploaded > 0 {
            parts.push(format!(
                "已上传 {} · 分享率 {:.2}",
                crate::fmt::bytes(self.uploaded),
                self.ratio()
            ));
        }
        parts.join(" · ")
    }
}

/// 外观：跟随系统 / 强制浅色 / 强制深色。
///
/// PRD 功能 27 的裁决是「砍掉主题引擎（9 套 + TOML + 图标集），只做浅/深色跟随系统」，
/// 所以这里只有三档，没有自定义配色。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Settings {
    /// 同时下载几个（排队上限）。
    pub limit: usize,
    /// 每个任务开几条连接。
    pub connections: usize,
    /// 全局限速（字节/秒），`None` = 不限。
    pub rate: Option<u64>,
    /// 下载目录。
    pub dir: PathBuf,
    /// 监视剪贴板里的链接。
    pub clipboard: bool,
    /// 下完继续做种（默认关：下完即停）。
    #[serde(default)]
    pub seed: bool,
    /// 做种到分享率多少就停；`0` = 不按分享率停。
    #[serde(default = "default_ratio")]
    pub seed_ratio: f64,
    /// 外观；老配置文件里没有这个键，按「跟随系统」补。
    #[serde(default)]
    pub theme: ThemeMode,
    /// 点击删除时先弹出确认；可在确认框里选“不再提示”。
    #[serde(default = "default_true")]
    pub confirm_delete: bool,
    /// 浏览器捕获和拖入种子时跳过“新建下载”确认，直接进入队列。
    #[serde(default)]
    pub skip_add_dialog: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            limit: 3,
            connections: 8,
            rate: None,
            dir: default_dir(),
            clipboard: true,
            seed: false,
            seed_ratio: 2.0,
            theme: ThemeMode::System,
            confirm_delete: true,
            skip_add_dialog: false,
        }
    }
}

impl Settings {
    pub fn normalize(&mut self) {
        self.limit = self.limit.clamp(1, MAX_PARALLEL_DOWNLOADS);
        self.connections = self.connections.clamp(1, MAX_CONNECTIONS_PER_TASK);
    }
}

#[derive(Serialize, Deserialize)]
pub struct Persisted {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub rows: Vec<Row>,
}

impl Default for Persisted {
    fn default() -> Self {
        Persisted {
            version: 1,
            next_id: 1,
            settings: Settings::default(),
            rows: Vec::new(),
        }
    }
}

fn one() -> u32 {
    1
}

fn default_ratio() -> f64 {
    2.0
}

fn default_true() -> bool {
    true
}

/// 粘贴框/剪贴板里认出来的是磁力链接。
pub fn is_magnet(s: &str) -> bool {
    s.len() > 7
        && s.get(..7)
            .is_some_and(|p| p.eq_ignore_ascii_case("magnet:"))
}

/// 列表和 toast 只解释下一步；原始技术错误仍保留在 `Row::error` 与详情/日志里。
pub fn friendly_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("403") {
        "服务器拒绝访问。请检查链接是否过期、是否需要登录。".into()
    } else if lower.contains("404") {
        "下载地址不存在。请检查链接是否有效。".into()
    } else if lower.contains("timeout") || lower.contains("timed out") {
        "连接超时。请检查网络后重试。".into()
    } else if lower.contains("拒绝访问")
        || lower.contains("permission denied")
        || lower.contains("os error 5")
    {
        "无法写入保存位置。请更换下载目录后重试。".into()
    } else if lower.contains("no space") || lower.contains("磁盘空间") {
        "磁盘空间不足。请清理空间或更换下载目录。".into()
    } else {
        "下载失败。请重试；若仍失败，检查链接、网络或保存位置。".into()
    }
}

pub struct Paths {
    /// 配置与状态都放这里。
    pub home: PathBuf,
    pub state: PathBuf,
    /// 主机连接上限的记忆文件（引擎的 `hosts.json`）。
    pub hosts: PathBuf,
    /// 拖进来的 `.torrent` 副本放这里。
    pub torrents: PathBuf,
    /// 首选目录写不进、换到别处时说明原因（`App::new` 弹一次提示，别让设置自己搬家）。
    pub note: Option<String>,
}

impl Paths {
    pub fn resolve() -> Paths {
        let candidates = Self::candidates();
        let (home, note) = choose(&candidates, probe_dir);
        Paths::at(home, note)
    }

    /// 依次试：显式指定 → 本地配置 → 漫游配置 → exe 旁边 → 临时目录。
    fn candidates() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(dir) = std::env::var_os("VOLANT_HOME").filter(|v| !v.is_empty()) {
            out.push(PathBuf::from(dir));
        }
        for var in ["LOCALAPPDATA", "APPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                out.push(PathBuf::from(base).join("Volant"));
            }
        }
        // 便携版解压在哪儿，数据就放哪儿：跟 exe 同一棵树，权限跟着 exe 走。
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
        {
            out.push(dir.join("Volant-data"));
        }
        out.push(std::env::temp_dir().join("Volant"));
        out
    }

    fn at(home: PathBuf, note: Option<String>) -> Paths {
        Paths {
            state: home.join("state.json"),
            hosts: home.join("hosts.json"),
            torrents: home.join("torrents"),
            home,
            note,
        }
    }

    /// 一份 `.torrent` 副本的落点。
    pub fn torrent_copy(&self, id: u64) -> PathBuf {
        self.torrents.join(format!("{id}.torrent"))
    }
}

/// 挑第一个真写得进去的候选；全都写不进就用第一个，并把原因带出去。
fn choose(
    candidates: &[PathBuf],
    probe: impl Fn(&Path) -> std::io::Result<()>,
) -> (PathBuf, Option<String>) {
    let mut first_err: Option<(PathBuf, std::io::Error)> = None;
    for dir in candidates {
        match probe(dir) {
            Ok(()) => {
                // 只说「谁写不进、为什么」，搬到哪儿由调用方补（设置里那行已经写了落点）。
                let note = first_err.map(|(bad, e)| format!("{} 写不进（{e}）", bad.display()));
                return (dir.clone(), note);
            }
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some((dir.clone(), e));
                }
            }
        }
    }
    let home = candidates
        .first()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."));
    let note = first_err.map(|(bad, e)| format!("{} 写不进：{e}", bad.display()));
    (home, note)
}

/// 真写一个探针文件再删掉：**目录存在不代表能写**。
///
/// 低完整性令牌、沙箱、只读的配置目录都会让 `create_dir_all` 成功，然后
/// `state.json` / `.torrent` 副本落盘时才报 os error 3/5——所以只信写入结果。
fn probe_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(format!(".probe-{}", std::process::id()));
    std::fs::write(&probe, b"")?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// 选择真正能写的下载目录；首选目录不可写时自动退到应用数据目录下的 `downloads`。
pub fn writable_download_dir(preferred: &Path, fallback: &Path) -> (PathBuf, Option<String>) {
    writable_download_dir_with(preferred, fallback, probe_dir)
}

/// 下载目录所在卷的 `(可用字节, 总字节)`；读取失败时返回 None。
#[cfg(windows)]
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    let mut total = 0u64;
    let ok =
        unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, std::ptr::null_mut()) };
    (ok != 0).then_some((free, total))
}

#[cfg(not(windows))]
pub fn disk_space(_path: &Path) -> Option<(u64, u64)> {
    None
}

fn writable_download_dir_with(
    preferred: &Path,
    fallback: &Path,
    probe: impl Fn(&Path) -> std::io::Result<()>,
) -> (PathBuf, Option<String>) {
    match probe(preferred) {
        Ok(()) => (preferred.to_path_buf(), None),
        Err(e) if fallback != preferred && probe(fallback).is_ok() => (
            fallback.to_path_buf(),
            Some(format!(
                "下载目录 {} 写不进（{e}），本次改存 {}",
                preferred.display(),
                fallback.display()
            )),
        ),
        Err(e) => (
            preferred.to_path_buf(),
            Some(format!("下载目录 {} 写不进：{e}", preferred.display())),
        ),
    }
}

/// `%USERPROFILE%\Downloads`，没有就用家目录。
pub fn default_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("USERPROFILE") {
        let p = PathBuf::from(p);
        let downloads = p.join("Downloads");
        if downloads.is_dir() {
            return downloads;
        }
        return p;
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub struct Store {
    pub path: PathBuf,
}

impl Store {
    pub fn new(path: PathBuf) -> Store {
        Store { path }
    }

    pub fn load(&self) -> Persisted {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Persisted::default();
        };
        match serde_json::from_str::<Persisted>(&text) {
            Ok(mut p) => {
                p.settings.normalize();
                // 上次退出时还在跑的任务已经不存在了，改成「已暂停」；用户点继续就是续传。
                for r in &mut p.rows {
                    r.speed = 0.0;
                    r.sample = None;
                    if r.status.needs_restart() {
                        r.status = Status::Paused;
                        r.connections = 0;
                    }
                    // 进程重启后没有会话在跑，做种行退回已完成。
                    if r.status == Status::Seeding {
                        r.status = Status::Done;
                    }
                    if r.status == Status::Done && r.size > 0 {
                        r.downloaded = r.size;
                    }
                }
                p.next_id = p
                    .next_id
                    .max(p.rows.iter().map(|r| r.id + 1).max().unwrap_or(1));
                p
            }
            Err(e) => {
                eprintln!("state.json 解析失败（{e}），从空列表开始");
                Persisted::default()
            }
        }
    }

    pub fn save(&self, p: &Persisted) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(p)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.path)
    }
}

/// 暂停/删除后要清的盘上残留：`<dest>.volant.part` 与 `<dest>.volant.state`。
pub fn sidecar(dest: &std::path::Path, suffix: &str) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    dest.with_file_name(name)
}

pub fn remove_leftovers(dest: &std::path::Path) {
    for suffix in [".volant.part", ".volant.state"] {
        let _ = std::fs::remove_file(sidecar(dest, suffix));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_paths_match_engine() {
        let d = std::path::Path::new(r"D:\dl\a.zip");
        assert_eq!(
            sidecar(d, ".volant.part"),
            PathBuf::from(r"D:\dl\a.zip.volant.part")
        );
        assert_eq!(
            sidecar(d, ".volant.state"),
            PathBuf::from(r"D:\dl\a.zip.volant.state")
        );
    }

    #[test]
    fn restart_turns_running_rows_into_paused() {
        let dir = std::env::temp_dir().join(format!("volant-state-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let store = Store::new(path.clone());
        let mut p = Persisted {
            next_id: 7,
            rows: vec![
                Row::new(3, "http://x/a".into()),
                Row::new(6, "http://x/b".into()),
            ],
            ..Default::default()
        };
        p.rows[0].status = Status::Active;
        p.rows[1].status = Status::Done;
        p.rows[1].size = 10;
        store.save(&p).unwrap();

        let got = store.load();
        assert_eq!(got.rows[0].status, Status::Paused);
        assert_eq!(got.rows[1].status, Status::Done);
        assert_eq!(got.rows[1].downloaded, 10);
        assert_eq!(got.next_id, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn magnet_prefix_is_case_insensitive() {
        assert!(is_magnet("magnet:?xt=urn:btih:abc"));
        assert!(is_magnet("MAGNET:?xt=urn:btih:abc"));
        assert!(!is_magnet("https://x/a.torrent"));
        assert!(!is_magnet("magnet:"));
    }

    #[test]
    fn torrent_row_reports_ratio_and_selection() {
        let mut r = Row::torrent(1, "magnet:?xt=urn:btih:abc".into());
        r.files = vec![
            TorrentFile {
                name: "a.iso".into(),
                len: 10,
            },
            TorrentFile {
                name: "b.srt".into(),
                len: 2,
            },
        ];
        r.only = Some(vec![0]);
        r.downloaded = 10;
        r.uploaded = 15;
        r.status = Status::Seeding;
        assert!((r.ratio() - 1.5).abs() < 1e-9);
        let note = r.note();
        assert!(note.contains("已选 1/2 个文件"), "{note}");
        assert!(note.contains("分享率 1.50"), "{note}");
    }

    #[test]
    fn row_notes_do_not_repeat_status_size_or_percentage() {
        let mut active = Row::new(1, "http://x/a".into());
        active.status = Status::Active;
        active.size = 100;
        active.downloaded = 50;
        active.connections = 8;
        let note = active.note();
        assert_eq!(note, "8 连接", "{note}");

        let mut paused = Row::new(2, "http://x/b".into());
        paused.status = Status::Paused;
        paused.size = 100;
        paused.downloaded = 50;
        let note = paused.note();
        assert_eq!(note, "可续传", "{note}");

        let mut done = Row::torrent(3, "magnet:?xt=urn:btih:abc".into());
        done.status = Status::Done;
        done.files = vec![
            TorrentFile {
                name: "a".into(),
                len: 10,
            },
            TorrentFile {
                name: "b".into(),
                len: 10,
            },
        ];
        let note = done.note();
        assert_eq!(note, "已选 2/2 个文件");
        assert!(!note.contains("已完成"));
        assert!(!note.contains('%'));
    }

    #[test]
    fn technical_errors_get_actionable_user_copy() {
        assert!(friendly_error("server returned HTTP 403").contains("检查链接"));
        assert!(friendly_error("拒绝访问。 (os error 5)").contains("更换下载目录"));
        assert!(friendly_error("request timed out").contains("检查网络"));
        let generic = friendly_error("unexpected EOF");
        assert!(generic.contains("重试"));
        assert!(!generic.contains("EOF"));
    }

    #[test]
    fn settings_are_clamped_to_supported_concurrency() {
        let mut settings = Settings {
            limit: 100,
            connections: 100,
            ..Default::default()
        };
        settings.normalize();
        assert_eq!(settings.limit, MAX_PARALLEL_DOWNLOADS);
        assert_eq!(settings.connections, MAX_CONNECTIONS_PER_TASK);

        settings.limit = 0;
        settings.connections = 0;
        settings.normalize();
        assert_eq!(settings.limit, 1);
        assert_eq!(settings.connections, 1);
    }

    #[test]
    fn seeding_rows_come_back_done() {
        let dir = std::env::temp_dir().join(format!("volant-state-seed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::new(dir.join("state.json"));
        let mut p = Persisted::default();
        let mut r = Row::torrent(1, "magnet:?xt=urn:btih:abc".into());
        r.status = Status::Seeding;
        p.rows.push(r);
        store.save(&p).unwrap();
        assert_eq!(store.load().rows[0].status, Status::Done);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn speed_history_keeps_only_the_last_minute() {
        let now = Instant::now();
        let mut row = Row::new(1, "http://x/a".into());
        row.speed_history
            .push_back((now - std::time::Duration::from_secs(61), 1.0));
        row.speed = 2.5;
        row.push_speed_sample(now);
        assert_eq!(row.speed_history.len(), 1);
        assert_eq!(row.speed_history.back().unwrap().1, 2.5);
    }

    #[cfg(windows)]
    #[test]
    fn disk_space_reports_free_and_total() {
        let (free, total) = disk_space(Path::new(".")).expect("Windows 应能读取当前卷空间");
        assert!(total > 0);
        assert!(free <= total);
    }

    #[test]
    fn unwritable_dir_falls_back_and_says_why() {
        let cands = vec![PathBuf::from("A"), PathBuf::from("B"), PathBuf::from("C")];
        let (home, note) = choose(&cands, |p| {
            if p == Path::new("C") {
                Ok(())
            } else {
                Err(std::io::Error::from_raw_os_error(5))
            }
        });
        assert_eq!(home, PathBuf::from("C"));
        let note = note.expect("换目录要说明原因");
        assert!(note.contains("A 写不进"), "{note}");
        assert!(note.contains("os error 5"), "{note}");

        // 全都写不进去：仍然给第一个，让保存/加种子时把错误原文弹出来。
        let (home, note) = choose(&cands, |_| Err(std::io::Error::from_raw_os_error(5)));
        assert_eq!(home, PathBuf::from("A"));
        assert!(note.is_some());
    }

    #[test]
    fn unwritable_download_dir_falls_back_to_app_data() {
        let preferred = PathBuf::from(r"C:\Users\me\Downloads");
        let fallback = PathBuf::from(r"D:\Volant-data\downloads");
        let (dir, note) = writable_download_dir_with(&preferred, &fallback, |p| {
            if p == fallback {
                Ok(())
            } else {
                Err(std::io::Error::from_raw_os_error(5))
            }
        });
        assert_eq!(dir, fallback);
        assert!(note.unwrap().contains("os error 5"));

        let (dir, note) = writable_download_dir_with(&fallback, &fallback, |_| Ok(()));
        assert_eq!(dir, fallback);
        assert!(note.is_none());
    }
}
