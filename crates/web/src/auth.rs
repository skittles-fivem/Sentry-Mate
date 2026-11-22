//! Signing in and one's own account (docs/ui-api.md, "Sign-in, accounts and preferences"):
//!
//! | Route | |
//! |---|---|
//! | `GET /setup` | `{ needs_setup, state, claimed, claim_expires_ms, job }` ([`crate::moving`]) |
//! | `POST /setup` | `{ code, name, password }`: the first admin, with the one-time setup code from the log; signs in and shows the recovery key once |
//! | `POST /session` | `{ name, password }` -> `{ status: "done", me }` or `{ status: "two_step_pending", pending, expires_ms }` |
//! | `POST /session/two-step` | `{ pending, code }`: an authenticator code or a recovery code -> `{ status: "done", me }` |
//! | `DELETE /session` | signs this browser out |
//! | `GET /me` | `{ id, name, role, cars, two_step, two_step_required, two_step_setup_only }` |
//! | `GET`/`DELETE /me/sessions[/{session}]` | the account's signed-in browsers; `DELETE /me/sessions` ends every other one |
//! | `PUT /me/password` | `{ password, new_password, code }`: the other sessions end |
//! | `GET`/`POST`/`DELETE /me/two-step`, `POST /me/two-step/confirm`, `POST /me/two-step/recovery-codes` | two-step sign-in |
//!
//! With two-step sign-in on, `PUT /me/password`, `DELETE /me/two-step` and `POST /me/two-step/recovery-codes` need
//! a `code` (from the app, or a recovery code) as well as the password: without it or wrong `401 wrong_code`.
//!
//! The sign-in answers never say whether an account exists; tries are throttled by the accounts crate (per name and
//! per address, home and outside counted apart). Every step is in the audit log (the accounts crate records it).

use crate::ctx::{JsonBody, Viewer, cars_json};
use crate::error::{ApiError, ApiResult};
use crate::events::Event;
use crate::{AppState, Conn, ids};
use accounts::{AccountView, CarAccess, Client, Confirm, NewSession, Role, SignIn};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use serde_json::{Value, json};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/setup", get(get_setup).post(post_setup))
        .route("/api/v1/session", post(sign_in).delete(sign_out))
        .route("/api/v1/session/two-step", post(two_step))
        .route("/api/v1/me", get(me))
        .route("/api/v1/me/sessions", get(my_sessions).delete(end_other_sessions))
        .route("/api/v1/me/sessions/{session}", get(my_session).delete(end_session))
        .route("/api/v1/me/password", put(change_password))
        .route("/api/v1/me/two-step", get(two_step_state).post(two_step_start).delete(two_step_off))
        .route("/api/v1/me/two-step/confirm", post(two_step_confirm))
        .route("/api/v1/me/two-step/recovery-codes", post(new_recovery_codes))
}

/// `admin` or `watch`, as the API writes roles.
pub fn role_str(r: Role) -> &'static str {
    match r {
        Role::Admin => "admin",
        Role::WatchOnly => "watch",
    }
}

pub fn parse_role(s: &str) -> Option<Role> {
    match s {
        "admin" => Some(Role::Admin),
        "watch" => Some(Role::WatchOnly),
        _ => None,
    }
}

/// `/me` for an account.
pub fn me_json(app: &AppState, a: &AccountView, cars: &CarAccess, local: bool) -> Value {
    let p = app.accounts.two_step_policy();
    json!({
        "id": ids::account(a.id),
        "name": a.name,
        "role": role_str(a.role),
        "cars": cars_json(cars),
        "two_step": a.two_step,
        "two_step_required": p.everyone || (p.outside && !local),
        "two_step_setup_only": p.everyone && !a.two_step,
    })
}

fn client(conn: &Conn, headers: &HeaderMap) -> Client {
    Client::new(conn.ip, &crate::policy::device(headers))
}

fn with_cookie(mut res: Response, cookie: &str) -> Response {
    if let Ok(v) = HeaderValue::from_str(cookie) {
        res.headers_mut().append(header::SET_COOKIE, v);
    }
    res
}

fn signed_in_answer(app: &AppState, ns: &NewSession, local: bool) -> ApiResult<Response> {
    let cars = app.accounts.car_access(ns.account.id)?;
    let body = json!({ "status": "done", "me": me_json(app, &ns.account, &cars, local) });
    Ok(with_cookie(Json(body).into_response(), &ns.set_cookie()))
}

async fn get_setup(State(app): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    app.blocking(move |app| {
        Ok(Json(match crate::moving::site(app) {
            Ok(s) => {
                let holds = crate::moving::holds_claim_now(app, &s, &headers);
                crate::moving::setup_answer(app, &s, holds)
            }
            Err(_) => crate::moving::plain_setup_answer(app),
        }))
    })
    .await
}

