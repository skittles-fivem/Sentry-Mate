//! `/cars/{car}/firmware`: the car's firmware fingerprints ([`crate::firmware`]): each module's current values and
//! when they were first and last seen, and the bridge's hint lines counted. The updates found from them are rows of
//! `/cars/{car}/updates` and the timeline ([`super::software`]).
//!
//! Bounds: four modules of at most nine fields, each with a few runs; read from one record.

use super::{QResult, Scoped, car_of, scoped};
use crate::Engine;
use crate::firmware::{FwEvent, History};
use dbc::firmware::MODULES;
use model::CarId;
use serde::Serialize;

/// One value a field had, from when it was first seen to when it was last seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldValue {
    pub hex: String,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldView {
    /// The DBC's name (`BMS_appCrc`).
    pub field: String,
    /// The current value: lowercase hex, 14 digits for a 56-bit hash, 8 for a 32-bit CRC.
    pub hex: String,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    /// The bits of the field (56, 32, 16 or 3).
    pub bits: u32,
    /// Every value it had, oldest first (the current one last).
    pub history: Vec<FieldValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModuleView {
    /// `vcfront`, `bms`, `dir`, `dif`.
    pub id: &'static str,
    pub name: &'static str,
    pub can_id: u32,
    /// The fields the car's logs carried (a field that reads zero is not sent), in the DBC's order.
    pub fields: Vec<FieldView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hints {
    /// `# firmware changed:` lines the bridge wrote (a hint only: the decode of the frames is what counts).
    pub count: u64,
    pub last_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Firmware {
    /// Always the four modules, in this order: front body controller, battery, rear and front drive unit.
    pub modules: Vec<ModuleView>,
    /// When a fingerprint was last seen (null: the logs had none).
    pub updated_ms: Option<u64>,
    /// How many updates were found from them.
    pub updates: u64,
    pub bridge_hints: Hints,
}

/// One field's change in an update, as the updates and timeline rows give it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeView {
    pub module: &'static str,
    pub module_name: &'static str,
    pub field: String,
    pub old: String,
    pub new: String,
}

/// An update's changes, by module then field.
pub(crate) fn changes_of(ev: &FwEvent) -> Vec<ChangeView> {
    let mut out: Vec<(usize, usize, ChangeView)> = ev
        .changes
        .iter()
        .filter_map(|c| {
            let (m, f) = dbc::firmware::field(&c.field)?;
            let mi = MODULES.iter().position(|x| x.id == m.id)?;
            let fi = m.fields.iter().position(|x| x.name == f.name)?;
            Some((
                mi,
                fi,
                ChangeView {
                    module: m.id,
                    module_name: m.name,
                    field: c.field.clone(),
                    old: f.hex(c.old),
                    new: f.hex(c.new),
                },
            ))
        })
        .collect();
    out.sort_by_key(|(m, f, _)| (*m, *f));
    out.into_iter().map(|x| x.2).collect()
}

/// The modules an update changed (`bms`, `vcfront` ...), in the pages' order.
pub(crate) fn modules_of(ev: &FwEvent) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in changes_of(ev) {
        if !out.iter().any(|m| m == c.module) {
            out.push(c.module.to_string());
        }
    }
    out
}

/// The update found from fingerprints with this row id.
pub(crate) fn event(e: &Engine, car: CarId, id: u64) -> QResult<Option<FwEvent>> {
    if !crate::firmware::is_fingerprint(id) {
        return Ok(None);
    }
    Ok(crate::firmware::history(e.db(), car)?.events.into_iter().find(|ev| ev.id == id))
}

fn view(h: &History) -> Firmware {
    let mut updated: Option<u64> = None;
    let modules = MODULES
        .iter()
        .map(|m| ModuleView {
            id: m.id,
            name: m.name,
            can_id: m.can_id,
            fields: m
                .fields
                .iter()
                .filter_map(|f| {
                    let runs = h.runs.get(f.name)?;
                    let cur = runs.last()?;
                    updated = updated.max(Some(cur.last_ms));
                    Some(FieldView {
                        field: f.name.to_string(),
                        hex: f.hex(cur.value),
                        first_seen_ms: cur.first_ms,
                        last_seen_ms: cur.last_ms,
                        bits: f.len,
                        history: runs
                            .iter()
                            .map(|r| FieldValue {
                                hex: f.hex(r.value),
                                first_seen_ms: r.first_ms,
                                last_seen_ms: r.last_ms,
                            })
                            .collect(),
                    })
                })
                .collect(),
        })
        .collect();
    Firmware {
        modules,
        updated_ms: updated,
        updates: h.events.len() as u64,
        bridge_hints: Hints { count: h.hints, last_ms: h.hint_last_ms },
    }
}

/// `/cars/{car}/firmware`.
pub fn firmware(e: &Engine, car: CarId) -> QResult<Scoped<Firmware>> {
    let c = car_of(e, car)?;
    let h = crate::firmware::history(e.db(), car)?;
    Ok(scoped(&c, view(&h)))
}

/// One fingerprint a module had (`kind: "module"`), or one version an import gave the car (`kind: "version"`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryRow {
    /// `<module>:<first_seen_ms>`, or `version:<update id>`.
    pub id: String,
    /// `module` or `version`.
    pub kind: &'static str,
    pub module: Option<&'static str>,
    pub module_name: Option<&'static str>,
    /// The module's main field (its app CRC) as lowercase hex; null on version rows.
    pub fingerprint: Option<String>,
    /// The fingerprint before it (null: the first one seen, and version rows).
    pub previous: Option<String>,
    /// Every field's value over this stretch, in the DBC's order.
    pub fields: Vec<FieldHex>,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    /// When the next one was first seen (null: the current one).
    pub until_ms: Option<u64>,
    pub duration_ms: u64,
    pub current: bool,
    /// The update it came with (null: the first one seen).
    pub update: Option<UpdateRef>,
    /// The version it came with (the update's).
    pub version: Option<String>,
    /// Where it changed: the first drive with the new value, and the log it was first seen in.
    pub changed_in: Option<ChangedIn>,
    #[serde(flatten)]
    pub from: super::Sources,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldHex {
    pub field: String,
    pub hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateRef {
    pub id: String,
    pub version: Option<String>,
    /// `owner`, `build_match`, `teslamate`, `teslafi`, `tesla_api`, or null without a version.
    pub version_source: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedIn {
    pub drive_id: Option<String>,
    pub log: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModuleSummary {
    pub id: &'static str,
    pub name: &'static str,
    pub can_id: u32,
    /// How many fingerprints it has had.
    pub fingerprints: u64,
    pub current: Option<String>,
}

/// One module fingerprint's stretch, for the "versions over time" strip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Span {
    pub module: &'static str,
    pub fingerprint: String,
    pub from_ms: u64,
    pub to_ms: u64,
    pub version: Option<String>,
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Range {
    pub from_ms: Option<u64>,
    pub to_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FirmwareHistory {
    pub items: Vec<HistoryRow>,
    pub next: Option<String>,
    /// Always the four modules, in the pages' order (whatever the filter).
    pub modules: Vec<ModuleSummary>,
    /// Every module fingerprint, oldest first (not paged, not filtered).
    pub spans: Vec<Span>,
    /// The first and last time a fingerprint was seen.
    pub range: Range,
}

/// How far after an update the first drive is looked for.
const DRIVE_LOOKAHEAD_MS: u64 = 60 * 86_400_000;

/// The version an update found from fingerprints shows: `owner`, or `build_match` when its set was named elsewhere.
fn update_ref(e: &Engine, car: CarId, ev: &FwEvent) -> QResult<UpdateRef> {
    let (version, version_source) = match crate::firmware::name_of(e.db(), car, ev)? {
        Some((v, "auto")) => (Some(v), Some("build_match")),
        Some((v, _)) => (Some(v), Some("owner")),
        None => (None, None),
    };
    Ok(UpdateRef { id: ev.id.to_string(), version, version_source })
}

/// The drive where it changed (the first that started after the old fingerprints were last seen and ended once the
/// new ones were seen), and the log the new ones came in.
fn changed_in(e: &Engine, car: CarId, ev: &FwEvent, shown: &super::system::Shown) -> QResult<ChangedIn> {
    let mut drive: Option<String> = None;
    super::drives::each::<model::Drive>(
        e,
        car,
        ev.from_ms,
        ev.to_ms.saturating_add(DRIVE_LOOKAHEAD_MS),
        None,
        shown,
        false,
        |d| {
            if d.start_ms >= ev.from_ms && d.end_ms >= ev.to_ms {
                drive = Some(d.id.to_string());
                return Ok(false);
            }
            Ok(true)
        },
    )?;
    Ok(ChangedIn { drive_id: drive, log: crate::firmware::log_at(e.db(), car, ev.to_ms)? })
}

/// Every module's fingerprints, oldest first per module: the stretches between the updates that changed one of its
/// fields (the update markers' old → new values), with the module's values over each.
fn module_rows(e: &Engine, car: CarId, h: &History) -> QResult<Vec<HistoryRow>> {
    let shown = super::system::Shown::new(e, car, None)?;
    let mut out = Vec::new();
    for m in MODULES.iter() {
        let fields: Vec<(&dbc::firmware::Field, &Vec<crate::firmware::Run>)> =
            m.fields.iter().filter_map(|f| Some((f, h.runs.get(f.name).filter(|r| !r.is_empty())?))).collect();
        let Some((main, _)) = fields.first().copied() else { continue };
        let evs: Vec<&FwEvent> = h
            .events
            .iter()
            .filter(|ev| ev.changes.iter().any(|c| fields.iter().any(|(f, _)| f.name == c.field)))
            .collect();
        let start = fields.iter().filter_map(|(_, rs)| rs.first().map(|r| r.first_ms)).min().unwrap_or(0);
        let end = fields.iter().filter_map(|(_, rs)| rs.iter().map(|r| r.last_ms).max()).max().unwrap_or(start);
        let mut prev: Option<String> = None;
        for k in 0..=evs.len() {
            let came = k.checked_sub(1).map(|j| evs[j]);
            let first = came.map_or(start, |ev| ev.to_ms);
            let last = evs.get(k).map_or(end, |ev| ev.from_ms).max(first);
            let values: Vec<FieldHex> = fields
                .iter()
                .filter_map(|(f, rs)| {
                    let changed = came.and_then(|ev| ev.changes.iter().find(|c| c.field == f.name)).map(|c| c.new);
                    let v = changed.or_else(|| {
                        let at = if k == 0 { rs.first() } else { rs.iter().rev().find(|r| r.first_ms <= first) };
                        at.or(rs.first()).map(|r| r.value)
                    })?;
                    Some(FieldHex { field: f.name.to_string(), hex: f.hex(v) })
                })
                .collect();
            let fp = values.iter().find(|v| v.field == main.name).map(|v| v.hex.clone()).unwrap_or_default();
            let update = came.map(|ev| update_ref(e, car, ev)).transpose()?;
            let changed = came.map(|ev| changed_in(e, car, ev, &shown)).transpose()?;
            out.push(HistoryRow {
                id: format!("{}:{first}", m.id),
                kind: "module",
                module: Some(m.id),
                module_name: Some(m.name),
                fingerprint: Some(fp.clone()),
                previous: prev.replace(fp),
                fields: values,
                first_seen_ms: first,
                last_seen_ms: last,
                until_ms: evs.get(k).map(|ev| ev.to_ms),
                duration_ms: last - first,
                current: k == evs.len(),
                version: update.as_ref().and_then(|u| u.version.clone()),
                update,
                changed_in: changed,
                from: super::Sources::of(model::Source::Live, model::Inputs::SMO),
            });
        }
    }
    Ok(out)
}

/// The versions imported updates gave the car (TeslaMate, TeslaFi: a car version, no module fingerprints).
fn version_rows(e: &Engine, car: CarId) -> QResult<Vec<HistoryRow>> {
    let all: Vec<db::UpdateRow> =
        super::software::all_updates(e, car)?.into_iter().filter(|u| u.update.source.site().is_some()).collect();
    let now = crate::now_ms();
    let mut out = Vec::with_capacity(all.len());
    for (i, u) in all.iter().enumerate() {
        let (version, version_source) = super::software::version_of(e, car, u)?;
        let start = u.update.start_ms;
        let until = all.get(i + 1).map(|n| n.update.start_ms);
        out.push(HistoryRow {
            id: format!("version:{}", u.id),
            kind: "version",
            module: None,
            module_name: None,
            fingerprint: None,
            previous: None,
            fields: Vec::new(),
            first_seen_ms: start,
            last_seen_ms: u.update.end_ms.unwrap_or(start).max(start),
            until_ms: until,
            duration_ms: until.unwrap_or(now).saturating_sub(start),
            current: i + 1 == all.len(),
            update: Some(UpdateRef { id: u.id.to_string(), version: version.clone(), version_source }),
            version,
            changed_in: None,
            from: super::Sources::of(u.update.source, u.update.inputs),
        });
    }
    Ok(out)
}

/// `/cars/{car}/firmware/history`: every fingerprint each module has had (and the versions imports gave), newest
/// first, a page at a time; `module` keeps one module's (no version rows then).
///
/// Bounds: made again from the stored fingerprint history (four modules, an update a few times a year) and the
/// car's update rows, both small; one day's log list and one drive lookup per update.
pub fn history(
    e: &Engine,
    car: CarId,
    module: Option<&str>,
    paging: &super::Paging,
) -> QResult<Scoped<FirmwareHistory>> {
    let c = car_of(e, car)?;
    let module = match module.map(str::trim).filter(|m| !m.is_empty()) {
        None => None,
        Some(m) => Some(
            MODULES
                .iter()
                .find(|x| x.id == m)
                .map(|x| x.id)
                .ok_or_else(|| super::QueryError::bad("module", "vcfront, bms, dir or dif"))?,
        ),
    };
    let limit = paging.limit()?;
    let sig = format!("firmware-history|{car}|{}", module.unwrap_or(""));
    let after = super::read_after(paging, &sig)?;
    let h = crate::firmware::history(e.db(), car)?;
    let modules_rows = module_rows(e, car, &h)?;
    let modules = MODULES
        .iter()
        .map(|m| {
            let rows: Vec<&HistoryRow> = modules_rows.iter().filter(|r| r.module == Some(m.id)).collect();
            ModuleSummary {
                id: m.id,
                name: m.name,
                can_id: m.can_id,
                fingerprints: rows.len() as u64,
                current: rows.last().and_then(|r| r.fingerprint.clone()),
            }
        })
        .collect();
    let mut spans: Vec<(usize, Span)> = modules_rows
        .iter()
        .filter_map(|r| {
            let id = r.module?;
            Some((
                MODULES.iter().position(|x| x.id == id)?,
                Span {
                    module: id,
                    fingerprint: r.fingerprint.clone()?,
                    from_ms: r.first_seen_ms,
                    to_ms: r.until_ms.unwrap_or(r.last_seen_ms),
                    version: r.version.clone(),
                    current: r.current,
                },
            ))
        })
        .collect();
    spans.sort_by_key(|(i, s)| (s.from_ms, *i));
    let range = Range {
        from_ms: modules_rows.iter().map(|r| r.first_seen_ms).min(),
        to_ms: modules_rows.iter().map(|r| r.last_seen_ms).max(),
    };
    let mut rows: Vec<HistoryRow> = match module {
        Some(m) => modules_rows.into_iter().filter(|r| r.module == Some(m)).collect(),
        None => {
            let mut all = modules_rows;
            all.extend(version_rows(e, car)?);
            all
        }
    };
    // ids compare as text (two modules changed by one update share a time; `bms:<ms>` would compare as the number)
    let key = |r: &HistoryRow| (r.first_seen_ms as f64, r.id.replace(':', "@"));
    rows.sort_by(|a, b| super::cmp_key(&key(a), &key(b), true));
    let mut page: Vec<HistoryRow> = rows
        .into_iter()
        .filter(|r| after.as_ref().is_none_or(|k| super::cmp_key(&key(r), k, true) == std::cmp::Ordering::Greater))
        .take(limit + 1)
        .collect();
    let more = page.len() > limit;
    page.truncate(limit);
    let next = match page.last() {
        Some(r) if more => Some(super::make_after(&key(r), &sig)?),
        _ => None,
    };
    Ok(scoped(
        &c,
        FirmwareHistory { items: page, next, modules, spans: spans.into_iter().map(|(_, s)| s).collect(), range },
    ))
}
