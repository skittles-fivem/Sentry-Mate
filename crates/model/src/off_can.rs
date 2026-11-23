//! Values no CAN message carries (the software version text, a waiting update, the lock state, the windows, the
//! sunroof, the charge schedule, the route), as the API inputs read them, and the car's newest of each fused across
//! the inputs that gave one.
//!
//! Each input hands over what it read as an [`OffCan`] with the time it was read. [`fuse`] takes them in the order
//! inputs win (the car's [`SourceOrder`], by default SMO, the Tesla API, TeslaFi's API, local TeslaMate), value by
//! value: the first input with a value
//! that is not stale gives it; when every input's value is stale, the first one's is kept, marked stale. A value is
//! stale once it is older than its age limit ([`max_age_ms`]): the lock state an hour after the input stopped
//! reading the car is no longer the lock state. Nothing is guessed: a value an input did not send is `None`.

use crate::{Input, SourceOrder};
use serde::{Deserialize, Serialize};

/// Locked, windows, sunroof: stale an hour after they were read.
pub const BODY_MAX_AGE_MS: u64 = 3_600_000;
/// A waiting or installing update: 6 hours.
pub const UPDATE_MAX_AGE_MS: u64 = 6 * 3_600_000;
/// The charge schedule and the departure time: 12 hours.
pub const SCHEDULE_MAX_AGE_MS: u64 = 12 * 3_600_000;
/// The route: 10 minutes (it changes by the minute while driving).
pub const ROUTE_MAX_AGE_MS: u64 = 600_000;
/// The software version text: 30 days (it changes only with an update).
pub const VERSION_MAX_AGE_MS: u64 = 30 * 86_400_000;

/// What one input read of the values CAN does not carry. Each `None` when the input did not say.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OffCan {
    /// The software version the car reports, e.g. `2026.32.5` (the build hash left out).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The update status word as the input wrote it, lower case: `""` (none waiting), `available`, `scheduled`,
    /// `downloading`, `downloading_wifi_wait`, `installing` ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_status: Option<String>,
    /// The version of the update waiting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_version: Option<String>,
    /// How much of the waiting update is downloaded, %.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_download_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
    /// Windows open (not closed), by seat: driver front, passenger front, driver rear, passenger rear (the Tesla
    /// API's `fd`, `fp`, `rd`, `rp`).
    pub windows: [Option<bool>; 4],
    /// Any window open, from an input that gives one flag for all four (local TeslaMate's `windows_open`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub windows_open: Option<bool>,
    /// The sunroof's position word (`closed`, `open`, `vent` ...), lower case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sunroof_state: Option<String>,
    /// How far the sunroof is open, %.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sunroof_pct: Option<f64>,
    /// When a scheduled charge starts (UTC ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_charge_start_ms: Option<u64>,
    /// The scheduled departure time (UTC ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_departure_ms: Option<u64>,
    /// The navigation's destination name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_destination: Option<String>,
    /// Distance left to the destination, km.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_km: Option<f64>,
    /// Minutes left to the destination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_min: Option<f64>,
    /// The traffic delay on the route, minutes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_traffic_min: Option<f64>,
}

impl OffCan {
    pub fn is_empty(&self) -> bool {
        *self == OffCan::default()
    }
}

/// One input's reading of [`OffCan`] values, with when it was read.
#[derive(Debug, Clone, Copy)]
pub struct Reading<'a> {
    pub input: Input,
    pub at_ms: u64,
    pub values: &'a OffCan,
}

/// A value with when it was read, which input gave it, and whether it is stale (older than its age limit).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sourced<T> {
    pub value: T,
    pub at_ms: u64,
    pub input: Input,
    pub stale: bool,
}

/// The car's newest [`OffCan`] values across its inputs ([`fuse`]), each tagged with the input that gave it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OffCanNow {
    pub version: Option<Sourced<String>>,
    /// An update is waiting (available, scheduled, downloading or installing).
    pub update_available: Option<Sourced<bool>>,
    pub update_version: Option<Sourced<String>>,
    pub update_download_pct: Option<Sourced<f64>>,
    /// An update is installing.
    pub update_installing: Option<Sourced<bool>>,
    pub locked: Option<Sourced<bool>>,
    /// Any window open: from one input's four windows (never mixed), `None` unless it gave all four or one open.
    pub windows_open: Option<Sourced<bool>>,
    /// Driver front, passenger front, driver rear, passenger rear.
    pub windows: [Option<Sourced<bool>>; 4],
    pub sunroof_state: Option<Sourced<String>>,
    pub sunroof_pct: Option<Sourced<f64>>,
    pub scheduled_charge_start_ms: Option<Sourced<u64>>,
    pub scheduled_departure_ms: Option<Sourced<u64>>,
    pub route_destination: Option<Sourced<String>>,
    pub route_km: Option<Sourced<f64>>,
    pub route_min: Option<Sourced<f64>>,
    pub route_traffic_min: Option<Sourced<f64>>,
}

