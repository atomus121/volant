//! `volant-cli get <url>`：不靠 UI 把文件下完。
//!
//! 这是 M3 的出口标准：引擎能独立跑完一次下载、中途 Ctrl+C 能保住进度、同一个 URL
//! 再跑一次就是续传。进度和结果都写 stderr，stdout 留给将来可能的 `--json`。

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use volant_engine::{Client, Config, Download, Error, Event, Url, client, probe};

const USAGE: &str = "\
volant-cli — Volant 下载引擎的命令行入口

用法:
  volant-cli get <url> [-d <目录>] [-n <文件名>] [-c <连接数>] [--rate <限速>] [--batch <攒批大小>] [--no-host-file]

  -d, --dir <目录>    存到哪（默认当前目录）
  -n, --name <文件名> 存成什么名字（默认取自 URL / Content-Disposition）
  -c, --conns <n>     最多开几条连接（默认 8）
      --rate <限速>   例如 8M、1.5M、500k（默认不限）
--batch <大小>  攒够多少字节再写盘，默认 256k
      --no-host-file  不读写主机并发记忆
      --trace         跑完打一份时间账（收块大小、限速等待、写盘耗时）
  -q, --quiet         不打进度
  -h, --help          看这段

Ctrl+C 停下并保存进度；用同一个 URL 再跑一次就是续传。
";

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args).await {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("volant-cli: {msg}");
            ExitCode::FAILURE
        }
    }
}

struct Args {
    url: String,
    dir: PathBuf,
    name: Option<String>,
    conns: Option<usize>,
    rate: Option<u64>,
    batch: Option<u64>,
    host_file: bool,
    quiet: bool,
    trace: bool,
}

async fn run(args: Vec<String>) -> Result<ExitCode, String> {
    let Some(a) = parse_args(&args)? else {
        print!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    };

    let c = client().map_err(|e| e.to_string())?;
    let url: Url = a.url.parse().map_err(|e| format!("URL 不对：{e}"))?;
    let mut sigint = Box::pin(tokio::signal::ctrl_c());
    let p = tokio::select! {
        biased;
        _ = &mut sigint => {
            eprintln!("收到 Ctrl+C，已取消解析。");
            return Ok(ExitCode::from(130));
        }
        p = probe(&c, &url) => p.map_err(|e| e.to_string())?,
    };

    let mut cfg = Config::new(url, &a.dir);
    if let Some(name) = &a.name {
        cfg.dest = Some(a.dir.join(name));
    }
    if let Some(n) = a.conns {
        cfg.connections = n.max(1);
    }
    cfg.rate_limit = a.rate;
    if let Some(b) = a.batch {
        cfg.batch = (b as usize).max(16 << 10);
    }
    if a.host_file {
        cfg.host_file = default_host_file();
    }
    cfg.trace = a.trace;

    download(c, cfg, p, a.quiet).await
}

