//! `/cars/{car}/updates` (and the version an admin types for one), `/fleet/versions`, and `/cars/{car}/events`: a
//! car's drive, charge and software events exactly as MQTT publishes them.
//!
//! A version is only ever what someone said: imported from TeslaMate (`teslamate`), given by the Tesla API
//! (`tesla_api`) or typed by an admin (`owner`, kept sealed in the db's key/value store, per update). CAN logs carry
//! no version number, but the modules' firmware fingerprints ([`crate::firmware`]) show when an update reflashed
//! them: such an update is `detected_by: "fingerprint"`, lists the changed modules and fields, and its version is the
//! one an admin typed on it (`owner`) or on an earlier update to the same fingerprint set (`build_match`, named by
//! `auto`).
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
use model::{Car, CarId, Charge, Drive};
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

/// The name an admin typed on a module's first fingerprint segment (no update came with it).
pub(crate) fn typed_first(e: &Engine, car: CarId, module: &str) -> QResult<Option<String>> {
    Ok(crate::records::get::<Typed>(e.db(), NS, &format!("{car}/first:{module}"))?.map(|t| t.version))
}

/// Types (`Some`) or forgets (`None`) the name of a module's first fingerprint segment.
pub(crate) fn set_typed_first(e: &Engine, car: CarId, module: &str, version: Option<&str>) -> QResult<()> {
    let key = format!("{car}/first:{module}");
    match version {
        Some(v) => crate::records::put(e.db(), NS, &key, &Typed { version: v.to_string(), at_ms: crate::now_ms() })?,
        None => {
            e.db().kv().delete(NS, &key)?;
        }
    }
    Ok(())
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
    /// `teslamate`, `teslafi`, `tesla_api`, `teslafi_api`, `teslamate_local` (a live input's version, named on a
    /// fingerprint update a day or less after it), `owner`, `build_match`, or null without a version.
    pub version_source: Option<&'static str>,
    /// `ota_status`, `fingerprint`, `teslamate` or `tesla_api`.
    pub detected_by: &'static str,
    pub build_id: Option<String>,
    pub changed_modules: Vec<String>,
    /// A fingerprint update's changed fields (old and new values as lowercase hex); empty for the others.
    pub changes: Vec<super::firmware::ChangeView>,
    /// A fingerprint update's version: typed on it (`owner`), or taken from an earlier update to the same fingerprint
    /// set (`auto`); null without a version and on the other updates.
    pub named_by: Option<&'static str>,
    /// A fingerprint update: the last time the old fingerprints were seen and the first time the new ones were.
    pub between_ms: Option<[u64; 2]>,
    /// A fingerprint update with no readings at all for a whole day or more inside `between_ms`: it came somewhere
    /// in there, when is not known (its `start_ms` and `end_ms` are where the new set was first seen, and
    /// `duration_ms` is null).
    pub readings_gap: bool,
    /// Charges that started while the car ran this version.
    pub charges: u64,
    /// The rated range at 100 % at the end of those charges (their median).
    pub full_range_km: Option<f64>,
    /// The pack's own reading (CAN updates only).
    pub full_pack_kwh: Option<f64>,
    #[serde(flatten)]
    pub from: super::Sources,
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
    /// The car's newest update whatever the period (null: none), so a period without one can say when it was.
    pub last_update: Option<LastUpdate>,
}

/// [`UpdatesSummary::last_update`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LastUpdate {
    pub id: String,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub version: Option<String>,
    pub version_source: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Updates {
    pub items: Vec<UpdateView>,
    pub next: Option<String>,
    pub summary: UpdatesSummary,
}

/// A car's update rows, oldest first (small: see the module's notes).
pub(crate) fn all_updates(e: &Engine, car: CarId) -> QResult<Vec<UpdateRow>> {
    let mut out = Vec::new();
    super::reports::overlapping::<UpdateRow>(e, car, 0, u64::MAX, None, false, |u| {
        out.push(u);
        Ok(out.len() < 100_000)
    })?;
    out.sort_by_key(|u| (u.update.start_ms, u.id));
    Ok(out)
}

/// The same install read by both imports, outside the spans both cover (one import has a hole there): rows of the
/// two sites with the same version this close are one update (ms). No version is installed twice in a day.
const TWIN_MS: u64 = 86_400_000;

