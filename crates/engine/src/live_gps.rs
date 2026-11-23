//! Whether SMO's GPS fixes are used for the car's live position (its Now: the now view, the place it is at, the
//! fleet's and the car's maps, MQTT's location), by the same decision as its history: a rebuild that finds SMO's
//! fixes out of step with an API's of the same moments ([`timeline::fuse::gps_cross_check`]) leaves them out of
//! positions, and the newest such verdict is kept ([`records::SmoGps`]). While SMO's fixes are left out, the Now's
//! position is the freshest an API (the Tesla API, TeslaFi, local TeslaMate) reported, with its own time and input,
//! or none. A rebuild that finds SMO's fixes agreeing with an API's again (a GPS layout put right, its logs decoded
//! again) trusts them again.

use crate::records::{self, DAY_MS, SmoGps};
use crate::{Engine, Result};
use model::{CarId, Input, Sample, Stream};
use std::collections::BTreeMap;

/// The values of a position: a Now takes them all from one input.
pub(crate) const POSITION_FIELDS: &[&str] = &["lat", "lon", "gps_accuracy_m", "elevation_m"];

/// SMO's fix and an API's are compared without a verdict only when at most this far apart in time (ms).
const LIVE_CROSS_MS: u64 = 3_600_000;

/// A fix of `s`: (ms, latitude, longitude), when it has a real one.
pub(crate) fn fix_of(s: &Sample) -> Option<(u64, f64, f64)> {
    let (lat, lon) = (s.lat?, s.lon?);
    timeline::real_fix(lat, lon).then_some((s.t_ms, lat, lon))
}

/// Whether SMO's fixes are left out of the live position: the newest verdict's word, else (no rebuild has compared
/// the inputs yet) whether SMO's newest fix `smo` is further from an API's newest `api` of about the same time than
/// the car can be ([`timeline::fixes_apart`]).
pub(crate) fn smo_out(verdict: Option<SmoGps>, smo: Option<(u64, f64, f64)>, api: Option<(u64, f64, f64)>) -> bool {
    if let Some(v) = verdict {
        return !v.trusted;
    }
    match (smo, api) {
        (Some(a), Some(b)) => a.0.abs_diff(b.0) <= LIVE_CROSS_MS && timeline::fixes_apart(a, b),
        _ => false,
    }
}

/// The newest of `a` and `b` (by the time of its sample).
fn newest(a: Option<(Sample, Input)>, b: Option<(Sample, Input)>) -> Option<(Sample, Input)> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if b.0.t_ms > a.0.t_ms { b } else { a }),
        (a, b) => a.or(b),
    }
}

/// The newest of `samples` (one input's) with a real fix, and that input.
pub(crate) fn newest_fix_in(samples: &[Sample], input: Input) -> Option<(Sample, Input)> {
    samples.iter().rev().find(|s| fix_of(s).is_some()).map(|s| (s.clone(), input))
}

/// The newest sample with a real fix any API input stored for the car (within two days of its newest data), and that
/// input.
pub(crate) fn newest_api_fix(e: &Engine, car: CarId) -> Result<Option<(Sample, Input)>> {
    let db = e.db();
    let meta = records::meta(db, car)?;
    let mut best = None;
    for input in Input::apis() {
        let Some(last) = meta.api_times(input).0 else { continue };
        for back in [3_600_000, 2 * DAY_MS] {
            let points = db.read_points::<Sample>(car, Stream::from(input), last.saturating_sub(back), last + 1)?;
            if let Some(f) = newest_fix_in(&points, input) {
                best = newest(best, Some(f));
                break;
            }
        }
    }
    Ok(best)
}

