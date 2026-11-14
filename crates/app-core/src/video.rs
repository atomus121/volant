//! Video lookup and download through yt-dlp.
//!
//! The browser never receives a page's media bytes. It sends the page, optional
//! stream manifest, cookies and request headers to the desktop process; yt-dlp
//! resolves the real formats and ffmpeg merges or converts them when needed.
//! This follows MDM's mechanism: format discovery and post-processing stay in
//! the video helper, while the queue owns lifecycle, progress and cancellation.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::capture::BrowserCookie;
use crate::fmt;

const YTDLP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";
const NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VideoContext {
    /// The video's own page where one exists, else the page that embeds it.
    pub page: String,
    /// A plain media URL or an HLS/DASH manifest the player loaded.
    pub src: String,
    pub title: String,
    pub referrer: String,
    pub user_agent: String,
    /// Browser cookies for both the page and the media host.
    pub cookies: Vec<BrowserCookie>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoChoice {
    pub index: usize,
    pub label: String,
    pub ext: String,
    pub size: Option<u64>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub audio: bool,
    /// Private selector used by the desktop process, never trusted from a page.
    #[serde(skip)]
    pub format_selector: String,
    #[serde(skip)]
    pub audio_format: Option<String>,
    #[serde(skip)]
    pub audio_quality: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoLookup {
    pub state: String,
    pub token: String,
    pub page: String,
    pub title: String,
    pub duration: Option<f64>,
    pub choices: Vec<VideoChoice>,
    #[serde(skip, default)]
    pub context: VideoContext,
}

fn ready_lookup(mut lookup: VideoLookup) -> VideoLookup {
    lookup.state = "ready".into();
    lookup
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoTask {
    pub context: VideoContext,
    pub choice: VideoChoice,
    pub title: String,
    #[serde(default)]
    pub subtitles: bool,
}

#[derive(Clone, Debug)]
pub struct VideoProgress {
    pub downloaded: u64,
    pub total: u64,
}

pub fn helper_dir() -> PathBuf {
    crate::model::Paths::resolve().home.join("video")
}

pub fn ensure_helper_dir() -> Result<PathBuf, String> {
    let dir = helper_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("无法建立视频组件目录 {}：{error}", dir.display()))?;
    Ok(dir)
}

/// A deterministic, short token for a lookup. The lookup itself stays in the
/// desktop process; the browser only receives this opaque handle.
pub fn lookup_token(request_id: u64) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("v{request_id:x}{nanos:x}")
}

/// Resolve a video page or media manifest into downloadable qualities.
pub async fn lookup(request_id: u64, context: &VideoContext) -> Result<VideoLookup, String> {
    let target = if !context.src.is_empty() && is_web_url(&context.src) {
        &context.src
    } else {
        &context.page
    };
    if !is_web_url(target) {
        return Err("没有找到可解析的视频页面".into());
    }
    let exe = ytdlp_path().await?;
    let cookie_jar = CookieJar::write(&context.cookies)?;
    let mut cmd = command_for(&exe);
    add_network_args(&mut cmd, context, cookie_jar.path());
    cmd.args([
        "--dump-single-json",
        "--no-playlist",
        "--skip-download",
        "--no-warnings",
        "--no-progress",
        "--ignore-config",
        target,
    ]);
    let output = cmd
        .output()
        .await
        .map_err(|error| format!("无法启动 yt-dlp：{error}"))?;
    if !output.status.success() {
        return Err(helper_error(&output.stderr));
    }
    let info: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("yt-dlp 返回的视频信息无法读取：{error}"))?;
    if info.get("_type").and_then(|value| value.as_str()) == Some("playlist") {
        return Err("这是一个播放列表。请打开单个视频后再下载。".into());
    }
    if info
        .get("has_drm")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        return Err("受 DRM 保护的视频无法下载".into());
    }
    let ytdlp_title = text(&info, "title").unwrap_or_default();
    let title = if ytdlp_title.trim().is_empty() {
        if context.title.trim().is_empty() {
            "视频".to_string()
        } else {
            context.title.clone()
        }
    } else {
        ytdlp_title
    };
    let page = text(&info, "webpage_url")
        .filter(|value| is_web_url(value))
        .unwrap_or_else(|| context.page.clone());
    let duration = number(&info, "duration").map(|value| value as f64);
    let ffmpeg = ffmpeg_path().await;
    let choices = qualities(&info, ffmpeg.is_some());
    if choices.is_empty() {
        return Err("这个页面没有提供可下载的格式".into());
    }
    Ok(ready_lookup(VideoLookup {
        state: String::new(),
        token: lookup_token(request_id),
        page,
        title,
        duration,
        choices,
        context: context.clone(),
    }))
}

