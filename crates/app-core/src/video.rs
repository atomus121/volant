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
const FFMPEG_RELEASE_API: &str =
    "https://api.github.com/repos/FFmpeg-Builds/FFmpeg-Builds/releases/latest";
const NO_WINDOW: u32 = 0x0800_0000;
const INSPECTION_TIMEOUT: Duration = Duration::from_secs(55);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VideoContext {
    /// The video's own page where one exists, else the page that embeds it.
    pub page: String,
    /// A plain media URL or an HLS/DASH manifest the player loaded.
    pub src: String,
    pub title: String,
    pub referrer: String,
    /// The Origin the browser's player sent with the media request, when known.
    pub origin: String,
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
    /// The size is an estimate from the site or from bitrate.
    #[serde(default)]
    pub approx: bool,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub range: Option<String>,
    #[serde(default)]
    pub kbps: Option<u32>,
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
    pub connections: usize,
}

/// One external program used by the video pipeline.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentStatus {
    pub ready: bool,
    pub path: Option<String>,
    pub managed: bool,
}

/// Everything the browser and settings screen need to explain whether video
/// downloads can parse a page, merge audio/video, and handle JavaScript-heavy
/// sites. Detection is deliberately side-effect free.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoDependencyStatus {
    pub ytdlp: ComponentStatus,
    pub ffmpeg: ComponentStatus,
    pub js_runtime: ComponentStatus,
    pub helper_dir: String,
    pub ready: bool,
    pub can_install: bool,
}

struct SelectedVideo {
    json: serde_json::Value,
    path: PathBuf,
}

impl Drop for SelectedVideo {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

struct HttpStream {
    id: String,
    path: PathBuf,
    url: volant_engine::Url,
    headers: Vec<(String, String)>,
    size: Option<u64>,
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

fn executable_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn component(explicit: Option<PathBuf>, managed: PathBuf, path_name: &str) -> ComponentStatus {
    if let Some(path) = explicit.filter(|path| path.is_file()) {
        return ComponentStatus {
            ready: true,
            path: Some(path.display().to_string()),
            managed: false,
        };
    }
    if managed.is_file() {
        return ComponentStatus {
            ready: true,
            path: Some(managed.display().to_string()),
            managed: true,
        };
    }
    match executable_in_path(path_name) {
        Some(path) => ComponentStatus {
            ready: true,
            path: Some(path.display().to_string()),
            managed: false,
        },
        None => ComponentStatus::default(),
    }
}

fn js_runtime() -> Option<ComponentStatus> {
    for name in ["deno.exe", "node.exe", "quickjs.exe", "bun.exe"] {
        if let Some(path) = executable_in_path(name) {
            return Some(ComponentStatus {
                ready: true,
                path: Some(path.display().to_string()),
                managed: false,
            });
        }
    }
    None
}

/// Check yt-dlp, ffmpeg and a JavaScript runtime without launching any of them.
/// This is safe to call from the lightweight native-messaging status request.
pub fn dependency_status() -> VideoDependencyStatus {
    let dir = helper_dir();
    let ytdlp = component(
        std::env::var_os("VOLANT_YTDLP").map(PathBuf::from),
        dir.join(if cfg!(windows) {
            "yt-dlp.exe"
        } else {
            "yt-dlp"
        }),
        if cfg!(windows) {
            "yt-dlp.exe"
        } else {
            "yt-dlp"
        },
    );
    let managed_ffmpeg = dir.join("ffmpeg").join(if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    });
    let legacy_ffmpeg = dir.join(if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    });
    let ffmpeg = component(
        std::env::var_os("VOLANT_FFMPEG").map(PathBuf::from),
        if managed_ffmpeg.is_file() {
            managed_ffmpeg
        } else {
            legacy_ffmpeg
        },
        if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        },
    );
    let js_runtime = js_runtime().unwrap_or_default();
    let ready = ytdlp.ready && ffmpeg.ready;
    VideoDependencyStatus {
        ytdlp,
        ffmpeg,
        js_runtime,
        helper_dir: dir.display().to_string(),
        ready,
        can_install: cfg!(windows),
    }
}

/// Install whichever components are missing. Existing system installations are
/// reused; Volant-owned copies live under its application data directory.
pub async fn install_dependencies() -> Result<VideoDependencyStatus, String> {
    let current = dependency_status();
    if !current.ytdlp.ready {
        install_ytdlp(&managed_ytdlp_path()).await?;
    }
    if !current.ffmpeg.ready {
        install_ffmpeg().await?;
    }
    Ok(dependency_status())
}

/// Blocking entry point for installer/headless maintenance commands.
pub fn install_dependencies_blocking() -> Result<VideoDependencyStatus, String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("无法启动视频组件安装任务：{error}"))?;
    runtime.block_on(install_dependencies())
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
pub async fn lookup(token: String, context: &VideoContext) -> Result<VideoLookup, String> {
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
    let output = inspect_output(cmd, "视频信息").await?;
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
        token,
        page,
        title,
        duration,
        choices,
        context: context.clone(),
    }))
}

