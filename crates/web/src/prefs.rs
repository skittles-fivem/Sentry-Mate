//! A person's preferences, `GET`/`PATCH /api/v1/me/preferences` (docs/ui-api.md): units, range, theme, language,
//! interface size, follow screen, UI pack, car order and pinned cars.
//!
//! Only what the person chose is kept (sealed, one small file per account); the rest comes from the server's
//! defaults ([`crate::units::Defaults`]) at the time it is read, so a change of the defaults reaches everyone who
//! didn't choose. The UI pack is kept with the account itself (the accounts crate), so serving a page never opens
//! this file. `car_order` and `pinned_cars` only ever list cars the account sees.

use crate::ctx::{JsonBody, Viewer};
use crate::error::{ApiResult, Fields};
use crate::events::Event;
use crate::{AppState, ids, units};
use accounts::{AccountId, CarAccess};
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use vault::Vault;

const LABEL: &str = "web/prefs";
pub const MAX_CAR_ORDER: usize = 10_000;
pub const MAX_PINNED: usize = 50;
pub const THEMES: &[&str] = &["light", "dark", "device"];

/// The preferences files. Every change runs under one lock ([`Prefs::update`]), so two changes at the same moment
/// (two tabs, a slider) neither mix their bytes in the file nor lose each other's fields.
pub struct Prefs {
    dir: PathBuf,
    vault: Arc<Vault>,
    lock: Mutex<()>,
}

impl Prefs {
    pub fn open(dir: PathBuf, vault: Arc<Vault>) -> std::io::Result<Prefs> {
        std::fs::create_dir_all(&dir)?;
        Ok(Prefs { dir, vault, lock: Mutex::new(()) })
    }

    fn path(&self, id: AccountId) -> PathBuf {
        self.dir.join(format!("{id}.sealed"))
    }

    /// What the person chose (an empty object when nothing). A damaged file counts as nothing chosen (and is
    /// replaced by the next change), so one bad file can't keep the person's preferences answering `500`.
    pub fn own(&self, id: AccountId) -> ApiResult<Map<String, Value>> {
        let raw = match self.vault.read_file(&self.path(id), LABEL) {
            Ok(Some(raw)) => raw,
            Ok(None) => return Ok(Map::new()),
            Err(e @ (vault::Error::Damaged | vault::Error::NotSealed)) => {
                tracing::error!("web: the preferences of account {id} can't be read ({e}); they start again empty");
                return Ok(Map::new());
            }
            Err(e) => return Err(e.into()),
        };
        match serde_json::from_slice(&raw) {
            Ok(Value::Object(m)) => Ok(m),
            _ => Ok(Map::new()),
        }
    }

    /// Replaces what the person chose.
    pub fn save(&self, id: AccountId, own: &Map<String, Value>) -> ApiResult<()> {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.write(id, own)
    }

    fn write(&self, id: AccountId, own: &Map<String, Value>) -> ApiResult<()> {
        let plain = serde_json::to_vec(own)?;
        Ok(self.vault.write_file(&self.path(id), LABEL, &plain)?)
    }

    /// Reads what the person chose, lets `change` change it, and keeps the result, all under the lock: the one way
    /// to change preferences that keeps changes made at the same moment apart. `change` may do more work that must
    /// not interleave with another change (the account's UI pack); an `Err` keeps nothing.
    pub fn update<R>(
        &self,
        id: AccountId,
        change: impl FnOnce(&mut Map<String, Value>) -> ApiResult<R>,
    ) -> ApiResult<R> {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut own = self.own(id)?;
        let r = change(&mut own)?;
        self.write(id, &own)?;
        Ok(r)
    }

    /// Forgets an account's choices (when it is removed).
    pub fn remove(&self, id: AccountId) -> ApiResult<()> {
        let _held = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        match std::fs::remove_file(self.path(id)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/me/preferences", get(get_prefs).patch(patch_prefs))
}

fn visible(app: &AppState, access: &CarAccess, list: Option<&Value>) -> Vec<Value> {
    let mut seen = std::collections::HashSet::new();
    list.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter_map(ids::parse_car)
                .filter(|c| access.allows(*c) && app.car_exists(*c) && seen.insert(*c))
                .map(|c| Value::from(ids::car(c)))
                .collect()
        })
        .unwrap_or_default()
}

/// The whole preferences object of an account, as it is now (defaults filled in, hidden cars left out).
pub fn view(app: &AppState, id: AccountId, access: &CarAccess) -> ApiResult<Value> {
    let own = app.prefs.own(id)?;
    let d = app.defaults();
    let mut u = serde_json::to_value(d.units)?;
    if let (Some(Value::Object(mine)), Some(base)) = (own.get("units"), u.as_object_mut()) {
        for (k, v) in mine {
            base.insert(k.clone(), v.clone());
        }
    }
    let pick = |k: &str, default: Value| own.get(k).cloned().unwrap_or(default);
    let pack = app.accounts.account(id).and_then(|a| a.ui_pack).filter(|p| app.packs.usable(p));
    let order_default = Value::Array(d.car_order.iter().map(|c| Value::from(c.as_str())).collect());
    let order = own.get("car_order").cloned().unwrap_or(order_default);
    Ok(json!({
        "units": u,
        "range": pick("range", serde_json::to_value(d.range)?),
        "theme": pick("theme", json!("device")),
        "language": pick("language", json!("auto")),
        "interface_size": pick("interface_size", json!(100)),
        "follow_screen": pick("follow_screen", json!(true)),
        "ui_pack": pack.unwrap_or_else(|| crate::packs::pick_id(app, None)),
        "car_order": visible(app, access, Some(&order)),
        "pinned_cars": visible(app, access, own.get("pinned_cars")),
    }))
}