async fn download(
    c: Client,
    cfg: Config,
    p: volant_engine::Probe,
    quiet: bool,
) -> Result<ExitCode, String> {
    let mut dl = Download::start(c, cfg, p);
    // select! 里另一条分支要用它，先克隆出来，免得和 events 借用打架。
    let handle = dl.handle.clone();
    let tty = std::io::stderr().is_terminal();
    let mut sigint = Box::pin(tokio::signal::ctrl_c());
    let mut cancelled = false;

    loop {
        tokio::select! {
            ev = dl.events.recv() => {
                let Some(ev) = ev else { break };
                match ev {
                    Event::Started { size, filename, ranges, resumed_from, .. } => {
                        let mut line = format!("开始：{filename}  {}", human(size));
                        line.push_str(if ranges { "  可分段" } else { "  单连接" });
                        if resumed_from > 0 {
                            line.push_str(&format!("  从 {} 续传", human(resumed_from)));
                        }
                        eprintln!("{line}");
                    }
                    Event::Progress { downloaded, total, connections, elapsed } => {
                        if tty && !quiet {
                            let pct = if total == 0 { 0.0 } else { 100.0 * downloaded as f64 / total as f64 };
                            let rate = downloaded as f64 / elapsed.as_secs_f64().max(1e-9);
                            eprint!(
                                "\r  {pct:5.1}%  {} / {}  {}/s  {connections} 连接\x1b[K",
                                human(downloaded), human(total), human(rate as u64)
                            );
                            let _ = std::io::stderr().flush();
                        }
                    }
                    Event::Done { path: _, elapsed: _ } => {
                        // 结果统一在 wait() 之后打，这里只把进度行收尾。
                        if tty && !quiet { eprintln!(); }
                    }
                    Event::Failed { reason } => {
                        if tty && !quiet { eprintln!(); }
                        eprintln!("失败：{reason}");
                    }
                }
            }
            _ = &mut sigint, if !cancelled => {
                cancelled = true;
                if tty && !quiet { eprintln!(); }
                eprintln!("收到 Ctrl+C，停下并保存进度…");
                handle.cancel();
            }
        }
    }

    match dl.wait().await {
        Ok(s) => {
            let mut tail = String::new();
            if s.resumed_from > 0 {
                tail.push_str(&format!("，其中 {} 是上次下的", human(s.resumed_from)));
            }
            eprintln!(
                "已保存到 {}（{}，{:.1} 秒{tail}）",
                s.dest.display(),
                human(s.size),
                s.elapsed.as_secs_f64()
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(Error::Cancelled) => {
            eprintln!("已取消，进度已保存；用同一个 URL 再跑一次会接着下。");
            Ok(ExitCode::from(130))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// `Ok(None)` 表示打印用法就够了。
fn parse_args(args: &[String]) -> Result<Option<Args>, String> {
    if args.is_empty() {
        return Err("缺子命令（目前只有 get）".into());
    }
    if matches!(args[0].as_str(), "-h" | "--help" | "help") {
        return Ok(None);
    }
    if args[0] != "get" {
        return Err(format!("不认识的子命令 `{}`（目前只有 get）", args[0]));
    }

    let mut a = Args {
        url: String::new(),
        dir: PathBuf::from("."),
        name: None,
        conns: None,
        rate: None,
        batch: None,
        host_file: true,
        quiet: false,
        trace: false,
    };
    let mut it = args[1..].iter();
    while let Some(arg) = it.next() {
        let mut value = |what: &str| -> Result<String, String> {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{what} 后面缺一个值"))
        };
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "-d" | "--dir" => a.dir = value("--dir")?.into(),
            "-n" | "--name" => a.name = Some(value("--name")?),
            "-c" | "--conns" => {
                let v = value("--conns")?;
                a.conns = Some(
                    v.parse()
                        .map_err(|_| format!("--conns 要一个整数，拿到的是 `{v}`"))?,
                );
            }
            "--rate" => {
                let v = value("--rate")?;
                a.rate = Some(parse_rate(&v)?);
            }
            "--batch" => {
                let v = value("--batch")?;
                a.batch = Some(parse_rate(&v)?);
            }
            "--no-host-file" => a.host_file = false,
            "--trace" => a.trace = true,
            "-q" | "--quiet" => a.quiet = true,
            s if s.starts_with('-') => return Err(format!("不认识的选项 `{s}`")),
            s => {
                if !a.url.is_empty() {
                    return Err(format!("只能下一个 URL，多出来的 `{s}` 不知道给谁"));
                }
                a.url = s.to_owned();
            }
        }
    }
    if a.url.is_empty() {
        return Err("缺 URL：volant-cli get <url>".into());
    }
    Ok(Some(a))
}

/// `8M` / `1.5M` / `500k` / `1048576`，单位按 1000 算（和 `ifstat`、运营商口径一致）。
fn parse_rate(s: &str) -> Result<u64, String> {
    let t = s.trim();
    let (num, mul) = match t.chars().last() {
        Some('k' | 'K') => (&t[..t.len() - 1], 1_000.0),
        Some('m' | 'M') => (&t[..t.len() - 1], 1_000_000.0),
        Some('g' | 'G') => (&t[..t.len() - 1], 1_000_000_000.0),
        _ => (t, 1.0),
    };
    let n: f64 = num
        .trim()
        .parse()
        .map_err(|_| format!("--rate 看不懂 `{s}`（试试 8M、1.5M、500k）"))?;
    if !n.is_finite() || n <= 0.0 {
        return Err(format!("--rate 要大于 0，拿到的是 `{s}`"));
    }
    Ok((n * mul) as u64)
}

/// 跨进程记住「这台主机最多几条连接」：`%LOCALAPPDATA%\Volant\hosts.json`。
fn default_host_file() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| Path::new(&d).join("Volant").join("hosts.json"))
}

fn human(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_specs_are_what_people_type() {
        assert_eq!(parse_rate("8M").unwrap(), 8_000_000);
        assert_eq!(parse_rate("1.5m").unwrap(), 1_500_000);
        assert_eq!(parse_rate("500k").unwrap(), 500_000);
        assert_eq!(parse_rate("1048576").unwrap(), 1_048_576);
        assert!(parse_rate("0").is_err());
        assert!(parse_rate("fast").is_err());
    }

    #[test]
    fn human_reads_like_a_download_manager() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(999), "999 B");
        assert_eq!(human(1536), "1.5 KiB");
        assert_eq!(human(16 << 20), "16.0 MiB");
    }
}
