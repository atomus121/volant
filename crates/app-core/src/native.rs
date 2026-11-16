//! native messaging host：浏览器拉起的那个小进程只做字节搬运。
//!
//! 扩展 → stdin 帧 → 命名管道 → GUI 进程，GUI 进程当成普通下载处理。
//! 这里刻意不碰网络、不碰磁盘（除了安装清单），因为它是网页能间接指挥的进程。

// 这个文件被两个 bin 各编一份：界面只用 `install()`，host 只用 `run_host()`。
#![allow(dead_code)]

use std::fs::File;
use std::io::{self};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::capture::{PIPE, read_frame, write_frame};

/// 清单名，也是扩展里 `connectNative` 的参数。
pub const HOST: &str = "com.volant.host";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostStatus {
    pub host_exe_present: bool,
    pub chromium_registered: bool,
    pub firefox_registered: bool,
}

impl HostStatus {
    pub fn installed(self) -> bool {
        self.host_exe_present && (self.chromium_registered || self.firefox_registered)
    }
}

/// 扩展 id 由 `extension/manifest.json` 的 `key` 固定死（见那个文件），所以这里能写常量。
///
/// 算法：`sha256(base64decode(key))` 取前 16 字节，每个半字节（0–15）映射成 `a`–`p`。
/// 抄错一个字符，浏览器就报 Specified native messaging host not found —— 之前这里
/// 就写成了把相邻两位互换的 `elghnpdlfghjeknbb…`，`allowed_origins` 永远对不上。
/// 换 key 就得重算，并且用 `--install-host` 打印出来的 id 跟 chrome://extensions 核对。
const CHROME_ID: &str = "lehgpnldgfjhkebnbblialhkkhcgaohm";
/// 同一个扩展在 Firefox 里的身份。
const FIREFOX_ID: &str = "volant@local";

const CHROMIUM_KEYS: [&str; 5] = [
    r"Software\Google\Chrome\NativeMessagingHosts",
    r"Software\Microsoft\Edge\NativeMessagingHosts",
    r"Software\BraveSoftware\Brave-Browser\NativeMessagingHosts",
    r"Software\Vivaldi\NativeMessagingHosts",
    r"Software\Chromium\NativeMessagingHosts",
];

/// `volant-host.exe` 走这条：读浏览器给的一帧，转发给界面，再把界面的一帧答复
/// 原样交给浏览器。视频解析和画质选择需要这条双向通道；普通下载也由同一路径确认。
pub fn run_host() -> i32 {
    let mut stdin = io::stdin().lock();
    let Ok(request) = read_frame(&mut stdin) else {
        return 0;
    };
    let reply = local_reply(&request)
        .or_else(|| relay_request(&request).ok())
        .unwrap_or_else(|| {
            serde_json::to_vec(&serde_json::json!({
                "ok": false,
                "why": "host-failed",
                "error": "Volant is not running",
            }))
            .unwrap_or_else(|_| br#"{"ok":false,"why":"host-failed"}"#.to_vec())
        });
    let mut stdout = io::stdout().lock();
    let _ = write_frame(&mut stdout, &reply);
    0
}

/// Popup status is a property of the installed host, not a reason to start the
/// desktop process. Only an actual download, video lookup, or explicit open
/// request needs the app.
fn local_reply(request: &[u8]) -> Option<Vec<u8>> {
    let request: serde_json::Value = serde_json::from_slice(request).ok()?;
    let action = request.get("type").and_then(serde_json::Value::as_str)?;
    if !matches!(action, "ping" | "rules") {
        return None;
    }
    serde_json::to_vec(&serde_json::json!({
        "ok": true,
        "req": request.get("req").cloned().unwrap_or_default(),
        "version": env!("CARGO_PKG_VERSION"),
        "capture": true,
        "videoLookup": true,
        "videoDependencies": crate::video::dependency_status(),
        "opened": false,
    }))
    .ok()
}

/// `volant.exe --install-host` 走这条：写清单、挂注册表，返回要给人看的几行。
pub fn install() -> Result<Vec<String>, String> {
    let dir = manifest_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("建目录 {} 失败：{e}", dir.display()))?;
    // 注册表里存的是清单路径，清单里存的是 exe 路径，两边都得是绝对路径。
    let dir = std::path::absolute(&dir).unwrap_or(dir);
    // 清单里必须写 host 那个 exe：浏览器会不带参数直接拉起它。
    let exe = sibling("volant-host");
    let exe = std::path::absolute(&exe).unwrap_or(exe);
    let exe = exe.to_string_lossy().to_string();

    let chromium = dir.join(format!("{HOST}.json"));
    let firefox = dir.join(format!("{HOST}.firefox.json"));
    write_json(&chromium, &chromium_manifest(&exe))?;
    write_json(&firefox, &firefox_manifest(&exe))?;

    let mut out = vec![
        format!("清单 {}", chromium.display()),
        format!("清单 {}", firefox.display()),
        // 这个 id 必须和 chrome://extensions 里看到的一致，不然 manifest 白写。
        format!("扩展 id {CHROME_ID}（核对 chrome://extensions 里 Volant 的 ID）"),
    ];
    // 注册表写不进去不代表清单白写了：把该跑的命令原样打出来，用户/安装器照着跑就行。
    // （安全软件、受限账户都会拦 HKCU 的写入。）
    let mut failed = Vec::new();
    for key in CHROMIUM_KEYS {
        let key = format!(r"HKCU\{key}\{HOST}");
        match reg_add(&key, &chromium) {
            Ok(line) => out.push(line),
            Err(e) => failed.push(e),
        }
    }
    let mozilla = format!(r"HKCU\Software\Mozilla\NativeMessagingHosts\{HOST}");
    match reg_add(&mozilla, &firefox) {
        Ok(line) => out.push(line),
        Err(e) => failed.push(e),
    }
    if !failed.is_empty() {
        out.push(
            "注册表没全部写上（安全软件 / 受限账户常见）。手动跑这几条，或换个能写 HKCU 的账户："
                .into(),
        );
        out.extend(failed);
    }
    Ok(out)
}

