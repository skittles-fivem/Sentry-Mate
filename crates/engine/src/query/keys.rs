//! `/can-keys[/{key}]`, `/upload-tokens[/{token}]`, `/cars/{car}/upload-tokens[/{token}]`, `/setup/status`: the CAN
//! Bridge keys the server has seen and which car each belongs to, and the upload tokens (over the tokens given with
//! [`Engine::set_tokens`]; without them there are none to list).
//!
//! A token's secret is shown once, when it is made. A token made on a car's page names that car: its first upload
//! ties its logger's key to it. A token made for a new car makes a car of its own with its first upload (named after
//! its model and VIN prefix until someone names it).

use super::{CarRef, List, Paging, QResult, QueryError, Scoped, car_of, paginate, scoped};
use crate::{Engine, records};
use ingest::tokens::Token;
use model::CarId;
use serde::Serialize;

/// One CAN Bridge key.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KeyRow {
    pub key: String,
    pub first_seen_ms: Option<u64>,
    pub last_upload_ms: Option<u64>,
    /// Logs not used yet: waiting for a car (a key no car claims), or for their check (a car's key).
    pub logs_waiting: u64,
    pub car: Option<CarRef>,
    pub preferred: bool,
    /// The name an admin gave the key ("Screenmate in the car"), or none.
    pub device: Option<String>,
    /// What pages call the key: its name, else [`device_label`]'s automatic label.
    pub device_label: String,
}

/// The most characters a key's name has (after trimming).
pub const DEVICE_NAME_MAX: usize = 60;

/// The short id of a key: its part after the last `_`, its first 6 characters.
fn short_id(key: &str) -> String {
    let tail = key.rsplit('_').next().filter(|t| !t.is_empty()).unwrap_or(key);
    tail.chars().take(6).collect()
}

/// What a key is called: its name, else "CAN Bridge key" and its short id. The server learns nothing about the
/// device from its uploads (the app and the Raspberry Pi logger send the same), so an unnamed key is told apart by
/// its id.
pub fn device_label(key: &str, name: Option<&str>) -> String {
    match name {
        Some(n) => n.to_string(),
        None => match short_id(key) {
            id if id.is_empty() => "CAN Bridge key".to_string(),
            id => format!("CAN Bridge key {id}"),
        },
    }
}

/// A key's name, from the names admins gave.
pub(crate) fn name_of(e: &Engine, key: &str) -> QResult<Option<String>> {
    Ok(e.key_names()?.remove(key))
}

/// A name as typed, trimmed: one line of 1 to [`DEVICE_NAME_MAX`] characters; else why not.
pub fn check_name(name: &str) -> Result<String, &'static str> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > DEVICE_NAME_MAX {
        return Err("A name has 1 to 60 characters.");
    }
    if n.chars().any(char::is_control) {
        return Err("A name is one line of text.");
    }
    Ok(n.to_string())
}

/// Every car's id, in order.
fn car_ids(e: &Engine) -> QResult<Vec<CarId>> {
    super::all_cars(e)
}

/// A car's key, from its logs' records.
fn car_key(e: &Engine, car: CarId, preferred: Option<&str>, key: &str, name: Option<String>) -> QResult<KeyRow> {
    let a = e.handle()?.block_on(e.judge().arrivals(key));
    Ok(KeyRow {
        device_label: device_label(key, name.as_deref()),
        device: name,
        key: key.to_string(),
        first_seen_ms: a.first_ms,
        last_upload_ms: a.last_ms,
        logs_waiting: a.waiting,
        car: Some(CarRef(car)),
        preferred: preferred == Some(key),
    })
}

/// Every key: the cars' (of `visible`) and those waiting for a car.
fn all_keys(e: &Engine, visible: &[CarId]) -> QResult<Vec<KeyRow>> {
    let db = e.db();
    let mut names = e.key_names()?;
    let mut out = Vec::new();
    for car in car_ids(e)?.into_iter().filter(|c| visible.contains(c)) {
        let Some(c) = db.car(car)? else { continue };
        for k in &c.vehicle_keys {
            out.push(car_key(e, car, c.preferred_key.as_deref(), k, names.remove(k))?);
        }
    }
    for (key, w) in e.waiting_keys()? {
        if db.car_for_key(&key)?.is_some() {
            continue;
        }
        let name = names.remove(&key);
        out.push(KeyRow {
            device_label: device_label(&key, name.as_deref()),
            device: name,
            key,
            first_seen_ms: Some(w.first_seen_ms),
            last_upload_ms: Some(w.last_upload_ms),
            logs_waiting: w.logs_waiting,
            car: None,
            preferred: false,
        });
    }
    Ok(out)
}

