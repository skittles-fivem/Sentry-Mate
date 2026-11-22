//! The car's build for `/cars/{car}` (`vehicle`): what its own CAN frames report of it, put together with its VIN,
//! the Tesla API's `vehicle_config`, an import's car row and what an admin typed ([`model::vehicle::vehicle_info`],
//! offline, built-in tables only).
//!
//! The gateway sends its configuration (`0x7FF GTW_carConfig`, four pages) every few seconds while the vehicle bus
//! is awake, and the pack its energy (0x352) and brick numbers (0x332). They are decoded from the car's newest stored
//! logs, the newest [`DAYS`] record days with decoded logs (at most [`LOGS_PER_DAY`] logs each, newest first), and
//! kept per day in the db's sealed key/value store ([`NS`]) with the logs they came from, so a day is decoded again
//! only when its logs change. A newer day's value of a signal wins; an older day fills what it did not send.

use super::{QResult, decoding};
use crate::Engine;
use crate::records::{self, Outcome};
use model::vehicle::{CanBuild, TextBuild};
use model::{Car, CarId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The key/value namespace of the build readings kept per day (and an import's car row).
pub const NS: &str = "vehicle";
/// Record days looked at, newest first.
const DAYS: usize = 3;
/// Logs of one day decoded at most (newest first).
const LOGS_PER_DAY: usize = 2;
/// Bytes of lines read of one log at most (the configuration pages come every few seconds).
const LOG_BYTES: u64 = 64 << 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BuildDay {
    stamp: String,
    can: CanBuild,
}

/// A stored log decoded frame by frame, keeping the build's signals; `None` when it cannot be read.
fn decode_build(e: &Engine, profile: dbc::Profile, name: &str) -> QResult<Option<CanBuild>> {
    let rt = e.handle()?;
    let read = |n: &str| rt.block_on(e.store().read(n)).map_err(|x| crate::Error::Store(x.to_string()));
    let Some(bytes) = read(name)? else { return Ok(None) };
    let side: Value = read(&format!("{name}.json"))?.and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut decoder = dbc::Decoder::new(profile, &side);
    let mut b = CanBuild::default();
    decoding::read(&bytes, LOG_BYTES, |l| {
        if let logfmt::Line::Frame(fr) = logfmt::Line::parse(l)
            && matches!(fr.id, 0x7FF | 0x352 | 0x332)
        {
            for (sig, v) in decoder.decode(&fr) {
                b.take(&sig.name, v);
            }
        }
    })?;
    Ok(Some(b))
}

/// The build readings of one UTC record day: the kept ones while the day's logs are the same, else decoded and kept.
fn day_build(e: &Engine, c: &Car, d: u32) -> QResult<Option<CanBuild>> {
    let rec = records::day(e.db(), c.id, d)?;
    let mut logs: Vec<(&String, &records::LogEntry)> =
        rec.logs.iter().filter(|(_, en)| en.usable && matches!(en.outcome, Outcome::Decoded { .. })).collect();
    if logs.is_empty() {
        return Ok(None);
    }
    logs.sort_by(|a, b| (b.1.span().1, b.0).cmp(&(a.1.span().1, a.0)));
    logs.truncate(LOGS_PER_DAY);
    let mut h = Sha256::new();
    for (n, en) in &logs {
        h.update(n.as_bytes());
        h.update(en.done_ms.to_be_bytes());
        h.update(en.profile.as_bytes());
    }
    let stamp: String = h.finalize()[..12].iter().map(|b| format!("{b:02x}")).collect();
    let key = format!("car/{}/day/{d}", c.id);
    if let Some(kept) = records::kept::<BuildDay>(e.db(), NS, &key)?
        && kept.stamp == stamp
    {
        return Ok(Some(kept.can));
    }
    let profile = dbc::Profile::from_name(c.profile().dbc_name());
    let mut can = CanBuild::default();
    for (n, _) in &logs {
        if let Some(b) = decode_build(e, profile, n)? {
            can.fill_from(&b);
        }
    }
    records::put(e.db(), NS, &key, &BuildDay { stamp, can: can.clone() })?;
    Ok(Some(can))
}

/// What the car's own frames report of its build: the newest record days' readings, and the message its VIN came in.
pub(crate) fn can_build(e: &Engine, c: &Car) -> QResult<CanBuild> {
    let meta = records::meta(e.db(), c.id)?;
    let mut can = CanBuild::default();
    for &d in meta.days.iter().rev().take(DAYS) {
        if let Some(b) = day_build(e, c, d)? {
            can.fill_from(&b);
        }
    }
    can.vin_via = c.vin.as_ref().map(|v| v.via).filter(|v| matches!(v, 0x405 | 0x508));
    Ok(can)
}

fn import_key(car: CarId) -> String {
    format!("car/{car}/teslamate")
}

/// What a TeslaMate import said of the car's build (kept when it is imported).
pub(crate) fn put_imported(db: &db::Db, car: CarId, b: &TextBuild) -> crate::Result<()> {
    records::put(db, NS, &import_key(car), b)
}

pub(crate) fn imported(e: &Engine, car: CarId) -> QResult<Option<TextBuild>> {
    Ok(records::get(e.db(), NS, &import_key(car))?)
}

/// The Tesla API's `vehicle_config`, as kept beside its samples.
pub(crate) fn from_api(api: &tesla::ApiMeta) -> TextBuild {
    TextBuild {
        car_type: api.car_type.clone(),
        trim_badging: api.trim_badging.clone(),
        exterior_color: api.exterior_color.clone(),
        wheel_type: api.wheel_type.clone(),
        roof_color: api.roof_color.clone(),
        spoiler_type: api.spoiler_type.clone(),
        right_hand_drive: api.right_hand_drive,
        lfp_battery: None,
    }
}
