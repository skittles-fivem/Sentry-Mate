//! `/cars/{car}/updates` (and the version an admin types for one), `/fleet/versions`, and `/cars/{car}/events`: a
//! car's drive, charge and software events exactly as MQTT publishes them.
//!
//! A version is only ever what someone said: imported from TeslaMate (`teslamate`) or typed by an admin
//! (`owner`, kept sealed in the db's key/value store, per update). CAN logs carry no firmware version and no module
//! build ids yet, so every update comes from imported history for now and none is `build_change` or `build_match`.
//!
//! Bounds: a car has a few dozen updates a year, so its update rows (start, end, version) are read whole; the charges
//! of an update's stretch are only counted, a db page at a time. The fleet's versions walk the cars in id order and
//! stop once the page is full. Events read drives and charges newest first and stop once the page is full.

use super::drives::{Best, each, page_of};
use super::reports::full_range_of;
use super::system::Shown;
use super::{CarRef, List, Paging, Period, QResult, QueryError, Scoped, car_of, cmp_key, r, read_after, scoped};
use crate::Engine;
use db::UpdateRow;
use model::{Car, CarId, Charge, Drive, Source};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Where typed versions are kept: `<car>/<update id>` → [`Typed`].
const NS: &str = "update-version";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Typed {
    version: String,
    at_ms: u64,
}

fn typed_key(car: CarId, id: u64) -> String {
    format!("{car}/{id}")
}

fn typed(e: &Engine, car: CarId, id: u64) -> QResult<Option<Typed>> {
    Ok(crate::records::get(e.db(), NS, &typed_key(car, id))?)
}

