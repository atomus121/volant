//! `probe`：一次请求问清「这个 URL 能不能分段、有多大、叫什么、怎么判断它变了」。
//!
//! 用 `GET` + `Range: bytes=0-0` 而不是 `HEAD`：HEAD 在不少 CDN/网关后面返回的头部和 GET
//! 不一样（甚至被拒），而 1 字节的 GET 一定走的是真正下载会走的那条路。
//!
//! 返回体直接丢掉。PRD 2.2 想要「复用探测连接继续收数据，省掉一次往返」，那要求把响应体
//! 交给第一个 worker；M1 没做（省 30 ms，代价是一整条 probe→worker 的数据交接），M2 再上。

use percent_encoding::percent_decode_str;
use reqwest::header::{
    CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, LAST_MODIFIED, RANGE,
};
use reqwest::{Client, StatusCode, Url};

use crate::Error;

const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    /// 用户给的 URL。
    pub url: Url,
    /// 跟随跳转后的 URL，之后所有请求都打这里。
    pub final_url: Url,
    /// 文件总长；`None` 表示服务器没给（chunked / 关连接表示结束）。
    pub size: Option<u64>,
    /// 服务器支持 Range 分段。
    pub ranges: bool,
    /// 强校验器；弱 ETag（`W/"..."`）当没有，因为它自己承认字节可能不同。
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
    /// 已经过 [`sanitize`] 的文件名。
    pub filename: Option<String>,
}

pub async fn probe(client: &Client, url: &Url) -> Result<Probe, Error> {
    probe_with(client, url, &[]).await
}

/// 带额外请求头的探测。浏览器捕获的下载常常要 `Cookie` / `Referer` 才不 403，
/// 而探测和真正的分段请求必须带同一套头，否则「探测能过、下载 403」。
pub async fn probe_with(
    client: &Client,
    url: &Url,
    headers: &[(String, String)],
) -> Result<Probe, Error> {
    probe_with_timeout(client, url, headers, PROBE_TIMEOUT).await
}

/// 带显式响应头超时的探测。下载器使用 15 秒默认值；测试可以传更短的时间。
pub async fn probe_with_timeout(
    client: &Client,
    url: &Url,
    headers: &[(String, String)],
    timeout: std::time::Duration,
) -> Result<Probe, Error> {
    let request = crate::with_headers(client.get(url.clone()), headers)
        .header(RANGE, "bytes=0-0")
        .send();
    let resp = tokio::time::timeout(timeout, request)
        .await
        .map_err(|_| {
            Error::Http(format!(
                "探测超时：{} 秒内没有收到响应头",
                timeout.as_secs_f64()
            ))
        })?
        .map_err(|e| Error::Http(e.to_string()))?;

    let final_url = resp.url().clone();
    let status = resp.status();
    let header = |name: reqwest::header::HeaderName| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .map(str::to_owned)
    };

    let etag = header(ETAG).filter(|e| is_strong_etag(e));
    let last_modified = header(LAST_MODIFIED);
    let content_type = header(CONTENT_TYPE);
    let disposition = header(CONTENT_DISPOSITION);
    let content_range = header(CONTENT_RANGE);
    let content_length = header(CONTENT_LENGTH).and_then(|v| v.parse::<u64>().ok());

    let (size, ranges) = match status {
        StatusCode::PARTIAL_CONTENT => {
            // `Content-Range: bytes 0-0/12345`
            let total = content_range.as_deref().and_then(total_of_content_range);
            (total, true)
        }
        StatusCode::OK => (content_length, false),
        StatusCode::RANGE_NOT_SATISFIABLE => {
            // `Content-Range: bytes */12345` —— 文件是空的或区间越界，但长度告诉我们了。
            (
                content_range.as_deref().and_then(total_of_content_range),
                true,
            )
        }
        other => return Err(Error::Status(other.as_u16())),
    };
    // 响应体我们不读，直接丢掉；连接随之关闭（probe 只做一次）。
    drop(resp);

    let filename = disposition
        .as_deref()
        .and_then(filename_from_disposition)
        .or_else(|| filename_from_url(&final_url))
        .map(|n| sanitize(&n));

    Ok(Probe {
        url: url.clone(),
        final_url,
        size,
        ranges,
        etag,
        last_modified,
        content_type,
        filename,
    })
}

