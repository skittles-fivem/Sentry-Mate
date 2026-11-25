//! Logs and trust: a car's uploaded CAN logs, newest first, with their check results and what was decoded from each
//! (`/cars/{car}/logs?from_ms=&to_ms=&limit=&after=`, CSV too), over the engine's [`engine::query::system::logs`].
//! Admin-only reads (the guard's list); a hidden car answers as an unknown one.
//!
//! A row: `{ name, key, start_ms, result: "verified" | "unsigned" | "failed" | "could_not_sign" | "waiting", reason,
//! used, override_use, unsigned_kind, before_first_signed, arrived_ms, decoded: "decoded" | "left_out" | "not_yet",
//! samples, dropped, buses, data_from_ms, data_to_ms, decoded_ms }`. A period filters by the log's start (from its
//! name). `unsigned_kind`: why a log without a signature has none (`no_sidecar`, `no_integrity`, `signed_false`,
//! `could_not_sign`, `unknown`), `null` for a signed (or badly signed) one.
//!
//! The owner's word on unsigned logs of a paired car (docs/security.md, "Pairing and the owner's word"):
//! `GET /cars/{car}/logs/unsigned` counts what can be used and lists the newest; `POST /cars/{car}/logs/use-unsigned`
//! `{ names | all: true, before_first_signed?, confirm_password, code? }` uses those that failed only for being
//! unsigned (any other name is `422` on `names`, nothing changes), `DELETE` the same route with the same body stops
//! using them. Both ask the admin's password (and two-step code) again and are recorded in the audit log; then the
//! car's logs are compared with what was recorded ([`Engine::reconcile`]), so those changed are decoded again (or
//! left out again) and the windows they are in rebuilt.

use super::now::{answer, list_answer, period};
use crate::accounts_admin::{confirm_code, confirm_password};
use crate::ctx::{Admin, target};
use crate::{ApiError, ApiResult, AppState, CarParam, JsonBody, Params, Viewer};
use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::{get, post};
use engine::Engine;
use engine::query as q;
use ingest::judge::Pick;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Names one request may give.
pub const NAMES_MAX: usize = 100_000;
/// The refused names a `422` lists (it says how many there are).
const REFUSED_SHOWN: usize = 10;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/cars/{car}/logs", get(logs))
        .route("/api/v1/cars/{car}/logs/unsigned", get(unsigned))
        .route("/api/v1/cars/{car}/logs/use-unsigned", post(use_unsigned).delete(stop_using))
}

async fn logs(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    let per = period(&p)?;
    list_answer(&app, &p, "logs", move |e, pg| q::system::logs(e, car, &per, pg)).await
}

async fn unsigned(State(app): State<AppState>, CarParam(car): CarParam, p: Params) -> ApiResult<Response> {
    answer(&app, &p, "logs-unsigned", move |e| q::system::unsigned_logs(e, car)).await
}

async fn use_unsigned(
    State(app): State<AppState>,
    Admin(v): Admin,
    CarParam(car): CarParam,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    change(app, v, car, headers, body, true).await
}

async fn stop_using(
    State(app): State<AppState>,
    Admin(v): Admin,
    CarParam(car): CarParam,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    change(app, v, car, headers, body, false).await
}

/// What the body asks for.
fn pick_of(body: &JsonBody) -> ApiResult<Pick> {
    let all = body.bool("all")?.unwrap_or(false);
    let before = body.bool("before_first_signed")?;
    let names = match body.get("names") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => {
            if a.len() > NAMES_MAX {
                return Err(ApiError::bad_input("names", &format!("At most {NAMES_MAX} names at a time.")));
            }
            let mut out = Vec::with_capacity(a.len());
            for n in a {
                let Some(s) = n.as_str() else {
                    return Err(ApiError::bad_input("names", "A list of log names."));
                };
                out.push(s.to_string());
            }
            Some(out)
        }
        Some(_) => return Err(ApiError::bad_input("names", "A list of log names.")),
    };
    match (names, all) {
        (Some(_), true) => Err(ApiError::bad_input("names", "Either names or all, not both.")),
        (None, false) => Err(ApiError::bad_input("names", "Name the logs, or say all: true.")),
        (Some(n), false) if n.is_empty() => Err(ApiError::bad_input("names", "Name at least one log.")),
        (Some(_), false) if before.is_some() => Err(ApiError::bad_input("before_first_signed", "Only with all: true.")),
        (Some(n), false) => Ok(Pick::Names(n)),
        (None, true) => Ok(Pick::All { before_first_signed: before.unwrap_or(false) }),
    }
}

fn logs_word(n: usize) -> String {
    if n == 1 { "1 unsigned CAN log".into() } else { format!("{n} unsigned CAN logs") }
}