/// A car's update rows as the lists show them, oldest first: with every source, or every import, an update both
/// imports have is one row, the site's that the span shows where both cover the time ([`Sites`]), else the earlier
/// one of two of the same version within [`TWIN_MS`]. One source alone (`import:teslamate`, `smo`, ...) shows its
/// own rows whatever was chosen.
pub(crate) fn shown_updates(e: &Engine, car: CarId, filter: Option<super::SourceFilter>) -> QResult<Vec<UpdateRow>> {
    let all = all_updates(e, car)?;
    let settle = matches!(filter, None | Some(super::SourceFilter::Import(None)));
    if !settle {
        let keep = |u: &UpdateRow| filter.is_none_or(|f| f.keeps(u.update.source, u.update.inputs));
        return Ok(all.into_iter().filter(keep).collect());
    }
    let two_sites = model::ImportSite::ALL.iter().all(|s| all.iter().any(|u| u.update.source.site() == Some(*s)));
    let sites = if two_sites { super::system::Sites::new(e, car)? } else { super::system::Sites::default() };
    let mut out: Vec<UpdateRow> = Vec::with_capacity(all.len());
    for u in all {
        let up = &u.update;
        if !filter.is_none_or(|f| f.keeps(up.source, up.inputs)) || !sites.keeps(up.source, up.start_ms) {
            continue;
        }
        let twin = up.source.site().filter(|_| !up.version.is_empty()).is_some_and(|site| {
            out.iter()
                .rev()
                .take_while(|k| k.update.start_ms.saturating_add(TWIN_MS) >= up.start_ms)
                .any(|k| k.update.source.site().is_some_and(|other| other != site) && k.update.version == up.version)
        });
        if !twin {
            out.push(u);
        }
    }
    Ok(out)
}

/// The version an update found from fingerprints shows, however it was named: typed on it (`owner`), else from a
/// live input, else from another update to the same set (`build_match`); none when an admin cleared it.
pub(crate) fn fw_version_of(
    e: &Engine,
    car: CarId,
    ev: &crate::firmware::FwEvent,
) -> QResult<(Option<String>, Option<&'static str>)> {
    let (name, cleared) = crate::firmware::naming_of(e.db(), car, ev)?;
    // typed on it wins, and a name an admin cleared stays cleared
    if let Some((v, "owner")) = &name {
        return Ok((Some(v.clone()), Some("owner")));
    }
    if cleared {
        return Ok((None, None));
    }
    if let Some((v, input)) = input_named(e, car, ev.from_ms, ev.to_ms)? {
        return Ok((Some(v), Some(model::Origin::from(input).kind())));
    }
    Ok(name.map_or((None, None), |(v, _)| (Some(v), Some("build_match"))))
}

/// The version an update shows and where it came from: typed by an admin, else imported. A fingerprint update's
/// is its name ([`crate::firmware::name_of`]): `owner`, or `build_match` when its set was named on another.
pub(crate) fn version_of(e: &Engine, car: CarId, u: &UpdateRow) -> QResult<(Option<String>, Option<&'static str>)> {
    if crate::firmware::is_fingerprint(u.id) {
        let Some(ev) = super::firmware::event(e, car, u.id)? else { return Ok((None, None)) };
        return fw_version_of(e, car, &ev);
    }
    if let Some(t) = typed(e, car, u.id)? {
        return Ok((Some(t.version), Some("owner")));
    }
    let v = Some(u.update.version.trim().to_string()).filter(|v| !v.is_empty());
    let src = v.as_ref().map(|_| {
        if let Some(site) = u.update.source.site() {
            site.word()
        } else if u.update.inputs.contains(model::Input::TeslaApi) {
            "tesla_api"
        } else {
            "owner"
        }
    });
    Ok((v, src))
}

/// How long after a fingerprint update a live input's new version still names it.
pub(crate) const INPUT_NAME_WINDOW_MS: u64 = 24 * 3600 * 1000;

/// The version a live input said the car ran after the update whose old fingerprints were last seen at `from_ms` and
/// the new ones first at `to_ms`, from the inputs in the car's order (its `source_order`, [`Engine::source_order`]):
///
/// 1. the first version the input began reporting from `to_ms` until a day after it (what it said right after the
///    new set appeared, even when it said that version before too: it may predate the window, or the window be wrong);
/// 2. else the newest one it changed to between `from_ms` and `to_ms` that is not the one it said before `from_ms`
///    (the input saw the update before the logs did);
/// 3. else the one it was reporting when the new set appeared and still reported after it.
fn input_named(e: &Engine, car: CarId, from_ms: u64, to_ms: u64) -> QResult<Option<(String, model::Input)>> {
    let seen = crate::records::input_versions(e.db(), car)?;
    let mut inputs: Vec<model::Input> = seen.iter().map(|s| s.input).collect();
    let order = e.source_order(car);
    inputs.sort_by_key(|&i| order.rank(i));
    inputs.dedup();
    for input in inputs {
        let mine: Vec<&crate::records::VersionSeen> = seen.iter().filter(|s| s.input == input).collect();
        if let Some(s) = pick_named(&mine, from_ms, to_ms) {
            return Ok(Some((s.version.clone(), input)));
        }
    }
    Ok(None)
}