impl OffCanNow {
    pub fn is_empty(&self) -> bool {
        *self == OffCanNow::default()
    }
}

/// Which value an age limit is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Version,
    Update,
    Body,
    Schedule,
    Route,
}

/// How long a value of `kind` stays current after it was read.
pub fn max_age_ms(kind: Kind) -> u64 {
    match kind {
        Kind::Version => VERSION_MAX_AGE_MS,
        Kind::Update => UPDATE_MAX_AGE_MS,
        Kind::Body => BODY_MAX_AGE_MS,
        Kind::Schedule => SCHEDULE_MAX_AGE_MS,
        Kind::Route => ROUTE_MAX_AGE_MS,
    }
}

/// Whether an update status word says an update is waiting, and whether it is installing. `None` for a word not
/// known (never guessed).
pub fn update_words(status: &str) -> Option<(bool, bool)> {
    match status.trim().to_ascii_lowercase().as_str() {
        "" | "none" => Some((false, false)),
        "available" | "scheduled" | "downloading" | "downloading_wifi_wait" | "wifi_wait" | "downloaded" => {
            Some((true, false))
        }
        "installing" => Some((true, true)),
        _ => None,
    }
}

/// The car's newest values across `readings` at `now_ms`, the inputs winning in `order` (see the module docs).
pub fn fuse(now_ms: u64, readings: &[Reading<'_>], order: &SourceOrder) -> OffCanNow {
    let mut rs: Vec<Reading<'_>> = readings.to_vec();
    rs.sort_by_key(|r| (order.rank(r.input), std::cmp::Reverse(r.at_ms)));
    let stale = |at: u64, kind: Kind| now_ms.saturating_sub(at) > max_age_ms(kind);
    fn pick<T: Clone>(
        rs: &[Reading<'_>],
        kind: Kind,
        stale: &dyn Fn(u64, Kind) -> bool,
        get: &dyn Fn(&OffCan) -> Option<T>,
    ) -> Option<Sourced<T>> {
        let all: Vec<Sourced<T>> = rs
            .iter()
            .filter_map(|r| {
                get(r.values).map(|value| Sourced {
                    value,
                    at_ms: r.at_ms,
                    input: r.input,
                    stale: stale(r.at_ms, kind),
                })
            })
            .collect();
        all.iter().find(|s| !s.stale).cloned().or_else(|| all.into_iter().next())
    }
    let st = &stale;
    let words = |o: &OffCan| o.update_status.as_deref().and_then(update_words);
    let mut out = OffCanNow {
        version: pick(&rs, Kind::Version, st, &|o| o.version.clone().filter(|v| !v.trim().is_empty())),
        update_available: pick(&rs, Kind::Update, st, &|o| words(o).map(|w| w.0)),
        update_version: pick(&rs, Kind::Update, st, &|o| {
            o.update_version.clone().filter(|v| !v.trim().is_empty() && words(o).is_none_or(|w| w.0))
        }),
        update_download_pct: pick(&rs, Kind::Update, st, &|o| {
            o.update_download_pct.filter(|p| (0.0..=100.0).contains(p) && words(o).is_none_or(|w| w.0))
        }),
        update_installing: pick(&rs, Kind::Update, st, &|o| words(o).map(|w| w.1)),
        locked: pick(&rs, Kind::Body, st, &|o| o.locked),
        windows_open: pick(&rs, Kind::Body, st, &|o| match o.windows {
            w if w.contains(&Some(true)) => Some(true),
            [Some(false), Some(false), Some(false), Some(false)] => Some(false),
            _ => o.windows_open,
        }),
        windows: [None, None, None, None],
        sunroof_state: pick(&rs, Kind::Body, st, &|o| o.sunroof_state.clone().filter(|s| !s.trim().is_empty())),
        sunroof_pct: pick(&rs, Kind::Body, st, &|o| o.sunroof_pct.filter(|p| (0.0..=100.0).contains(p))),
        scheduled_charge_start_ms: pick(&rs, Kind::Schedule, st, &|o| o.scheduled_charge_start_ms),
        scheduled_departure_ms: pick(&rs, Kind::Schedule, st, &|o| o.scheduled_departure_ms),
        route_destination: pick(&rs, Kind::Route, st, &|o| {
            o.route_destination.clone().filter(|s| !s.trim().is_empty())
        }),
        route_km: pick(&rs, Kind::Route, st, &|o| o.route_km.filter(|k| *k >= 0.0)),
        route_min: pick(&rs, Kind::Route, st, &|o| o.route_min.filter(|k| *k >= 0.0)),
        route_traffic_min: pick(&rs, Kind::Route, st, &|o| o.route_traffic_min.filter(|k| *k >= 0.0)),
    };
    for i in 0..4 {
        out.windows[i] = pick(&rs, Kind::Body, st, &|o| o.windows[i]);
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const H: u64 = 3_600_000;

    #[test]
    fn the_order_inputs_win_in_and_staleness() {
        let api = OffCan { locked: Some(true), version: Some("2026.32.5".into()), ..OffCan::default() };
        let fi = OffCan {
            locked: Some(false),
            windows: [Some(false); 4],
            update_status: Some("available".into()),
            update_version: Some("2026.38.1".into()),
            ..OffCan::default()
        };
        let now = 10 * H;
        let n = fuse(
            now,
            &[
                Reading { input: Input::TeslafiApi, at_ms: now - 60_000, values: &fi },
                Reading { input: Input::TeslaApi, at_ms: now - 120_000, values: &api },
            ],
            &SourceOrder::default(),
        );
        let l = n.locked.unwrap();
        assert_eq!((l.value, l.input, l.stale), (true, Input::TeslaApi, false), "the Tesla API wins over TeslaFi");
        assert_eq!(n.windows_open.as_ref().map(|w| (w.value, w.input)), Some((false, Input::TeslafiApi)));
        assert_eq!(n.update_available.as_ref().map(|w| w.value), Some(true));
        assert_eq!(n.update_installing.as_ref().map(|w| w.value), Some(false));
        assert_eq!(n.version.as_ref().map(|v| v.value.as_str()), Some("2026.32.5"));

        // the Tesla API stopped reading the car two hours ago: TeslaFi's fresh lock state wins
        let n = fuse(
            now,
            &[
                Reading { input: Input::TeslaApi, at_ms: now - 2 * H, values: &api },
                Reading { input: Input::TeslafiApi, at_ms: now - 60_000, values: &fi },
            ],
            &SourceOrder::default(),
        );
        assert_eq!(n.locked.as_ref().map(|l| (l.value, l.input, l.stale)), Some((false, Input::TeslafiApi, false)));
        // every input stopped: the first one's value, marked stale (the version stays current far longer)
        let n =
            fuse(now, &[Reading { input: Input::TeslaApi, at_ms: now - 2 * H, values: &api }], &SourceOrder::default());
        assert_eq!(n.locked.as_ref().map(|l| (l.value, l.stale)), Some((true, true)));
        assert_eq!(n.version.as_ref().map(|v| v.stale), Some(false));
        assert!(fuse(now, &[], &SourceOrder::default()).is_empty());

        // the car puts TeslaFi above the Tesla API: TeslaFi's lock state wins, the Tesla API still gives the version
        let order = SourceOrder::new(&[Input::TeslafiApi, Input::Smo, Input::TeslaApi, Input::TeslamateLocal]).unwrap();
        let n = fuse(
            now,
            &[
                Reading { input: Input::TeslaApi, at_ms: now - 30_000, values: &api },
                Reading { input: Input::TeslafiApi, at_ms: now - 60_000, values: &fi },
            ],
            &order,
        );
        assert_eq!(n.locked.as_ref().map(|l| (l.value, l.input)), Some((false, Input::TeslafiApi)));
        assert_eq!(n.version.as_ref().map(|v| v.input), Some(Input::TeslaApi));
    }

    #[test]
    fn update_words_and_windows_are_never_guessed() {
        assert_eq!(update_words(""), Some((false, false)));
        assert_eq!(update_words("Installing"), Some((true, true)));
        assert_eq!(update_words("something_new"), None);
        let half = OffCan { windows: [Some(false), None, None, None], ..OffCan::default() };
        let n = fuse(0, &[Reading { input: Input::TeslaApi, at_ms: 0, values: &half }], &SourceOrder::default());
        assert_eq!(n.windows_open, None, "three windows unknown: not all closed");
        assert_eq!(n.windows[0].as_ref().map(|w| w.value), Some(false));
        let none = OffCan {
            update_status: Some(String::new()),
            update_version: Some("2026.38.1".into()),
            ..OffCan::default()
        };
        let n = fuse(0, &[Reading { input: Input::TeslaApi, at_ms: 0, values: &none }], &SourceOrder::default());
        assert_eq!(n.update_version, None, "no update waiting: no waiting version");
    }
}