#[derive(Clone)]
struct Format {
    id: String,
    ext: String,
    height: Option<u32>,
    fps: Option<u32>,
    video: bool,
    audio: bool,
    size: Option<u64>,
    approx: bool,
    tbr: Option<f64>,
    hls: bool,
    vcodec: String,
    acodec: String,
    range: Option<String>,
    language_preference: Option<f64>,
    quality: Option<f64>,
    channels: Option<f64>,
    preference: Option<f64>,
}

fn formats(info: &serde_json::Value) -> Vec<Format> {
    info.get("formats")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|format| format.get("has_drm").and_then(serde_json::Value::as_bool) != Some(true))
        .filter_map(|format| {
            let id = text(format, "format_id")?;
            let vcodec = text(format, "vcodec").unwrap_or_else(|| "none".into());
            let acodec = text(format, "acodec").unwrap_or_else(|| "none".into());
            let protocol = text(format, "protocol").unwrap_or_default();
            Some(Format {
                id,
                ext: text(format, "ext").unwrap_or_else(|| "mp4".into()),
                height: number(format, "height").map(|value| value as u32),
                fps: number(format, "fps")
                    .map(|value| value as u32)
                    .filter(|value| *value > 0),
                video: vcodec != "none",
                audio: acodec != "none",
                size: number(format, "filesize").or_else(|| number(format, "filesize_approx")),
                approx: number(format, "filesize").is_none(),
                tbr: decimal(format, "tbr"),
                hls: protocol.contains("m3u8"),
                vcodec,
                acodec,
                range: text(format, "dynamic_range")
                    .filter(|range| !range.eq_ignore_ascii_case("SDR")),
                language_preference: decimal(format, "language_preference"),
                quality: decimal(format, "quality"),
                channels: decimal(format, "audio_channels"),
                preference: decimal(format, "preference"),
            })
        })
        .collect()
}

fn more(a: Option<f64>, b: Option<f64>, default: f64) -> std::cmp::Ordering {
    a.unwrap_or(default).total_cmp(&b.unwrap_or(default))
}

fn picture_rank(codec: &str) -> u8 {
    let codec = codec.to_ascii_lowercase();
    if codec == "none" {
        0
    } else if codec.starts_with("av01") || codec.starts_with("av1") {
        10
    } else if codec.starts_with("vp9.2") || codec.starts_with("vp09.02") {
        9
    } else if codec.starts_with("vp9") || codec.starts_with("vp09") {
        8
    } else if codec.starts_with("h265") || codec.starts_with("hev") || codec.starts_with("hv") {
        7
    } else if codec.starts_with("h264") || codec.starts_with("avc") {
        6
    } else if codec.starts_with("vp8") {
        5
    } else {
        2
    }
}

fn sound_rank(codec: &str) -> u8 {
    let codec = codec.to_ascii_lowercase();
    if codec == "none" {
        0
    } else if codec.starts_with("flac") || codec.starts_with("alac") {
        13
    } else if codec.starts_with("opus") {
        11
    } else if codec.starts_with("vorbis") || codec.starts_with("ogg") {
        10
    } else if codec.starts_with("aac") || codec.starts_with("m4") {
        9
    } else if codec.starts_with("mp3") {
        7
    } else {
        2
    }
}

fn codec_name(codec: &str) -> Option<&'static str> {
    let codec = codec.to_ascii_lowercase();
    if codec.starts_with("dvh") {
        Some("H.265")
    } else if codec.starts_with("dav1") || codec.starts_with("av01") || codec.starts_with("av1") {
        Some("AV1")
    } else if codec.starts_with("dva") {
        Some("H.264")
    } else {
        match picture_rank(&codec) {
            10 => Some("AV1"),
            8 | 9 => Some("VP9"),
            7 => Some("H.265"),
            6 => Some("H.264"),
            5 => Some("VP8"),
            _ => None,
        }
    }
}

fn range_rank(range: Option<&str>) -> u8 {
    match range.map(str::to_ascii_uppercase).as_deref() {
        Some("HDR12") => 6,
        Some("HDR10+") => 5,
        Some("HDR10") => 4,
        Some("HLG") => 3,
        Some("DV") => 2,
        _ => 1,
    }
}

fn range_name(range: Option<&str>) -> Option<&'static str> {
    match range.map(str::to_ascii_uppercase).as_deref() {
        None | Some("SDR" | "") => None,
        Some("DV") => Some("Dolby Vision"),
        Some(_) => Some("HDR"),
    }
}

fn plain_id(id: &str) -> bool {
    const WORDS: [&str; 27] = [
        "b",
        "w",
        "best",
        "worst",
        "bv",
        "ba",
        "bestvideo",
        "bestaudio",
        "worstvideo",
        "worstaudio",
        "all",
        "mergeall",
        "3gp",
        "aac",
        "flv",
        "m4a",
        "mp3",
        "mp4",
        "ogg",
        "wav",
        "webm",
        "mov",
        "mkv",
        "opus",
        "flac",
        "m4v",
        "mhtml",
    ];
    !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        && !WORDS.contains(&id.to_ascii_lowercase().as_str())
}

