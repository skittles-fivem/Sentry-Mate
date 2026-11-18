//! Accounts, the two-step rule and the audit log, for admins (docs/ui-api.md, "Sign-in, accounts and
//! preferences"):
//!
//! | Route | |
//! |---|---|
//! | `GET /accounts` | `{ items, next, total }`, by name |
//! | `POST /accounts` | `{ name, password, role, cars, confirm_password }` -> 201 |
//! | `GET`/`PATCH`/`DELETE /accounts/{account}` | PATCH with `version`: `role`, `cars`, `password` |
//! | `DELETE /accounts/{account}/two-step` | turns another account's two-step sign-in off (a lost phone) |
//! | `GET`/`PUT /two-step-policy` | `{ everyone, outside }` |
//! | `GET /audit` | `{ items: [{ at_ms, account, action, target, summary, car, address }], next }` |
//!
//! An account is `{ id, name, role, cars, two_step, two_step_required, created_ms, version }`. Every change to an
//! account asks for the acting admin's own password again (`confirm_password` in the body, or the
//! `X-Sentry-Mate-Password` header): without it `403 password_needed`, a wrong one `403 wrong_password`. Account
//! names can't be changed (remove the account and add it again). Two-step sign-in is required for everyone or from
//! outside the home network ([`/two-step-policy`]), not per account: `two_step_required: true` on one account is
//! `422`.

use crate::auth::{parse_role, role_str};
use crate::ctx::{Admin, JsonBody, cars_json, split_details};
use crate::error::{ApiError, ApiResult, Fields};
use crate::events::Event;
use crate::{AppState, Params, ids};
use accounts::{AccountId, AccountView, CarAccess, NewAccount, TwoStepPolicy};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use model::CarId;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub const PASSWORD_HEADER: &str = "x-sentry-mate-password";
pub const CODE_HEADER: &str = "x-sentry-mate-code";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/accounts", get(list).post(create))
        .route("/api/v1/accounts/{account}", get(one).patch(patch).delete(remove))
        .route("/api/v1/accounts/{account}/two-step", delete(two_step_off))
        .route("/api/v1/two-step-policy", get(get_policy).put(put_policy))
        .route("/api/v1/audit", get(audit))
}

/// The acting admin's password asked again: `confirm_password` in the body, else the header.
pub fn confirm_password(body: Option<&JsonBody>, headers: &HeaderMap) -> ApiResult<Zeroizing<String>> {
    if let Some(b) = body
        && let Some(p) = b.str("confirm_password")?
    {
        return Ok(Zeroizing::new(p.to_string()));
    }
    headers.get(PASSWORD_HEADER).and_then(|v| v.to_str().ok()).map(|p| Zeroizing::new(p.to_string())).ok_or_else(|| {
        ApiError::new(403, "password_needed", "Type your own password again to change this.").with_fields(
            Fields::one("confirm_password", "Your own password, again.").into_bad_input().fields.unwrap_or_default(),
        )
    })
}

/// The two-step code that may come with it (`code` in the body, else the header).
pub fn confirm_code(body: Option<&JsonBody>, headers: &HeaderMap) -> ApiResult<Option<String>> {
    if let Some(b) = body
        && let Some(c) = b.str("code")?
    {
        return Ok(Some(c.to_string()));
    }
    Ok(headers.get(CODE_HEADER).and_then(|v| v.to_str().ok()).map(str::to_string))
}

fn version_of(a: &AccountView, cars: &CarAccess) -> u64 {
    let mut h = Sha256::new();
    h.update(a.name.as_bytes());
    h.update([0, a.role as u8, u8::from(a.two_step)]);
    match cars.to_list() {
        None => h.update(b"all"),
        Some(list) => {
            for c in list {
                h.update(c.to_le_bytes());
            }
        }
    }
    let d = h.finalize();
    let mut b = [0u8; 8];
    b[2..].copy_from_slice(&d[..6]);
    u64::from_be_bytes(b).max(1)
}

/// An account as the API writes it.
pub fn account_json(app: &AppState, a: &AccountView, cars: &CarAccess) -> Value {
    json!({
        "id": ids::account(a.id),
        "name": a.name,
        "role": role_str(a.role),
        "cars": cars_json(cars),
        "two_step": a.two_step,
        "two_step_required": app.accounts.two_step_policy().everyone,
        "created_ms": a.created_ms,
        "version": version_of(a, cars),
    })
}

fn load(app: &AppState, id: AccountId) -> ApiResult<(AccountView, CarAccess)> {
    let a = app.accounts.account(id).ok_or_else(|| ApiError::not_found("no_such_account"))?;
    let cars = app.accounts.car_access(id)?;
    Ok((a, cars))
}

fn account_id(text: &str) -> ApiResult<AccountId> {
    ids::parse_account(text).ok_or_else(|| ApiError::not_found("no_such_account"))
}