/// Check the local files and per-user registry entries used by browsers.
pub fn host_status() -> HostStatus {
    let dir = manifest_dir();
    let host_exe_present = sibling("volant-host").is_file();
    #[cfg(windows)]
    {
        let chromium = CHROMIUM_KEYS
            .iter()
            .any(|key| reg_installed(&format!(r"HKCU\{key}\{HOST}")));
        let firefox = reg_installed(&format!(
            r"HKCU\Software\Mozilla\NativeMessagingHosts\{HOST}"
        ));
        let _ = dir;
        HostStatus {
            host_exe_present,
            chromium_registered: chromium,
            firefox_registered: firefox,
        }
    }
    #[cfg(not(windows))]
    {
        let chromium = dir.join(format!("{HOST}.json")).is_file();
        HostStatus {
            host_exe_present,
            chromium_registered: chromium,
            firefox_registered: false,
        }
    }
}

#[cfg(windows)]
fn reg_installed(key: &str) -> bool {
    Command::new("reg")
        .args(["query", key, "/ve"])
        .output()
        .is_ok_and(|out| out.status.success())
}

fn manifest_dir() -> PathBuf {
    crate::model::Paths::resolve().home.join("native")
}

/// 同目录下的另一个 exe（`volant` / `volant-host`）。找不到就用自己，至少不会装到别处去。
fn sibling(stem: &str) -> PathBuf {
    let me = std::env::current_exe().unwrap_or_else(|_| PathBuf::from(stem));
    let name = if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    };
    let candidate = me.with_file_name(name);
    if candidate.is_file() { candidate } else { me }
}

fn chromium_manifest(exe: &str) -> serde_json::Value {
    serde_json::json!({
        "name": HOST,
        "description": "Volant 下载捕获",
        "path": exe,
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{CHROME_ID}/")],
    })
}

fn firefox_manifest(exe: &str) -> serde_json::Value {
    serde_json::json!({
        "name": HOST,
        "description": "Volant 下载捕获",
        "path": exe,
        "type": "stdio",
        "allowed_extensions": [FIREFOX_ID],
    })
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("写 {} 失败：{e}", path.display()))
}