fn sounds(formats: &[Format], can_merge: bool) -> Vec<&Format> {
    let mut out: Vec<&Format> = formats
        .iter()
        .filter(|format| format.audio && !format.video)
        .collect();
    out.sort_by(|a, b| {
        (can_merge || !b.hls)
            .cmp(&(can_merge || !a.hls))
            .then(more(b.preference, a.preference, 0.0))
            .then(more(b.language_preference, a.language_preference, -1.0))
            .then(more(b.quality, a.quality, -1.0))
            .then(more(b.channels, a.channels, 0.0))
            .then(sound_rank(&b.acodec).cmp(&sound_rank(&a.acodec)))
            .then(b.size.unwrap_or(0).cmp(&a.size.unwrap_or(0)))
            .then(more(b.tbr, a.tbr, 0.0))
            .then(a.id.cmp(&b.id))
    });
    out
}

fn video_label(
    height: u32,
    ext: &str,
    codec: Option<&str>,
    fps: Option<u32>,
    range: Option<&str>,
) -> String {
    let mut parts = vec![format!("{height}p")];
    if !ext.eq_ignore_ascii_case("mp4") || codec.is_none() {
        parts.push(ext.to_ascii_uppercase());
    }
    parts.extend(codec.map(str::to_owned));
    parts.extend(fps.map(|fps| format!("{fps} fps")));
    parts.extend(range.map(str::to_owned));
    parts.join(" · ")
}

fn video_choices(
    formats: &[Format],
    can_merge: bool,
    best_sound: Option<&Format>,
) -> Vec<VideoChoice> {
    let candidates = formats
        .iter()
        .filter(|format| format.video && format.height.is_some() && (can_merge || format.audio));
    let better = |a: &Format, b: &Format| {
        (can_merge && !a.audio)
            .cmp(&(can_merge && !b.audio))
            .then(more(a.preference, b.preference, 0.0))
            .then((!a.hls).cmp(&!b.hls))
            .then((a.size.is_some() && !a.approx).cmp(&(b.size.is_some() && !b.approx)))
            .then(range_rank(a.range.as_deref()).cmp(&range_rank(b.range.as_deref())))
            .then(picture_rank(&a.vcodec).cmp(&picture_rank(&b.vcodec)))
            .then(a.size.unwrap_or(0).cmp(&b.size.unwrap_or(0)))
            .then(more(a.tbr, b.tbr, 0.0))
            .then(a.id.cmp(&b.id))
    };

    let mut lines: Vec<&Format> = Vec::new();
    for format in candidates {
        let fps = format.fps;
        let codec = codec_name(&format.vcodec);
        let range = range_name(format.range.as_deref());
        let existing = lines.iter().position(|line| {
            line.height == format.height
                && line.fps == fps
                && codec_name(&line.vcodec) == codec
                && range_name(line.range.as_deref()) == range
                && (can_merge || line.ext.eq_ignore_ascii_case(&format.ext))
        });
        match existing {
            Some(index) if better(format, lines[index]).is_gt() => lines[index] = format,
            Some(_) => {}
            None => lines.push(format),
        }
    }
    lines.sort_by(|a, b| {
        b.height
            .cmp(&a.height)
            .then(b.fps.cmp(&a.fps))
            .then(range_rank(b.range.as_deref()).cmp(&range_rank(a.range.as_deref())))
            .then(picture_rank(&b.vcodec).cmp(&picture_rank(&a.vcodec)))
            .then(b.size.unwrap_or(0).cmp(&a.size.unwrap_or(0)))
            .then(a.id.cmp(&b.id))
    });

    let hdr_heights: HashSet<u32> = lines
        .iter()
        .filter(|format| range_name(format.range.as_deref()).is_some())
        .filter_map(|format| format.height)
        .collect();
    lines
        .into_iter()
        .map(|format| {
            let height = format.height.unwrap_or(0);
            let range = range_name(format.range.as_deref())
                .or_else(|| hdr_heights.contains(&height).then_some("SDR"));
            let sound = best_sound.filter(|_| can_merge && !format.audio);
            let exact = match sound {
                Some(sound) => (plain_id(&format.id) && plain_id(&sound.id))
                    .then(|| format!("{}+{}", format.id, sound.id)),
                None => plain_id(&format.id).then(|| format.id.clone()),
            };
            let near = if can_merge {
                format!("bv*[height={height}]+ba/b[height={height}]")
            } else {
                format!("b[height={height}]")
            };
            let ext = if can_merge && !format.audio {
                "mp4".to_owned()
            } else {
                format.ext.clone()
            };
            let size = format
                .size
                .map(|size| size + sound.and_then(|sound| sound.size).unwrap_or(0));
            VideoChoice {
                index: 0,
                label: video_label(height, &ext, codec_name(&format.vcodec), format.fps, range),
                ext,
                size,
                approx: format.approx
                    || exact.is_none()
                    || sound.is_some_and(|sound| sound.approx || sound.size.is_none()),
                height: Some(height),
                fps: format.fps.map(f64::from),
                codec: codec_name(&format.vcodec).map(str::to_owned),
                range: range.map(str::to_owned),
                kbps: None,
                audio: false,
                format_selector: exact
                    .map_or_else(|| near.clone(), |exact| format!("{exact}/{near}")),
                audio_format: None,
                audio_quality: None,
            }
        })
        .collect()
}