fn qualities(info: &serde_json::Value, ffmpeg: bool) -> Vec<VideoChoice> {
    let formats = info
        .get("formats")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    out.push(VideoChoice {
        index: 0,
        label: "最佳画质".into(),
        ext: "自动".into(),
        size: None,
        height: None,
        fps: None,
        audio: false,
        format_selector: "bestvideo*+bestaudio/best".into(),
        audio_format: None,
        audio_quality: None,
    });

    let mut seen = HashSet::new();
    for format in formats.iter().filter(|format| {
        !format
            .get("has_drm")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }) {
        let id = text(format, "format_id").unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let vcodec = text(format, "vcodec").unwrap_or_else(|| "none".into());
        let acodec = text(format, "acodec").unwrap_or_else(|| "none".into());
        if vcodec == "none" {
            continue;
        }
        let height = number(format, "height").map(|value| value as u32);
        let fps = number(format, "fps").map(|value| value as f64);
        let ext = text(format, "ext")
            .unwrap_or_else(|| "mp4".into())
            .to_ascii_uppercase();
        let key = (height, ext.clone());
        if !seen.insert(key) {
            continue;
        }
        let selector = if acodec == "none" {
            format!("{id}+bestaudio/best")
        } else {
            id
        };
        let label = match height {
            Some(2160) => format!("4K · {ext}"),
            Some(1440) => format!("2K · {ext}"),
            Some(value) => format!("{value}p · {ext}"),
            None => format!("视频 · {ext}"),
        };
        out.push(VideoChoice {
            index: out.len(),
            label,
            ext,
            size: number(format, "filesize").or_else(|| number(format, "filesize_approx")),
            height,
            fps,
            audio: false,
            format_selector: selector,
            audio_format: None,
            audio_quality: None,
        });
        if out.len() >= 13 {
            break;
        }
    }

    let audio_ext = formats
        .iter()
        .find(|format| {
            text(format, "acodec").is_some_and(|codec| codec != "none")
                && text(format, "vcodec").is_none_or(|codec| codec == "none")
        })
        .and_then(|format| text(format, "ext"))
        .unwrap_or_else(|| "m4a".into())
        .to_ascii_uppercase();
    out.push(VideoChoice {
        index: out.len(),
        label: format!("仅音频 · {audio_ext}"),
        ext: audio_ext,
        size: None,
        height: None,
        fps: None,
        audio: true,
        format_selector: "bestaudio/best".into(),
        audio_format: None,
        audio_quality: None,
    });
    if ffmpeg {
        for quality in ["320", "256", "192", "128"] {
            out.push(VideoChoice {
                index: out.len(),
                label: format!("仅音频 · MP3 {quality} kbps"),
                ext: "MP3".into(),
                size: None,
                height: None,
                fps: None,
                audio: true,
                format_selector: "bestaudio/best".into(),
                audio_format: Some("mp3".into()),
                audio_quality: Some(quality.into()),
            });
        }
    }
    out
}

