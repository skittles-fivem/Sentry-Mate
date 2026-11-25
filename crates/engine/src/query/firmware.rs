//! `/cars/{car}/firmware`: the car's firmware fingerprints ([`crate::firmware`]): each module's current values and
//! when they were first and last seen, and the bridge's hint lines counted. The updates found from them are rows of
//! `/cars/{car}/updates` and the timeline ([`super::software`]).
//!
//! Bounds: four modules of at most nine fields, each with a few runs; read from one record.

use super::{QResult, QueryError, Scoped, car_of, scoped};
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
    /// The version it came with (the update's), or the name its first segment got: a reading that overlaps it, or one
    /// an admin typed.
    pub version: Option<String>,
    /// Who named it: `owner`, `teslamate`, `teslafi`, `tesla_api`, `teslafi_api`, `teslamate_local`, `build_match`;
    /// null while unnamed.
    pub version_source: Option<&'static str>,
    /// A module's first segment without a name, where the version last known before it (an import's or an input's)
    /// has no update between it and the segment and nothing contradicts it. The name stays null.
    pub name_inferred: Option<NameInferred>,
    /// Where it changed: the first drive with the new value, and the log it was first seen in.
    pub changed_in: Option<ChangedIn>,
    #[serde(flatten)]
    pub from: super::Sources,
}

/// [`HistoryRow::name_inferred`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NameInferred {
    pub version: String,
    /// `last known before`.
    pub from: &'static str,
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
    pub name_inferred: Option<NameInferred>,
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

/// The version an update found from fingerprints shows, however it was named (typed, a live input, a build match):
/// what the fingerprints it brought came with.
fn update_ref(e: &Engine, car: CarId, ev: &FwEvent) -> QResult<UpdateRef> {
    let (version, version_source) = super::software::fw_version_of(e, car, ev)?;
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
    let known = Known::new(e, car, h)?;
    let mut out: Vec<HistoryRow> = Vec::new();
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
                version_source: update.as_ref().and_then(|u| u.version_source),
                name_inferred: None,
                update,
                changed_in: changed,
                from: super::Sources::of(model::Source::Live, model::Inputs::SMO),
            });
            if k == 0 {
                let Some(row) = out.last_mut() else { continue };
                match super::software::typed_first(e, car, m.id)? {
                    Some(v) => {
                        row.version = Some(v);
                        row.version_source = Some("owner");
                    }
                    None => match known.verdict(row.first_seen_ms, row.until_ms.unwrap_or(row.last_seen_ms)) {
                        Verdict::Named(v, source) => {
                            row.version = Some(v);
                            row.version_source = Some(source);
                        }
                        Verdict::Inferred(v) => row.name_inferred = Some(NameInferred { version: v, from: LAST_KNOWN }),
                        Verdict::None => {}
                    },
                }
            }
        }
    }
    // the same fingerprint seen again under another name: what named or inferred this one is in doubt
    type Seen = (Option<&'static str>, Option<String>, Option<String>);
    let seen: Vec<Seen> = out.iter().map(|r| (r.module, r.fingerprint.clone(), r.version.clone())).collect();
    for r in out.iter_mut().filter(|r| r.update.is_none() && r.version_source != Some("owner")) {
        let mine = r.version.clone().or_else(|| r.name_inferred.as_ref().map(|n| n.version.clone()));
        let other = |(m, f, v): &Seen| *m == r.module && *f == r.fingerprint && v.is_some() && *v != mine;
        if mine.is_some() && seen.iter().any(other) {
            r.version = None;
            r.version_source = None;
            r.name_inferred = None;
        }
    }
    Ok(out)
}

const LAST_KNOWN: &str = "last known before";

/// One stretch the car was said to run a version: an import's update row (until the next update row or the end of
/// what the import covers) or an input's readings (their first and last).
struct Said {
    version: String,
    start: u64,
    end: u64,
    source: &'static str,
}