fn qualities(info: &serde_json::Value, ffmpeg: bool) -> Vec<VideoChoice> {
    let formats = formats(info);
    let sounds = sounds(&formats, ffmpeg);
    let best_sound = sounds.first().copied();
    let mut out = vec![VideoChoice {
        index: 0,
        label: "最佳画质".into(),
        ext: "自动".into(),
        size: None,
        approx: false,
        height: None,
        fps: None,
        codec: None,
        range: None,
        kbps: None,
        audio: false,
        format_selector: if ffmpeg {
            "bestvideo*+bestaudio/best".into()
        } else {
            "best[protocol!*=m3u8]/best".into()
        },
        audio_format: None,
        audio_quality: None,
    }];
    out.extend(video_choices(&formats, ffmpeg, best_sound));

    let mut known_audio = HashSet::new();
    for sound in sounds
        .iter()
        .filter(|sound| !sound.hls || !sounds.iter().any(|other| !other.hls))
    {
        let ext = sound.ext.to_ascii_uppercase();
        if !known_audio.insert(ext.clone()) {
            continue;
        }
        out.push(VideoChoice {
            index: 0,
            label: format!("仅音频 · {ext}"),
            ext,
            size: sound.size,
            approx: sound.approx || !plain_id(&sound.id),
            height: None,
            fps: None,
            codec: None,
            range: None,
            kbps: sound.tbr.map(|rate| rate.round() as u32),
            audio: true,
            format_selector: if plain_id(&sound.id) {
                format!("{}/ba", sound.id)
            } else {
                "ba".into()
            },
            audio_format: None,
            audio_quality: None,
        });
    }
    if ffmpeg {
        for quality in ["320", "256", "192", "128"] {
            out.push(VideoChoice {
                index: 0,
                label: format!("仅音频 · MP3 {quality} kbps"),
                ext: "MP3".into(),
                size: None,
                approx: true,
                height: None,
                fps: None,
                codec: None,
                range: None,
                kbps: quality.parse().ok(),
                audio: true,
                format_selector: "bestaudio/best".into(),
                audio_format: Some("mp3".into()),
                audio_quality: Some(format!("{quality}K")),
            });
        }
        for (label, ext) in [("OPUS", "OPUS"), ("FLAC", "FLAC")] {
            out.push(VideoChoice {
                index: 0,
                label: format!("仅音频 · {label}"),
                ext: ext.into(),
                size: None,
                approx: true,
                height: None,
                fps: None,
                codec: None,
                range: None,
                kbps: None,
                audio: true,
                format_selector: "bestaudio/best".into(),
                audio_format: Some(label.to_ascii_lowercase()),
                audio_quality: None,
            });
        }
    }
    for (index, choice) in out.iter_mut().enumerate() {
        choice.index = index;
    }
    if out.len() == 1 {
        out.clear();
    }
    out
}

struct ReservedStem {
    stem: String,
    marker: PathBuf,
    complete: bool,
}

impl Drop for ReservedStem {
    fn drop(&mut self) {
        if self.complete {
            let _ = std::fs::remove_file(&self.marker);
        }
    }
}

fn output_exists(dir: &Path, stem: &str) -> bool {
    let prefix = format!("{stem}.");
    std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with(&prefix)
                && !name.ends_with(".volant.part")
                && !name.ends_with(".volant.state")
        })
}

/// Reserve a title before yt-dlp starts so a second task with the same title
/// cannot reuse a completed file, paused stream, or another active download.
fn reserve_stem(dir: &Path, title: &str, id: u64) -> Result<ReservedStem, String> {
    let base = safe_stem(title);
    let owner = id.to_string();
    for index in 0..10_000u32 {
        let stem = if index == 0 {
            base.clone()
        } else {
            format!("{base} ({index})")
        };
        // shortcut: a forced exit can leave a reservation behind, so the next
        // task skips one name; never delete an unknown user file just to reclaim it.
        let marker = dir.join(format!(".{stem}.volant-name"));
        match std::fs::read_to_string(&marker) {
            Ok(current) if current.trim() == owner => {
                return Ok(ReservedStem {
                    stem,
                    marker,
                    complete: false,
                });
            }
            Ok(_) => continue,
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("无法读取视频文件名预留：{error}"));
            }
            Err(_) => {}
        }
        if output_exists(dir, &stem) {
            continue;
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
        {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(owner.as_bytes())
                    .map_err(|error| format!("无法保存视频文件名预留：{error}"))?;
                return Ok(ReservedStem {
                    stem,
                    marker,
                    complete: false,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!("无法在 {} 预留视频文件名：{error}", dir.display()));
            }
        }
    }
    Err("同名视频过多，无法生成新的文件名".into())
}

