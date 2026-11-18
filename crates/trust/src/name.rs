//! CAN Bridge log names: `<vehicleKey>_<UTC yyyyMMddTHHmmssZ>_<seq>.log.xz`, and the same plus `.json` for a sidecar.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogName<'a> {
    /// 1 to 64 letters, digits or `-` (per install, not the VIN).
    pub vehicle_key: &'a str,
    /// The file's UTC start, ms since 1970.
    pub started_ms: i64,
    /// 1 to 6 digits; restarts at 1 when the app starts, so sort by time first.
    pub seq: u32,
    pub sidecar: bool,
}

fn num(s: &str) -> Option<i64> {
    if s.bytes().all(|c| c.is_ascii_digit()) { s.parse().ok() } else { None }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's days_from_civil).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day counted from 1970-01-01, as (year, month, day): the inverse of [`days_from_civil`] (Howard
/// Hinnant's civil_from_days).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// `yyyyMMddTHHmmssZ` as ms since 1970.
pub fn parse_stamp(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    if b.len() != 16 || b[8] != b'T' || b[15] != b'Z' {
        return None;
    }
    let (y, mo, d) = (num(&t[0..4])?, num(&t[4..6])?, num(&t[6..8])?);
    let (h, mi, s) = (num(&t[9..11])?, num(&t[11..13])?, num(&t[13..15])?);
    if !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) || h > 23 || mi > 59 || s > 59 {
        return None;
    }
    Some(((days_from_civil(y, mo, d) * 24 + h) * 60 + mi) * 60_000 + s * 1000)
}

impl<'a> LogName<'a> {
    pub fn parse(name: &'a str) -> Option<LogName<'a>> {
        let (stem, sidecar) = if let Some(s) = name.strip_suffix(".log.xz.json") {
            (s, true)
        } else {
            (name.strip_suffix(".log.xz")?, false)
        };
        let (rest, seq) = stem.rsplit_once('_')?;
        let (key, stamp) = rest.rsplit_once('_')?;
        if key.is_empty() || key.len() > 64 || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
            return None;
        }
        if seq.is_empty() || seq.len() > 6 {
            return None;
        }
        let seq = u32::try_from(num(seq)?).ok()?;
        Some(LogName { vehicle_key: key, started_ms: parse_stamp(stamp)?, seq, sidecar })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        let n = LogName::parse("abcdef123456_20261108T143000Z_1.log.xz").unwrap();
        assert_eq!((n.vehicle_key, n.seq, n.sidecar), ("abcdef123456", 1, false));
        assert_eq!(n.started_ms, 1_794_148_200_000);
        assert!(LogName::parse("MY-car_20261108T143000Z_000001.log.xz.json").unwrap().sidecar);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(n.started_ms.div_euclid(86_400_000)), (2026, 11, 8));
        for d in [-800_000, -1, 59, 60, 11_016, 20_000, 2_932_896] {
            let (y, m, day) = civil_from_days(d);
            assert_eq!(days_from_civil(y, i64::from(m), i64::from(day)), d, "{d}");
        }
        for bad in [
            "abc_20261108T143000Z_1.log",
            "ab_c_20261108T143000Z_1.log.xz",
            "abc_20261108T143000_1.log.xz",
            "abc_20261308T143000Z_1.log.xz",
            "abc_20260230T143000Z_1.log.xz",
            "abc_20261108T143000Z_1234567.log.xz",
            "abc_20261108T143000Z_.log.xz",
            "_20261108T143000Z_1.log.xz",
            "../x_20261108T143000Z_1.log.xz",
        ] {
            assert!(LogName::parse(bad).is_none(), "{bad}");
        }
    }
}
