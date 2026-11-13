//! 无窗口自检：把控制器从「添加 → 暂停 → 续传 → 完成」跑一遍。
//!
//! 界面没法自动点，但 M4 真正新写的逻辑就是这条链路（暂停 = cancel 并存档，
//! 继续 = 同 URL 同 dest 重新 start）。这里直接驱动 `Control`，验证的是同一套代码。

use std::path::PathBuf;

use eframe::egui;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use crate::control::{Cmd, Control, UiEvent};
use crate::model::{Settings, sidecar};

const TIMEOUT: Duration = Duration::from_secs(120);

pub fn run(url: &str, dir: PathBuf) -> Result<String, String> {
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("建目录失败：{e}"))?;

    let settings = Settings {
        limit: 2,
        connections: 4,
        // 限速：不限速的话文件在「暂停」生效前就下完了，测不到存档。
        rate: Some(2_000_000),
        dir: dir.clone(),
        ..Settings::default()
    };
    let hosts = dir.join("hosts.json");
    let (ctl, rx) = Control::start(
        egui::Context::default(),
        settings,
        hosts,
        dir.join("fallback"),
    );
    let mut log = String::new();

    ctl.send(Cmd::Add {
        id: 1,
        url: url.to_string(),
        headers: Vec::new(),
    });

    let (dest, size) = loop {
        match next(&rx)? {
            UiEvent::Started { dest, size, .. } => break (dest, size),
            UiEvent::Failed { reason, .. } => return Err(format!("解析失败：{reason}")),
            _ => {}
        }
    };
    log.push_str(&format!("started {} size={size}\n", dest.display()));
    if size == 0 {
        return Err("服务端没给长度，无法校验完整性".into());
    }

    loop {
        match next(&rx)? {
            UiEvent::Progress { downloaded, .. } if downloaded > 0 => {
                log.push_str(&format!("progress {downloaded}\n"));
                break;
            }
            UiEvent::Failed { reason, .. } => return Err(format!("下载失败：{reason}")),
            UiEvent::Done { .. } => return Err("限速没起作用，任务在暂停前就完成了".into()),
            _ => {}
        }
    }

    ctl.send(Cmd::Pause(1));
    loop {
        match next(&rx)? {
            UiEvent::Paused { .. } => break,
            UiEvent::Failed { reason, .. } => return Err(format!("暂停时失败：{reason}")),
            UiEvent::Done { .. } => return Err("任务在暂停之前就完成了".into()),
            _ => {}
        }
    }

    let state = sidecar(&dest, ".volant.state");
    let part = sidecar(&dest, ".volant.part");
    let state_len = std::fs::metadata(&state).map(|m| m.len()).unwrap_or(0);
    if state_len == 0 {
        return Err(format!(
            "暂停后没有可用的存档：{}（{} 字节）",
            state.display(),
            state_len
        ));
    }
    if !part.exists() {
        return Err(format!("暂停后 part 丢了：{}", part.display()));
    }
    // part 是按总长度预分配的，长度不能当进度看；真正的续传量用引擎回的 resumed_from。
    log.push_str(&format!("paused state={state_len} bytes\n"));

    ctl.send(Cmd::Resume {
        id: 1,
        url: url.to_string(),
        dest: dest.clone(),
        headers: Vec::new(),
    });

    let resumed = loop {
        match next(&rx)? {
            UiEvent::Started { resumed_from, .. } => break resumed_from,
            UiEvent::Failed { reason, .. } => return Err(format!("续传失败：{reason}")),
            _ => {}
        }
    };
    if resumed == 0 {
        return Err("续传没认出盘上的进度（resumed_from=0）".to_string());
    }
    log.push_str(&format!("resumed from={resumed}\n"));

    let mut first_after = None;
    let (path, done_size) = loop {
        match next(&rx)? {
            UiEvent::Progress { downloaded, .. } if first_after.is_none() => {
                first_after = Some(downloaded)
            }
            UiEvent::Done { path, size, .. } => break (path, size),
            UiEvent::Failed { reason, .. } => return Err(format!("下载失败：{reason}")),
            _ => {}
        }
    };
    if let Some(d) = first_after {
        if d < resumed {
            return Err(format!("续传从 {d} 重来，而不是接上 {resumed}"));
        }
        log.push_str(&format!("resume progress {d}\n"));
    }

    let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if len != done_size || len != size {
        return Err(format!("文件长度 {len} != {done_size} != {size}"));
    }
    if sidecar(&path, ".volant.state").exists() || sidecar(&path, ".volant.part").exists() {
        return Err("完成后还留着 .volant.state / .volant.part".into());
    }
    log.push_str(&format!("done {} {len} bytes\n", path.display()));
    Ok(log)
}

fn next(rx: &Receiver<UiEvent>) -> Result<UiEvent, String> {
    rx.recv_timeout(TIMEOUT).map_err(|e| match e {
        RecvTimeoutError::Timeout => format!("{} 秒内没等到事件", TIMEOUT.as_secs()),
        RecvTimeoutError::Disconnected => "控制器线程已退出".to_string(),
    })
}