/// Download a resolved choice. `on_progress` is called on the controller task;
/// cancellation kills yt-dlp and ffmpeg together through yt-dlp's process tree.
pub async fn download(
    task: &VideoTask,
    destination: &Path,
    id: u64,
    connections: usize,
    rate: Option<u64>,
    cancel: Arc<AtomicBool>,
    mut on_progress: impl FnMut(VideoProgress) + Send,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(destination)
        .map_err(|error| format!("无法建立下载目录 {}：{error}", destination.display()))?;
    let exe = ytdlp_path().await?;
    let ffmpeg = ffmpeg_path().await;
    let needs_ffmpeg = task.choice.audio_format.is_some()
        || (!task.choice.audio
            && (task.choice.format_selector.contains('+')
                || task.choice.format_selector.contains("bestvideo")));
    if needs_ffmpeg && ffmpeg.is_none() {
        return Err(format!(
            "缺少 ffmpeg，无法合并音视频或输出 MP4。请打开 Volant 设置中的“视频”页一键安装，或把 ffmpeg 放到 {}。",
            helper_dir().join("ffmpeg").display()
        ));
    }
    let cookie_jar = CookieJar::write(&task.context.cookies)?;
    let mut reserved = reserve_stem(destination, &task.title, id)?;
    let stem = reserved.stem.as_str();
    let template = destination.join(format!("{stem}.%(ext)s"));
    let mut selected = select_video(&exe, task, destination, stem, cookie_jar.path())
        .await
        .ok()
        .filter(|selected| plan_http_streams(&selected.json).is_some());
    let mut use_preloaded = false;
    if let Some(selection) = selected.as_ref()
        && let Some(streams) = plan_http_streams(&selection.json)
    {
        match fetch_http_streams(&streams, connections, rate, &cancel, &mut on_progress).await {
            Ok(()) => use_preloaded = true,
            Err(error) if error == "已暂停" => return Err(error),
            Err(_) => {
                // A site may change its signed stream URLs after selection. yt-dlp
                // remains the fallback owner and will fetch or resume those bytes.
                selected = None;
            }
        }
    }
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
    if let Some(ffmpeg) = &ffmpeg
        && let Some(parent) = ffmpeg.parent()
    {
        cmd.arg("--ffmpeg-location").arg(parent);
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
    if use_preloaded {
        if let Some(selection) = &selected {
            cmd.arg("--load-info-json").arg(&selection.path);
        }
    } else {
        cmd.arg(if task.context.src.is_empty() {
            &task.context.page
        } else {
            &task.context.src
        });
    }
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
        reserved.complete = true;
        return Ok(path);
    }
    let path = find_output(destination, stem)
        .ok_or_else(|| "yt-dlp 已完成，但没有找到保存的视频文件".to_string())?;
    reserved.complete = true;
    Ok(path)
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
        connections: 1,
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

fn splitext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(index) if name[..index].chars().any(|ch| ch != '.') => {
            (&name[..index], &name[index..])
        }
        _ => (name, ""),
    }
}

/// Where yt-dlp expects one stream of a merged output. The engine writes the same
/// `<stem>.f<id>.<ext>` file yt-dlp would have produced, so its post-processing can
/// still own the merge and subtitle step.
fn stream_path(output: &Path, merged: &str, id: &str, ext: &str) -> PathBuf {
    let name = output
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, real) = splitext(&name);
    let real = real.trim_start_matches('.');
    let base = if real == merged || real == ext {
        stem
    } else {
        &name
    };
    let corrected = format!("{base}.{ext}");
    let (stem, real) = splitext(&corrected);
    let named = if real.trim_start_matches('.') == ext {
        format!("{stem}.f{id}{real}")
    } else {
        format!("{corrected}.f{id}")
    };
    output.with_file_name(named)
}

fn cookie_pairs(cookies: &str) -> Option<String> {
    const ATTRIBUTES: [&str; 9] = [
        "domain", "path", "expires", "max-age", "secure", "httponly", "version", "samesite",
        "comment",
    ];
    let pairs: Vec<&str> = cookies
        .split(';')
        .map(str::trim)
        .filter(|pair| {
            pair.contains('=')
                && !ATTRIBUTES.contains(
                    &pair
                        .split_once('=')
                        .map_or("", |(name, _)| name)
                        .trim()
                        .to_ascii_lowercase()
                        .as_str(),
                )
        })
        .collect();
    (!pairs.is_empty()).then(|| pairs.join("; "))
}

fn stream_headers(format: &serde_json::Value) -> Vec<(String, String)> {
    let mut out = format
        .get("http_headers")
        .and_then(serde_json::Value::as_object)
        .map(|headers| {
            headers
                .iter()
                .filter(|(name, _)| !name.eq_ignore_ascii_case("cookie"))
                .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_owned())))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(cookie) = text(format, "cookies").and_then(|value| cookie_pairs(&value)) {
        out.push(("Cookie".into(), cookie));
    }
    out
}

