//! Volant 的 HTTP 下载引擎。不依赖任何 UI。
//!
//! 对外只有五个概念（见 docs/HANDOFF.md）：
//! - [`probe`]：问清能不能分段、多大、叫什么、怎么判断它变了；
//! - [`Planner`]：纯逻辑的分段规划（谁写哪一段）；
//! - [`Download`]：真正下载，边跑边出 [`Event`]；
//! - [`Handle::cancel`]：停下并保住进度；
//! - 续传 = 用同一个 URL 再 [`Download::start`] 一次，盘上的 `.volant.state` 会被认出来。
//!
//! ```no_run
//! # async fn demo() -> Result<(), volant_engine::Error> {
//! use volant_engine::{client, probe, Config, Download};
//! let c = client()?;
//! let url = "https://example.com/big.iso".parse().unwrap();
//! let p = probe(&c, &url).await?;
//! println!("{} ({} 字节, 可分段: {})", p.filename.clone().unwrap_or_default(), p.size.unwrap_or(0), p.ranges);
//! let mut dl = Download::start(c, Config::new(url, "."), p);
//! while let Some(ev) = dl.events.recv().await {
//!     if let volant_engine::Event::Progress { downloaded, .. } = ev {
//!         println!("{downloaded}");
//!     }
//! }
//! dl.wait().await?;
//! # Ok(())
//! # }
//! ```

mod disk;
mod download;
mod hosts;
mod limiter;
mod part;
pub mod planner;
pub mod probe;
pub mod state;
mod trace;
mod writer;

use std::fmt;
use std::time::Duration;

pub use download::{Config, Download, Event, Handle, Summary, pick_free_name};
pub use planner::{Assignment, Claim, Planner, SavedSegment, Segment};
pub use probe::{Probe, probe, probe_with, probe_with_timeout};
pub use reqwest::{Client, Url};
pub use state::ResumeState;

/// 把调用方给的额外请求头挂上去。浏览器捕获来的 `Cookie` / `Referer` 靠它带进引擎；
/// 名字或值不合法的直接跳过——`RequestBuilder::header` 遇到非法值会 panic，而这里的数据
/// 来自别的进程，不能让它把下载线程打崩。
pub(crate) fn with_headers(
    mut req: reqwest::RequestBuilder,
    headers: &[(String, String)],
) -> reqwest::RequestBuilder {
    use reqwest::header::{HeaderName, HeaderValue};
    for (name, value) in headers {
        let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) else {
            continue;
        };
        req = req.header(name, value);
    }
    req
}

/// 默认的 HTTP 客户端。DNS 用系统解析；HTTP/2 不开（分段要 HTTP/1.1 的 Range 可控）。
pub fn client() -> Result<Client, Error> {
    Client::builder()
        .user_agent(concat!("Volant/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| Error::Http(e.to_string()))
}

#[derive(Debug)]
pub enum Error {
    /// 网络层：连不上、TLS、读超时。
    Http(String),
    /// 服务器给了不该给的状态码。
    Status(u16),
    Io(std::io::Error),
    /// 服务器对同一个区间的回答变了（ETag 不匹配 / 回了完整文件）。
    SourceChanged,
    /// 磁盘装不下，里面是给人看的一句话。
    NoSpace(String),
    /// 被 [`Handle::cancel`] 停下；进度已保存，可以续传。
    Cancelled,
    /// 连续重试都没有进展。
    Stalled,
    Internal(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Http(e) => write!(f, "网络错误：{e}"),
            Error::Status(c) => write!(f, "服务器返回 HTTP {c}"),
            Error::Io(e) => {
                // 写文件被拒最常见的原因是「这个目录当前账户/完整性级别写不进」，
                // 光一句「拒绝访问」用户不知道该动哪儿。
                let hint = if e.kind() == std::io::ErrorKind::PermissionDenied {
                    "（写不进这个目录：换一个目录，或确认程序没有以受限权限运行）"
                } else {
                    ""
                };
                write!(f, "文件读写失败：{e}{hint}")
            }
            Error::SourceChanged => write!(
                f,
                "源文件在下载过程中变了（校验值或区间对不上），已停止；已下载的部分留在 .part 文件里"
            ),
            Error::NoSpace(msg) => f.write_str(msg),
            Error::Cancelled => f.write_str("已取消，进度已保存"),
            Error::Stalled => f.write_str("下载卡住了：连续重试都没有任何进展"),
            Error::Internal(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