async fn list(State(app): State<AppState>, Admin(_): Admin, q: Params) -> ApiResult<Json<Value>> {
    let paging = q.paging()?;
    let after: Option<String> = app.cursor(&q)?;
    let q2 = q.clone();
    app.blocking(move |app| {
        let mut page = app.accounts.list(after.as_deref(), paging.limit + 1);
        let more = page.len() > paging.limit;
        page.truncate(paging.limit);
        let next = match (more, page.last()) {
            (true, Some(last)) => Some(app.next_cursor(&q2, &last.name)?),
            _ => None,
        };
        let mut items = Vec::with_capacity(page.len());
        for a in &page {
            let cars = app.accounts.car_access(a.id)?;
            items.push(account_json(app, a, &cars));
        }
        Ok(Json(json!({ "items": items, "next": next, "total": app.accounts.count() })))
    })
    .await
}

async fn one(State(app): State<AppState>, Admin(_): Admin, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let id = account_id(&id)?;
    app.blocking(move |app| {
        let (a, cars) = load(app, id)?;
        Ok(Json(account_json(app, &a, &cars)))
    })
    .await
}

/// The cars of a body's `cars`, each one existing.
fn body_cars(app: &AppState, body: &JsonBody, f: &mut Fields) -> ApiResult<Option<Option<Vec<CarId>>>> {
    let cars = body.cars("cars")?;
    if let Some(Some(list)) = &cars
        && list.iter().any(|c| !app.car_exists(*c))
    {
        f.add("cars", "Unknown car in the list.");
    }
    Ok(cars)
}

fn no_per_account_two_step(body: &JsonBody, f: &mut Fields) -> ApiResult<()> {
    if body.bool("two_step_required")? == Some(true) {
        f.add(
            "two_step_required",
            "Two-step sign-in is required for every account or from outside the home network (Accounts, \
             two-step rule), not for one account.",
        );
    }
    Ok(())
}

