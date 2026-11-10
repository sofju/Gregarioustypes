//! Locale-aware formatting of sizes, numbers and timestamps.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::Globalization::{GetDateFormatEx, GetLocaleInfoEx, GetTimeFormatEx, DATE_LONGDATE, DATE_SHORTDATE, LOCALE_STHOUSAND, TIME_FORMAT_FLAGS, TIME_NOSECONDS};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

fn thousands_sep() -> String {
    let mut buf = [0u16; 8];
    let n = unsafe { GetLocaleInfoEx(PCWSTR::null(), LOCALE_STHOUSAND, Some(&mut buf)) };
    if n > 1 {
        String::from_utf16_lossy(&buf[..(n - 1) as usize])
    } else {
        ",".into()
    }
}

/// 1234567 -> "1,234,567" using the user's thousands separator.
pub fn group(n: u64) -> String {
    let digits = n.to_string();
    let sep = thousands_sep();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push_str(&sep);
        }
        out.push(ch);
    }
    out
}

/// Human size in binary units like Explorer: "1.4 GB".
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["bytes", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} {}", if bytes == 1 { "byte" } else { "bytes" });
    }
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else if v >= 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{v:.2} {}", UNITS[u])
    }
}

/// "1.4 GB (1,503,238,553 bytes)".
pub fn size_long(bytes: u64) -> String {
    if bytes < 1024 {
        size(bytes)
    } else {
        format!("{} ({} bytes)", size(bytes), group(bytes))
    }
}

fn wide_to_string(buf: &[u16], n: i32) -> String {
    if n <= 1 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..(n - 1) as usize])
    }
}

/// FILETIME ticks (100 ns since 1601, UTC) -> localized local date and time.
pub fn filetime(ticks: u64) -> String {
    filetime_fmt(ticks, false)
}

/// Compact form for table columns ("08/11/2026 10:48").
pub fn filetime_short(ticks: u64) -> String {
    filetime_fmt(ticks, true)
}

/// Unix seconds -> compact local date and time.
pub fn unix_time_short(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    filetime_short(((secs as u64) + 11_644_473_600) * 10_000_000)
}

fn filetime_fmt(ticks: u64, short: bool) -> String {
    if ticks == 0 {
        return String::new();
    }
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    unsafe {
        if FileTimeToSystemTime(&ft, &mut utc).is_err() || SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_err() {
            return String::new();
        }
        let mut d = [0u16; 128];
        let mut t = [0u16; 64];
        let nd = GetDateFormatEx(PCWSTR::null(), if short { DATE_SHORTDATE } else { DATE_LONGDATE }, Some(&local), PCWSTR::null(), Some(&mut d), PCWSTR::null());
        let nt = GetTimeFormatEx(PCWSTR::null(), if short { TIME_NOSECONDS } else { TIME_FORMAT_FLAGS(0) }, Some(&local), PCWSTR::null(), Some(&mut t));
        format!("{} {}", wide_to_string(&d, nd), wide_to_string(&t, nt))
    }
}

/// Unix seconds -> localized local date and time.
pub fn unix_time(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    filetime(((secs as u64) + 11_644_473_600) * 10_000_000)
}

/// SystemTime -> localized local date and time.
pub fn system_time(t: std::time::SystemTime) -> String {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => filetime(d.as_secs() * 10_000_000 + d.subsec_nanos() as u64 / 100 + 11_644_473_600 * 10_000_000),
        Err(_) => String::new(),
    }
}

/// Seconds -> "1:02:03" / "2:03".
pub fn duration(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sizes() {
        assert_eq!(super::size(0), "0 bytes");
        assert_eq!(super::size(1536), "1.50 KB");
        assert_eq!(super::size(10 * 1024 * 1024), "10.0 MB");
        assert_eq!(super::duration(3723.0), "1:02:03");
    }
}