/// `/can-keys`: newest first (by when each was first seen). `assigned` keeps keys with (or without) a car, `car` one
/// car's keys.
pub fn can_keys(
    e: &Engine,
    visible: &[CarId],
    assigned: Option<bool>,
    car: Option<CarId>,
    paging: &Paging,
) -> QResult<List<KeyRow>> {
    if let Some(c) = car
        && !visible.contains(&c)
    {
        return Err(QueryError::NoSuchCar);
    }
    let rows: Vec<KeyRow> = all_keys(e, visible)?
        .into_iter()
        .filter(|k| assigned.is_none_or(|a| k.car.is_some() == a))
        .filter(|k| car.is_none_or(|c| k.car == Some(CarRef(c))))
        .collect();
    let sig = format!("can-keys|{assigned:?}|{car:?}");
    paginate(rows, |k| (k.first_seen_ms.unwrap_or(0) as f64, k.key.clone()), true, &sig, paging, false)
}

/// What `PATCH /can-keys/{key}` changes.
#[derive(Debug, Clone, Default)]
pub struct KeyChange {
    /// `Some(Some(car))`: give the key to that car (from waiting, or from another car: what its logs gave that car
    /// goes with it); `Some(None)`: take it from its car (it waits for an admin).
    pub car: Option<Option<CarId>>,
    pub preferred: Option<bool>,
    /// `Some(Some(name))`: names the key (checked with [`check_name`]); `Some(None)`: forgets its name.
    pub name: Option<Option<String>>,
}

/// `PATCH /can-keys/{key}`: assigns a key, sets the preferred key or names it. Unknown key: `no_such_key`; a car that isn't
/// there (or that the caller does not see): `422 invalid`.
pub fn set_key(e: &Engine, visible: &[CarId], key: &str, change: &KeyChange) -> QResult<KeyRow> {
    let db = e.db();
    let known = db.car_for_key(key)?.is_some() || e.waiting_keys()?.contains_key(key) || e.judge().known(key);
    if !known {
        return Err(QueryError::NotFound("no_such_key"));
    }
    if let Some(Some(car)) = change.car
        && (!visible.contains(&car) || db.car(car)?.is_none())
    {
        return Err(QueryError::invalid("car", "No such car."));
    }
    if let Some(c) = db.car_for_key(key)?
        && !visible.contains(&c)
    {
        return Err(QueryError::NotFound("no_such_key"));
    }
    let name = match &change.name {
        Some(Some(n)) => Some(Some(check_name(n).map_err(|m| QueryError::invalid("name", m))?)),
        Some(None) => Some(None),
        None => None,
    };
    if change.preferred == Some(true) && change.car == Some(None) {
        return Err(QueryError::invalid("preferred", "A key with no car can't be its car's preferred key."));
    }
    match change.car {
        Some(Some(car)) if db.car_for_key(key)? != Some(car) => e.assign_key(key, car)?,
        Some(None) => e.unassign_key(key)?,
        _ => {}
    }
    if let Some(p) = change.preferred {
        if p && db.car_for_key(key)?.is_none() {
            return Err(QueryError::invalid("preferred", "A key with no car can't be its car's preferred key."));
        }
        e.set_preferred(key, p)?;
    }
    if let Some(n) = name {
        e.set_key_name(key, n.as_deref())?;
    }
    key_row(e, key)
}

