//! Human-readable formatting helpers.

/// Formats a byte count using binary units, e.g. `1.50 GB`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
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

/// Formats an integer with thousands separators.
pub fn human_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Converts unix seconds to `(year, month, day, hour, minute, second)` in UTC.
pub fn civil(unix: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (
        y,
        m,
        d,
        (secs / 3600) as u32,
        (secs % 3600 / 60) as u32,
        (secs % 60) as u32,
    )
}

/// `YYYY-MM-DD` in UTC, or an empty string for a zero timestamp.
pub fn date(unix: i64) -> String {
    if unix == 0 {
        return String::new();
    }
    let (y, m, d, ..) = civil(unix);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `YYYY-MM-DD HH:MM:SS` in UTC.
pub fn datetime(unix: i64) -> String {
    if unix == 0 {
        return String::new();
    }
    let (y, m, d, h, mi, s) = civil(unix);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// Rough age such as `3 days ago`.
pub fn age(unix: i64, now: i64) -> String {
    if unix == 0 {
        return String::new();
    }
    let d = now - unix;
    if d < 0 {
        return "in the future".into();
    }
    let (n, unit) = if d < 60 {
        return "just now".into();
    } else if d < 3600 {
        (d / 60, "minute")
    } else if d < 86_400 {
        (d / 3600, "hour")
    } else if d < 86_400 * 30 {
        (d / 86_400, "day")
    } else if d < 86_400 * 365 {
        (d / (86_400 * 30), "month")
    } else {
        (d / (86_400 * 365), "year")
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.50 KB");
        assert_eq!(human_size(1024 * 1024 * 250), "250 MB");
        assert_eq!(human_count(1234567), "1,234,567");
        assert_eq!(human_count(999), "999");
    }
    #[test]
    fn dates() {
        assert_eq!(datetime(1), "1970-01-01 00:00:01");
        assert_eq!(date(1_700_000_000), "2023-11-14");
        assert_eq!(date(951_782_400), "2000-02-29");
    }
}