enum Verdict {
    Named(String, &'static str),
    Inferred(String),
    None,
}

/// What imports and inputs said about versions, for naming a module's first segment (no update came with it).
struct Known {
    said: Vec<Said>,
    /// Every update row that is not from fingerprints: its start and its version (empty: none).
    rows: Vec<(u64, String)>,
    /// When the modules' builds changed.
    builds: Vec<u64>,
}

impl Known {
    fn new(e: &Engine, car: CarId, h: &History) -> QResult<Known> {
        let all = super::software::shown_updates(e, car, None)?;
        let rows: Vec<(u64, String)> = all
            .iter()
            .filter(|u| !crate::firmware::is_fingerprint(u.id))
            .map(|u| (u.update.start_ms, u.update.version.trim().to_string()))
            .collect();
        let cover = super::fleet::imports_of(e, car)?;
        let mut said = Vec::new();
        for u in all.iter().filter(|u| u.update.source.site().is_some()) {
            let (version, _) = super::software::version_of(e, car, u)?;
            let (Some(version), Some(site)) =
                (version.filter(|v| super::software::valid_version(v)), u.update.source.site())
            else {
                continue;
            };
            let start = u.update.start_ms;
            let next = rows.iter().map(|r| r.0).filter(|&t| t > start).min().unwrap_or(u64::MAX);
            let covered = cover.iter().find(|c| c.site == site.word()).map(|c| c.to_ms).filter(|&t| t >= start);
            let end = match covered {
                Some(t) => t.min(next),
                None => u.update.end_ms.unwrap_or(start).max(start).min(next),
            };
            said.push(Said { version, start, end, source: site.word() });
        }
        for s in crate::records::input_versions(e.db(), car)? {
            if super::software::valid_version(&s.version) {
                said.push(Said {
                    version: s.version.clone(),
                    start: s.first_ms,
                    end: s.last_ms.max(s.first_ms),
                    source: model::Origin::from(s.input).kind(),
                });
            }
        }
        Ok(Known { said, rows, builds: h.events.iter().map(|ev| ev.to_ms).collect() })
    }

    /// Whether something happened in `(after, before)` (`before` included with `inclusive`) that says the version
    /// may have changed away from `v`: an update row without that version, or a module's build change.
    fn changed(&self, v: &str, after: u64, before: u64, inclusive: bool) -> bool {
        let inside = |t: u64| t > after && (t < before || inclusive && t == before);
        self.rows.iter().any(|(t, x)| inside(*t) && x != v) || self.builds.iter().any(|&t| inside(t))
    }

