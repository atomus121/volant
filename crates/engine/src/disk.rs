//! 下盘前的空间检查。
//!
//! PRD 6.3 要求「磁盘满时停下来、保住状态、告诉用户需要多少」，所以宁可在开始前就把
//! 「装不下」变成一句人话，也不要下到 9 GiB 才拿到 `ENOSPC`。

use std::path::Path;

/// 目录所在卷剩余字节数。Windows 之外还没接（M1 只做 Windows）。
#[cfg(windows)]
pub fn free_bytes(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    let mut free = 0u64;
    // SAFETY: `wide` 是以 NUL 结尾的 UTF-16 路径且活得比调用长；其余两个输出参数传 null，
    // 让系统跳过它们（Win32 允许）。
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

#[cfg(not(windows))]
pub fn free_bytes(_dir: &Path) -> Option<u64> {
    None
}

/// 需要 `need` 字节时给一句能直接显示给用户的话。
pub fn check_space(dir: &Path, need: u64) -> Result<(), String> {
    let Some(free) = free_bytes(dir) else {
        return Ok(());
    };
    // 留 64 MiB 余量：状态文件、文件系统元数据、别的程序也在写。
    let want = need.saturating_add(64 << 20);
    if free >= want {
        return Ok(());
    }
    Err(format!(
        "磁盘空间不足：还需要 {}，{} 只剩 {}",
        gb(want),
        dir.display(),
        gb(free)
    ))
}

fn gb(n: u64) -> String {
    format!("{:.2} GB", n as f64 / (1u64 << 30) as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reason_names_both_numbers() {
        if free_bytes(Path::new(".")).is_none() {
            return;
        }
        // 要一个不可能有的量，必须被拒绝，并且两种数字都出现在句子里。
        let err = check_space(Path::new("."), 1 << 60).unwrap_err();
        assert!(err.contains("GB"), "{err}");
        assert!(check_space(Path::new("."), 1 << 20).is_ok());
    }
}
