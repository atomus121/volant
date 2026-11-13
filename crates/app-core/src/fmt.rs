//! 给人看的数字：字节、速度、剩余时间。UI 框架无关。
//!
//! 列表里的宽度是固定的（速度 96 DIP、大小 84 DIP），所以格式必须短且稳定：
//! 「12.4 MB/s」「1.2 GB」「2 分 10 秒」。

const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

pub fn bytes(n: u64) -> String {
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

pub fn speed(bps: f64) -> String {
    if bps.is_nan() || bps < 1.0 {
        return "—".into();
    }
    format!("{}/s", bytes(bps as u64))
}

/// 剩余时间。`None` 或非有限值显示破折号，不显示「inf 秒」。
pub fn eta(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "—".into();
    }
    let s = secs as u64;
    if s >= 3600 {
        format!("{} 小时 {} 分", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{} 分 {} 秒", s / 60, s % 60)
    } else {
        format!("{s} 秒")
    }
}

pub fn percent(part: u64, total: u64) -> String {
    if total == 0 {
        "—".into()
    } else {
        format!("{:.0}%", 100.0 * part as f64 / total as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_keeps_one_decimal_below_hundred() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1024), "1.0 KB");
        assert_eq!(bytes(1234 * 1024), "1.2 MB");
        assert_eq!(bytes(312 * 1024 * 1024), "312 MB");
    }

    #[test]
    fn eta_is_chinese_and_bounded() {
        assert_eq!(eta(0.0), "—");
        assert_eq!(eta(f64::INFINITY), "—");
        assert_eq!(eta(9.0), "9 秒");
        assert_eq!(eta(130.0), "2 分 10 秒");
        assert_eq!(eta(3700.0), "1 小时 1 分");
    }
}
