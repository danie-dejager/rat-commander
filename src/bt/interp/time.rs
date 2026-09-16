//! Dates: DOS dates and times, FILETIME, time_t and OLE times, formatted with
//! 010 Editor's patterns (`MM/dd/yyyy hh:mm:ss`). All times are shown as UTC.

/// A broken-down date and time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub min: u32,
    pub sec: u32,
    pub nanos: u32,
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A calendar date to days since 1970-01-01.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn from_unix(secs: i64, nanos: u32) -> Civil {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    Civil {
        year,
        month,
        day,
        hour: (rem / 3600) as u32,
        min: (rem % 3600 / 60) as u32,
        sec: (rem % 60) as u32,
        nanos,
    }
}

/// A FILETIME (100 ns ticks since 1601-01-01).
pub fn from_filetime(ft: u64) -> Civil {
    const EPOCH_DIFF: i64 = 11_644_473_600;
    let secs = (ft / 10_000_000) as i64 - EPOCH_DIFF;
    from_unix(secs, ((ft % 10_000_000) * 100) as u32)
}

/// An OLE automation date (days since 1899-12-30, fraction is the time).
pub fn from_oletime(d: f64) -> Civil {
    let secs = ((d - 25_569.0) * 86_400.0).round() as i64;
    from_unix(secs, 0)
}

pub fn from_dosdate(d: u16) -> Civil {
    Civil {
        year: 1980 + (d >> 9) as i64,
        month: ((d >> 5) & 0xf) as u32,
        day: (d & 0x1f) as u32,
        ..Civil::default()
    }
}

pub fn from_dostime(t: u16) -> Civil {
    Civil {
        hour: (t >> 11) as u32,
        min: ((t >> 5) & 0x3f) as u32,
        sec: ((t & 0x1f) * 2) as u32,
        ..Civil::default()
    }
}

/// Format with 010 Editor's tokens: `yyyy yy MM M dd d hh HH h mm m ss s zzz`
/// and `AP`/`ap` (switching `h` to 12-hour).
pub fn format(c: &Civil, pattern: &str) -> String {
    let twelve = pattern.contains("AP") || pattern.contains("ap");
    let hour12 = match c.hour % 12 {
        0 => 12,
        h => h,
    };
    let b = pattern.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let run = b[i..].iter().take_while(|&&x| x == b[i]).count();
        let tok = b[i];
        let hours = if twelve { hour12 } else { c.hour };
        match (tok, run) {
            (b'y', 4..) => out.push_str(&format!("{:04}", c.year)),
            (b'y', _) => out.push_str(&format!("{:02}", c.year.rem_euclid(100))),
            (b'M', 2..) => out.push_str(&format!("{:02}", c.month)),
            (b'M', _) => out.push_str(&c.month.to_string()),
            (b'd', 2..) => out.push_str(&format!("{:02}", c.day)),
            (b'd', _) => out.push_str(&c.day.to_string()),
            (b'h' | b'H', 2..) => out.push_str(&format!("{hours:02}")),
            (b'h' | b'H', _) => out.push_str(&hours.to_string()),
            (b'm', 2..) => out.push_str(&format!("{:02}", c.min)),
            (b'm', _) => out.push_str(&c.min.to_string()),
            (b's', 2..) => out.push_str(&format!("{:02}", c.sec)),
            (b's', _) => out.push_str(&c.sec.to_string()),
            (b'z', 3..) => out.push_str(&format!("{:03}", c.nanos / 1_000_000)),
            (b'A', _) if b.get(i + 1) == Some(&b'P') => {
                out.push_str(if c.hour < 12 { "AM" } else { "PM" });
                i += 2;
                continue;
            }
            (b'a', _) if b.get(i + 1) == Some(&b'p') => {
                out.push_str(if c.hour < 12 { "am" } else { "pm" });
                i += 2;
                continue;
            }
            _ => {
                for _ in 0..run {
                    out.push(tok as char);
                }
            }
        }
        i += run;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_convert_and_format() {
        let c = from_unix(1_700_000_000, 0);
        assert_eq!(format(&c, "MM/dd/yyyy hh:mm:ss"), "11/14/2023 22:13:20");
        assert_eq!(days_from_civil(2023, 11, 14), 19_675);
        // 2000-01-01 00:00:00 as a FILETIME.
        assert_eq!(
            format(&from_filetime(125_911_584_000_000_000), "yyyy-MM-dd hh:mm"),
            "2000-01-01 00:00"
        );
        let d = from_dosdate((44 << 9) | (7 << 5) | 21);
        assert_eq!(format(&d, "MM/dd/yyyy"), "07/21/2024");
        let t = from_dostime((13 << 11) | (45 << 5) | 15);
        assert_eq!(format(&t, "hh:mm:ss"), "13:45:30");
        assert_eq!(format(&from_oletime(36_526.5), "yyyy-MM-dd hh:mm"), "2000-01-01 12:00");
        assert_eq!(format(&from_unix(3600 * 15, 0), "h:mm AP"), "3:00 PM");
    }
}
