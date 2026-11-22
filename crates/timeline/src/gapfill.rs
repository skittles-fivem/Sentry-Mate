//! An import filling what the live history lacks: where SMO and the Tesla API both have nothing for a value at a
//! time, the import shown for that time (TeslaMate's or TeslaFi's) gives it. Rows still come from one source per
//! span; only values are filled, and each filled value is named so the answer can say so.
//!
//! - **Samples** ([`fill`]): inside the live samples' coverage (live seconds at most `cover_ms` apart) every live
//!   second stays, and a value it lacks is taken from the nearest import sample within that value's hold time
//!   ([`FuseParams`], as the Tesla API fills SMO's). No import sample becomes a second of its own there. Outside it,
//!   the import's samples are the seconds when `own` is set (a drive's track gap), else left out; but a live value
//!   still inside its hold time stands for its field there, so such a second keeps only the values live has none of
//!   then (and, with `values` off, is left out wherever a live value holds).
//! - **Rows** ([`fill_drive`], [`fill_charge`]): a live drive's or charge's field that is null (its start, the
//!   charge's position, the odometer, battery level and range at its ends, its mean temperatures, its climb) from the import's samples of
//!   the same time. Distance, energy and costs are never filled: they stay what the live row measured (or null),
//!   so totals are not changed by an import.
//!
//! Pure and deterministic, like [`crate::fuse`].

use crate::calc::{climb, mean, position};
use crate::fuse::{Field, FuseParams, SLOTS, seconds};
use model::{Charge, Drive, Sample};

/// The fused samples and what the import gave them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Filled {
    pub samples: Vec<Sample>,
    /// `(t_ms, mask)` for every live second with values from the import: the bits are [`slot_names`]'s.
    pub filled: Vec<(u64, u64)>,
    /// The times of the import's own seconds (outside the live coverage, with `own`).
    pub own: Vec<u64>,
}

impl Filled {
    /// The mask of the values the import gave the second at `t_ms` (all of them for one of its own seconds).
    pub fn mask_at(&self, t_ms: u64) -> u64 {
        if self.own.binary_search(&t_ms).is_ok() {
            return u64::MAX;
        }
        match self.filled.binary_search_by_key(&t_ms, |m| m.0) {
            Ok(i) => self.filled[i].1,
            Err(_) => 0,
        }
    }
}

/// The sample field names of a mask's bits (`outside_temp_c`, `lat`, `tpms_bar_0`, ...).
pub fn slot_names(mask: u64) -> impl Iterator<Item = &'static str> {
    SLOTS.iter().enumerate().filter(move |(i, _)| mask & (1u64 << i) != 0).map(|(_, s)| s.name)
}

/// The bit of a sample field name (0 for a field an import never fills).
pub fn slot_bit(name: &str) -> u64 {
    SLOTS.iter().position(|s| s.name == name).map_or(0, |i| 1u64 << i)
}

/// How [`fill`] fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct How {
    /// Live seconds this close are one stretch of coverage.
    pub cover_ms: u64,
    /// Outside the coverage, the import's samples are seconds of their own.
    pub own: bool,
    /// Inside it, the import fills the values a live second lacks (off: the live seconds as they are).
    pub values: bool,
}

/// Fills `live` (any order) from `import` (any order). See the module's notes.
pub fn fill(live: &[Sample], import: &[Sample], p: &FuseParams, how: How) -> Filled {
    let How { cover_ms, own, values } = how;
    let live = seconds(live);
    let import = seconds(import);
    if import.is_empty() {
        return Filled { samples: live.into_iter().cloned().collect(), ..Filled::default() };
    }
    let lt: Vec<u64> = live.iter().map(|s| s.t_ms).collect();
    let covered = |t: u64| {
        let i = lt.partition_point(|&x| x < t);
        if lt.get(i) == Some(&t) {
            return true;
        }
        match (i.checked_sub(1).map(|j| lt[j]), lt.get(i)) {
            (Some(a), Some(&b)) => b - a <= cover_ms,
            _ => false,
        }
    };
    let looks: Vec<Field<'_>> = SLOTS.iter().map(|slot| Field::new(&import, slot.has)).collect();
    let mut out: Vec<Sample> = Vec::with_capacity(live.len());
    let mut filled = Vec::new();
    for s in &live {
        let mut x = (*s).clone();
        let mut mask = 0u64;
        for (i, (slot, look)) in SLOTS.iter().zip(&looks).enumerate() {
            if values
                && !(slot.has)(&x)
                && let Some(a) = look.near(x.t_ms, (slot.hold)(p))
            {
                (slot.copy)(&mut x, a);
                mask |= 1u64 << i;
            }
        }
        if mask != 0 {
            filled.push((x.t_ms, mask));
        }
        out.push(x);
    }
    let mut own_t = Vec::new();
    if own {
        // a live value still inside its hold time stands for its field there: the import's second never shows
        // another one (it keeps only the values live has none of then, and with `values` off it is left out)
        let held: Vec<Field<'_>> = SLOTS.iter().map(|slot| Field::new(&live, slot.has)).collect();
        for a in import.iter().filter(|a| !covered(a.t_ms)) {
            let mut x = (*a).clone();
            let mut live_then = false;
            for (slot, look) in SLOTS.iter().zip(&held) {
                if look.near(a.t_ms, (slot.hold)(p)).is_some() {
                    live_then = true;
                    (slot.copy)(&mut x, &Sample::default());
                }
            }
            if live_then && (!values || !SLOTS.iter().any(|slot| (slot.has)(&x))) {
                continue;
            }
            own_t.push(x.t_ms);
            out.push(x);
        }
        out.sort_by_key(|s| s.t_ms);
    }
    Filled { samples: out, filled, own: own_t }
}