/// `2026.38.4` style: a four-digit year, a week of one or two digits, and up to two more parts of up to three digits.
pub fn valid_version(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    let digits = |p: &str, lo: usize, hi: usize| (lo..=hi).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit());
    (2..=4).contains(&parts.len())
        && digits(parts[0], 4, 4)
        && digits(parts[1], 1, 2)
        && parts[2..].iter().all(|p| digits(p, 1, 3))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UpdateView {
    pub id: String,
    pub car: CarRef,
    pub start_ms: u64,
    /// Null while the update goes on.
    pub end_ms: Option<u64>,
    /// When the next update came; null for the current one.
    pub until_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    /// From the end of the update before.
    pub since_previous_ms: Option<u64>,
    pub version: Option<String>,
    /// `teslamate`, `owner`, `build_match`, or null without a version.
    pub version_source: Option<&'static str>,
    /// `ota_status`, `build_change` or `teslamate`.
    pub detected_by: &'static str,
    pub build_id: Option<String>,
    pub changed_modules: Vec<String>,
    /// Charges that started while the car ran this version.
    pub charges: u64,
    /// The rated range at 100 % at the end of those charges (their median).
    pub full_range_km: Option<f64>,
    /// The pack's own reading (CAN updates only).
    pub full_pack_kwh: Option<f64>,
    pub source: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UpdatesSummary {
    pub count: u64,
    pub median_gap_ms: Option<u64>,
    pub current_version: Option<String>,
    pub current_version_source: Option<&'static str>,
    pub current_version_since_ms: Option<u64>,
    /// When the module builds changed after the current version was named (null: they did not, or no builds known).
    pub build_changed_since: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Updates {
    pub items: Vec<UpdateView>,
    pub next: Option<String>,
    pub summary: UpdatesSummary,
}

/// A car's update rows, oldest first (small: see the module's notes).
fn all_updates(e: &Engine, car: CarId) -> QResult<Vec<UpdateRow>> {
    let mut out = Vec::new();
    super::reports::overlapping::<UpdateRow>(e, car, 0, u64::MAX, None, false, |u| {
        out.push(u);
        Ok(out.len() < 100_000)
    })?;
    out.sort_by_key(|u| (u.update.start_ms, u.id));
    Ok(out)
}

/// The version an update shows and where it came from: typed by an admin, else imported.
fn version_of(e: &Engine, car: CarId, u: &UpdateRow) -> QResult<(Option<String>, Option<&'static str>)> {
    if let Some(t) = typed(e, car, u.id)? {
        return Ok((Some(t.version), Some("owner")));
    }
    let v = Some(u.update.version.trim().to_string()).filter(|v| !v.is_empty());
    let src = v.as_ref().map(|_| if u.update.source == Source::Teslamate { "teslamate" } else { "owner" });
    Ok((v, src))
}

/// The row of `all[i]`: its version, the stretch until the next update, the charges in that stretch.
fn view(e: &Engine, c: &Car, all: &[UpdateRow], i: usize, shown: &Shown) -> QResult<UpdateView> {
    let u = &all[i];
    let up = &u.update;
    let now = crate::now_ms();
    let until = all.get(i + 1).map(|n| n.update.start_ms);
    let prev_end = i.checked_sub(1).map(|j| all[j].update.end_ms.unwrap_or(all[j].update.start_ms));
    let (version, version_source) = version_of(e, c.id, u)?;
    let from = up.end_ms.unwrap_or(up.start_ms);
    let (mut charges, mut ranges) = (0u64, Vec::new());
    each::<Charge>(e, c.id, from, until.unwrap_or(now.saturating_add(1)).max(from), None, shown, false, |ch| {
        charges += 1;
        if ranges.len() < 10_000
            && let Some(v) = full_range_of(&ch)
        {
            ranges.push(v);
        }
        Ok(true)
    })?;
    ranges.sort_by(f64::total_cmp);
    let full_range = match ranges.len() {
        0 => None,
        n if n % 2 == 1 => Some(ranges[n / 2]),
        n => Some((ranges[n / 2 - 1] + ranges[n / 2]) / 2.0),
    };
    let tm = up.source == Source::Teslamate;
    Ok(UpdateView {
        id: u.id.to_string(),
        car: CarRef(c.id),
        start_ms: up.start_ms,
        end_ms: up.end_ms,
        until_ms: until,
        duration_ms: Some(up.end_ms.unwrap_or(now).saturating_sub(up.start_ms)),
        since_previous_ms: prev_end.map(|p| up.start_ms.saturating_sub(p)),
        version,
        version_source,
        detected_by: if tm { "teslamate" } else { "ota_status" },
        build_id: None,
        changed_modules: Vec::new(),
        charges,
        full_range_km: r(full_range, 1),
        full_pack_kwh: None,
        source: super::source_word(up.source),
    })
}

/// What the car runs now: the newest update's version, else the version an admin typed in the car's settings.
fn current(e: &Engine, c: &Car, all: &[UpdateRow]) -> QResult<(Option<String>, Option<&'static str>, Option<u64>)> {
    if let Some(u) = all.last() {
        let (v, s) = version_of(e, c.id, u)?;
        let since = Some(u.update.end_ms.unwrap_or(u.update.start_ms));
        if v.is_some() {
            return Ok((v, s, since));
        }
        let typed = super::fleet::settings_of(e, c.id)?.software_version_typed;
        return Ok((typed.clone(), typed.map(|_| "owner"), since));
    }
    let typed = super::fleet::settings_of(e, c.id)?.software_version_typed;
    Ok((typed.clone(), typed.map(|_| "owner"), None))
}

/// `/cars/{car}/updates`: the updates that started in the period, newest first, and a summary of them (the current
/// version is the car's, whatever the period).
pub fn updates(e: &Engine, car: CarId, period: &Period, paging: &Paging) -> QResult<Scoped<Updates>> {
    let c = car_of(e, car)?;
    let (a, b) = period.bounds()?;
    let limit = paging.limit()?;
    let sig = format!("updates|{car}|{}", period.sig());
    let after = read_after(paging, &sig)?;
    let all = all_updates(e, car)?;
    let shown = Shown::new(e, car, None)?;
    let sel: Vec<usize> = (0..all.len()).filter(|&i| (a..b).contains(&all[i].update.start_ms)).collect();
    let mut gaps: Vec<u64> = sel
        .iter()
        .filter_map(|&i| {
            let p = i.checked_sub(1)?;
            Some(all[i].update.start_ms.saturating_sub(all[p].update.end_ms.unwrap_or(all[p].update.start_ms)))
        })
        .collect();
    gaps.sort_unstable();
    let key = |i: usize| (all[i].update.start_ms as f64, all[i].id.to_string());
    let page: Vec<usize> = sel
        .iter()
        .rev()
        .copied()
        .filter(|&i| after.as_ref().is_none_or(|k| cmp_key(&key(i), k, true) == Ordering::Greater))
        .take(limit + 1)
        .collect();
    let more = page.len() > limit;
    let mut items = Vec::with_capacity(page.len().min(limit));
    for &i in page.iter().take(limit) {
        items.push(view(e, &c, &all, i, &shown)?);
    }
    let next = match page.get(limit.saturating_sub(1)) {
        Some(&i) if more => Some(super::make_after(&key(i), &sig)?),
        _ => None,
    };
    let (cv, cs, since) = current(e, &c, &all)?;
    let median = match gaps.len() {
        0 => None,
        n if n % 2 == 1 => Some(gaps[n / 2]),
        n => Some((gaps[n / 2 - 1] + gaps[n / 2]) / 2),
    };
    let summary = UpdatesSummary {
        count: sel.len() as u64,
        median_gap_ms: median,
        current_version: cv,
        current_version_source: cs,
        current_version_since_ms: since,
        build_changed_since: None,
    };
    Ok(scoped(&c, Updates { items, next, summary }))
}

/// One update as `PATCH` answers it: the row with the car's zone.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UpdateOne {
    #[serde(flatten)]
    pub update: UpdateView,
    pub time_zone: String,
}

/// `PATCH /cars/{car}/updates/{update}` `{ version }`: an admin names the version of an update (`None` forgets the
/// typed one, so an imported version shows again). `no_such_update` for an id the car has not.
pub fn set_version(e: &Engine, car: CarId, id: u64, version: Option<&str>) -> QResult<UpdateOne> {
    let c = car_of(e, car)?;
    let all = all_updates(e, car)?;
    let i = all.iter().position(|u| u.id == id).ok_or(QueryError::NotFound("no_such_update"))?;
    let key = typed_key(car, id);
    match version.map(str::trim) {
        Some(v) if !valid_version(v) => return Err(QueryError::invalid("version", "A version such as 2026.38.4.")),
        Some(v) => crate::records::put(e.db(), NS, &key, &Typed { version: v.to_string(), at_ms: crate::now_ms() })?,
        None => {
            e.db().kv().delete(NS, &key)?;
        }
    }
    let shown = Shown::new(e, car, None)?;
    Ok(UpdateOne { update: view(e, &c, &all, i, &shown)?, time_zone: c.time_zone.clone() })
}

/// The update's start and end, for the change's live event.
pub fn update_span(e: &Engine, car: CarId, id: u64) -> QResult<(u64, u64)> {
    let u: UpdateRow = e.db().get(car, id)?.ok_or(QueryError::NotFound("no_such_update"))?;
    Ok((u.update.start_ms, u.update.end_ms.unwrap_or(u.update.start_ms)))
}

// ---------------------------------------------------------------- /fleet/versions

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VersionRow {
    pub car: CarRef,
    pub name: String,
    pub version: Option<String>,
    pub version_source: Option<&'static str>,
    pub build_changed_since: Option<u64>,
    /// When the newest update ended.
    pub last_update_ms: Option<u64>,
    pub time_zone: String,
}

/// `/fleet/versions`: each car's current version, in car id order; `search` matches the name and the version.
/// Walks the cars after the cursor and stops once the page is full.
pub fn versions(e: &Engine, cars: &[CarId], search: Option<&str>, paging: &Paging) -> QResult<List<VersionRow>> {
    let limit = paging.limit()?;
    let needle = search.map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
    let sig = format!("versions|{cars:?}|{needle:?}");
    let after = read_after(paging, &sig)?;
    let mut ids = cars.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let mut items = Vec::new();
    let mut more = false;
    for car in ids {
        let key = (car as f64, String::new());
        if after.as_ref().is_some_and(|k| cmp_key(&key, k, false) != Ordering::Greater) {
            continue;
        }
        let c = car_of(e, car)?;
        let newest: Option<UpdateRow> =
            e.db().list::<UpdateRow>(car, &db::Query::default().limit(1))?.items.into_iter().next();
        let all: Vec<UpdateRow> = newest.iter().cloned().collect();
        let (version, version_source, _) = current(e, &c, &all)?;
        let row = VersionRow {
            car: CarRef(car),
            name: c.name.clone(),
            version,
            version_source,
            build_changed_since: None,
            last_update_ms: newest.as_ref().map(|u| u.update.end_ms.unwrap_or(u.update.start_ms)),
            time_zone: c.time_zone.clone(),
        };
        if let Some(n) = &needle
            && !row.name.to_lowercase().contains(n.as_str())
            && !row.version.as_deref().is_some_and(|v| v.to_lowercase().contains(n.as_str()))
        {
            continue;
        }
        if items.len() == limit {
            more = true;
            break;
        }
        items.push(row);
    }
    let next = match items.last() {
        Some(last) if more => Some(super::make_after(&(last.car.0 as f64, String::new()), &sig)?),
        _ => None,
    };
    Ok(List { items, next, total: None })
}

// ---------------------------------------------------------------- /cars/{car}/events

/// The event types a car's events list knows.
pub const EVENT_TYPES: [&str; 6] =
    ["drive_started", "drive_ended", "charge_started", "charge_ended", "software_changed", "user_present"];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventRow {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub at_ms: u64,
    pub topic: String,
    pub retained: bool,
    /// Exactly what MQTT publishes.
    pub payload: serde_json::Value,
}

/// `types=` checked: `400` on `types` for one the list does not know. Absent: all.
pub fn parse_types(s: Option<&str>) -> QResult<Vec<&'static str>> {
    let Some(s) = s.map(str::trim).filter(|s| !s.is_empty()) else { return Ok(EVENT_TYPES.to_vec()) };
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let t = EVENT_TYPES.iter().find(|t| **t == part).ok_or_else(|| {
            QueryError::bad(
                "types",
                "drive_started, drive_ended, charge_started, charge_ended, software_changed or user_present",
            )
        })?;
        if !out.contains(t) {
            out.push(*t);
        }
    }
    Ok(out)
}