/// [`input_named`] for one input's stretches (oldest first): the stretch that names the update, if one does.
fn pick_named<'a>(
    mine: &[&'a crate::records::VersionSeen],
    from_ms: u64,
    to_ms: u64,
) -> Option<&'a crate::records::VersionSeen> {
    let until = to_ms.saturating_add(INPUT_NAME_WINDOW_MS);
    let ok = |s: &&&crate::records::VersionSeen| valid_version(&s.version);
    let after = mine.iter().filter(ok).find(|s| (to_ms..=until).contains(&s.first_ms));
    let before = mine.iter().rev().find(|s| s.first_ms < from_ms).map(|s| s.version.as_str());
    let changed = |k: usize| k.checked_sub(1).is_some_and(|j| mine[j].version != mine[k].version);
    let during = (0..mine.len())
        .rev()
        .filter(|&k| {
            let s = mine[k];
            (from_ms..to_ms).contains(&s.first_ms) && before != Some(s.version.as_str()) && changed(k)
        })
        .map(|k| &mine[k])
        .find(ok);
    let holding = mine.iter().filter(ok).rev().find(|s| s.first_ms < to_ms && s.last_ms >= to_ms);
    after.or(during).or(holding).copied()
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
    let site = up.source.site();
    let fw = super::firmware::event(e, c.id, u.id)?;
    let named_by = match (&fw, version_source) {
        (Some(_), Some("owner")) => Some("owner"),
        (Some(_), Some(_)) => Some("auto"),
        _ => None,
    };
    Ok(UpdateView {
        id: u.id.to_string(),
        car: CarRef(c.id),
        start_ms: up.start_ms,
        end_ms: up.end_ms,
        until_ms: until,
        // across a gap in the readings, how long it took is not known
        duration_ms: (!fw.as_ref().is_some_and(|f| f.gap))
            .then_some(up.end_ms.unwrap_or(now).saturating_sub(up.start_ms)),
        since_previous_ms: prev_end.map(|p| up.start_ms.saturating_sub(p)),
        version,
        version_source,
        detected_by: if let Some(site) = site {
            site.word()
        } else if fw.is_some() {
            "fingerprint"
        } else if up.inputs.contains(model::Input::TeslaApi) {
            "tesla_api"
        } else {
            "ota_status"
        },
        build_id: fw.as_ref().map(|f| f.set.clone()),
        changed_modules: fw.as_ref().map(super::firmware::modules_of).unwrap_or_default(),
        changes: fw.as_ref().map(super::firmware::changes_of).unwrap_or_default(),
        named_by,
        between_ms: fw.as_ref().map(|f| [f.from_ms, f.to_ms]),
        readings_gap: fw.as_ref().is_some_and(|f| f.gap),
        charges,
        full_range_km: r(full_range, 1),
        full_pack_kwh: None,
        from: super::Sources::of(up.source, up.inputs),
    })
}

/// What the car runs now: a version an admin typed on the newest update; else the version a live input reads (the
/// inputs in their order) when it read it after the newest update started; else the newest update's version; else
/// the version an admin typed in the car's settings.
fn current(e: &Engine, c: &Car, all: &[UpdateRow]) -> QResult<(Option<String>, Option<&'static str>, Option<u64>)> {
    let newest = match all.last() {
        Some(u) => Some((u, version_of(e, c.id, u)?)),
        None => None,
    };
    if let Some((u, (v, Some("owner")))) = &newest {
        return Ok((v.clone(), Some("owner"), Some(u.update.end_ms.unwrap_or(u.update.start_ms))));
    }
    let live = super::now::off_can_now(e, c.id, crate::now_ms())?.version;
    if let Some(l) = live.filter(|l| newest.as_ref().is_none_or(|(u, _)| l.at_ms >= u.update.start_ms)) {
        let since = match &newest {
            Some((u, (v, _))) if v.as_ref().is_none_or(|v| *v == l.value) => {
                Some(u.update.end_ms.unwrap_or(u.update.start_ms))
            }
            _ => crate::records::input_versions(e.db(), c.id)?
                .iter()
                .rev()
                .find(|s| s.input == l.input && s.version == l.value)
                .map(|s| s.first_ms),
        };
        return Ok((Some(l.value), Some(model::Origin::from(l.input).kind()), since));
    }
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
    let all = shown_updates(e, car, None)?;
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
    let (changed_since, named_end) = build_changed_since(e, car, &all)?;
    // after a build change the version shown is the last named one: it ran from when it was named until the change
    // (never from the change itself: that is the unnamed update's end)
    let since = match changed_since {
        Some(t) => since.filter(|s| *s < t).or(named_end.filter(|s| *s < t)),
        None => since,
    };
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
        build_changed_since: changed_since,
        last_update: match all.last() {
            Some(u) => {
                let (version, version_source) = version_of(e, car, u)?;
                Some(LastUpdate {
                    id: u.id.to_string(),
                    start_ms: u.update.start_ms,
                    end_ms: u.update.end_ms,
                    version,
                    version_source,
                })
            }
            None => None,
        },
    };
    Ok(scoped(&c, Updates { items, next, summary }))
}