/// The import's samples of `[a, b]` (sorted), and the nearest one to `t` with a value within `hold`.
fn nearest<T>(s: &[&Sample], t: u64, hold: u64, get: impl Fn(&Sample) -> Option<T>) -> Option<T> {
    s.iter()
        .filter(|x| x.t_ms.abs_diff(t) <= hold)
        .filter_map(|x| get(x).map(|v| (x.t_ms.abs_diff(t), v)))
        .min_by_key(|(d, _)| *d)
        .map(|(_, v)| v)
}

/// Fills a live drive's null fields from `import` (the samples around it, any order); answers the fields filled
/// (the drive's own names: `start`, `end_odometer_km`, `outside_temp_avg_c`, `ascent_m`, ...).
pub fn fill_drive(d: &mut Drive, import: &[Sample], p: &FuseParams) -> Vec<&'static str> {
    let all = seconds(import);
    let (a, b) = (d.start_ms, d.end_ms.max(d.start_ms));
    let inside: Vec<&Sample> = all.iter().copied().filter(|x| x.t_ms >= a && x.t_ms <= b).collect();
    let mut out = Vec::new();
    let mut set = |name: &'static str, slot: &mut Option<f64>, v: Option<f64>| {
        if slot.is_none()
            && let Some(v) = v.filter(|v| v.is_finite())
        {
            *slot = Some(v);
            out.push(name);
        }
    };
    set("start_odometer_km", &mut d.start_odometer_km, nearest(&all, a, p.odometer_ms, |x| x.odometer_km));
    set("end_odometer_km", &mut d.end_odometer_km, nearest(&all, b, p.odometer_ms, |x| x.odometer_km));
    set("start_soc", &mut d.start_soc, nearest(&all, a, p.battery_ms, |x| x.soc));
    set("end_soc", &mut d.end_soc, nearest(&all, b, p.battery_ms, |x| x.soc));
    set("start_range_km", &mut d.start_range_km, nearest(&all, a, p.battery_ms, |x| x.range_km));
    set("end_range_km", &mut d.end_range_km, nearest(&all, b, p.battery_ms, |x| x.range_km));
    let temp = |get: fn(&Sample) -> Option<f64>| {
        mean(inside.iter().filter_map(|x| get(x))).or_else(|| nearest(&all, a, p.slow_ms, get))
    };
    set("outside_temp_avg_c", &mut d.outside_temp_avg_c, temp(|x| x.outside_temp_c));
    set("inside_temp_avg_c", &mut d.inside_temp_avg_c, temp(|x| x.inside_temp_c));
    if d.ascent_m.is_none() && d.descent_m.is_none() {
        let (up, down) = climb(&inside);
        set("ascent_m", &mut d.ascent_m, up);
        set("descent_m", &mut d.descent_m, down);
    }
    if d.start.is_none()
        && let Some(x) = nearest(&all, a, p.position_ms, |x| x.lat.and(x.lon).map(|_| x.t_ms))
            .and_then(|t| all.iter().find(|x| x.t_ms == t))
    {
        d.start = position(x);
        out.push("start");
    }
    // (never `end`: a live drive without one is one whose data stopped, and the import's position then is where the
    // data stopped, not where the drive went)
    out
}