/// Puts `api`'s position in place of SMO's in a Now's values (`s`, with when each was recorded in `at` and which input
/// sent it in `from`); with no API position, the Now has none. A position an API sent is kept.
pub(crate) fn put_api_position(
    s: &mut Sample,
    at: &mut BTreeMap<String, u64>,
    from: &mut BTreeMap<String, Input>,
    api: Option<&(Sample, Input)>,
) {
    if from.contains_key("lat") && s.lat.is_some() {
        return;
    }
    let (fix, input) = match api {
        Some((x, i)) => (x.clone(), Some(*i)),
        None => (Sample::default(), None),
    };
    s.lat = fix.lat;
    s.lon = fix.lon;
    s.gps_accuracy_m = fix.gps_accuracy_m;
    s.elevation_m = fix.elevation_m;
    for f in POSITION_FIELDS {
        match input {
            Some(i) if fix.lat.is_some() => {
                at.insert((*f).to_string(), fix.t_ms);
                from.insert((*f).to_string(), i);
            }
            _ => {
                at.remove(*f);
                from.remove(*f);
            }
        }
    }
    // a value the API's fix lacks is not left as SMO's
    for (f, v) in [("gps_accuracy_m", fix.gps_accuracy_m), ("elevation_m", fix.elevation_m)] {
        if v.is_none() {
            at.remove(f);
            from.remove(f);
        }
    }
}

/// `s` without its position (SMO's fixes left out: its other values stay).
pub(crate) fn without_position(s: &Sample) -> Sample {
    Sample { lat: None, lon: None, gps_accuracy_m: None, elevation_m: None, ..s.clone() }
}

impl Engine {
    /// Keeps a rebuild's word on SMO's fixes (`trusted`, from comparing them up to `to_ms` of the car's history), when
    /// no newer word is kept; when they are left out, the car's Now takes an API's position in place of SMO's.
    pub(crate) fn note_smo_gps(&self, car: CarId, trusted: bool, to_ms: u64) -> Result<()> {
        let db = self.db();
        let old = records::smo_gps(db, car)?;
        if old.is_some_and(|o| o.to_ms > to_ms) {
            return Ok(());
        }
        if old.is_none_or(|o| o.trusted != trusted) {
            tracing::info!(car, trusted, "SMO's GPS for the car's live position");
        }
        records::put_smo_gps(db, car, &SmoGps { trusted, to_ms })?;
        if !trusted {
            self.now_position_from_apis(car)?;
        } else if old.is_some_and(|o| !o.trusted) {
            self.now_position_from_smo(car)?;
        }
        Ok(())
    }

    /// SMO's fixes trusted again: its newest fix (of the hour before the car's Now) offered to the Now as any SMO
    /// value is, so it takes the place of an API's that is not much newer.
    fn now_position_from_smo(&self, car: CarId) -> Result<()> {
        let Some(n) = self.db().entry(car)?.and_then(|e| e.summary.now) else { return Ok(()) };
        let points = self.smo_points(car, n.at_ms.saturating_sub(3_600_000), n.at_ms.saturating_add(1))?;
        let Some((s, _)) = newest_fix_in(&points, Input::Smo) else { return Ok(()) };
        let only = Sample {
            t_ms: s.t_ms,
            lat: s.lat,
            lon: s.lon,
            gps_accuracy_m: s.gps_accuracy_m,
            elevation_m: s.elevation_m,
            ..Sample::default()
        };
        self.note_now_from(car, &[only], Input::Smo)
    }

