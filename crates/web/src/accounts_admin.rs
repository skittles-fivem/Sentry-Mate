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
//! | `GET /audit` | `{ items: [{ at_ms, account, action, group, target, summary, car, address }], next }`; `group=`, `who=` |
//!
//! An account is `{ id, name, role, cars, two_step, two_step_required, created_ms, version }`. Every change to an
//! account asks for the acting admin's own password again (`confirm_password` in the body, or the
//! `X-Sentry-Mate-Password` header): without it `403 password_needed`, a wrong one `403 wrong_password`. When the
//! acting admin has two-step sign-in on, a code from the app or a recovery code too (`code`, or the
//! `X-Sentry-Mate-Code` header; used up, one per request): without it or wrong `401 wrong_code`. Account
//! names can't be changed (remove the account and add it again). Two-step sign-in is required for everyone or from
//! outside the home network ([`/two-step-policy`]), not per account: `two_step_required: true` on one account is
//! `422`.

use crate::auth::{parse_role, role_str};
use crate::ctx::{Admin, JsonBody, cars_json, split_details};
use crate::error::{ApiError, ApiResult, Fields};
use crate::events::Event;
use crate::{AppState, Params, ids};
use accounts::{AccountId, AccountView, CarAccess, CodeOnce, Confirm, NewAccount, TwoStepPolicy};
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
pub const PASSWORD_ENCODED_HEADER: &str = "x-sentry-mate-password-encoded";
pub const CODE_HEADER: &str = "x-sentry-mate-code";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/accounts", get(list).post(create))
        .route("/api/v1/accounts/{account}", get(one).patch(patch).delete(remove))
        .route("/api/v1/accounts/{account}/two-step", delete(two_step_off))
        .route("/api/v1/two-step-policy", get(get_policy).put(put_policy))
        .route("/api/v1/audit", get(audit))
}

/// A header's value as text: its bytes as UTF-8 (a password is any text, not only visible ASCII). `Err` when the
/// bytes are not UTF-8 (a browser's `fetch` writes U+0080 to U+00FF as single bytes).
fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<Result<&'a str, ()>> {
    headers.get(name).map(|v| std::str::from_utf8(v.as_bytes()).map_err(|_| ()))
}

/// The acting admin's password asked again: `confirm_password` in the body, else the
/// `X-Sentry-Mate-Password-Encoded` header (percent-encoded UTF-8, for a request whose body is not JSON and a
/// password a header can't hold as it is), else the `X-Sentry-Mate-Password` header (UTF-8).
pub fn confirm_password(body: Option<&JsonBody>, headers: &HeaderMap) -> ApiResult<Zeroizing<String>> {
    if let Some(b) = body
        && let Some(p) = b.str("confirm_password")?
    {
        return Ok(Zeroizing::new(p.to_string()));
    }
    if let Some(raw) = headers.get(PASSWORD_ENCODED_HEADER) {
        return percent_encoding::percent_decode(raw.as_bytes())
            .decode_utf8()
            .map(|p| Zeroizing::new(p.into_owned()))
            .map_err(|_| {
                ApiError::bad_input("confirm_password", "X-Sentry-Mate-Password-Encoded is percent-encoded UTF-8.")
            });
    }
    match header_text(headers, PASSWORD_HEADER) {
        Some(Ok(p)) => Ok(Zeroizing::new(p.to_string())),
        Some(Err(())) => Err(ApiError::bad_input(
            "confirm_password",
            "X-Sentry-Mate-Password is not UTF-8: send the password percent-encoded in X-Sentry-Mate-Password-Encoded.",
        )),
        None => Err(ApiError::new(403, "password_needed", "Type your own password again to change this.").with_fields(
            Fields::one("confirm_password", "Your own password, again.").into_bad_input().fields.unwrap_or_default(),
        )),
    }
}

