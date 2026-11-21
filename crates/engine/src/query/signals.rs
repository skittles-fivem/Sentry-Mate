//! `/cars/{car}/signals/latest` and `/cars/{car}/signals`: the car's DBC signals as decoded from its stored logs, by
//! their series names (`DI_vehicleSpeed`, `UI_SOC`, ...), in the units the car sends them.
//!
//! The latest values come from the newest decoded log. A signal over a period is read from the logs of the period
//! (each decoded again, frame by frame, into buckets as they stream past): at most [`MAX_LOGS`] of them per answer,
//! spread evenly over the period when it holds more, and at most [`decoding::MAX_DECODED_BYTES`] of their lines in
//! all (shared out over the logs), so a long period is a thinner read, never an unbounded one. Both stop once their
//! request is gone ([`decoding::with_cancel`]).

use super::{List, Paging, QResult, QueryError, Scoped, car_of, decoding, make_after, r, read_after, scoped};
use crate::Engine;
use crate::records::{self, Outcome};
use model::CarId;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};

/// Logs decoded for one `/signals` answer at most.
pub const MAX_LOGS: usize = 240;
/// `/signals` default window: the last hour.
pub const DEFAULT_WINDOW_MS: u64 = 3_600_000;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SignalRow {
    pub series: String,
    /// `0x243`.
    pub message_id: String,
    /// `vehicle`, `chassis`, or the DBC's own bus name.
    pub bus: String,
    pub value: Option<f64>,
    pub unit: Option<String>,
    /// The value's name, for a signal with named values.
    pub value_name: Option<String>,
    pub comment: Option<String>,
    pub at_ms: Option<u64>,
    /// False for enumerations (named values that cover the signal): they have a latest value only.
    pub has_series: bool,
}

fn bus_word(dbc_bus: &str) -> String {
    match dbc_bus {
        "VehicleBus" => "vehicle".into(),
        "ChassisBus" => "chassis".into(),
        other => other.to_ascii_lowercase(),
    }
}

fn profile_of(c: &model::Car) -> dbc::Profile {
    dbc::Profile::from_name(c.profile().dbc_name())
}

