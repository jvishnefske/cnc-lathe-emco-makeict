//! Parsers for the board's /proc and /sys files, pure so they are tested
//! here and not on the board.

/// `/proc/uptime`: whole seconds.
pub fn parse_uptime(s: &str) -> Option<u64> {
    s.split_whitespace().next()?.split('.').next()?.parse().ok()
}

/// `/proc/loadavg`: the three averages.
pub fn parse_loadavg(s: &str) -> Option<[f32; 3]> {
    let mut it = s.split_whitespace().map(|v| v.parse::<f32>().ok());
    Some([it.next()??, it.next()??, it.next()??])
}

/// `/proc/meminfo`: (MemTotal, MemAvailable) in KiB.
pub fn parse_meminfo(s: &str) -> (u64, u64) {
    let get = |key: &str| {
        s.lines()
            .find_map(|l| {
                l.strip_prefix(key)?
                    .trim_start()
                    .strip_prefix(':')
                    .or_else(|| l.strip_prefix(key))
            })
            .and_then(|rest| rest.split_whitespace().find_map(|w| w.parse::<u64>().ok()))
            .unwrap_or(0)
    };
    (get("MemTotal"), get("MemAvailable"))
}

/// An IIO temperature channel (e.g. the Zynq UltraScale+ AMS): milli-degrees
/// Celsius are `(raw + offset) * scale` (Linux IIO ABI).
pub fn iio_milli_c(raw: &str, offset: Option<&str>, scale: Option<&str>) -> Option<i64> {
    let raw: f64 = raw.trim().parse().ok()?;
    let offset: f64 = offset.map(|o| o.trim().parse().ok()).unwrap_or(Some(0.0))?;
    let scale: f64 = scale.map(|s| s.trim().parse().ok()).unwrap_or(Some(1.0))?;
    let v = (raw + offset) * scale;
    v.is_finite().then(|| v.round() as i64)
}

/// A hwmon `temp*_input`: already milli-degrees.
pub fn hwmon_milli_c(s: &str) -> Option<i64> {
    s.trim().parse().ok()
}

/// Non-empty lines (`systemctl list-units --state=failed --no-legend --plain`).
pub fn count_lines(s: &str) -> u32 {
    s.lines().filter(|l| !l.trim().is_empty()).count() as u32
}

/// Clip a string to `max` bytes on a character boundary (keeps telemetry small).
pub fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_files() {
        assert_eq!(parse_uptime("12345.67 23456.78\n"), Some(12345));
        assert_eq!(parse_uptime(""), None);
        assert_eq!(
            parse_loadavg("0.52 0.40 0.31 1/123 4567\n"),
            Some([0.52, 0.40, 0.31])
        );
        assert_eq!(parse_loadavg("0.5"), None);
        let mem =
            "MemTotal:        4008380 kB\nMemFree:          100 kB\nMemAvailable:    3500000 kB\n";
        assert_eq!(parse_meminfo(mem), (4008380, 3500000));
        assert_eq!(parse_meminfo(""), (0, 0));
    }

    #[test]
    fn temperatures() {
        // xilinx-ams style: raw 40000, offset -36058, scale 7.771514892 -> ~30.6 C
        assert_eq!(
            iio_milli_c("40000\n", Some("-36058"), Some("7.771514892")),
            Some(30635)
        );
        assert_eq!(iio_milli_c("41000", None, None), Some(41000));
        assert_eq!(iio_milli_c("x", None, None), None);
        assert_eq!(iio_milli_c("1", Some("x"), None), None);
        assert_eq!(hwmon_milli_c("45250\n"), Some(45250));
    }

    #[test]
    fn misc() {
        assert_eq!(count_lines(""), 0);
        assert_eq!(
            count_lines("a.service loaded failed failed A\n\nb.mount loaded failed failed B\n"),
            2
        );
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("ab", 3), "ab");
        assert_eq!(clip("äää", 3), "ä…");
    }
}