/// [`view`] off the async threads.
pub async fn view_for(app: &AppState, id: AccountId, access: &CarAccess) -> ApiResult<Value> {
    let access = access.clone();
    app.blocking(move |app| view(app, id, &access)).await
}

async fn get_prefs(State(app): State<AppState>, v: Viewer) -> ApiResult<Json<Value>> {
    Ok(Json(view_for(&app, v.id(), v.cars()).await?))
}

const KEYS: &[&str] =
    &["units", "range", "theme", "language", "interface_size", "follow_screen", "ui_pack", "car_order", "pinned_cars"];

fn check_cars(
    app: &AppState,
    v: &Viewer,
    body: &JsonBody,
    key: &str,
    most: usize,
    bad: &mut Fields,
    rule: &mut Fields,
) {
    let Some(val) = body.get(key) else { return };
    let Some(list) = val.as_array().filter(|a| a.iter().all(Value::is_string)) else {
        bad.add(key, "Must be a list of car ids.");
        return;
    };
    if list.len() > most {
        rule.add(key, &format!("At most {most} cars."));
        return;
    }
    let all_known =
        list.iter().filter_map(Value::as_str).all(|c| ids::parse_car(c).is_some_and(|c| v.sees_existing(app, c)));
    if !all_known {
        rule.add(key, "Unknown car in the list.");
    }
}

async fn patch_prefs(State(app): State<AppState>, v: Viewer, body: JsonBody) -> ApiResult<Json<Value>> {
    let mut bad = Fields::new();
    let mut rule = Fields::new();
    for k in body.0.keys() {
        if !KEYS.contains(&k.as_str()) {
            bad.add(k, "Unknown field.");
        }
    }
    if let Some(u) = body.get("units") {
        match u.as_object() {
            None => bad.add("units", "Must be an object."),
            Some(m) => {
                for (k, val) in m {
                    let allowed = match k.as_str() {
                        "distance" => units::DISTANCE,
                        "temperature" => units::TEMPERATURE,
                        "pressure" => units::PRESSURE,
                        _ => {
                            bad.add(&format!("units.{k}"), "Unknown unit.");
                            continue;
                        }
                    };
                    if !val.as_str().is_some_and(|s| allowed.contains(&s)) {
                        rule.add(&format!("units.{k}"), &format!("Must be one of: {}.", allowed.join(", ")));
                    }
                }
            }
        }
    }
    for (k, allowed) in [("range", units::RANGE), ("theme", THEMES)] {
        if let Some(val) = body.get(k)
            && !val.as_str().is_some_and(|s| allowed.contains(&s))
        {
            rule.add(k, &format!("Must be one of: {}.", allowed.join(", ")));
        }
    }
    if let Some(val) = body.get("language") {
        let ok = val
            .as_str()
            .is_some_and(|s| (2..=35).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
        if !ok {
            rule.add("language", "A language code such as en or de-CH, or auto.");
        }
    }
    if let Some(val) = body.get("interface_size") {
        match val.as_u64() {
            None if !val.is_number() => bad.add("interface_size", "Must be a whole number (percent)."),
            Some(n) if (50..=200).contains(&n) => {}
            _ => rule.add("interface_size", "Must be between 50 and 200 (percent)."),
        }
    }
    if let Some(val) = body.get("follow_screen")
        && !val.is_boolean()
    {
        bad.add("follow_screen", "Must be true or false.");
    }
    let mut pack_change: Option<Option<String>> = None;
    if let Some(val) = body.get("ui_pack") {
        match val {
            Value::Null => pack_change = Some(None),
            Value::String(p) => match app.packs.meta(p) {
                Some(m) if m.supported() => pack_change = Some(Some(p.clone())),
                Some(m) => rule.add("ui_pack", &crate::packs::unsupported(&m)),
                None => rule.add("ui_pack", "No such UI pack on this server."),
            },
            _ => bad.add("ui_pack", "Must be a pack id, or null for the server's choice."),
        }
    }
    check_cars(&app, &v, &body, "car_order", MAX_CAR_ORDER, &mut bad, &mut rule);
    check_cars(&app, &v, &body, "pinned_cars", MAX_PINNED, &mut bad, &mut rule);
    bad.bad_input()?;
    rule.invalid()?;

    let changed: Vec<String> = body.0.keys().cloned().collect();
    let viewer = v.clone();
    let prefs = app
        .blocking(move |app| {
            let id = viewer.id();
            app.prefs.update(id, |own| {
                for (k, val) in body.0 {
                    match k.as_str() {
                        "ui_pack" => {}
                        "units" => {
                            let mut u = own.get("units").and_then(Value::as_object).cloned().unwrap_or_default();
                            if let Value::Object(m) = val {
                                u.extend(m);
                            }
                            own.insert(k, Value::Object(u));
                        }
                        "car_order" | "pinned_cars" => {
                            let mut seen = std::collections::HashSet::new();
                            let list: Vec<Value> = val
                                .as_array()
                                .map(|a| {
                                    a.iter().filter(|c| seen.insert(c.as_str().map(str::to_string))).cloned().collect()
                                })
                                .unwrap_or_default();
                            own.insert(k, Value::Array(list));
                        }
                        _ => {
                            own.insert(k, val);
                        }
                    }
                }
                if let Some(p) = pack_change {
                    app.accounts.set_ui_pack(viewer.signed_in(), p.as_deref())?;
                }
                Ok(())
            })?;
            view(app, id, viewer.cars())
        })
        .await?;
    v.audit(
        &app,
        "preferences-changed",
        Some(&format!("account:{}", ids::account(v.id()))),
        &format!("Changed: {}.", changed.join(", ")),
    )
    .await?;
    app.events.publish(Event::Prefs { account: v.id() });
    Ok(Json(prefs))
}