async fn change(
    app: AppState,
    v: Viewer,
    car: model::CarId,
    headers: HeaderMap,
    body: JsonBody,
    on: bool,
) -> ApiResult<Json<Value>> {
    body.only(&["names", "all", "before_first_signed", "confirm_password", "code"])?;
    let pick = pick_of(&body)?;
    let password = confirm_password(Some(&body), &headers)?;
    let code = confirm_code(Some(&body), &headers)?;
    let v2 = v.clone();
    app.blocking(move |app| Ok(app.accounts.confirm(v2.signed_in(), &password, code.as_deref())?)).await?;
    let e = app.service::<Engine>()?;
    let keys = {
        let e = e.clone();
        app.blocking(move |_| {
            let c = e.db().car(car).map_err(ApiError::server)?.ok_or_else(|| ApiError::not_found("no_such_car"))?;
            Ok(c.vehicle_keys.clone())
        })
        .await?
    };
    // each key's part of the ask: names go to the key they name (a name of no key of this car is refused)
    let mut parts: Vec<(String, Pick)> = Vec::new();
    let mut refused: Vec<(String, String)> = Vec::new();
    match &pick {
        Pick::All { .. } => parts.extend(keys.iter().map(|k| (k.clone(), pick.clone()))),
        Pick::Names(names) => {
            let mut by_key: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for n in names {
                match trust::LogName::parse(n).filter(|l| !l.sidecar) {
                    Some(l) if keys.iter().any(|k| k == l.vehicle_key) => {
                        by_key.entry(l.vehicle_key.to_string()).or_default().push(n.clone())
                    }
                    _ => refused.push((n.clone(), "not a log of this car".into())),
                }
            }
            parts.extend(by_key.into_iter().map(|(k, n)| (k, Pick::Names(n))));
        }
    }
    // looked at first, every key: refused anywhere, nothing changes anywhere
    for (key, p) in &parts {
        let out = e.judge().use_unsigned(key, p, on, false).await.map_err(ApiError::server)?;
        refused.extend(out.refused);
    }
    if !refused.is_empty() {
        refused.sort();
        let n = refused.len();
        let listed: Vec<String> =
            refused.iter().take(REFUSED_SHOWN).map(|(name, why)| format!("{name} ({why})")).collect();
        let more = if n > REFUSED_SHOWN { format!(" and {} more", n - REFUSED_SHOWN) } else { String::new() };
        let what = if on {
            "only a log that failed its check for being unsigned can be used"
        } else {
            "not a log of this car"
        };
        let logs = if n == 1 { "1 of these logs".to_string() } else { format!("{n} of these logs") };
        let msg = format!("{logs}: {what}, and nothing was changed: {}{more}.", listed.join(", "));
        return Err(ApiError::invalid("names", &msg));
    }
    let mut changed: Vec<String> = Vec::new();
    let mut kinds: BTreeMap<&'static str, u64> = BTreeMap::new();
    for (key, p) in &parts {
        let out = e.judge().use_unsigned(key, p, on, true).await.map_err(ApiError::server)?;
        for (name, r) in out.changed {
            *kinds.entry(r.unsigned_kind().unwrap_or("unknown")).or_default() += 1;
            changed.push(name);
        }
    }
    if changed.is_empty() {
        // nothing was as asked to change: nothing to record
        v.mark_audited();
    } else {
        // the car's logs compared with what was recorded: those changed are decoded (or left out) again, and the
        // windows they are in rebuilt, joined where they meet
        e.reconcile(Some(car));
        let which: Vec<String> = kinds.iter().map(|(k, n)| format!("{n} {}", kind_words(k))).collect();
        let whose = app.car_name(car);
        let summary = if on {
            format!(
                "Used {} of {whose} that had failed their check only for being unsigned ({}); bad and other-key logs are never used",
                logs_word(changed.len()),
                which.join(", ")
            )
        } else {
            format!("Stopped using {} of {whose} ({})", logs_word(changed.len()), which.join(", "))
        };
        let code = if on { "can-logs-unsigned-used" } else { "can-logs-unsigned-stopped" };
        v.audit(&app, code, Some(&target("car", Some(car), "")), &summary).await?;
    }
    changed.sort();
    Ok(Json(json!({ "changed": changed.len(), "kinds": kinds, "names": changed })))
}

/// A kind in the audit log's words.
fn kind_words(k: &str) -> &'static str {
    match k {
        "no_sidecar" => "with no sidecar",
        "no_integrity" => "from before the app signed logs",
        "signed_false" => "the app said were not signed",
        "could_not_sign" => "the app could not sign",
        _ => "of a kind not known",
    }
}