/// 写进注册表的那个值：**裸路径**。
///
/// 不要自己加引号——`reg add /d` 收到的是 argv，reg.exe 会把引号原样存进注册表，
/// 浏览器读到 `"C:\...\host.json"` 就报 Specified native messaging host not found。
fn reg_value(manifest: &Path) -> String {
    // ponytail: 路径含空格时这里是 std 负责给 argv 加引号，reg.exe 会不会原样存下
    // 没在本机验证（本机没有带空格的配置目录）。真碰上就用 `reg query` 回读核对。
    manifest.display().to_string()
}

/// `reg add` 那一行（成功失败都打它，失败时就是给人复制的）。
fn reg_command(key: &str, manifest: &Path) -> String {
    // 这是给人粘进 cmd 的文本：cmd 自己按空格切参数，所以引号得写在文本里。
    format!(
        "reg add \"{key}\" /ve /t REG_SZ /d \"{}\" /f",
        manifest.display()
    )
}

fn reg_add(key: &str, manifest: &Path) -> Result<String, String> {
    // 不为一条注册表写操作引 FFI：reg.exe 够用，报错也容易解释。
    let value = reg_value(manifest);
    let out = Command::new("reg")
        .args(["add", key, "/ve", "/t", "REG_SZ", "/d", &value, "/f"])
        .output()
        .map_err(|e| format!("reg 起不来（{e}），手动跑：{}", reg_command(key, manifest)))?;
    if out.status.success() {
        Ok(format!("注册 {key}"))
    } else {
        // reg 的输出是控制台代码页（中文系统是 GBK），这里只丢掉，别把乱码摆给人看。
        Err(reg_command(key, manifest))
    }
}

fn relay_request(request: &[u8]) -> io::Result<Vec<u8>> {
    // Two chances: an existing app may be restarting, and the host may have
    // started it on the first try but connected before its pipe was ready.
    let mut last = None;
    for _ in 0..2 {
        let Some(mut sink) = connect_or_start() else {
            last = Some(io::Error::new(
                io::ErrorKind::NotConnected,
                "Volant is not running",
            ));
            continue;
        };
        if let Err(error) = write_frame(&mut sink, request) {
            last = Some(error);
            continue;
        }
        return read_frame(&mut sink);
    }
    Err(last.unwrap_or_else(|| io::Error::other("Volant did not answer")))
}

fn connect_or_start() -> Option<File> {
    if let Some(f) = try_connect(Duration::ZERO) {
        return Some(f);
    }
    // 界面进程没开：拉起来。用户点了下载就该看到窗口，比悄悄丢掉一条捕获好。
    let _ = start_app();
    try_connect(Duration::from_secs(15))
}

fn try_connect(patience: Duration) -> Option<File> {
    let deadline = Instant::now() + patience;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(PIPE)
        {
            Ok(f) => return Some(f),
            Err(_) if Instant::now() >= deadline => return None,
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn start_app() -> io::Result<()> {
    // 别用 current_exe：这里跑的是 volant-host.exe，再起一次就是死循环。
    // A GUI app must not inherit the native-messaging pipes. Chrome owns the
    // host's stdin/stdout, and a desktop process writing to that inherited
    // stderr can block its UI thread before the capture pipe is ready.
    Command::new(sibling("volant"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Open the native folder picker used by settings.
#[cfg(windows)]
pub fn pick_folder(title: &str) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoTaskMemFree,
    };
    use windows_sys::Win32::UI::Shell::{
        BIF_EDITBOX, BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW, SHBrowseForFolderW,
        SHGetPathFromIDListW,
    };

    let title: Vec<u16> = std::ffi::OsStr::new(title)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut shown = [0u16; 260];
    unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        );
        let browse = BROWSEINFOW {
            hwndOwner: std::ptr::null_mut(),
            pidlRoot: std::ptr::null_mut(),
            pszDisplayName: shown.as_mut_ptr(),
            lpszTitle: title.as_ptr(),
            ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE | BIF_EDITBOX,
            lpfn: None,
            lParam: 0,
            iImage: 0,
        };
        let pidl = SHBrowseForFolderW(&browse);
        if pidl.is_null() {
            return None;
        }
        let mut path = [0u16; 260];
        let ok = SHGetPathFromIDListW(pidl, path.as_mut_ptr());
        CoTaskMemFree(pidl as *const core::ffi::c_void);
        if ok == 0 {
            return None;
        }
        let len = path.iter().position(|&c| c == 0).unwrap_or(0);
        (len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&path[..len])))
    }
}