fn plan_http_streams(selected: &serde_json::Value) -> Option<Vec<HttpStream>> {
    if selected.get("is_live").and_then(serde_json::Value::as_bool) == Some(true) {
        return None;
    }
    let output = PathBuf::from(text(selected, "filename")?);
    let merged = text(selected, "ext").unwrap_or_default();
    let formats = selected
        .get("requested_formats")
        .and_then(serde_json::Value::as_array)?;
    if formats.is_empty() {
        return None;
    }
    let mut streams = Vec::new();
    for format in formats {
        let protocol = text(format, "protocol").unwrap_or_default();
        if matches!(
            protocol.as_str(),
            "m3u8" | "m3u8_native" | "http_dash_segments" | "f4m" | "ism"
        ) || format
            .get("fragments")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|fragments| !fragments.is_empty())
        {
            // Streams in pieces stay with yt-dlp until Volant has a piece scheduler.
            return None;
        }
        let url = text(format, "url")
            .and_then(|url| volant_engine::Url::parse(&url).ok())
            .filter(|url| matches!(url.scheme(), "http" | "https"))?;
        let id = text(format, "format_id").unwrap_or_else(|| "0".into());
        let ext = text(format, "ext").unwrap_or_else(|| merged.clone());
        streams.push(HttpStream {
            path: stream_path(&output, &merged, &id, &ext),
            url,
            headers: stream_headers(format),
            size: number(format, "filesize").or_else(|| number(format, "filesize_approx")),
            id,
        });
    }
    (!streams.is_empty()).then_some(streams)
}

async fn select_video(
    exe: &Path,
    task: &VideoTask,
    destination: &Path,
    stem: &str,
    cookie_jar: &Path,
) -> Result<SelectedVideo, String> {
    let template = destination.join(format!("{stem}.%(ext)s"));
    let mut cmd = command_for(exe);
    add_network_args(&mut cmd, &task.context, cookie_jar);
    cmd.args([
        "--no-playlist",
        "--no-warnings",
        "--ignore-config",
        "-f",
        &task.choice.format_selector,
        "--print",
        "%()j",
        "-o",
    ]);
    cmd.arg(&template);
    cmd.arg(if task.context.src.is_empty() {
        &task.context.page
    } else {
        &task.context.src
    });
    let output = inspect_output(cmd, "视频流信息").await?;
    if !output.status.success() {
        return Err(helper_error(&output.stderr));
    }
    let line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .ok_or_else(|| "yt-dlp 没有返回已选择的视频流".to_string())?
        .to_owned();
    let json =
        serde_json::from_str(&line).map_err(|error| format!("已选择的视频流无法读取：{error}"))?;
    let path = ensure_helper_dir()?.join("tmp").join(format!(
        "selected-{}-{}.json",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("无法建立视频临时目录：{error}"))?;
    }
    std::fs::write(&path, &line).map_err(|error| format!("无法保存视频流信息：{error}"))?;
    Ok(SelectedVideo { json, path })
}

async fn fetch_http_streams(
    streams: &[HttpStream],
    connections: usize,
    rate: Option<u64>,
    cancel: &AtomicBool,
    on_progress: &mut (dyn FnMut(VideoProgress) + Send),
) -> Result<(), String> {
    let client = volant_engine::client().map_err(|error| error.to_string())?;
    let total: u64 = streams.iter().filter_map(|stream| stream.size).sum();
    let mut before = 0u64;
    for stream in streams {
        let part = crate::model::sidecar(&stream.path, ".volant.part");
        let state = crate::model::sidecar(&stream.path, ".volant.state");
        if stream.path.is_file() && !part.exists() && !state.exists() {
            before += stream.size.unwrap_or_else(|| {
                std::fs::metadata(&stream.path).map_or(0, |metadata| metadata.len())
            });
            continue;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err("已暂停".into());
        }
        let probe = volant_engine::probe_with(&client, &stream.url, &stream.headers)
            .await
            .map_err(|error| format!("视频流 {}：{error}", stream.id))?;
        let mut config = volant_engine::Config::new(
            stream.url.clone(),
            stream
                .path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
        );
        config.dest = Some(stream.path.clone());
        config.connections = connections.clamp(1, crate::model::MAX_CONNECTIONS_PER_TASK);
        config.rate_limit = rate;
        config.headers = stream.headers.clone();
        let mut download = volant_engine::Download::start(client.clone(), config, probe);
        let mut tick = tokio::time::interval(Duration::from_millis(150));
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if cancel.load(Ordering::Relaxed) {
                        download.handle.cancel();
                    }
                }
                event = download.events.recv() => {
                    match event {
                        Some(volant_engine::Event::Progress {
                            downloaded,
                            total: stream_total,
                            connections,
                            ..
                        }) => {
                            let now_total = if total > 0 {
                                total.max(before + stream_total)
                            } else {
                                before + stream_total
                            };
                            on_progress(VideoProgress {
                                downloaded: before + downloaded,
                                total: now_total,
                                connections,
                            });
                        }
                        Some(volant_engine::Event::Failed { reason }) => {
                            return Err(format!("视频流 {}：{reason}", stream.id));
                        }
                        Some(_) => {}
                        None => break,
                    }
                }
            }
        }
        let summary = download
            .wait()
            .await
            .map_err(|error| format!("视频流 {}：{error}", stream.id))?;
        if cancel.load(Ordering::Relaxed) {
            return Err("已暂停".into());
        }
        before += summary.size;
    }
    Ok(())
}

