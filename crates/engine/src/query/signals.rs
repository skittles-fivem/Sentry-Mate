//! `/cars/{car}/signals/latest` and `/cars/{car}/signals`: the car's DBC signals as decoded from its stored logs, by
//! their series names (`DI_vehicleSpeed`, `UI_SOC`, ...), in the units the car sends them.
//!
//! The latest values come from the newest decoded log. A signal over a period is read from the logs of the period
//! (each decoded again, frame by frame, into buckets as they stream past): at most [`MAX_LOGS`] of them per answer,
//! spread evenly over the period when it holds more, so a long period is a thinner read, never an unbounded one.

use super::{List, Paging, QResult, QueryError, Scoped, car_of, paginate, r, scoped};
use crate::Engine;
use crate::records::{self, Outcome};
use model::{CarId, Source};
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
    /// False for signals with named values: they have a latest value only.
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

/// A stored log and its sidecar, decoded frame by frame: `f(time_ms, signal, value)`.
fn decode_log(
    e: &Engine,
    profile: dbc::Profile,
    name: &str,
    mut f: impl FnMut(u64, &'static dbc::Signal, f64),
) -> QResult<bool> {
    let rt = e.handle()?;
    let read = |n: &str| rt.block_on(e.store().read(n)).map_err(|x| crate::Error::Store(x.to_string()));
    let Some(bytes) = read(name)? else { return Ok(false) };
    let side: serde_json::Value =
        read(&format!("{name}.json"))?.and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut decoder = dbc::Decoder::new(profile, &side);
    let res = logfmt::read_log(&bytes, logfmt::Limits::default(), |l| {
        if let logfmt::Line::Frame(fr) = logfmt::Line::parse(l) {
            let t = fr.time_us / 1000;
            for (sig, v) in decoder.decode(&fr) {
                if v.is_finite() {
                    f(t, sig, v);
                }
            }
        }
    });
    // a log that cannot be read again gives what it gave before the trouble
    Ok(res.is_ok())
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
        decode_log(e, profile, &name, |t, sig, v| {
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
                value_name: named.and_then(|m| m.get(&(v.round() as i64)).cloned()),
                comment: sig.comment.clone().filter(|c| !c.is_empty()),
                at_ms: Some(t),
                has_series: named.is_none(),
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
    // paged by place in that order (series names are text that ends in digits now and then: no number sort)
    let rows: Vec<(usize, SignalRow)> = rows.into_iter().enumerate().collect();
    let sig = format!("signals-latest|{car}|{search:?}");
    let page = paginate(rows, |(i, row)| (*i as f64, row.series.clone()), false, &sig, paging, false)?;
    Ok(scoped(&c, List { items: page.items.into_iter().map(|(_, row)| row).collect(), next: page.next, total: None }))
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
        if sig.values.is_some() {
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
    for (name, _) in &chosen {
        decode_log(e, profile, name, |t, sig, v| {
            if t < a || t >= b {
                return;
            }
            let Some(&k) = wanted.get(sig.series.as_str()) else { return };
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
    let (coverage, state) = super::series::coverage_and_state(e, car, &t_ms, w, b, Some(Source::Can))?;
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
