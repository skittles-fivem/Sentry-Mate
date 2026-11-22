//! Local days, weeks, months and years in a car's time zone (ISO weeks, Monday first; a day the clocks change is 23 or
//! 25 hours long).

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

/// The zone of a name, UTC for one that is not known.
pub fn zone(name: &str) -> Tz {
    name.parse().unwrap_or(Tz::UTC)
}

/// Whether a name is a known IANA zone.
pub fn is_zone(name: &str) -> bool {
    name.parse::<Tz>().is_ok()
}

/// The time zone boundaries built into the program (tzf's simplified world data, about 4 MB, read in place;
/// timezone-boundary-builder's, from OpenStreetMap, ODbL).
fn finder() -> &'static tzf_rs::EmbeddedFinder {
    static F: std::sync::OnceLock<tzf_rs::EmbeddedFinder> = std::sync::OnceLock::new();
    F.get_or_init(tzf_rs::EmbeddedFinder::new)
}

/// The IANA zone a position is in, looked up in the boundaries built in: nothing is sent anywhere. At sea it is
/// the nautical zone (`Etc/GMT+6`); `None` for a position that is no position or a zone chrono-tz doesn't know.
pub fn zone_at(lat: f64, lon: f64) -> Option<&'static str> {
    if !(lat.is_finite() && lon.is_finite() && (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)) {
        return None;
    }
    Some(finder().get_tz_name(lon, lat)).filter(|z| !z.is_empty() && is_zone(z))
}

/// Where a car's time zone comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneSource {
    /// The owner set it.
    Owner,
    /// The zone the car's newest position is in.
    Position,
    /// The server's `default_time_zone` (no zone set and no position known).
    Default,
}

impl ZoneSource {
    /// As the API writes it: `owner`, `position` or `default`.
    pub fn word(self) -> &'static str {
        match self {
            ZoneSource::Owner => "owner",
            ZoneSource::Position => "position",
            ZoneSource::Default => "default",
        }
    }
}

/// The zone a car's days and clocks follow: the owner's, else the one its position is in, else the server's
/// default.
pub fn pick(typed: Option<&str>, at_position: Option<&str>, default: &str) -> (String, ZoneSource) {
    match (typed, at_position) {
        (Some(z), _) => (z.to_string(), ZoneSource::Owner),
        (None, Some(z)) => (z.to_string(), ZoneSource::Position),
        (None, None) => (default.to_string(), ZoneSource::Default),
    }
}

fn local(ms: u64, tz: Tz) -> DateTime<Tz> {
    let ms = i64::try_from(ms).unwrap_or(i64::MAX);
    DateTime::<Utc>::from_timestamp_millis(ms).unwrap_or_default().with_timezone(&tz)
}

/// The local date of a moment.
pub fn date(ms: u64, tz: Tz) -> NaiveDate {
    local(ms, tz).date_naive()
}

/// `2026-11-16`.
pub fn date_str(ms: u64, tz: Tz) -> String {
    date(ms, tz).format("%Y-%m-%d").to_string()
}

/// Local hour (0-23) and weekday (0 = Monday).
pub fn hour_weekday(ms: u64, tz: Tz) -> (u32, u32) {
    let l = local(ms, tz);
    (l.hour(), l.weekday().num_days_from_monday())
}

/// The moment a local date starts (its first valid local time, for a day that starts in a clock change).
pub fn start_of(d: NaiveDate, tz: Tz) -> u64 {
    let mut t = NaiveTime::MIN;
    for _ in 0..4 {
        if let Some(x) = tz.from_local_datetime(&d.and_time(t)).earliest() {
            return u64::try_from(x.timestamp_millis()).unwrap_or(0);
        }
        t += chrono::Duration::minutes(30);
    }
    u64::try_from(d.and_time(NaiveTime::MIN).and_utc().timestamp_millis()).unwrap_or(0)
}

/// How rows are grouped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Day,
    Week,
    Month,
    Year,
    /// The whole period as one.
    All,
}