/// `bytes 0-0/12345` → 12345；`bytes 0-0/*` → None。
fn total_of_content_range(v: &str) -> Option<u64> {
    let (_, total) = v.rsplit_once('/')?;
    let total = total.trim();
    (total != "*").then(|| total.parse().ok()).flatten()
}

/// `Content-Range: bytes <start>-<end>/...` 的起点。
pub fn start_of_content_range(v: &str) -> Option<u64> {
    let rest = v.trim().strip_prefix("bytes")?;
    let (range, _) = rest.trim().split_once('/')?;
    let (start, _) = range.split_once('-')?;
    start.trim().parse().ok()
}

fn is_strong_etag(e: &str) -> bool {
    e.len() >= 2 && e.starts_with('"') && e.ends_with('"')
}

/// 从 `Content-Disposition` 里取文件名：优先 `filename*=`（RFC 5987），退回 `filename=`。
pub fn filename_from_disposition(v: &str) -> Option<String> {
    let mut plain = None;
    let mut extended = None;
    for (key, value) in params(v) {
        match key.to_ascii_lowercase().as_str() {
            "filename*" => extended = Some(value.to_owned()),
            "filename" => plain = Some(unquote(value)),
            _ => {}
        }
    }
    extended
        .as_deref()
        .and_then(decode_rfc5987)
        .or(plain)
        .filter(|s| !s.trim().is_empty())
}

/// 按 `;` 切参数，但引号里的 `;` 不算分隔符。
fn params(v: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (i, c) in v.char_indices() {
        match c {
            '\\' if quoted => escaped = !escaped,
            '"' if !escaped => quoted = !quoted,
            ';' if !quoted => {
                push_param(&mut out, &v[start..i]);
                start = i + 1;
            }
            _ => escaped = false,
        }
    }
    push_param(&mut out, &v[start..]);
    out
}

fn push_param<'a>(out: &mut Vec<(&'a str, &'a str)>, piece: &'a str) {
    if let Some((k, v)) = piece.split_once('=') {
        out.push((k.trim(), v.trim()));
    }
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    match v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        Some(inner) => {
            // 只解 \" 和 \\，别的反斜杠序列原样留着（Windows 文件名里本来就少见）。
            let mut out = String::with_capacity(inner.len());
            let mut chars = inner.chars();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    match chars.next() {
                        Some(n @ ('"' | '\\')) => out.push(n),
                        Some(n) => {
                            out.push('\\');
                            out.push(n);
                        }
                        None => out.push('\\'),
                    }
                } else {
                    out.push(c);
                }
            }
            out
        }
        None => v.to_owned(),
    }
}

/// `UTF-8''%E4%B8%AD.bin` / `iso-8859-1'en'latin.bin`
fn decode_rfc5987(v: &str) -> Option<String> {
    let v = unquote(v);
    let mut parts = v.splitn(3, '\'');
    let charset = parts.next()?.to_ascii_lowercase();
    let _lang = parts.next()?;
    let value = parts.next()?;
    let bytes = percent_decode_str(value).collect::<Vec<u8>>();
    if charset == "utf-8" || charset.is_empty() {
        String::from_utf8(bytes).ok()
    } else {
        // iso-8859-1 之类：一个字节一个字符，够用了。
        Some(bytes.into_iter().map(char::from).collect())
    }
}

fn filename_from_url(url: &Url) -> Option<String> {
    let last = url.path_segments()?.next_back()?;
    if last.is_empty() {
        return None;
    }
    Some(percent_decode_str(last).decode_utf8_lossy().into_owned())
}