/// Download a resolved choice. `on_progress` is called on the controller task;
/// cancellation kills yt-dlp and ffmpeg together through yt-dlp's process tree.
pub async fn download(
    task: &VideoTask,
    destination: &Path,
    cancel: Arc<AtomicBool>,
    mut on_progress: impl FnMut(VideoProgress) + Send,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(destination)
        .map_err(|error| format!("无法建立下载目录 {}：{error}", destination.display()))?;
    let exe = ytdlp_path().await?;
    let ffmpeg = ffmpeg_path().await;
    let cookie_jar = CookieJar::write(&task.context.cookies)?;
    let stem = safe_stem(&task.title);
    let template = destination.join(format!("{stem}.%(ext)s"));
    let mut cmd = command_for(&exe);
    add_network_args(&mut cmd, &task.context, cookie_jar.path());
    cmd.args([
        "--no-playlist",
        "--no-warnings",
        "--ignore-config",
        "--newline",
        "--continue",
        "--no-overwrites",
        "--print",
        "after_move:VOLANT_FILE %(filepath)s",
        "--progress-template",
        "download:VOLANT_PROGRESS %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s %(progress.speed)s",
        "-f",
        &task.choice.format_selector,
        "-o",
    ]);
    cmd.arg(&template);
    if let Some(ffmpeg) = &ffmpeg {
        if let Some(parent) = ffmpeg.parent() {
            cmd.arg("--ffmpeg-location").arg(parent);
        }
    }
    if task.choice.audio {
        if let Some(format) = &task.choice.audio_format {
            cmd.args(["-x", "--audio-format", format]);
            if let Some(quality) = &task.choice.audio_quality {
                cmd.args(["--audio-quality", quality]);
            }
        }
    } else if ffmpeg.is_some() {
        cmd.args(["--merge-output-format", "mp4"]);
    }
    if task.subtitles && ffmpeg.is_some() {
        cmd.args([
            "--write-subs",
            "--write-auto-subs",
            "--sub-langs",
            "zh.*,zh-Hans,zh-CN,en.*",
            "--convert-subs",
            "srt",
            "--ignore-errors",
        ]);
    }
    cmd.arg(if task.context.src.is_empty() {
        &task.context.page
    } else {
        &task.context.src
    });
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|error| format!("无法启动 yt-dlp：{error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法读取 yt-dlp 输出".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "无法读取 yt-dlp 错误输出".to_string())?;
    let mut out_lines = BufReader::new(stdout).lines();
    let mut err_lines = BufReader::new(stderr).lines();
    let mut final_path = None;
    let mut errors = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_millis(120));
    let mut out_open = true;
    let mut err_open = true;
    while out_open || err_open {
        tokio::select! {
            _ = tick.tick() => {
                if cancel.load(Ordering::Relaxed) {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    return Err("已暂停".into());
                }
            }
            line = out_lines.next_line(), if out_open => {
                match line {
                    Ok(Some(line)) => {
                        if let Some(path) = line.strip_prefix("VOLANT_FILE ") {
                            final_path = Some(PathBuf::from(path.trim()));
                        } else if let Some(progress) = parse_progress(&line) {
                            on_progress(progress);
                        }
                    }
                    Ok(None) => out_open = false,
                    Err(error) => {
                        errors.push(error.to_string());
                        out_open = false;
                    }
                }
            }
            line = err_lines.next_line(), if err_open => {
                match line {
                    Ok(Some(line)) => {
                        errors.push(line);
                        if errors.len() > 40 {
                            errors.remove(0);
                        }
                    }
                    Ok(None) => err_open = false,
                    Err(error) => {
                        errors.push(error.to_string());
                        err_open = false;
                    }
                }
            }
        }
    }
    let status = child
        .wait()
        .await
        .map_err(|error| format!("等待 yt-dlp 结束失败：{error}"))?;
    if cancel.load(Ordering::Relaxed) {
        return Err("已暂停".into());
    }
    if !status.success() {
        let detail = errors
            .iter()
            .rev()
            .find(|line| !line.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| "yt-dlp 没有完成下载".into());
        return Err(detail);
    }
    if let Some(path) = final_path.filter(|path| path.is_file()) {
        return Ok(path);
    }
    find_output(destination, &stem).ok_or_else(|| "yt-dlp 已完成，但没有找到保存的视频文件".into())
}

fn parse_progress(line: &str) -> Option<VideoProgress> {
    let rest = line.strip_prefix("VOLANT_PROGRESS ")?;
    let mut values = rest.split_whitespace();
    let downloaded = parse_metric(values.next()?)?;
    let total = parse_metric(values.next()?)
        .or_else(|| parse_metric(values.next().unwrap_or("")))
        .unwrap_or(0.0);
    Some(VideoProgress {
        downloaded: downloaded as u64,
        total: total as u64,
    })
}

fn parse_metric(value: &str) -> Option<f64> {
    if value == "NA" || value == "N/A" || value.is_empty() {
        None
    } else {
        value.parse::<f64>().ok().filter(|value| *value >= 0.0)
    }
}