/// One key as `/can-keys` lists it.
pub fn key_row(e: &Engine, key: &str) -> QResult<KeyRow> {
    let db = e.db();
    if let Some(car) = db.car_for_key(key)? {
        let c = car_of(e, car)?;
        return car_key(e, car, c.preferred_key.as_deref(), key, name_of(e, key)?);
    }
    let w = e.waiting_keys()?.get(key).cloned();
    let a = e.handle()?.block_on(e.judge().arrivals(key));
    let name = name_of(e, key)?;
    Ok(KeyRow {
        device_label: device_label(key, name.as_deref()),
        device: name,
        key: key.to_string(),
        first_seen_ms: w.as_ref().map(|w| w.first_seen_ms).or(a.first_ms),
        last_upload_ms: w.as_ref().map(|w| w.last_upload_ms).or(a.last_ms),
        logs_waiting: w.map_or(0, |w| w.logs_waiting),
        car: None,
        preferred: false,
    })
}

/// An upload token (never its secret, which is shown once).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TokenRow {
    pub id: String,
    pub created_ms: u64,
    pub revoked: bool,
    /// The CAN Bridge key its first upload tied it to.
    pub key: Option<String>,
    /// Its key's name and label, as the key rows give them (none while no key is tied).
    pub device: Option<String>,
    pub device_label: Option<String>,
    pub last_upload_ms: Option<u64>,
    pub car: Option<CarRef>,
}

/// A token just made: its row and its secret.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NewToken {
    #[serde(flatten)]
    pub row: TokenRow,
    pub secret: String,
}

/// Every token, and the car it belongs to (by its key, else the car it was made for).
fn tokens(e: &Engine) -> QResult<Vec<(Token, Option<CarId>)>> {
    let Some(t) = e.tokens() else { return Ok(Vec::new()) };
    let rt = e.handle()?;
    let list = rt.block_on(t.list());
    let made_for = records::token_cars(e.db())?;
    let mut out = Vec::new();
    for tok in list {
        let car = match &tok.vehicle_key {
            Some(k) => e.db().car_for_key(k)?,
            None => made_for.get(&tok.id).copied().filter(|&c| e.db().car(c).ok().flatten().is_some()),
        };
        out.push((tok, car));
    }
    Ok(out)
}

fn row(e: &Engine, t: &Token, car: Option<CarId>) -> QResult<TokenRow> {
    let last = match &t.vehicle_key {
        Some(k) => e.handle()?.block_on(e.judge().arrivals(k)).last_ms,
        None => None,
    };
    let device = match &t.vehicle_key {
        Some(k) => name_of(e, k)?,
        None => None,
    };
    let device_label = t.vehicle_key.as_deref().map(|k| device_label(k, device.as_deref()));
    Ok(TokenRow {
        id: t.id.clone(),
        created_ms: t.created_ms,
        revoked: t.revoked,
        key: t.vehicle_key.clone(),
        device,
        device_label,
        last_upload_ms: last,
        car: car.map(CarRef),
    })
}

fn page_tokens(rows: Vec<TokenRow>, sig: &str, paging: &Paging) -> QResult<List<TokenRow>> {
    paginate(rows, |t| (t.created_ms as f64, t.id.clone()), true, sig, paging, false)
}

/// `/upload-tokens`: the tokens of no car yet (not used, or their key waits for a car), newest first.
pub fn new_car_tokens(e: &Engine, paging: &Paging) -> QResult<List<TokenRow>> {
    let mut rows = Vec::new();
    for (t, car) in tokens(e)? {
        if car.is_none() {
            rows.push(row(e, &t, None)?);
        }
    }
    page_tokens(rows, "upload-tokens", paging)
}

/// `/cars/{car}/upload-tokens`: a car's tokens, newest first.
pub fn car_tokens(e: &Engine, car: CarId, paging: &Paging) -> QResult<Scoped<List<TokenRow>>> {
    let c = car_of(e, car)?;
    let mut rows = Vec::new();
    for (t, of) in tokens(e)? {
        if of == Some(car) {
            rows.push(row(e, &t, of)?);
        }
    }
    Ok(scoped(&c, page_tokens(rows, &format!("upload-tokens|{car}"), paging)?))
}

/// One token of a car (`car`), or of no car yet (`None`); else `no_such_token`.
pub fn token(e: &Engine, car: Option<CarId>, id: &str) -> QResult<TokenRow> {
    if let Some(c) = car {
        car_of(e, c)?;
    }
    for (t, of) in tokens(e)? {
        if t.id == id && of == car {
            return row(e, &t, of);
        }
    }
    Err(QueryError::NotFound("no_such_token"))
}