/// The two-step code that may come with it (`code` in the body, else the header).
pub fn confirm_code(body: Option<&JsonBody>, headers: &HeaderMap) -> ApiResult<Option<String>> {
    if let Some(b) = body
        && let Some(c) = b.str("code")?
    {
        return Ok(Some(c.to_string()));
    }
    // a code is digits or a recovery code: bytes that are not text are a wrong code, not a missing one
    Ok(header_text(headers, CODE_HEADER).map(|c| c.unwrap_or("\u{fffd}").to_string()))
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

async fn list(State(app): State<AppState>, Admin(_): Admin, q: Params) -> ApiResult<Response> {
    let paging = q.paging()?;
    let after: Option<String> = app.cursor(&q)?;
    // a CSV answer is every account, not one page
    let (limit, after) = if q.wants_csv()? { (usize::MAX - 1, None) } else { (paging.limit, after) };
    let q2 = q.clone();
    app.blocking(move |app| {
        let mut page = app.accounts.list(after.as_deref(), limit + 1);
        let more = page.len() > limit;
        page.truncate(limit);
        let next = match (more, page.last()) {
            (true, Some(last)) => Some(app.next_cursor(&q2, &last.name)?),
            _ => None,
        };
        let mut items = Vec::with_capacity(page.len());
        for a in &page {
            let cars = app.accounts.car_access(a.id)?;
            items.push(account_json(app, a, &cars));
        }
        crate::csv::answer(&q2, "accounts", json!({ "items": items, "next": next, "total": app.accounts.count() }))
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
    let code = confirm_code(Some(&body), &headers)?;
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
            let a = app.accounts.create_account(v2.signed_in(), Confirm::new(&confirm, code.as_deref()), new)?;
            let cars = app.accounts.car_access(a.id)?;
            Ok(account_json(app, &a, &cars))
        })
        .await?;
    Ok((StatusCode::CREATED, Json(out)).into_response())
}

/// Held while an account change is compared with its version and made.
static PATCHING: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    if let Some(p) = &password
        && let Err(accounts::Error::WeakPassword(why)) = accounts::check_password("", p)
    {
        // refused before anything is changed: a 422 leaves the role and the cars as they were
        f.add("password", why);
    }
    let name = body.str("name")?.map(str::to_string);
    no_per_account_two_step(&body, &mut f)?;
    f.invalid()?;
    // asked for only when something really changes (a page may send the unchanged role and cars along)
    let confirm = confirm_password(Some(&body), &headers);
    let code = confirm_code(Some(&body), &headers)?;
    v.mark_audited();
    let v2 = v.clone();
    let (out, cars_changed, password_changed) = app
        .blocking(move |app| {
            // one change at a time: the version is compared and the change made under one lock, so of two changes
            // sent with the same version one wins and the other gets 409
            let _one = PATCHING.lock().unwrap_or_else(|e| e.into_inner());
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
            // the code is used up by the first change; the others in this request ask for the password only
            let once = CodeOnce::default();
            let confirm = Confirm::in_request(&confirm, code.as_deref(), &once);
            // what would be refused is refused before the first step: the steps are saved one by one
            if role.is_some_and(|r| r != a.role)
                && a.role == accounts::Role::Admin
                && app.accounts.list(None, usize::MAX).iter().filter(|x| x.role == accounts::Role::Admin).count() <= 1
            {
                return Err(accounts::Error::LastAdmin.into());
            }
            // another account's password first: it is the step that can still fail (the hashing is busy), and then
            // nothing else has changed. One's own last: each step checks the acting password again, and after a new
            // own password the one typed would no longer be it.
            let own = id == v2.id();
            let set_password = |app: &AppState| -> ApiResult<()> {
                if let Some(p) = &password {
                    app.accounts.set_password(v2.signed_in(), confirm, id, p).map_err(|e| match e {
                        accounts::Error::WeakPassword(why) => ApiError::invalid("password", why),
                        e => e.into(),
                    })?;
                }
                Ok(())
            };
            if !own {
                set_password(app)?;
            }
            let mut cars_changed = false;
            if let Some(r) = role
                && r != a.role
            {
                app.accounts.set_role(v2.signed_in(), confirm, id, r)?;
                cars_changed = true;
            }
            if let Some(c) = cars
                && CarAccess::from_list(c.clone()) != now_cars
            {
                app.accounts.set_cars(v2.signed_in(), confirm, id, c)?;
                cars_changed = true;
            }
            if own {
                set_password(app)?;
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
    let code = confirm_code(Some(&body), &headers)?;
    v.mark_audited();
    let v2 = v.clone();
    app.blocking(move |app| {
        app.accounts.remove_account(v2.signed_in(), Confirm::new(&confirm, code.as_deref()), id)?;
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
    let code = confirm_code(Some(&body), &headers)?;
    v.mark_audited();
    let v2 = v.clone();
    let out = app
        .blocking(move |app| {
            app.accounts.disable_two_step(v2.signed_in(), Confirm::new(&confirm, code.as_deref()), id)?;
            let (a, cars) = load(app, id)?;
            Ok(Json(account_json(app, &a, &cars)))
        })
        .await?;
    // its open streams check again whether the two-step rules still let it in
    app.events.publish(Event::SessionsEnded { account: id });
    Ok(out)
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
    let code = confirm_code(Some(&body), &headers)?;
    v.mark_audited();
    let v2 = v.clone();
    app.blocking(move |app| {
        Ok(app.accounts.set_two_step_policy(v2.signed_in(), Confirm::new(&confirm, code.as_deref()), policy)?)
    })
    .await?;
    // every open stream checks at once whether the new rule still lets its account in
    app.events.publish(Event::PolicyChanged);
    Ok(Json(json!({ "everyone": policy.everyone, "outside": policy.outside })))
}

async fn audit(State(app): State<AppState>, Admin(_): Admin, q: Params) -> ApiResult<Response> {
    let period = q.period(app.now(), None)?;
    let mut paging = q.paging()?;
    let mut start: Option<String> = app.cursor(&q)?;
    // a CSV answer is every matching entry of the period, not one page
    if q.wants_csv()? {
        (paging.limit, start) = (usize::MAX, None);
    }
    let filter = AuditFilter::of(&q)?;
    let q2 = q.clone();
    app.blocking(move |app| {
        let audit = app.accounts.audit();
        let mut cursor = start;
        let mut items = Vec::new();
        let next: Option<String>;
        // newest first: skip what is newer than the period, stop at what is older; a filtered list reads on until
        // its page is full of matching entries
        'pages: loop {
            let want =
                if filter.any() { 200 } else { (paging.limit - items.len()).clamp(1, accounts::audit::MAX_PAGE) };
            let (entries, page_next) = audit.list_at(want, cursor.as_deref())?;
            let n = entries.len();
            for (i, (at, e)) in entries.into_iter().enumerate() {
                if e.t_ms >= period.to_ms {
                    continue;
                }
                if e.t_ms < period.from_ms {
                    next = None;
                    break 'pages;
                }
                let (target, summary, car) = split_details(&e.details);
                let row = crate::audit_codes::readable(&e.action, target, car, summary);
                let group = crate::audit_codes::group_of(&row.action);
                if !filter.keeps(group, e.account.as_deref()) {
                    continue;
                }
                items.push(json!({
                    "at_ms": e.t_ms,
                    "account": e.account,
                    "action": row.action,
                    "group": group,
                    "target": row.target,
                    "summary": row.summary,
                    "car": row.car,
                    "address": e.address,
                }));
                if items.len() == paging.limit {
                    // the last of a page that was the oldest: nothing after it
                    next = if i + 1 == n && page_next.is_none() { None } else { Some(at) };
                    break 'pages;
                }
            }
            match page_next {
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
        crate::csv::answer(&q2, "audit-log", json!({ "items": items, "next": next }))
    })
    .await
}

/// The audit list's filters: `group=<id>[,<id>]` (see [`crate::audit_codes::GROUPS`], and `other`) and
/// `who=<account name>` (`*server`: the entries the server wrote itself, with no account).
struct AuditFilter {
    groups: Option<Vec<String>>,
    who: Option<Option<String>>,
}

impl AuditFilter {
    fn of(q: &Params) -> ApiResult<AuditFilter> {
        let groups = match q.get("group").map(str::trim).filter(|g| !g.is_empty()) {
            None => None,
            Some(text) => {
                let mut list = Vec::new();
                for g in text.split(',').map(str::trim).filter(|g| !g.is_empty()) {
                    let known =
                        g == crate::audit_codes::OTHER || crate::audit_codes::GROUPS.iter().any(|(id, _)| *id == g);
                    if !known {
                        let ids: Vec<&str> = crate::audit_codes::GROUPS.iter().map(|(id, _)| *id).collect();
                        return Err(ApiError::bad_input(
                            "group",
                            &format!("Must be one of {}, other.", ids.join(", ")),
                        ));
                    }
                    list.push(g.to_string());
                }
                Some(list)
            }
        };
        let who = match q.get("who").map(str::trim).filter(|w| !w.is_empty()) {
            None => None,
            Some("*server") => Some(None),
            Some(name) => Some(Some(name.to_string())),
        };
        Ok(AuditFilter { groups, who })
    }

    fn any(&self) -> bool {
        self.groups.is_some() || self.who.is_some()
    }

    fn keeps(&self, group: &str, account: Option<&str>) -> bool {
        self.groups.as_ref().is_none_or(|g| g.iter().any(|x| x == group))
            && self.who.as_ref().is_none_or(|w| match (w, account) {
                (None, None) => true,
                (Some(w), Some(a)) => w.eq_ignore_ascii_case(a),
                _ => false,
            })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use axum::http::HeaderValue;

    fn headers(name: &'static str, value: &[u8]) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(name, HeaderValue::from_bytes(value).unwrap());
        h
    }

    #[test]
    fn the_password_header_is_utf8_or_percent_encoded() {
        let p = confirm_password(None, &headers(PASSWORD_HEADER, "pässwort 鍵".as_bytes())).unwrap();
        assert_eq!(p.as_str(), "pässwort 鍵");
        // what a browser's fetch writes for U+00E4: one byte, not UTF-8
        let e = confirm_password(None, &headers(PASSWORD_HEADER, b"p\xe4sswort")).unwrap_err();
        assert_eq!((e.status, e.code), (400, "bad_input"));
        let p = confirm_password(None, &headers(PASSWORD_ENCODED_HEADER, b"%20p%C3%A4ss%25wort+%E9%8D%B5")).unwrap();
        assert_eq!(p.as_str(), " päss%wort+鍵");
        let e = confirm_password(None, &headers(PASSWORD_ENCODED_HEADER, b"p%E4sswort")).unwrap_err();
        assert_eq!((e.status, e.code), (400, "bad_input"));
        let e = confirm_password(None, &HeaderMap::new()).unwrap_err();
        assert_eq!((e.status, e.code), (403, "password_needed"));
        // a code that is not text is a wrong code, never "no code"
        assert_eq!(confirm_code(None, &headers(CODE_HEADER, b"123456")).unwrap().as_deref(), Some("123456"));
        assert!(confirm_code(None, &headers(CODE_HEADER, b"\xe4")).unwrap().is_some());
        assert_eq!(confirm_code(None, &HeaderMap::new()).unwrap(), None);
    }
}