fn find_output(dir: &Path, stem: &str) -> Option<PathBuf> {
    let mut found = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            let media = matches!(
                path.extension()?.to_str()?.to_ascii_lowercase().as_str(),
                "mp4" | "mkv" | "webm" | "mov" | "m4a" | "mp3" | "opus" | "flac"
            );
            (media && name.starts_with(stem))
                .then(|| {
                    std::fs::metadata(&path)
                        .ok()
                        .map(|meta| (meta.modified().ok(), path))
                })
                .flatten()
        })
        .collect::<Vec<_>>();
    found.sort_by_key(|(modified, _)| *modified);
    found.pop().map(|(_, path)| path)
}

fn add_network_args(cmd: &mut Command, context: &VideoContext, cookie_jar: &Path) {
    cmd.arg("--cookies").arg(cookie_jar);
    if !context.user_agent.trim().is_empty() {
        cmd.args(["--user-agent", context.user_agent.trim()]);
    }
    if !context.referrer.trim().is_empty() {
        cmd.args([
            "--add-header",
            &format!("Referer: {}", context.referrer.trim()),
        ]);
    }
}

async fn ytdlp_path() -> Result<PathBuf, String> {
    if let Some(explicit) = std::env::var_os("VOLANT_YTDLP")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        return Ok(explicit);
    }
    let managed = helper_dir().join(if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    });
    if managed.is_file() {
        return Ok(managed);
    }
    if Command::new("yt-dlp")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
    {
        return Ok(PathBuf::from("yt-dlp"));
    }
    if cfg!(windows) {
        install_ytdlp(&managed).await.map_err(|error| {
            format!(
                "视频解析组件不可用：{error}。也可以把 yt-dlp.exe 放到 {}。",
                managed.display()
            )
        })?;
        Ok(managed)
    } else {
        Err(format!(
            "未找到 yt-dlp。请安装 yt-dlp，或把程序放到 {}。",
            managed.display()
        ))
    }
}

async fn install_ytdlp(destination: &Path) -> Result<(), String> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("无法建立 {}：{error}", parent.display()))?;
    }
    let response = reqwest::get(YTDLP_URL)
        .await
        .map_err(|error| format!("下载 yt-dlp 失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("下载 yt-dlp 返回 HTTP {}", response.status()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("读取 yt-dlp 失败：{error}"))?;
    if bytes.len() < 1_000_000 || !bytes.starts_with(b"MZ") {
        return Err("下载到的 yt-dlp 文件无效".into());
    }
    let temp = destination.with_extension("exe.tmp");
    std::fs::write(&temp, &bytes).map_err(|error| format!("保存 yt-dlp 失败：{error}"))?;
    std::fs::rename(&temp, destination).map_err(|error| format!("启用 yt-dlp 失败：{error}"))
}

async fn ffmpeg_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("VOLANT_FFMPEG").map(PathBuf::from)
        && explicit.is_file()
    {
        return Some(explicit);
    }
    let managed = helper_dir().join(if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    });
    if managed.is_file() {
        return Some(managed);
    }
    let status = Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .ok()?;
    status.success().then(|| PathBuf::from("ffmpeg"))
}

fn command_for(exe: &Path) -> Command {
    let mut command = Command::new(exe);
    hide_console(&mut command);
    command
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.as_std_mut().creation_flags(NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

fn helper_error(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("yt-dlp 无法解析这个视频");
    let lower = line.to_ascii_lowercase();
    if lower.contains("unsupported url") {
        "这个页面没有可下载的视频".into()
    } else if lower.contains("drm") {
        "受 DRM 保护的视频无法下载".into()
    } else {
        line.trim().to_string()
    }
}

pub fn safe_stem(title: &str) -> String {
    let mut out = String::with_capacity(title.len().min(120));
    for ch in title.chars() {
        let ch = if ch.is_control() || r#"<>:"/\|?*"#.contains(ch) {
            '_'
        } else {
            ch
        };
        out.push(ch);
        if out.chars().count() >= 120 {
            break;
        }
    }
    let out = out.trim().trim_end_matches('.').trim();
    let out = if out.is_empty() { "视频" } else { out };
    let upper = out.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        format!("{out}_")
    } else {
        out.to_string()
    }
}