#[cfg(not(windows))]
pub fn pick_folder(_title: &str) -> Option<PathBuf> {
    None
}

/// Native torrent file picker used by the compact add dialog.
#[cfg(windows)]
pub fn pick_torrent_file(title: &str) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
    };
    use windows_sys::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };

    unsafe {
        let _ = CoInitializeEx(
            std::ptr::null_mut(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        );
    }
    let mut file = vec![0u16; 32_768];
    let filter: Vec<u16> = "种子文件 (*.torrent)\0*.torrent\0所有文件 (*.*)\0*.*\0\0"
        .encode_utf16()
        .collect();
    let title_wide: Vec<u16> = std::ffi::OsStr::new(title)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        lpstrFilter: filter.as_ptr(),
        lpstrFile: file.as_mut_ptr(),
        nMaxFile: file.len() as u32,
        lpstrTitle: title_wide.as_ptr(),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_HIDEREADONLY,
        ..unsafe { std::mem::zeroed() }
    };
    if unsafe { GetOpenFileNameW(&mut dialog) } == 0 {
        return None;
    }
    let length = file.iter().position(|ch| *ch == 0).unwrap_or(file.len());
    Some(PathBuf::from(OsString::from_wide(&file[..length])))
}

#[cfg(not(windows))]
pub fn pick_torrent_file(_title: &str) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chromium_manifest_points_at_us_and_the_fixed_extension() {
        let m = chromium_manifest(r"C:\Apps\Volant\volant.exe");
        assert_eq!(m["name"], HOST);
        assert_eq!(m["type"], "stdio");
        assert_eq!(m["path"], r"C:\Apps\Volant\volant.exe");
        assert_eq!(
            m["allowed_origins"][0],
            format!("chrome-extension://{CHROME_ID}/")
        );
        // 扩展 id 只有 32 个 a–p，抄错一个字符浏览器就不认。这个串的来历见 CHROME_ID
        // 的文档注释；改 manifest 的 key 就必须重算这一行。
        assert_eq!(CHROME_ID, "lehgpnldgfjhkebnbblialhkkhcgaohm");
        assert!(CHROME_ID.chars().all(|c| ('a'..='p').contains(&c)));
    }

    #[test]
    fn firefox_manifest_uses_the_gecko_id() {
        let m = firefox_manifest("x");
        assert_eq!(m["allowed_extensions"][0], FIREFOX_ID);
        assert!(m.get("allowed_origins").is_none());
    }

    #[test]
    fn packaged_extension_matches_the_native_host_identity_and_version() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extension/manifest.json");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(
            manifest["browser_specific_settings"]["gecko"]["id"],
            FIREFOX_ID
        );
    }

    #[test]
    fn registry_value_is_bare_but_the_manual_command_is_quoted() {
        // 带引号的值 reg.exe 会原样存下来，Chrome 就报
        // 「Specified native messaging host not found」。
        let v = reg_value(Path::new(r"C:\Apps\My App\com.volant.host.json"));
        assert!(!v.contains('"'), "{v}");
        assert_eq!(v, r"C:\Apps\My App\com.volant.host.json");
        // 给人粘到 cmd 里的那条要带引号：cmd 按空格切参数。
        let cmd = reg_command(
            r"HKCU\Software\Google\Chrome\NativeMessagingHosts\com.volant.host",
            Path::new(r"C:\Apps\My App\com.volant.host.json"),
        );
        assert!(
            cmd.contains(r#"/d "C:\Apps\My App\com.volant.host.json""#),
            "{cmd}"
        );
    }

    #[test]
    fn popup_status_is_answered_without_starting_the_app() {
        let reply = local_reply(br#"{"type":"rules","req":42}"#).unwrap();
        let reply: serde_json::Value = serde_json::from_slice(&reply).unwrap();
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["req"], 42);
        assert_eq!(reply["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(
            local_reply(br#"{"type":"download","url":"https://example.com/a"}"#),
            None
        );
    }
}