/// When the module builds changed after the newest named version: the end of the oldest fingerprint update without
/// a version after the newest update with one (null: none since, or no update was ever named); and the end of that
/// named update (null: none named).
fn build_changed_since(e: &Engine, car: CarId, all: &[UpdateRow]) -> QResult<(Option<u64>, Option<u64>)> {
    let mut since = None;
    for u in all.iter().rev() {
        let (v, _) = version_of(e, car, u)?;
        if v.is_some() {
            return Ok((since, Some(u.update.end_ms.unwrap_or(u.update.start_ms))));
        }
        if crate::firmware::is_fingerprint(u.id) {
            since = Some(u.update.end_ms.unwrap_or(u.update.start_ms));
        }
    }
    Ok((None, None))
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
        // a fingerprint update's name is kept with its fingerprint set, so later updates to the same set get it
        v if crate::firmware::is_fingerprint(id) => {
            if !e.fw_set_name(car, id, v)? {
                return Err(QueryError::NotFound("no_such_update"));
            }
        }
        Some(v) => crate::records::put(e.db(), NS, &key, &Typed { version: v.to_string(), at_ms: crate::now_ms() })?,
        None => {
            e.db().kv().delete(NS, &key)?;
        }
    }
    let shown = Shown::new(e, car, None)?;
    // a fingerprint update's row is written again with its name
    let all = if crate::firmware::is_fingerprint(id) { all_updates(e, car)? } else { all };
    let i = all.iter().position(|u| u.id == id).unwrap_or(i);
    // among the updates the list shows (its neighbours there), unless it is one the list leaves out
    let listed = shown_updates(e, car, None)?;
    let (all, i) = match listed.iter().position(|u| u.id == id) {
        Some(k) => (listed, k),
        None => (all, i),
    };
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
        for u in shown_updates(e, car, None)?.iter().rev() {
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

    #[test]
    fn an_input_names_an_update_by_what_it_reported_right_after_the_new_set() {
        use crate::records::VersionSeen;
        const H: u64 = 3_600_000;
        let s = |v: &str, a: u64, b: u64| VersionSeen {
            input: model::Input::TeslamateLocal,
            version: v.into(),
            first_ms: a,
            last_ms: b,
        };
        let pick = |all: &[VersionSeen], from: u64, to: u64| {
            let mine: Vec<&VersionSeen> = all.iter().collect();
            pick_named(&mine, from, to).map(|s| s.version.clone())
        };
        let (from, to) = (100 * H, 400 * H);
        // reported first before the window (it predates it, or the window was too wide) and still after the new
        // set appeared: it names the update
        let all = [s("2026.40.2", 0, 50 * H), s("2026.44.1", 60 * H, 500 * H)];
        assert_eq!(pick(&all, from, to).as_deref(), Some("2026.44.1"));
        // a version begun right after the new set wins over one held from before
        let all = [s("2026.40.2", 0, 401 * H), s("2026.44.1", 402 * H, 500 * H)];
        assert_eq!(pick(&all, from, to).as_deref(), Some("2026.44.1"));
        // a change seen before the logs saw the new set
        let all = [s("2026.40.2", 0, 150 * H), s("2026.44.1", 200 * H, 300 * H)];
        assert_eq!(pick(&all, from, to).as_deref(), Some("2026.44.1"));
        // nothing reported after the new set appeared, and no change: no name
        let all = [s("2026.40.2", 0, 150 * H)];
        assert_eq!(pick(&all, from, to), None);
        // a change more than a day after the new set, and nothing held across it: no name
        let all = [s("2026.40.2", 0, 150 * H), s("2026.44.1", to + 30 * H, to + 40 * H)];
        assert_eq!(pick(&all, from, to), None);
        // not a version: skipped
        let all = [s("unknown", 0, 500 * H)];
        assert_eq!(pick(&all, from, to), None);
    }
}