pub fn expected_path(task: &VideoTask, destination: &Path) -> PathBuf {
    let ext = if task.choice.ext.eq_ignore_ascii_case("自动") {
        "mp4"
    } else {
        &task.choice.ext
    };
    destination.join(format!(
        "{}.{}",
        safe_stem(&task.title),
        ext.to_ascii_lowercase()
    ))
}

pub fn size_label(size: Option<u64>) -> String {
    size.map(fmt::bytes).unwrap_or_else(|| "大小未知".into())
}

fn is_web_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn text(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn number(value: &serde_json::Value, key: &str) -> Option<u64> {
    let value = value.get(key)?;
    value
        .as_u64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|value| *value >= 0.0)
                .map(|value| value as u64)
        })
        .filter(|value| *value > 0)
}

struct CookieJar {
    path: PathBuf,
}

impl CookieJar {
    fn write(cookies: &[BrowserCookie]) -> Result<CookieJar, String> {
        let dir = ensure_helper_dir()?.join("tmp");
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("无法建立临时目录 {}：{error}", dir.display()))?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let path = dir.join(format!("cookies-{}-{stamp}.txt", std::process::id()));
        let mut lines = String::from("# Netscape HTTP Cookie File\n");
        let mut seen = BTreeMap::new();
        for cookie in cookies {
            let key = (
                cookie.domain.clone(),
                cookie.path.clone(),
                cookie.name.clone(),
            );
            if cookie.name.is_empty() || seen.insert(key, ()).is_some() {
                continue;
            }
            let domain = if cookie.domain.starts_with('.') {
                cookie.domain.clone()
            } else {
                format!(".{}", cookie.domain)
            };
            lines.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                domain,
                if domain.starts_with('.') {
                    "TRUE"
                } else {
                    "FALSE"
                },
                if cookie.path.is_empty() {
                    "/"
                } else {
                    &cookie.path
                },
                if cookie.secure { "TRUE" } else { "FALSE" },
                cookie.expiration_date.unwrap_or(0.0).max(0.0) as u64,
                cookie.name,
                cookie.value
            ));
        }
        std::fs::write(&path, lines).map_err(|error| format!("写临时 Cookie 文件失败：{error}"))?;
        Ok(CookieJar { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for CookieJar {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_names_remove_windows_reserved_characters() {
        assert_eq!(safe_stem(r#"A/B:C*D?"#), "A_B_C_D_");
        assert_eq!(safe_stem("CON"), "CON_");
        assert!(safe_stem(&"文".repeat(400)).chars().count() <= 120);
    }

    #[test]
    fn progress_accepts_exact_and_estimated_totals() {
        let exact = parse_progress("VOLANT_PROGRESS 25 100 NA 1024").unwrap();
        assert_eq!((exact.downloaded, exact.total), (25, 100));
        let estimated = parse_progress("VOLANT_PROGRESS 25 NA 200 1024").unwrap();
        assert_eq!((estimated.downloaded, estimated.total), (25, 200));
    }

    #[test]
    fn qualities_keep_the_best_choice_and_audio() {
        let info = serde_json::json!({
            "formats": [
                {"format_id":"137","height":1080,"fps":60,"ext":"mp4","vcodec":"avc1","acodec":"none","filesize":100},
                {"format_id":"18","height":360,"ext":"mp4","vcodec":"avc1","acodec":"mp4a","filesize":30},
                {"format_id":"140","ext":"m4a","vcodec":"none","acodec":"mp4a"}
            ]
        });
        let list = qualities(&info, false);
        assert_eq!(list[0].label, "最佳画质");
        assert_eq!(list[1].format_selector, "137+bestaudio/best");
        assert!(!list[1].audio);
        assert!(
            list.iter()
                .any(|choice| choice.audio && choice.ext == "M4A")
        );
    }

    #[test]
    fn output_path_uses_the_resolved_extension() {
        let task = VideoTask {
            context: VideoContext::default(),
            choice: VideoChoice {
                index: 1,
                label: "1080p · WEBM".into(),
                ext: "WEBM".into(),
                size: None,
                height: Some(1080),
                fps: None,
                audio: false,
                format_selector: "x".into(),
                audio_format: None,
                audio_quality: None,
            },
            title: "A/B".into(),
            subtitles: false,
        };
        assert_eq!(
            expected_path(&task, Path::new(r"D:\Downloads")),
            PathBuf::from(r"D:\Downloads\A_B.webm")
        );
    }
}