fn add_network_args(cmd: &mut Command, context: &VideoContext, cookie_jar: &Path) {
    cmd.arg("--cookies").arg(cookie_jar);
    // Current yt-dlp enables Deno by default. If the user already has Node,
    // explicitly enable it so JavaScript-heavy sites such as YouTube can use
    // the bundled yt-dlp-ejs helper without installing another runtime.
    if executable_in_path(if cfg!(windows) { "node.exe" } else { "node" }).is_some() {
        cmd.args(["--js-runtimes", "node"]);
    }
    if !context.user_agent.trim().is_empty() {
        cmd.args(["--user-agent", context.user_agent.trim()]);
    }
    if !context.referrer.trim().is_empty() {
        cmd.args([
            "--add-header",
            &format!("Referer: {}", context.referrer.trim()),
        ]);
    }
    if !context.origin.trim().is_empty() {
        cmd.args([
            "--add-header",
            &format!("Origin: {}", context.origin.trim()),
        ]);
    }
}

fn managed_ytdlp_path() -> PathBuf {
    helper_dir().join(if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    })
}

async fn ytdlp_path() -> Result<PathBuf, String> {
    if let Some(explicit) = std::env::var_os("VOLANT_YTDLP")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        return Ok(explicit);
    }
    let managed = managed_ytdlp_path();
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

pub async fn install_ytdlp(destination: &Path) -> Result<(), String> {
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
    let managed = helper_dir().join("ffmpeg").join(if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    });
    if managed.is_file() {
        return Some(managed);
    }
    let legacy = helper_dir().join(if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    });
    if legacy.is_file() {
        return Some(legacy);
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

/// Download yt-dlp's FFmpeg build and extract only the binaries and shared
/// libraries Volant needs. The archive is installed atomically under the
/// application-data directory so an interrupted download never leaves a
/// half-usable ffmpeg.
pub async fn install_ffmpeg() -> Result<PathBuf, String> {
    if !cfg!(windows) {
        return Err("自动安装 ffmpeg 目前仅支持 Windows；请通过系统包管理器安装。".into());
    }
    let root = ensure_helper_dir()?;
    let destination = root.join("ffmpeg");
    let staging = root.join(format!(
        "ffmpeg.installing-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis())
    ));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|error| format!("无法清理旧的 ffmpeg 临时目录：{error}"))?;
    }
    std::fs::create_dir_all(&staging)
        .map_err(|error| format!("无法建立 ffmpeg 临时目录：{error}"))?;

    let result = async {
        let release = reqwest::Client::new()
            .get(FFMPEG_RELEASE_API)
            .header("User-Agent", "Volant/0.6")
            .send()
            .await
            .map_err(|error| format!("读取 ffmpeg 发布信息失败：{error}"))?;
        if !release.status().is_success() {
            return Err(format!(
                "读取 ffmpeg 发布信息返回 HTTP {}",
                release.status()
            ));
        }
        let release: serde_json::Value = release
            .json()
            .await
            .map_err(|error| format!("ffmpeg 发布信息无法读取：{error}"))?;
        let asset = release
            .get("assets")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .find(|asset| {
                asset
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|name| name.ends_with("win64-lgpl-shared.zip"))
            })
            .ok_or_else(|| "ffmpeg 最新发布中没有 Windows x64 共享版".to_string())?;
        let url = asset
            .get("browser_download_url")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "ffmpeg 下载地址无效".to_string())?;
        let response = reqwest::Client::new()
            .get(url)
            .header("User-Agent", "Volant/0.6")
            .send()
            .await
            .map_err(|error| format!("下载 ffmpeg 失败：{error}"))?;
        if !response.status().is_success() {
            return Err(format!("下载 ffmpeg 返回 HTTP {}", response.status()));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| format!("读取 ffmpeg 安装包失败：{error}"))?;
        if bytes.len() < 10_000_000 || !bytes.starts_with(b"PK") {
            return Err("下载到的 ffmpeg 安装包无效".into());
        }
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .map_err(|error| format!("ffmpeg 安装包无法打开：{error}"))?;
        let names = (0..archive.len())
            .filter_map(|index| Some(archive.by_index(index).ok()?.name().to_string()))
            .collect::<Vec<_>>();
        let mut extracted = 0usize;
        for name in names {
            let lower = name.to_ascii_lowercase();
            if !lower.contains("/bin/") {
                continue;
            }
            let extension = lower.rsplit('.').next().unwrap_or("");
            if !matches!(extension, "exe" | "dll") {
                continue;
            }
            let Some(file_name) = Path::new(&name).file_name() else {
                continue;
            };
            if file_name.eq_ignore_ascii_case("ffplay.exe") {
                continue;
            }
            let output = staging.join(file_name);
            let mut source = archive
                .by_name(&name)
                .map_err(|error| format!("无法读取 ffmpeg 组件 {name}：{error}"))?;
            let mut target = std::fs::File::create(&output)
                .map_err(|error| format!("无法写入 {}：{error}", output.display()))?;
            std::io::copy(&mut source, &mut target)
                .map_err(|error| format!("解压 {} 失败：{error}", output.display()))?;
            extracted += 1;
        }
        if extracted == 0 || !staging.join("ffmpeg.exe").is_file() {
            return Err("ffmpeg 安装包中没有可用的 Windows 程序".into());
        }
        if destination.exists() {
            std::fs::remove_dir_all(&destination)
                .map_err(|error| format!("无法替换旧的 ffmpeg：{error}"))?;
        }
        std::fs::rename(&staging, &destination)
            .map_err(|error| format!("启用 ffmpeg 失败：{error}"))?;
        Ok(destination.join("ffmpeg.exe"))
    }
    .await;

    if result.is_err() && staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

