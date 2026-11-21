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