    /// The car's Now with an API's position in place of SMO's ([`put_api_position`]); a Now whose position an API sent
    /// stays as it is.
    pub(crate) fn now_position_from_apis(&self, car: CarId) -> Result<()> {
        let db = self.db();
        let Some(mut n) = db.entry(car)?.and_then(|e| e.summary.now) else { return Ok(()) };
        let mut from = records::now_from(db, car)?;
        if n.sample.lat.is_none() || from.contains_key("lat") {
            return Ok(());
        }
        let mut at = records::now_at(db, car)?;
        let api = newest_api_fix(self, car)?;
        put_api_position(&mut n.sample, &mut at, &mut from, api.as_ref());
        let places = self.places();
        n.place = n.sample.lat.zip(n.sample.lon).and_then(|(a, o)| places.place_at(a, o)).map(|p| p.id);
        records::put_now_at(db, car, &at)?;
        records::put_now_from(db, car, &from)?;
        db.set_now(n)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // made-up: a car parked at 1.5, 20.0; SMO's longitude decoded 16.777216 degrees west of it
    const API: (u64, f64, f64) = (1_000_000, 1.5, 20.0);
    const SMO_OFF: (u64, f64, f64) = (1_060_000, 1.5, 20.0 - 16.777216);
    const SMO_OK: (u64, f64, f64) = (1_060_000, 1.5001, 20.0001);

    #[test]
    fn smo_out_of_step_with_an_api_is_left_out_of_the_live_position() {
        // no verdict yet: SMO's newest fix against an API's of about the same time
        assert!(smo_out(None, Some(SMO_OFF), Some(API)));
        assert!(!smo_out(None, Some(SMO_OK), Some(API)));
        // fixes hours apart say nothing (the car may have driven), nor does SMO or an API alone
        assert!(!smo_out(None, Some((API.0 + 5 * 3_600_000, SMO_OFF.1, SMO_OFF.2)), Some(API)));
        assert!(!smo_out(None, Some(SMO_OFF), None));
        assert!(!smo_out(None, None, Some(API)));
        // a rebuild's verdict decides: left out, or trusted again (a GPS layout put right)
        assert!(smo_out(Some(SmoGps { trusted: false, to_ms: 1 }), Some(SMO_OK), Some(API)));
        assert!(!smo_out(Some(SmoGps { trusted: true, to_ms: 1 }), Some(SMO_OFF), Some(API)));
    }

    fn sample(f: (u64, f64, f64), acc: Option<f64>) -> Sample {
        Sample { t_ms: f.0, lat: Some(f.1), lon: Some(f.2), gps_accuracy_m: acc, ..Sample::default() }
    }

    #[test]
    fn an_apis_position_takes_smos_place_with_its_own_time_and_input() {
        let mut s = Sample { speed_kmh: Some(0.0), elevation_m: Some(1600.0), ..sample(SMO_OFF, Some(25.0)) };
        let mut at: BTreeMap<String, u64> =
            POSITION_FIELDS.iter().map(|f| ((*f).to_string(), SMO_OFF.0)).chain([("speed_kmh".into(), 5)]).collect();
        let mut from = BTreeMap::new();
        let api = (sample(API, Some(8.0)), Input::TeslafiApi);
        put_api_position(&mut s, &mut at, &mut from, Some(&api));
        assert_eq!((s.lat, s.lon, s.gps_accuracy_m), (Some(API.1), Some(API.2), Some(8.0)));
        assert_eq!(s.elevation_m, None, "the API's fix has no elevation: SMO's goes with its position");
        assert_eq!((at["lat"], from["lat"], from["lon"]), (API.0, Input::TeslafiApi, Input::TeslafiApi));
        assert!(!at.contains_key("elevation_m") && !from.contains_key("elevation_m"));
        assert_eq!((s.speed_kmh, at["speed_kmh"]), (Some(0.0), 5), "SMO's other values stay");
        // an API's position is kept as it is
        let other = (sample((API.0 + 1, 3.0, 4.0), None), Input::TeslaApi);
        put_api_position(&mut s, &mut at, &mut from, Some(&other));
        assert_eq!((s.lat, from["lat"]), (Some(API.1), Input::TeslafiApi));
        // no API position: none rather than SMO's
        let mut s = sample(SMO_OFF, Some(25.0));
        let mut at: BTreeMap<String, u64> = [("lat".to_string(), SMO_OFF.0)].into();
        let mut from = BTreeMap::new();
        put_api_position(&mut s, &mut at, &mut from, None);
        assert_eq!((s.lat, s.lon, s.gps_accuracy_m), (None, None, None));
        assert!(at.is_empty() && from.is_empty());
    }

    #[test]
    fn the_newest_api_fix_wins() {
        let a = newest_fix_in(&[sample(API, None), Sample { t_ms: API.0 + 9, ..Sample::default() }], Input::TeslaApi);
        assert_eq!(a.as_ref().map(|x| x.0.t_ms), Some(API.0), "a sample without a fix is passed over");
        let b = Some((sample((API.0 + 5, 1.0, 2.0), None), Input::TeslamateLocal));
        assert_eq!(newest(a.clone(), b).map(|x| x.1), Some(Input::TeslamateLocal));
        assert_eq!(newest(a, None).map(|x| x.1), Some(Input::TeslaApi));
        assert!(newest_fix_in(&[sample((1, 0.0, 0.0), None)], Input::TeslaApi).is_none(), "a placeholder fix");
    }
}