async fn post_setup(
    State(app): State<AppState>,
    Extension(conn): Extension<Conn>,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Response> {
    body.only(&["code", "name", "password"])?;
    let (code, name, password) =
        (body.req_str("code")?.to_string(), body.req_str("name")?.to_string(), body.req_str("password")?.to_string());
    let c = client(&conn, &headers);
    let local = c.local;
    app.blocking(move |app| {
        // like `POST /setup/fresh`: no first admin while a move or a restore into this server is under way (its
        // switch would set the new account aside)
        // the code first (a wrong one is a wrong one, and counted, while a move or a restore runs too), then a
        // move or a restore that runs. The gate is held until the account is made: none begins in between.
        let site = crate::moving::site(app).ok();
        let gate = site.as_ref().map(|s| s.setup_gate());
        let ns = app.accounts.setup_when(&code, &name, &password, &c, || -> ApiResult<()> {
            if let Some(s) = &site {
                crate::moving::refuse_if_busy(s)?;
                s.paused("setup-ready");
            }
            Ok(())
        })?;
        drop(gate);
        // like `POST /setup/fresh`: the recovery key is shown this once (from its file). A claim is left alone: it
        // may be another browser's, which still reads its restore's result.
        let recovery = site.as_ref().and_then(|s| moving::import::recovery_from_file(&s.recovery_file()));
        let cars = app.accounts.car_access(ns.account.id)?;
        let body = json!({ "status": "done", "me": me_json(app, &ns.account, &cars, local),
                           "recovery_key": recovery.as_ref().map(|k| k.as_str()) });
        Ok(with_cookie(Json(body).into_response(), &ns.set_cookie()))
    })
    .await
}

async fn sign_in(
    State(app): State<AppState>,
    Extension(conn): Extension<Conn>,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Response> {
    body.only(&["name", "password"])?;
    let (name, password) = (body.req_str("name")?.to_string(), body.req_str("password")?.to_string());
    let c = client(&conn, &headers);
    let local = c.local;
    app.blocking(move |app| match app.accounts.sign_in(&name, &password, &c)? {
        SignIn::Done(ns) => signed_in_answer(app, &ns, local),
        SignIn::NeedCode { pending, expires_ms } => Ok(Json(json!({
            "status": "two_step_pending",
            "pending": pending.as_str(),
            "expires_ms": expires_ms,
        }))
        .into_response()),
    })
    .await
}

async fn two_step(
    State(app): State<AppState>,
    Extension(conn): Extension<Conn>,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Response> {
    body.only(&["pending", "code"])?;
    let (pending, code) = (body.req_str("pending")?.to_string(), body.req_str("code")?.to_string());
    let c = client(&conn, &headers);
    let local = c.local;
    app.blocking(move |app| {
        let ns = app.accounts.finish_sign_in(&pending, &code, &c)?;
        signed_in_answer(app, &ns, local)
    })
    .await
}

async fn sign_out(State(app): State<AppState>, v: Viewer) -> ApiResult<Response> {
    let (token, c, id) = (zeroize::Zeroizing::new(v.token().to_string()), v.client().clone(), v.id());
    v.mark_audited();
    app.blocking(move |app| Ok(app.accounts.sign_out(&token, &c)?)).await?;
    app.events.publish(Event::SessionsEnded { account: id });
    Ok(with_cookie(Json(json!({ "status": "signed_out" })).into_response(), &accounts::clear_cookie()))
}

async fn me(State(app): State<AppState>, v: Viewer) -> Json<Value> {
    Json(me_json(&app, &v.signed_in().account, v.cars(), v.client().local))
}

fn session_json(s: &accounts::SessionView, this: &str) -> Value {
    json!({
        "id": s.id,
        "created_ms": s.created_ms,
        "last_seen_ms": s.last_seen_ms,
        "expires_ms": s.expires_ms,
        "device": s.device,
        "address": s.address,
        "this_one": s.id == this,
    })
}

async fn sessions_of(app: &AppState, v: &Viewer) -> ApiResult<Vec<accounts::SessionView>> {
    let v2 = v.clone();
    app.blocking(move |app| Ok(app.accounts.sessions(v2.signed_in(), v2.id())?)).await
}

async fn my_sessions(State(app): State<AppState>, v: Viewer) -> ApiResult<Json<Value>> {
    let list = sessions_of(&app, &v).await?;
    let items: Vec<Value> = list.iter().map(|s| session_json(s, v.session_id())).collect();
    Ok(Json(json!({ "items": items, "next": null })))
}

async fn my_session(State(app): State<AppState>, v: Viewer, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let list = sessions_of(&app, &v).await?;
    list.iter()
        .find(|s| s.id == id)
        .map(|s| Json(session_json(s, v.session_id())))
        .ok_or_else(|| ApiError::not_found("no_such_session"))
}

async fn end_other_sessions(State(app): State<AppState>, v: Viewer) -> ApiResult<Json<Value>> {
    v.mark_audited();
    let v2 = v.clone();
    let n = app.blocking(move |app| Ok(app.accounts.revoke_sessions(v2.signed_in(), v2.id(), true)?)).await?;
    app.events.publish(Event::SessionsEnded { account: v.id() });
    Ok(Json(json!({ "ended": n })))
}

async fn end_session(State(app): State<AppState>, v: Viewer, Path(id): Path<String>) -> ApiResult<Response> {
    v.mark_audited();
    let this = id == v.session_id();
    let (v2, id2) = (v.clone(), id.clone());
    let ended = app.blocking(move |app| Ok(app.accounts.revoke_session(v2.signed_in(), v2.id(), &id2)?)).await?;
    if !ended {
        return Err(ApiError::not_found("no_such_session"));
    }
    app.events.publish(Event::SessionsEnded { account: v.id() });
    let res = Json(json!({ "ended": 1, "id": id })).into_response();
    Ok(if this { with_cookie(res, &accounts::clear_cookie()) } else { res })
}

async fn change_password(State(app): State<AppState>, v: Viewer, body: JsonBody) -> ApiResult<Json<Value>> {
    body.only(&["password", "new_password", "code"])?;
    let (old, new) = (body.req_str("password")?.to_string(), body.req_str("new_password")?.to_string());
    let code = body.str("code")?.map(str::to_string);
    v.mark_audited();
    let v2 = v.clone();
    let n = app
        .blocking(move |app| {
            app.accounts.set_password(v2.signed_in(), Confirm::new(&old, code.as_deref()), v2.id(), &new).map_err(|e| {
                match e {
                    accounts::Error::WeakPassword(why) => ApiError::invalid("new_password", why),
                    e => e.into(),
                }
            })
        })
        .await?;
    app.events.publish(Event::SessionsEnded { account: v.id() });
    Ok(Json(json!({ "signed_out_others": n })))
}

async fn two_step_state(State(app): State<AppState>, v: Viewer) -> ApiResult<Json<Value>> {
    let on = app.accounts.account(v.id()).is_some_and(|a| a.two_step);
    let v2 = v.clone();
    let left = if on {
        Some(app.blocking(move |app| Ok(app.accounts.recovery_codes_left(v2.signed_in())?)).await?)
    } else {
        None
    };
    let p = app.accounts.two_step_policy();
    Ok(Json(json!({
        "enabled": on,
        "recovery_codes_left": left,
        "required": { "everyone": p.everyone, "outside": p.outside },
    })))
}

async fn two_step_start(State(app): State<AppState>, v: Viewer) -> ApiResult<Response> {
    v.mark_audited();
    let v2 = v.clone();
    let setup = app.blocking(move |app| Ok(app.accounts.start_two_step(v2.signed_in())?)).await?;
    let body = json!({ "secret": setup.secret.as_str(), "uri": setup.uri.as_str() });
    Ok((StatusCode::CREATED, Json(body)).into_response())
}

async fn two_step_confirm(State(app): State<AppState>, v: Viewer, body: JsonBody) -> ApiResult<Json<Value>> {
    body.only(&["password", "code"])?;
    let (password, code) = (body.req_str("password")?.to_string(), body.req_str("code")?.to_string());
    v.mark_audited();
    let v2 = v.clone();
    let codes = app.blocking(move |app| Ok(app.accounts.confirm_two_step(v2.signed_in(), &password, &code)?)).await?;
    Ok(Json(json!({ "enabled": true, "recovery_codes": codes.as_slice() })))
}

async fn two_step_off(State(app): State<AppState>, v: Viewer, body: JsonBody) -> ApiResult<Json<Value>> {
    body.only(&["password", "code"])?;
    let password = body.req_str("password")?.to_string();
    let code = body.str("code")?.map(str::to_string);
    v.mark_audited();
    let v2 = v.clone();
    app.blocking(move |app| {
        Ok(app.accounts.disable_two_step(v2.signed_in(), Confirm::new(&password, code.as_deref()), v2.id())?)
    })
    .await?;
    Ok(Json(json!({ "enabled": false })))
}

async fn new_recovery_codes(State(app): State<AppState>, v: Viewer, body: JsonBody) -> ApiResult<Json<Value>> {
    body.only(&["password", "code"])?;
    let password = body.req_str("password")?.to_string();
    let code = body.str("code")?.map(str::to_string);
    v.mark_audited();
    let v2 = v.clone();
    let codes = app
        .blocking(move |app| {
            Ok(app.accounts.new_recovery_codes(v2.signed_in(), Confirm::new(&password, code.as_deref()))?)
        })
        .await?;
    Ok(Json(json!({ "recovery_codes": codes.as_slice() })))
}