/// Fills a live charge's null fields from `import` (the samples around it, any order); answers the fields filled
/// (`position`, `start_soc`, `end_range_km`, `outside_temp_avg_c`, `battery_temp_avg_c`, ...).
pub fn fill_charge(c: &mut Charge, import: &[Sample], p: &FuseParams) -> Vec<&'static str> {
    let all = seconds(import);
    let (a, b) = (c.start_ms, c.end_ms.max(c.start_ms));
    let inside: Vec<&Sample> = all.iter().copied().filter(|x| x.t_ms >= a && x.t_ms <= b).collect();
    let mut out = Vec::new();
    let mut set = |name: &'static str, slot: &mut Option<f64>, v: Option<f64>| {
        if slot.is_none()
            && let Some(v) = v.filter(|v| v.is_finite())
        {
            *slot = Some(v);
            out.push(name);
        }
    };
    set("start_soc", &mut c.start_soc, nearest(&all, a, p.battery_ms, |x| x.soc));
    set("end_soc", &mut c.end_soc, nearest(&all, b, p.battery_ms, |x| x.soc));
    set("start_range_km", &mut c.start_range_km, nearest(&all, a, p.battery_ms, |x| x.range_km));
    set("end_range_km", &mut c.end_range_km, nearest(&all, b, p.battery_ms, |x| x.range_km));
    let temp = |get: fn(&Sample) -> Option<f64>| {
        mean(inside.iter().filter_map(|x| get(x))).or_else(|| nearest(&all, a, p.slow_ms, get))
    };
    set("outside_temp_avg_c", &mut c.outside_temp_avg_c, temp(|x| x.outside_temp_c));
    set("battery_temp_avg_c", &mut c.battery_temp_avg_c, temp(|x| x.battery_temp_c));
    if c.position.is_none()
        && let Some(x) = inside.iter().find(|x| x.lat.is_some() && x.lon.is_some()).copied().or_else(|| {
            nearest(&all, a, p.position_ms, |x| x.lat.and(x.lon).map(|_| x.t_ms))
                .and_then(|t| all.iter().copied().find(|x| x.t_ms == t))
        })
    {
        c.position = position(x);
        out.push("position");
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    fn live(t: u64) -> Sample {
        Sample { t_ms: t * 1000, speed_kmh: Some(50.0), outside_temp_c: (t >= 30).then_some(12.0), ..Sample::default() }
    }

    fn imported(t: u64) -> Sample {
        Sample {
            t_ms: t * 1000,
            speed_kmh: Some(40.0),
            outside_temp_c: Some(20.0),
            elevation_m: Some(100.0 + t as f64),
            ..Sample::default()
        }
    }

    #[test]
    fn the_import_fills_only_what_live_lacks() {
        let l: Vec<Sample> = (0..60).map(live).collect();
        let i: Vec<Sample> = (0..6).map(|k| imported(k * 10 + 5)).collect();
        let f = fill(&l, &i, &FuseParams::default(), How { cover_ms: 90_000, own: true, values: true });
        assert_eq!(f.samples.len(), 60, "no import second of its own inside the live coverage");
        assert!(f.samples.iter().all(|x| x.speed_kmh == Some(50.0)), "a live value is never overridden");
        assert!(f.samples[..30].iter().all(|x| x.outside_temp_c == Some(20.0)), "the gap filled");
        assert!(f.samples[30..].iter().all(|x| x.outside_temp_c == Some(12.0)), "live wins where it has one");
        let bit = slot_bit("outside_temp_c");
        assert!(f.mask_at(0) & bit != 0 && f.mask_at(45_000) & bit == 0);
        assert!(f.mask_at(45_000) & slot_bit("elevation_m") != 0);
        assert_eq!(slot_names(bit).collect::<Vec<_>>(), ["outside_temp_c"]);
    }

    #[test]
    fn outside_live_coverage_the_import_seconds_stand_only_when_asked() {
        let l: Vec<Sample> = (0..10).chain(300..310).map(live).collect();
        let i: Vec<Sample> = (0..31).map(|k| imported(k * 10)).collect();
        let f = fill(&l, &i, &FuseParams::default(), How { cover_ms: 90_000, own: true, values: true });
        assert!(f.own.iter().all(|t| *t > 9_000 && *t < 300_000) && !f.own.is_empty());
        assert!(f.samples.windows(2).all(|w| w[0].t_ms < w[1].t_ms));
        let g = fill(&l, &i, &FuseParams::default(), How { cover_ms: 90_000, own: false, values: true });
        assert_eq!(g.samples.len(), 20);
        assert!(g.own.is_empty());
        let off = fill(&l, &i, &FuseParams::default(), How { cover_ms: 90_000, own: true, values: false });
        assert!(off.filled.is_empty(), "values off: nothing filled");
        assert!(off.own.is_empty(), "values off: the live outside temperature holds (300 s) over the whole gap");
        assert!(off.samples.iter().filter(|x| x.t_ms < 10_000).all(|x| x.outside_temp_c.is_none()));
        // where no live value holds, the import's own seconds stand even with values off
        let far: Vec<Sample> = (0..10).chain(1000..1010).map(|t| Sample { outside_temp_c: None, ..live(t) }).collect();
        let off = fill(&far, &i, &FuseParams::default(), How { cover_ms: 90_000, own: true, values: false });
        assert!(off.filled.is_empty() && !off.own.is_empty());
        assert!(off.own.iter().all(|t| *t >= 20_000), "none within the live speed's hold (10 s)");
    }

    #[test]
    fn an_import_second_never_shows_a_value_live_still_holds() {
        // the Tesla API alone, every 100 s (live seconds further apart than the coverage): soc and the outside
        // temperature; the import every 10 s, 3 s later, with its own soc, outside temperature and elevation
        let api =
            |k: u64| Sample { t_ms: k * 100_000, soc: Some(50.0), outside_temp_c: Some(5.0), ..Sample::default() };
        let imp = |k: u64| Sample {
            t_ms: k * 10_000 + 3_000,
            soc: Some(60.0),
            outside_temp_c: Some(25.0),
            elevation_m: Some(10.0),
            ..Sample::default()
        };
        let l: Vec<Sample> = (0..12).map(api).collect();
        let i: Vec<Sample> = (0..115).map(imp).collect();
        let p = FuseParams::default();
        let f = fill(&l, &i, &p, How { cover_ms: 90_000, own: true, values: true });
        assert!(!f.own.is_empty(), "the elevation live has none of");
        for x in f.samples.iter().filter(|x| f.own.binary_search(&x.t_ms).is_ok()) {
            assert_eq!(x.soc, None, "soc 50 holds (120 s) at {}", x.t_ms);
            assert_eq!(x.outside_temp_c, None, "outside 5 holds (300 s) at {}", x.t_ms);
            assert_eq!(x.elevation_m, Some(10.0));
        }
        assert!(f.samples.iter().all(|x| x.soc != Some(60.0) && x.outside_temp_c != Some(25.0)));
        // values off: no import second at all where live values hold
        let off = fill(&l, &i, &p, How { cover_ms: 90_000, own: true, values: false });
        assert!(off.own.is_empty() && off.filled.is_empty());
        assert_eq!(off.samples.len(), 12);
    }

    #[test]
    fn a_drive_gets_only_its_missing_fields() {
        let mut d = Drive {
            id: 1,
            car: 1,
            start_ms: 10_000,
            end_ms: 50_000,
            start: None,
            end: None,
            start_place: None,
            end_place: None,
            start_address: None,
            end_address: None,
            start_odometer_km: None,
            end_odometer_km: None,
            distance_km: None,
            speed_max_kmh: None,
            power_max_kw: None,
            power_min_kw: None,
            energy_used_kwh: None,
            start_soc: None,
            end_soc: None,
            start_range_km: None,
            end_range_km: None,
            outside_temp_avg_c: None,
            inside_temp_avg_c: Some(21.0),
            ascent_m: None,
            descent_m: None,
            source: model::Source::Live,
            inputs: model::Inputs::SMO,
            rules_version: 0,
        };
        let i: Vec<Sample> =
            (0..7).map(|k| Sample { lat: Some(37.0), lon: Some(-122.0), ..imported(k * 10) }).collect();
        let names = fill_drive(&mut d, &i, &FuseParams::default());
        assert_eq!(d.outside_temp_avg_c, Some(20.0));
        assert_eq!(d.inside_temp_avg_c, Some(21.0));
        assert!(d.ascent_m.unwrap() > 30.0);
        assert!(names.contains(&"outside_temp_avg_c") && names.contains(&"ascent_m"));
        assert!(!names.contains(&"inside_temp_avg_c"));
        assert!(d.distance_km.is_none() && d.energy_used_kwh.is_none(), "never distance or energy");
        assert!(d.start.is_some() && names.contains(&"start"));
        assert!(
            d.end.is_none() && !names.contains(&"end"),
            "the data stopped: the import's position is no destination"
        );
        assert!(fill_drive(&mut d, &[], &FuseParams::default()).is_empty());
    }
}