/// Makes a token for a car (`car`: its first upload ties its key to that car) or for a new car (`None`).
pub fn create_token(e: &Engine, car: Option<CarId>) -> QResult<NewToken> {
    let Some(tokens) = e.tokens() else {
        return Err(QueryError::Engine(crate::Error::Store("the upload tokens are not open here".into())));
    };
    // a car's token may take over that car's keys (a replacement token); any other key in use is refused at its
    // first upload, so a token can't be pointed at another car's key
    let (name, keys) = match car {
        Some(c) => (car_of(e, c)?.name, e.db().car(c)?.map(|c| c.vehicle_keys).unwrap_or_default()),
        None => (String::new(), Vec::new()),
    };
    let rt = e.handle()?;
    let (secret, t) = rt.block_on(tokens.create_for(&name, &keys)).map_err(crate::Error::Io)?;
    if let Some(c) = car {
        let _one = crate::lock(&e.inner.claim);
        let mut m = records::token_cars(e.db())?;
        m.insert(t.id.clone(), c);
        records::put_token_cars(e.db(), &m)?;
    }
    Ok(NewToken { row: row(e, &t, car)?, secret })
}

/// Revokes a token of a car (`car`) or of no car yet (`None`); else `no_such_token`.
pub fn revoke_token(e: &Engine, car: Option<CarId>, id: &str) -> QResult<TokenRow> {
    let mut r = token(e, car, id)?;
    if !r.revoked
        && let Some(tokens) = e.tokens()
    {
        e.handle()?.block_on(tokens.revoke(id)).map_err(crate::Error::Io)?;
        r.revoked = true;
    }
    Ok(r)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TokenCount {
    pub count: u64,
    pub first_used_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fix {
    pub cause: &'static str,
    pub link: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SetupStatus {
    pub cars: u64,
    pub tokens: TokenCount,
    pub keys_waiting: u64,
    pub logs_waiting_trust: u64,
    /// The web server sets it (and its fix) while an import is unfinished.
    pub import_running: bool,
    pub fixes: Vec<Fix>,
}

/// `/setup/status`: what a fleet with no history yet still needs (the cars of `visible`). An admin (`admin`) also
/// hears about what belongs to no car yet: the tokens made for a new car and the keys waiting for a car. Anyone
/// else hears only about its own cars (CAN Bridge keys and upload tokens are an admin's to read).
pub fn setup_status(e: &Engine, visible: &[CarId], admin: bool) -> QResult<SetupStatus> {
    let live: Vec<(Token, Option<CarId>)> = tokens(e)?
        .into_iter()
        .filter(|(t, c)| {
            !t.revoked
                && if admin { c.is_none_or(|c| visible.contains(&c)) } else { c.is_some_and(|c| visible.contains(&c)) }
        })
        .collect();
    let mut first_used: Option<u64> = None;
    let mut used = false;
    for (t, _) in &live {
        if t.vehicle_key.is_some() {
            used = true;
            first_used = Some(first_used.map_or(t.created_ms, |f| f.min(t.created_ms)));
        }
    }
    let keys = all_keys(e, visible)?;
    let waiting: Vec<&KeyRow> = keys.iter().filter(|k| admin && k.car.is_none()).collect();
    let trust: u64 = keys.iter().filter(|k| k.car.is_some()).map(|k| k.logs_waiting).sum();
    let mut fixes = Vec::new();
    if live.is_empty() {
        fixes.push(Fix { cause: "no_token", link: "/settings/cars", key: None });
    } else if !used {
        fixes.push(Fix { cause: "token_unused", link: "/settings/cars", key: None });
    }
    if let Some(k) = waiting.first() {
        fixes.push(Fix { cause: "key_waiting", link: "/settings/cars", key: Some(k.key.clone()) });
    }
    if trust > 0 {
        fixes.push(Fix { cause: "logs_waiting_trust", link: "/settings/cars", key: None });
    }
    Ok(SetupStatus {
        cars: visible.len() as u64,
        tokens: TokenCount { count: live.len() as u64, first_used_ms: first_used },
        keys_waiting: waiting.len() as u64,
        logs_waiting_trust: trust,
        import_running: false,
        fixes,
    })
}