/// A stored log and its sidecar, decoded frame by frame within `budget` bytes of lines: `f(time_ms, signal, value)`.
/// The bytes of lines read (a log that cannot be read again gives what it gave before the trouble).
fn decode_log(
    e: &Engine,
    profile: dbc::Profile,
    name: &str,
    budget: u64,
    mut f: impl FnMut(u64, &'static dbc::Signal, f64),
) -> QResult<u64> {
    if decoding::cancelled() {
        return Err(decoding::stopped());
    }
    let rt = e.handle()?;
    let read = |n: &str| rt.block_on(e.store().read(n)).map_err(|x| crate::Error::Store(x.to_string()));
    let Some(bytes) = read(name)? else { return Ok(0) };
    let side: serde_json::Value =
        read(&format!("{name}.json"))?.and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut decoder = dbc::Decoder::new(profile, &side);
    let done = decoding::read(&bytes, budget, |l| {
        if let logfmt::Line::Frame(fr) = logfmt::Line::parse(l) {
            let t = fr.time_us / 1000;
            for (sig, v) in decoder.decode(&fr) {
                if v.is_finite() {
                    f(t, sig, v);
                }
            }
        }
    })?;
    Ok(done)
}

/// The car's decoded logs meeting `[a, b)`, oldest first.
fn decoded_logs(e: &Engine, car: CarId, a: u64, b: u64) -> QResult<Vec<(String, (u64, u64))>> {
    let meta = records::meta(e.db(), car)?;
    Ok(records::entries_between(e.db(), car, &meta, a, b)?
        .into_iter()
        .filter(|(_, en)| en.usable && matches!(en.outcome, Outcome::Decoded { .. }))
        .map(|(n, en)| {
            let s = en.span();
            (n, s)
        })
        .collect())
}

/// A signal's raw value (what its named values are keyed by) of a physical one.
fn raw_of(sig: &dbc::Signal, v: f64) -> i64 {
    let f = if sig.factor == 0.0 { 1.0 } else { sig.factor };
    ((v - sig.offset) / f).round() as i64
}

/// Whether a signal is an enumeration (gear, charge status, ...), with a latest value only: its named values cover
/// every raw value it can have, or it has names and is a plain number (no scale, offset or unit). A number that names
/// only a sentinel or two (`SNA`, ...) is a series, without those values.
pub(crate) fn enumerated(sig: &dbc::Signal) -> bool {
    let Some(names) = sig.values.as_ref().filter(|m| !m.is_empty()) else { return false };
    if sig.factor == 1.0 && sig.offset == 0.0 && sig.unit.trim().is_empty() {
        return true;
    }
    let by_length = if sig.length < 63 { 1u64 << sig.length } else { u64::MAX };
    let (lo, hi) = (raw_of(sig, sig.min), raw_of(sig, sig.max));
    let by_range = if hi > lo { hi.abs_diff(lo).saturating_add(1) } else { by_length };
    names.len() as u64 >= by_length.min(by_range)
}

/// A value of a series signal that is one of its named sentinels (`SNA`, ...): no reading.
fn sentinel(sig: &dbc::Signal, v: f64) -> bool {
    sig.values.as_ref().is_some_and(|m| m.contains_key(&raw_of(sig, v)))
}

/// The newest decoded log of a car.
fn newest_log(e: &Engine, car: CarId) -> QResult<Option<String>> {
    let meta = records::meta(e.db(), car)?;
    for &d in meta.days.iter().rev() {
        let rec = records::day(e.db(), car, d)?;
        let best = rec
            .logs
            .iter()
            .filter(|(_, en)| en.usable && matches!(en.outcome, Outcome::Decoded { .. }))
            .max_by_key(|(n, en)| (en.span().1, (*n).clone()));
        if let Some((n, _)) = best {
            return Ok(Some(n.clone()));
        }
    }
    Ok(None)
}

/// `/cars/{car}/signals/latest`: every signal decoded from the newest log, by series name. `search` keeps the rows
/// whose series, comment, message id, bus, unit or value name hold every word of it.
pub fn latest(e: &Engine, car: CarId, search: Option<&str>, paging: &Paging) -> QResult<Scoped<List<SignalRow>>> {
    let c = car_of(e, car)?;
    let profile = profile_of(&c);
    let mut last: HashMap<&'static str, (&'static dbc::Signal, f64, u64)> = HashMap::new();
    if let Some(name) = newest_log(e, car)? {
        decode_log(e, profile, &name, decoding::MAX_DECODED_BYTES, |t, sig, v| {
            last.insert(sig.series.as_str(), (sig, v, t));
        })?;
    }
    let words: Vec<String> =
        search.map(|s| s.to_lowercase().split_whitespace().map(str::to_string).collect()).unwrap_or_default();
    let mut rows: Vec<SignalRow> = last
        .into_values()
        .map(|(sig, v, t)| {
            let named = sig.values.as_ref();
            SignalRow {
                series: sig.series.clone(),
                message_id: format!("0x{:03X}", sig.message_id),
                bus: bus_word(&sig.bus),
                value: r(Some(v), 6),
                unit: Some(sig.unit.clone()).filter(|u| !u.is_empty()),
                value_name: named.and_then(|m| m.get(&raw_of(sig, v)).cloned()),
                comment: sig.comment.clone().filter(|c| !c.is_empty()),
                at_ms: Some(t),
                has_series: !enumerated(sig),
            }
        })
        .filter(|row| {
            if words.is_empty() {
                return true;
            }
            let hay = [
                Some(row.series.as_str()),
                row.comment.as_deref(),
                Some(row.message_id.as_str()),
                Some(row.bus.as_str()),
                row.unit.as_deref(),
                row.value_name.as_deref(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .collect();
    rows.sort_by(|a, b| a.series.cmp(&b.series));
    // paged by name (unique), so a signal that comes or goes between two pages moves no other row
    let sig = format!("signals-latest|{car}|{search:?}");
    let limit = paging.limit()?;
    if let Some((_, name)) = read_after(paging, &sig)? {
        rows.retain(|row| row.series.as_str() > name.as_str());
    }
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = match rows.last() {
        Some(last) if more => Some(make_after(&(0.0, last.series.clone()), &sig)?),
        _ => None,
    };
    Ok(scoped(&c, List { items: rows, next, total: None }))
}

/// `/signals` options.
#[derive(Debug, Clone, Default)]
pub struct SignalsQuery {
    pub series: Vec<String>,
    pub from_ms: Option<u64>,
    pub to_ms: Option<u64>,
    /// 2 to 20000, default 2000.
    pub max_points: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SignalColumns {
    pub series: Vec<String>,
    pub units: BTreeMap<String, Option<String>>,
    pub t_ms: Vec<u64>,
    pub values: BTreeMap<String, Vec<Option<f64>>>,
    pub bucket_ms: u64,
    pub coverage_pct: Vec<f64>,
    pub state: Vec<Option<&'static str>>,
    /// Logs of the period, and how many were read (fewer when the period holds more than [`MAX_LOGS`]).
    pub logs: usize,
    pub logs_read: usize,
}

/// `/cars/{car}/signals`: the named signals over the period (default the last hour), as columns: each bucket the
/// mean of the values in it, null where nothing was decoded.
pub fn signals(e: &Engine, car: CarId, q: &SignalsQuery, now_ms: u64) -> QResult<Scoped<SignalColumns>> {
    let c = car_of(e, car)?;
    let (a, b) = super::Period::new(q.from_ms, q.to_ms).window(DEFAULT_WINDOW_MS, now_ms)?;
    let max = q.max_points.unwrap_or(2000);
    if !(2..=20_000).contains(&max) {
        return Err(QueryError::bad("max_points", "2 to 20000"));
    }
    if q.series.is_empty() {
        return Err(QueryError::bad("series", "name at least one series (from /signals/latest)"));
    }
    let profile = profile_of(&c);
    let dbc = profile.dbc();
    let mut wanted: HashMap<&str, usize> = HashMap::new();
    let mut units = BTreeMap::new();
    for (i, name) in q.series.iter().enumerate() {
        let sig = dbc.messages.values().flat_map(|m| m.signals.iter()).find(|s| &s.series == name);
        let Some(sig) = sig else {
            return Err(QueryError::bad("series", &format!("{name} is not a signal of this car")));
        };
        if enumerated(sig) {
            return Err(QueryError::bad("series", &format!("{name} has named values: its latest value only")));
        }
        wanted.insert(sig.series.as_str(), i);
        units.insert(name.clone(), Some(sig.unit.clone()).filter(|u| !u.is_empty()));
    }
    let w = super::series::bucket_for(a, b, max);
    let first = a / w * w;
    let n = usize::try_from((b.saturating_sub(1) / w).saturating_sub(a / w) + 1).unwrap_or(0).min(max as usize);
    let mut sums: Vec<Vec<(f64, u32)>> = vec![vec![(0.0, 0); n]; q.series.len()];
    let logs = decoded_logs(e, car, a, b)?;
    let chosen: Vec<&(String, (u64, u64))> = if logs.len() <= MAX_LOGS {
        logs.iter().collect()
    } else {
        (0..MAX_LOGS).filter_map(|i| logs.get(i * logs.len() / MAX_LOGS)).collect()
    };
    // the lines' budget shared out over the logs: what one leaves unused goes to the ones after it
    let mut left = decoding::MAX_DECODED_BYTES;
    for (j, (name, _)) in chosen.iter().enumerate() {
        let share = left / (chosen.len() - j) as u64;
        left -= decode_log(e, profile, name, share, |t, sig, v| {
            if t < a || t >= b {
                return;
            }
            let Some(&k) = wanted.get(sig.series.as_str()) else { return };
            if sentinel(sig, v) {
                return;
            }
            let Some(i) = t.checked_sub(first).and_then(|d| usize::try_from(d / w).ok()).filter(|&i| i < n) else {
                return;
            };
            let cell = &mut sums[k][i];
            cell.0 += v;
            cell.1 += 1;
        })?;
    }
    let t_ms: Vec<u64> = (0..n as u64).map(|i| first + i * w).collect();
    let mut values = BTreeMap::new();
    for (k, name) in q.series.iter().enumerate() {
        values.insert(
            name.clone(),
            sums[k].iter().map(|&(s, c)| if c == 0 { None } else { r(Some(s / f64::from(c)), 6) }).collect(),
        );
    }
    let (coverage, state) = super::series::coverage_and_state(e, car, &t_ms, w, b, Some(super::SourceFilter::Live))?;
    Ok(scoped(
        &c,
        SignalColumns {
            series: q.series.clone(),
            units,
            t_ms,
            values,
            bucket_ms: w,
            coverage_pct: coverage,
            state,
            logs: logs.len(),
            logs_read: chosen.len(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(factor: f64, offset: f64, unit: &str, length: u32, names: &[(i64, &str)]) -> dbc::Signal {
        dbc::Signal {
            name: "S".into(),
            series: "S".into(),
            message: "M".into(),
            message_id: 0x257,
            bus: "VehicleBus".into(),
            start: 0,
            length,
            little: true,
            signed: false,
            factor,
            offset,
            min: offset,
            max: offset + factor * ((1u64 << length) - 1) as f64,
            unit: unit.into(),
            mux: dbc::Mux::None,
            values: (!names.is_empty()).then(|| names.iter().map(|(k, v)| (*k, (*v).to_string())).collect()),
            comment: None,
        }
    }

    #[test]
    fn a_number_that_names_only_its_sentinel_is_a_series() {
        // a speed in kph, 12 bits at 0.08 from -40: only 4095 is named (SNA)
        let speed = signal(0.08, -40.0, "kph", 12, &[(4095, "SNA")]);
        assert!(!enumerated(&speed));
        let sna = 4095.0 * 0.08 - 40.0;
        assert_eq!(raw_of(&speed, sna), 4095);
        assert!(sentinel(&speed, sna) && !sentinel(&speed, 50.0));
        // a gear: a plain number with names is an enumeration, and so is one whose names cover it
        assert!(enumerated(&signal(1.0, 0.0, "", 3, &[(1, "P"), (2, "R")])));
        let all: Vec<(i64, &str)> = (0..4).map(|i| (i, "x")).collect();
        assert!(enumerated(&signal(0.5, 0.0, "A", 2, &all)));
        // no names: a series
        assert!(!enumerated(&signal(1.0, 0.0, "", 1, &[])));
    }
}