    /// The first segment of a module, seen from `s0` to `s1`: named by readings that overlap it (all the same
    /// version, nothing changing it), else inferred from the newest version said before it (no update between).
    fn verdict(&self, s0: u64, s1: u64) -> Verdict {
        let overlap: Vec<&Said> = self.said.iter().filter(|x| x.start <= s1 && x.end >= s0).collect();
        if let Some(first) = overlap.first() {
            let v = &first.version;
            let from = overlap.iter().map(|x| x.start).min().unwrap_or(s0);
            if overlap.iter().any(|x| x.version != *v) || self.changed(v, from, s1, false) {
                return Verdict::None;
            }
            let newest = overlap.iter().max_by_key(|x| (x.start, x.end)).unwrap_or(first);
            return Verdict::Named(v.clone(), newest.source);
        }
        let Some(c) = self.said.iter().filter(|x| x.end < s0).max_by_key(|x| (x.start, x.end)) else {
            return Verdict::None;
        };
        if self.changed(&c.version, c.start, s0, true) {
            return Verdict::None;
        }
        Verdict::Inferred(c.version.clone())
    }
}

/// The versions the car was said to run (no module fingerprints): imported updates (TeslaMate, TeslaFi), and the
/// versions the live inputs read (the Tesla API, TeslaFi, local TeslaMate; a stretch of one version, the inputs
/// together, from when one first said it). Each lasts until the next one starts, or until an update found from
/// fingerprints (the build changed), whichever comes first.
fn version_rows(e: &Engine, car: CarId, h: &History) -> QResult<Vec<HistoryRow>> {
    struct V {
        id: String,
        start: u64,
        last: u64,
        version: Option<String>,
        version_source: Option<&'static str>,
        update: Option<String>,
        from: super::Sources,
    }
    let mut vs: Vec<V> = Vec::new();
    for u in super::software::shown_updates(e, car, None)?.into_iter().filter(|u| u.update.source.site().is_some()) {
        let (version, version_source) = super::software::version_of(e, car, &u)?;
        let start = u.update.start_ms;
        vs.push(V {
            id: format!("version:{}", u.id),
            start,
            last: u.update.end_ms.unwrap_or(start).max(start),
            version,
            version_source,
            update: Some(u.id.to_string()),
            from: super::Sources::of(u.update.source, u.update.inputs),
        });
    }
    let mut seen = crate::records::input_versions(e.db(), car)?;
    seen.retain(|s| super::software::valid_version(&s.version));
    seen.sort_by_key(|s| (s.first_ms, s.last_ms));
    for s in seen {
        vs.push(V {
            id: format!("version:live:{}", s.first_ms),
            start: s.first_ms,
            last: s.last_ms.max(s.first_ms),
            version: Some(s.version.clone()),
            version_source: Some(model::Origin::from(s.input).kind()),
            update: None,
            from: super::Sources::of(model::Source::Live, model::Inputs::from(s.input)),
        });
    }
    vs.sort_by(|a, b| (a.start, &a.id).cmp(&(b.start, &b.id)));
    // an input saying the version the row before says, with no build change between: the same stretch
    let builds: Vec<u64> = h.events.iter().map(|ev| ev.to_ms).collect();
    let mut kept: Vec<V> = Vec::with_capacity(vs.len());
    for v in vs {
        match kept.last_mut() {
            Some(l)
                if v.update.is_none()
                    && l.version.is_some()
                    && l.version == v.version
                    && !builds.iter().any(|&b| b > l.start && b <= v.start) =>
            {
                l.last = l.last.max(v.last);
            }
            _ => kept.push(v),
        }
    }
    let vs = kept;
    // where a newer version starts: the next row's start, or an update found from fingerprints
    let now = crate::now_ms();
    let mut out = Vec::with_capacity(vs.len());
    for (i, v) in vs.iter().enumerate() {
        let next = vs[i + 1..].iter().map(|n| n.start).find(|&s| s > v.start);
        let build = builds.iter().copied().filter(|&b| b > v.start).min();
        let until = match (next, build) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        out.push(HistoryRow {
            id: v.id.clone(),
            kind: "version",
            module: None,
            module_name: None,
            fingerprint: None,
            previous: None,
            fields: Vec::new(),
            first_seen_ms: v.start,
            last_seen_ms: v.last,
            until_ms: until,
            duration_ms: until.unwrap_or(now).saturating_sub(v.start),
            current: until.is_none(),
            update: v.update.as_ref().map(|id| UpdateRef {
                id: id.clone(),
                version: v.version.clone(),
                version_source: v.version_source,
            }),
            version: v.version.clone(),
            version_source: v.version_source,
            name_inferred: None,
            changed_in: None,
            from: v.from.clone(),
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
                    name_inferred: r.name_inferred.clone(),
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
            all.extend(version_rows(e, car, &h)?);
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

/// One segment as `PATCH /cars/{car}/firmware/history/{row}` answers it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SegmentOne {
    #[serde(flatten)]
    pub row: HistoryRow,
    pub time_zone: String,
}

/// `PATCH /cars/{car}/firmware/history/{row}` `{ version }`: an admin names a module's fingerprint segment (`None`
/// forgets the typed name). A segment that came with an update names that update (as typing on it does, and every
/// update of the same fingerprint set); a module's first segment, which no update came with, keeps the name itself.
/// `no_such_update` for a row the car has not, or a segment that is neither.
pub fn name_segment(e: &Engine, car: CarId, row_id: &str, version: Option<&str>) -> QResult<SegmentOne> {
    let c = car_of(e, car)?;
    let version = version.map(str::trim);
    if version.is_some_and(|v| !super::software::valid_version(v)) {
        return Err(QueryError::invalid("version", "A version such as 2026.38.4."));
    }
    let h = crate::firmware::history(e.db(), car)?;
    let find = |h: &History| -> QResult<Option<HistoryRow>> {
        Ok(module_rows(e, car, h)?.into_iter().find(|r| r.id == row_id))
    };
    let row = find(&h)?.ok_or(QueryError::NotFound("no_such_update"))?;
    match (&row.update, row.module) {
        (Some(u), _) => {
            let id = super::parse_id(&u.id).ok_or(QueryError::NotFound("no_such_update"))?;
            super::software::set_version(e, car, id, version)?;
        }
        (None, Some(m)) if row.previous.is_none() => super::software::set_typed_first(e, car, m, version)?,
        _ => return Err(QueryError::NotFound("no_such_update")),
    }
    let h = crate::firmware::history(e.db(), car)?;
    let row = find(&h)?.ok_or(QueryError::NotFound("no_such_update"))?;
    Ok(SegmentOne { row, time_zone: c.time_zone.clone() })
}