fn command_for(exe: &Path) -> Command {
    let mut command = Command::new(exe);
    // PyInstaller (yt-dlp.exe) needs a writable temp directory. Keep its
    // unpacked files under Volant instead of relying on a restricted TEMP.
    let temp = helper_dir().join("tmp");
    let _ = std::fs::create_dir_all(&temp);
    command.env("TMP", &temp).env("TEMP", &temp);
    hide_console(&mut command);
    command
}

async fn inspect_output(mut command: Command, label: &str) -> Result<std::process::Output, String> {
    command.kill_on_drop(true);
    match tokio::time::timeout(INSPECTION_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(format!("无法启动 yt-dlp 读取{label}：{error}")),
        Err(_) => Err(format!(
            "读取{label}超过 {} 秒，请稍后重试。",
            INSPECTION_TIMEOUT.as_secs()
        )),
    }
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

fn decimal(value: &serde_json::Value, key: &str) -> Option<f64> {
    let value = value.get(key)?;
    value
        .as_f64()
        .or_else(|| value.as_u64().map(|value| value as f64))
        .filter(|value| value.is_finite())
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
            let domain = if cookie.host_only {
                cookie.domain.trim_start_matches('.').to_owned()
            } else {
                let domain = cookie.domain.trim_start_matches('.');
                format!(".{domain}")
            };
            lines.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                domain,
                if cookie.host_only { "FALSE" } else { "TRUE" },
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
        let list = qualities(&info, true);
        assert_eq!(list[0].label, "最佳画质");
        assert_eq!(
            list[1].format_selector,
            "137+140/bv*[height=1080]+ba/b[height=1080]"
        );
        assert!(!list[1].audio);
        assert!(
            list.iter()
                .any(|choice| choice.audio && choice.ext == "M4A")
        );
    }

    #[test]
    fn codec_hdr_and_frame_rate_variants_remain_distinct() {
        let info = serde_json::json!({
            "formats": [
                {"format_id":"a","height":2160,"fps":60,"ext":"webm","vcodec":"av01","acodec":"none","dynamic_range":"HDR10","filesize":500},
                {"format_id":"b","height":2160,"fps":60,"ext":"webm","vcodec":"vp9","acodec":"none","filesize":450},
                {"format_id":"c","height":2160,"fps":30,"ext":"webm","vcodec":"vp9","acodec":"none","filesize":400},
                {"format_id":"sound","ext":"opus","vcodec":"none","acodec":"opus","filesize":50}
            ]
        });
        let list = qualities(&info, true);
        let video = list
            .iter()
            .filter(|choice| choice.height == Some(2160))
            .collect::<Vec<_>>();
        assert_eq!(video.len(), 3);
        assert!(
            video
                .iter()
                .any(|choice| choice.codec.as_deref() == Some("AV1"))
        );
        assert!(
            video
                .iter()
                .any(|choice| choice.range.as_deref() == Some("HDR"))
        );
        assert!(video.iter().any(|choice| choice.fps == Some(30.0)));
    }

    #[test]
    fn stream_path_matches_yt_dlp_merge_names() {
        assert_eq!(
            stream_path(Path::new("/v/Clip.mp4"), "mp4", "137", "mp4"),
            PathBuf::from("/v/Clip.f137.mp4")
        );
        assert_eq!(
            stream_path(Path::new("/v/Clip.mp4"), "mp4", "140", "m4a"),
            PathBuf::from("/v/Clip.f140.m4a")
        );
    }

    #[test]
    fn same_title_reserves_a_new_output_name() {
        let dir = std::env::temp_dir().join(format!(
            "volant-video-name-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Clip.mp4"), b"existing").unwrap();

        let mut reserved = reserve_stem(&dir, "Clip", 7).unwrap();
        assert_eq!(reserved.stem, "Clip (1)");
        assert!(reserved.marker.is_file());
        std::fs::write(dir.join("Clip (1).f137.mp4"), b"partial").unwrap();
        assert_eq!(reserve_stem(&dir, "Clip", 7).unwrap().stem, "Clip (1)");
        assert_eq!(reserve_stem(&dir, "Clip", 8).unwrap().stem, "Clip (2)");
        let marker = reserved.marker.clone();
        reserved.complete = true;
        drop(reserved);
        assert!(!marker.exists());

        std::fs::remove_dir_all(dir).unwrap();
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
                approx: false,
                height: Some(1080),
                fps: None,
                codec: None,
                range: None,
                kbps: None,
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