impl Period {
    pub fn parse(s: &str) -> Option<Period> {
        match s {
            "day" => Some(Period::Day),
            "week" => Some(Period::Week),
            "month" => Some(Period::Month),
            "year" => Some(Period::Year),
            "all" => Some(Period::All),
            _ => None,
        }
    }
}

/// The local period a moment is in: its start, its end, and its label (`2026-11-16`, `2026-W47`, `2026-11`, `2026`).
pub fn period_of(ms: u64, p: Period, tz: Tz) -> (u64, u64, String) {
    let d = date(ms, tz);
    let (a, b, label) = match p {
        Period::Day => (d, d.succ_opt().unwrap_or(d), d.format("%Y-%m-%d").to_string()),
        Period::Week => {
            let a = d - chrono::Duration::days(i64::from(d.weekday().num_days_from_monday()));
            let w = d.iso_week();
            (a, a + chrono::Duration::days(7), format!("{}-W{:02}", w.year(), w.week()))
        }
        Period::Month => {
            let a = NaiveDate::from_ymd_opt(d.year(), d.month(), 1).unwrap_or(d);
            let b = if d.month() == 12 {
                NaiveDate::from_ymd_opt(d.year() + 1, 1, 1)
            } else {
                NaiveDate::from_ymd_opt(d.year(), d.month() + 1, 1)
            }
            .unwrap_or(d);
            (a, b, format!("{:04}-{:02}", d.year(), d.month()))
        }
        Period::Year => {
            let a = NaiveDate::from_ymd_opt(d.year(), 1, 1).unwrap_or(d);
            let b = NaiveDate::from_ymd_opt(d.year() + 1, 1, 1).unwrap_or(d);
            (a, b, format!("{:04}", d.year()))
        }
        Period::All => return (0, u64::MAX, "All".into()),
    };
    (start_of(a, tz), start_of(b, tz), label)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn local_days_follow_the_zone_and_its_clock_changes() {
        let tz = zone("America/Chicago");
        // 2026-03-08 is the day the clocks go forward in Chicago: 23 hours
        let ms = 1_772_978_400_000; // 2026-03-08T14:00:00Z
        let (a, b, label) = period_of(ms, Period::Day, tz);
        assert_eq!(label, "2026-03-08");
        assert_eq!(b - a, 23 * 3_600_000);
        let (_, _, w) = period_of(ms, Period::Week, tz);
        assert_eq!(w, "2026-W10");
        assert_eq!(period_of(ms, Period::Month, tz).2, "2026-03");
        assert_eq!(period_of(ms, Period::Year, tz).2, "2026");
        assert_eq!(zone("Not/AZone"), Tz::UTC);
        assert!(is_zone("Europe/Berlin") && !is_zone("Mars/Base"));
    }

    #[test]
    fn the_zone_of_a_position_is_looked_up_offline() {
        // made-up points in well-known public places
        assert_eq!(zone_at(41.8800, -87.6300), Some("America/Chicago"), "central Chicago");
        assert_eq!(zone_at(52.5200, 13.4050), Some("Europe/Berlin"), "central Berlin");
        assert_eq!(zone_at(35.6800, 139.7600), Some("Asia/Tokyo"), "central Tokyo");
        assert_eq!(zone_at(-33.8700, 151.2100), Some("Australia/Sydney"), "central Sydney");
        // the middle of the Atlantic: a nautical zone
        let sea = zone_at(30.0, -40.0);
        assert!(sea.is_some_and(|z| z.starts_with("Etc/")), "{sea:?}");
        assert_eq!(zone_at(f64::NAN, 0.0), None);
        assert_eq!(zone_at(91.0, 0.0), None);
    }

    #[test]
    fn the_owners_zone_wins_then_the_position_then_the_default() {
        let owner = pick(Some("Europe/Berlin"), Some("America/Chicago"), "UTC");
        assert_eq!(owner, ("Europe/Berlin".into(), ZoneSource::Owner));
        assert_eq!(pick(None, Some("America/Chicago"), "UTC"), ("America/Chicago".into(), ZoneSource::Position));
        assert_eq!(pick(None, None, "Asia/Tokyo"), ("Asia/Tokyo".into(), ZoneSource::Default));
        assert_eq!(ZoneSource::Position.word(), "position");
    }
}