async fn create(
    State(app): State<AppState>,
    Admin(v): Admin,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Response> {
    body.only(&["name", "password", "role", "cars", "two_step_required", "confirm_password", "code"])?;
    let confirm = confirm_password(Some(&body), &headers)?;
    let name = body.req_str("name")?.to_string();
    let password = body.req_str("password")?.to_string();
    let role = body.req_str("role")?;
    let mut f = Fields::new();
    let role = parse_role(role).unwrap_or_else(|| {
        f.add("role", "Must be admin or watch.");
        accounts::Role::WatchOnly
    });
    let cars = body_cars(&app, &body, &mut f)?.unwrap_or(None);
    no_per_account_two_step(&body, &mut f)?;
    f.invalid()?;
    v.mark_audited();
    let v2 = v.clone();
    let out = app
        .blocking(move |app| {
            let new = NewAccount { name: &name, password: &password, role, cars };
            let a = app.accounts.create_account(v2.signed_in(), &confirm, new)?;
            let cars = app.accounts.car_access(a.id)?;
            Ok(account_json(app, &a, &cars))
        })
        .await?;
    Ok((StatusCode::CREATED, Json(out)).into_response())
}

async fn patch(
    State(app): State<AppState>,
    Admin(v): Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    let id = account_id(&id)?;
    body.only(&["version", "name", "role", "cars", "password", "two_step_required", "confirm_password", "code"])?;
    let version = body.version()?;
    let mut f = Fields::new();
    let role = body.str("role")?.map(|r| {
        parse_role(r).unwrap_or_else(|| {
            f.add("role", "Must be admin or watch.");
            accounts::Role::WatchOnly
        })
    });
    let cars = body_cars(&app, &body, &mut f)?;
    let password = body.str("password")?.filter(|p| !p.is_empty()).map(|p| Zeroizing::new(p.to_string()));
    let name = body.str("name")?.map(str::to_string);
    no_per_account_two_step(&body, &mut f)?;
    f.invalid()?;
    // asked for only when something really changes (a page may send the unchanged role and cars along)
    let confirm = confirm_password(Some(&body), &headers);
    v.mark_audited();
    let v2 = v.clone();
    let (out, cars_changed, password_changed) = app
        .blocking(move |app| {
            let (a, now_cars) = load(app, id)?;
            let current = version_of(&a, &now_cars);
            if version != current {
                return Err(ApiError::stale_version(current));
            }
            if let Some(n) = &name
                && n.trim() != a.name
            {
                return Err(ApiError::invalid("name", "Names can't be changed: add a new account instead."));
            }
            let changes = role.is_some_and(|r| r != a.role)
                || cars.as_ref().is_some_and(|c| CarAccess::from_list(c.clone()) != now_cars)
                || password.is_some();
            let confirm = if changes { confirm? } else { Zeroizing::new(String::new()) };
            let mut cars_changed = false;
            if let Some(r) = role
                && r != a.role
            {
                app.accounts.set_role(v2.signed_in(), &confirm, id, r)?;
                cars_changed = true;
            }
            if let Some(c) = cars
                && CarAccess::from_list(c.clone()) != now_cars
            {
                app.accounts.set_cars(v2.signed_in(), &confirm, id, c)?;
                cars_changed = true;
            }
            if let Some(p) = &password {
                app.accounts.set_password(v2.signed_in(), &confirm, id, p).map_err(|e| match e {
                    accounts::Error::WeakPassword(why) => ApiError::invalid("password", why),
                    e => e.into(),
                })?;
            }
            let (a, cars) = load(app, id)?;
            Ok((account_json(app, &a, &cars), cars_changed, password.is_some()))
        })
        .await?;
    if cars_changed {
        app.events.publish(Event::AccountCars { account: id });
    }
    if password_changed {
        app.events.publish(Event::SessionsEnded { account: id });
    }
    Ok(Json(out))
}

async fn remove(
    State(app): State<AppState>,
    Admin(v): Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    let id = account_id(&id)?;
    body.only(&["confirm_password", "code", "version"])?;
    let confirm = confirm_password(Some(&body), &headers)?;
    v.mark_audited();
    let v2 = v.clone();
    app.blocking(move |app| {
        app.accounts.remove_account(v2.signed_in(), &confirm, id)?;
        app.prefs.remove(id)
    })
    .await?;
    app.events.publish(Event::SessionsEnded { account: id });
    Ok(Json(json!({ "deleted": true, "id": ids::account(id) })))
}

async fn two_step_off(
    State(app): State<AppState>,
    Admin(v): Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    let id = account_id(&id)?;
    body.only(&["confirm_password", "code"])?;
    let confirm = confirm_password(Some(&body), &headers)?;
    v.mark_audited();
    let v2 = v.clone();
    app.blocking(move |app| {
        app.accounts.disable_two_step(v2.signed_in(), &confirm, id)?;
        let (a, cars) = load(app, id)?;
        Ok(Json(account_json(app, &a, &cars)))
    })
    .await
}

async fn get_policy(State(app): State<AppState>, Admin(_): Admin) -> Json<Value> {
    let p = app.accounts.two_step_policy();
    Json(json!({ "everyone": p.everyone, "outside": p.outside }))
}

async fn put_policy(
    State(app): State<AppState>,
    Admin(v): Admin,
    headers: HeaderMap,
    body: JsonBody,
) -> ApiResult<Json<Value>> {
    body.only(&["everyone", "outside", "confirm_password", "code"])?;
    let now = app.accounts.two_step_policy();
    let policy = TwoStepPolicy {
        everyone: body.bool("everyone")?.unwrap_or(now.everyone),
        outside: body.bool("outside")?.unwrap_or(now.outside),
    };
    let confirm = confirm_password(Some(&body), &headers)?;
    v.mark_audited();
    let v2 = v.clone();
    app.blocking(move |app| Ok(app.accounts.set_two_step_policy(v2.signed_in(), &confirm, policy)?)).await?;
    Ok(Json(json!({ "everyone": policy.everyone, "outside": policy.outside })))
}

async fn audit(State(app): State<AppState>, Admin(_): Admin, q: Params) -> ApiResult<Json<Value>> {
    let period = q.period(app.now(), None)?;
    let paging = q.paging()?;
    let start: Option<String> = app.cursor(&q)?;
    let q2 = q.clone();
    app.blocking(move |app| {
        let audit = app.accounts.audit();
        let mut cursor = start;
        let mut items = Vec::new();
        let next: Option<String>;
        // newest first: skip what is newer than the period, stop at what is older
        'pages: loop {
            let want = (paging.limit - items.len()).clamp(1, accounts::audit::MAX_PAGE);
            let page = audit.list(want, cursor.as_deref())?;
            let n = page.entries.len();
            for (i, e) in page.entries.into_iter().enumerate() {
                if e.t_ms >= period.to_ms {
                    continue;
                }
                if e.t_ms < period.from_ms {
                    next = None;
                    break 'pages;
                }
                let (target, summary, car) = split_details(&e.details);
                items.push(json!({
                    "at_ms": e.t_ms,
                    "account": e.account,
                    "action": e.action,
                    "target": target,
                    "summary": summary,
                    "car": car,
                    "address": e.address,
                }));
                // a page holds at most what is still wanted, so the list is full only at a page's last entry
                if items.len() == paging.limit && i + 1 == n {
                    next = page.next.clone();
                    break 'pages;
                }
            }
            match page.next {
                Some(c) if n > 0 => cursor = Some(c),
                _ => {
                    next = None;
                    break;
                }
            }
        }
        let next = match next {
            Some(c) => Some(app.next_cursor(&q2, &c)?),
            None => None,
        };
        Ok(Json(json!({ "items": items, "next": next })))
    })
    .await
}