/// The MQTT settings events are rendered with: those saved under the `mqtt` setting, else the defaults (the default
/// prefix, metric units, no positions).
pub fn mqtt_settings(e: &Engine) -> QResult<mqtt::Settings> {
    Ok(e.db().setting("mqtt")?.and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default())
}

fn message(layout: &mqtt::Layout, ev: &mqtt::Event) -> (String, serde_json::Value) {
    let m = mqtt::render::event_message(layout, ev);
    let payload = serde_json::from_str(&m.payload).unwrap_or(serde_json::Value::String(m.payload));
    (m.topic, payload)
}

/// `/cars/{car}/events`: the car's events newest first, each as MQTT publishes it with `settings` (its prefix, units
/// and whether positions go out). Drives and charges are read newest first, and reading stops once the page is full.
/// `user_present` comes from no signal yet, so it gives no rows.
pub fn events(
    e: &Engine,
    car: CarId,
    types: &[&'static str],
    period: &Period,
    paging: &Paging,
    settings: &mqtt::Settings,
) -> QResult<Scoped<List<EventRow>>> {
    let c = car_of(e, car)?;
    let (a, b) = period.bounds()?;
    let limit = paging.limit()?;
    let mut tsorted = types.to_vec();
    tsorted.sort_unstable();
    let sig = format!("events|{car}|{tsorted:?}|{}", period.sig());
    let after = read_after(paging, &sig)?;
    let layout =
        mqtt::Layout::new(settings).map_err(|err| QueryError::Engine(crate::Error::Damaged(err.to_string())))?;
    let has = |t: &str| types.contains(&t);
    let past = |k: &(f64, String)| after.as_ref().is_none_or(|x| cmp_key(k, x, true) == Ordering::Greater);
    // nothing after the cursor's time (an event never comes before its row's start)
    let top = after.as_ref().map_or(b, |k| b.min((k.0 as u64).saturating_add(1)));
    let shown = Shown::new(e, car, None)?;
    let places = e.places();
    let name_at = |p: Option<&model::Position>| p.and_then(|p| places.place_at(p.lat, p.lon)).map(|pl| pl.name.clone());
    let now = crate::now_ms();
    let mut best: Best<EventRow> = Best::new(limit + 1, true, after.clone());
    let row = |kind: &'static str, rid: &str, at: u64, topic: String, payload| EventRow {
        id: format!("{kind}:{rid}"),
        kind,
        at_ms: at,
        topic,
        retained: false,
        payload,
    };
    let reach = a.saturating_sub(super::reports::LONGEST_DRIVE_MS);
    if has("drive_started") || has("drive_ended") {
        let live = super::drives::ctx(e, &c)?.live;
        let mut n = 0usize;
        each::<Drive>(e, car, reach, top, None, &shown, true, |d| {
            let (sp, ep) = (name_at(d.start.as_ref()), name_at(d.end.as_ref()));
            let id = d.id.to_string();
            if has("drive_ended") && live != Some(d.id) && (a..b).contains(&d.end_ms) {
                let k = (d.end_ms as f64, format!("drive_ended:{id}"));
                if past(&k) {
                    let (t, p) = message(
                        &layout,
                        &mqtt::Event::DriveEnded { drive: Box::new(d.clone()), start_place: sp.clone(), end_place: ep },
                    );
                    best.offer(k, row("drive_ended", &id, d.end_ms, t, p));
                    n += 1;
                }
            }
            if has("drive_started") && (a..b).contains(&d.start_ms) {
                let k = (d.start_ms as f64, format!("drive_started:{id}"));
                if past(&k) {
                    let (t, p) =
                        message(&layout, &mqtt::Event::DriveStarted { drive: Box::new(d.clone()), start_place: sp });
                    best.offer(k, row("drive_started", &id, d.start_ms, t, p));
                    n += 1;
                }
            }
            // drives don't overlap: every older one's events come before these
            Ok(n <= limit)
        })?;
    }
    if has("charge_started") || has("charge_ended") {
        let cx = super::charges::ctx(e, &c)?;
        let mut n = 0usize;
        each::<Charge>(e, car, reach, top, None, &shown, true, |mut ch| {
            let place = name_at(ch.position.as_ref());
            if let Some(k) = super::charges::cost_of(e, &c, &ch)? {
                ch.cost = Some(k.amount);
            }
            let id = ch.id.to_string();
            if has("charge_ended") && cx.live != Some(ch.id) && (a..b).contains(&ch.end_ms) {
                let k = (ch.end_ms as f64, format!("charge_ended:{id}"));
                if past(&k) {
                    let (t, p) = message(
                        &layout,
                        &mqtt::Event::ChargeEnded { charge: Box::new(ch.clone()), place: place.clone() },
                    );
                    best.offer(k, row("charge_ended", &id, ch.end_ms, t, p));
                    n += 1;
                }
            }
            if has("charge_started") && (a..b).contains(&ch.start_ms) {
                let k = (ch.start_ms as f64, format!("charge_started:{id}"));
                if past(&k) {
                    let (t, p) = message(&layout, &mqtt::Event::ChargeStarted { charge: Box::new(ch.clone()), place });
                    best.offer(k, row("charge_started", &id, ch.start_ms, t, p));
                    n += 1;
                }
            }
            Ok(n <= limit)
        })?;
    }
    if has("software_changed") {
        for u in all_updates(e, car)?.iter().rev() {
            let Some(at) = u.update.end_ms.filter(|t| *t <= now) else { continue };
            if !(a..b).contains(&at) {
                continue;
            }
            let id = u.id.to_string();
            let k = (at as f64, format!("software_changed:{id}"));
            if !past(&k) {
                continue;
            }
            let (version, source) = version_of(e, car, u)?;
            let payload = serde_json::json!({
                "event_type": "software_changed",
                "car": car,
                "update": id,
                "at": mqtt::render::iso_utc(at),
                "version": version,
                "version_source": source,
            });
            best.offer(k, row("software_changed", &id, at, layout.topics.event(car), payload));
        }
    }
    let (items, next) = page_of(best, limit, &sig)?;
    Ok(scoped(&c, List { items, next, total: None }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn versions_and_event_types() {
        assert!(valid_version("2026.38.4") && valid_version("2026.2") && valid_version("2026.38.4.1"));
        assert!(!valid_version("26.38") && !valid_version("2026") && !valid_version("2026.38.x") && !valid_version(""));
        assert_eq!(parse_types(None).unwrap().len(), EVENT_TYPES.len());
        assert_eq!(
            parse_types(Some("drive_ended, drive_ended,charge_started")).unwrap(),
            ["drive_ended", "charge_started"]
        );
        assert_eq!(parse_types(Some("door_opened")).unwrap_err().code(), "bad_input");
    }
}
