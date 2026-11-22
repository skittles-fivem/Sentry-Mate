//! Small tables shared by all cars: places (geofences) and settings (key → JSON). Each is a snapshot and a journal
//! like everything else, loaded the first time it is used.

use crate::error::{Error, Result};
use crate::file::{Ctx, Kind};
use crate::logged::Logged;
use model::{Place, PlaceId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize)]
pub(crate) enum PlaceOp {
    Put(Place),
    Remove(PlaceId),
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Places {
    pub next_id: PlaceId,
    pub places: BTreeMap<PlaceId, Place>,
}

impl Logged for Places {
    type Op = PlaceOp;
    const KIND: Kind = Kind::Snapshot;

    fn empty() -> Self {
        Places { next_id: 1, places: BTreeMap::new() }
    }

    fn apply(&mut self, op: PlaceOp) {
        match op {
            PlaceOp::Put(p) => {
                self.next_id = self.next_id.max(p.id.saturating_add(1));
                self.places.insert(p.id, p);
            }
            PlaceOp::Remove(id) => {
                self.places.remove(&id);
            }
        }
    }

    fn encode_op(_ctx: &Ctx, op: &PlaceOp) -> Result<Vec<u8>> {
        serde_json::to_vec(op).map_err(|e| Error::Corrupt(e.to_string()))
    }

    fn decode_op(b: &[u8]) -> Result<PlaceOp> {
        serde_json::from_slice(b).map_err(|e| Error::Corrupt(format!("a places entry: {e}")))
    }

    fn encode_state(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|e| Error::Corrupt(e.to_string()))
    }

    fn decode_state(b: &[u8]) -> Result<Self> {
        serde_json::from_slice(b).map_err(|e| Error::Corrupt(format!("the places: {e}")))
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) enum SettingOp {
    Set(String, serde_json::Value),
    Remove(String),
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Settings {
    pub values: BTreeMap<String, serde_json::Value>,
}

impl Logged for Settings {
    type Op = SettingOp;
    const KIND: Kind = Kind::Snapshot;

    fn empty() -> Self {
        Settings { values: BTreeMap::new() }
    }

    fn apply(&mut self, op: SettingOp) {
        match op {
            SettingOp::Set(k, v) => {
                self.values.insert(k, v);
            }
            SettingOp::Remove(k) => {
                self.values.remove(&k);
            }
        }
    }

    fn encode_op(_ctx: &Ctx, op: &SettingOp) -> Result<Vec<u8>> {
        serde_json::to_vec(op).map_err(|e| Error::Corrupt(e.to_string()))
    }

    fn decode_op(b: &[u8]) -> Result<SettingOp> {
        serde_json::from_slice(b).map_err(|e| Error::Corrupt(format!("a settings entry: {e}")))
    }

    fn encode_state(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|e| Error::Corrupt(e.to_string()))
    }

    fn decode_state(b: &[u8]) -> Result<Self> {
        serde_json::from_slice(b).map_err(|e| Error::Corrupt(format!("the settings: {e}")))
    }
}