const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 变成一个能在 Windows 上落地的文件名：去掉路径分隔符、控制字符、结尾的点和空格，
/// 避开设备名，长度封顶。**绝不返回空串**。
pub fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => out.push('_'),
            c if (c as u32) < 0x20 || c == '\u{7f}' => out.push('_'),
            c => out.push(c),
        }
    }
    // Windows 会静默吃掉结尾的点与空格，那会让「同名」判断出错。
    let out = out.trim_end_matches([' ', '.']).trim_start();
    let stem = out.split('.').next().unwrap_or("");
    let mut out = out.to_owned();
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        out.insert(0, '_');
    }
    let mut out = cap_bytes(&out, 200);
    if out.is_empty() {
        out.push_str("download");
    }
    out
}

/// 按字节上限截断，但不切在多字节字符中间。
fn cap_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].trim_end_matches([' ', '.']).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range_totals() {
        assert_eq!(total_of_content_range("bytes 0-0/12345"), Some(12345));
        assert_eq!(total_of_content_range("bytes 0-0/*"), None);
        assert_eq!(total_of_content_range("garbage"), None);
        assert_eq!(start_of_content_range("bytes 4096-8191/9000"), Some(4096));
        assert_eq!(start_of_content_range("bytes */0"), None);
    }

    #[test]
    fn utf8_filename_star_wins_over_plain() {
        let v =
            "attachment; filename=\"fallback.zip\"; filename*=UTF-8''%E4%B8%AD%E6%96%87%20a.bin";
        assert_eq!(
            filename_from_disposition(v).unwrap(),
            "中文 a.bin",
            "RFC 5987 优先"
        );
    }

    #[test]
    fn plain_quoted_filename_with_a_semicolon_inside() {
        let v = "attachment; filename=\"a;b.txt\"; size=12";
        assert_eq!(filename_from_disposition(v).unwrap(), "a;b.txt");
    }

    #[test]
    fn unquoted_and_escaped_filenames() {
        assert_eq!(
            filename_from_disposition("attachment; filename=plain.bin").unwrap(),
            "plain.bin"
        );
        assert_eq!(
            filename_from_disposition(r#"attachment; filename="a\"b.zip""#).unwrap(),
            r#"a"b.zip"#
        );
    }

    #[test]
    fn a_missing_filename_is_none() {
        assert_eq!(filename_from_disposition("attachment"), None);
        assert_eq!(filename_from_disposition("attachment; filename=\"\""), None);
    }

    #[test]
    fn url_fallback_decodes() {
        let u: Url = "http://x/a/%E4%B8%AD.txt?y=1".parse().unwrap();
        assert_eq!(filename_from_url(&u).unwrap(), "中.txt");
        let u: Url = "http://x/".parse().unwrap();
        assert_eq!(filename_from_url(&u), None);
    }

    #[test]
    fn sanitize_makes_windows_happy() {
        assert_eq!(sanitize("a/b:c*?.txt"), "a_b_c__.txt");
        assert_eq!(sanitize("trailing. . "), "trailing");
        assert_eq!(sanitize("CON"), "_CON");
        assert_eq!(sanitize("con.txt"), "_con.txt", "设备名带扩展名也照样被占");
        assert_eq!(sanitize(""), "download");
        assert_eq!(sanitize("   "), "download");
        assert_eq!(sanitize("ok\nname"), "ok_name");
        let long = "字".repeat(300);
        let s = sanitize(&long);
        assert!(
            s.len() <= 200 && s.chars().count() == 66,
            "按字节封顶且不切坏字符"
        );
    }

    #[test]
    fn latin1_filename_star() {
        assert_eq!(
            filename_from_disposition("attachment; filename*=iso-8859-1'en'caf%E9.bin").unwrap(),
            "café.bin"
        );
    }
}
