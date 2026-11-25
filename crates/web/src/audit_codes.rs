//! Every audit log code: its group (the "What" filter of Settings > Audit log) and the kind of item its entry's
//! `target` names (`<kind>:<id>`; `None`: the entry names no item). `ui/shared/audit-codes.json` is this table as the
//! UI reads it (a test keeps the two the same: `SENTRY_MATE_WRITE_AUDIT_CODES=1` writes it again).
//!
//! Entries stored before a code existed are read back mapped ([`readable`]): the guard's old `change` with a
//! `METHOD /route` summary becomes the route's own code when it has one, else "Changed <route words>"; the old
//! `settings:teslamate_local` target is `teslamate:local`, and a car match's car part becomes the row's `car`.

use crate::ids;

/// The groups, in the order the UI shows them: `(id, label)`.
pub const GROUPS: &[(&str, &str)] = &[
    ("sign-ins", "Sign-ins"),
    ("accounts", "Accounts and access"),
    ("settings", "Settings"),
    ("cars", "Cars and trips"),
    ("integrations", "Integrations"),
    ("imports", "Imports and moves"),
];

/// The group of a code the table does not know.
pub const OTHER: &str = "other";

/// One code of the audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Code {
    pub code: &'static str,
    pub group: &'static str,
    /// The kind of item the entry's `target` names (`car`, `session`, `ui-pack`...), `None` when it names none.
    pub item: Option<&'static str>,
    /// The sentence for an entry that was stored without one (or with only a few words of detail).
    pub said: &'static str,
}

const fn c(code: &'static str, group: &'static str, item: Option<&'static str>, said: &'static str) -> Code {
    Code { code, group, item, said }
}

const S: &str = "sign-ins";
const A: &str = "accounts";
const SET: &str = "settings";
const CARS: &str = "cars";
const INT: &str = "integrations";
const IMP: &str = "imports";

/// Every code, sorted by code.
pub const CODES: &[Code] = &[
    c("account-changed", A, Some("account"), "Changed an account"),
    c("account-created", A, Some("account"), "Added an account"),
    c("account-removed", A, Some("account"), "Removed an account"),
    c("can-key-renamed", CARS, Some("key"), "Named a CAN Bridge key"),
    c(
        "can-logs-missing",
        CARS,
        Some("car"),
        "Found CAN logs missing on the server and asked the car to send them again",
    ),
    c("can-logs-rechecked", CARS, Some("car"), "Checked CAN logs that had failed their check again, by newer rules"),
    c("can-logs-unsigned-stopped", CARS, Some("car"), "Stopped using unsigned CAN logs"),
    c("can-logs-unsigned-used", CARS, Some("car"), "Used unsigned CAN logs that had failed only for being unsigned"),
    c("car-changed", CARS, Some("car"), "Changed a car"),
    c("car-full-rebuild", CARS, Some("car"), "Ran a full rebuild"),
    c("car-gps-fit-removed", CARS, Some("car"), "Removed a past GPS layout fit of a car"),
    c("car-gps-layout", CARS, Some("car"), "Set how a car's Screenmate GPS frames are read"),
    c("car-overlap", CARS, Some("car"), "Chose how a car's overlapping data is read"),
    c("car-rebuild", CARS, Some("car"), "Rebuilt a car's history"),
    c("car-settings-changed", CARS, Some("car"), "Changed a car's settings"),
    c("cars-changed", A, Some("account"), "Changed which cars an account sees"),
    c("certificate-made", SET, Some("server"), "Made a new certificate"),
    c("change", SET, None, "Changed something"),
    c("charge-cost-entered", CARS, Some("charge"), "Entered the cost of a charge"),
    c("charge-cost-removed", CARS, Some("charge"), "Removed the cost of a charge"),
    c("confirm-failed", S, Some("account"), "Typed a wrong password or code to confirm a change"),
    c("drive-purpose-removed", CARS, Some("drive"), "Removed the purpose of a drive"),
    c("drive-purpose-set", CARS, Some("drive"), "Set the purpose of a drive"),
    c("export-downloaded", IMP, Some("export"), "Downloaded an export file"),
    c("export-made", IMP, Some("export"), "Made an export file"),
    c("export-removed", IMP, Some("export"), "Removed an export file"),
    c("import-cars", IMP, Some("import"), "Matched the cars of an import"),
    c("import-created", IMP, Some("import"), "Started uploading an import"),
    c("import-discarded", IMP, Some("import"), "Discarded an import"),
    c("import-rows-ignored", IMP, Some("import-hold"), "Left aside imported rows that wait for a car"),
    c("import-rows-moved", IMP, None, "Moved imported rows to another car"),
    c("import-started", IMP, Some("import"), "Started an import"),
    c("import-uploaded", IMP, Some("import"), "Uploaded an import file"),
    c("invoices-downloaded", INT, None, "Downloaded Supercharger invoices"),
    c("key-assigned", CARS, Some("key"), "Gave a car a key"),
    c("key-preferred", CARS, Some("key"), "Chose a car's preferred key"),
    c("key-unassigned", CARS, Some("key"), "Took a key from a car"),
    c("move-cancelled", IMP, Some("move"), "Cancelled the move"),
    c("move-code-made", IMP, Some("move"), "Made a move code"),
    c("move-undone", IMP, Some("move"), "Undid the move"),
    c("mqtt-changed", INT, Some("settings"), "Changed the MQTT settings"),
    c("mqtt-tested", INT, Some("settings"), "Tested the MQTT connection"),
    c("password-changed", A, Some("account"), "Changed a password"),
    c("place-changed", CARS, Some("place"), "Changed a place"),
    c("place-created", CARS, Some("place"), "Added a place"),
    c("place-deleted", CARS, Some("place"), "Deleted a place"),
    c("place-invoice-priced", CARS, Some("place"), "Priced a place from its Supercharger invoices"),
    c("place-repriced", CARS, Some("place"), "Priced a place's charges again"),
    c("preferences-changed", SET, Some("account"), "Changed preferences"),
    c("purpose-rule-created", CARS, Some("car"), "Added a purpose rule"),
    c("purpose-rule-removed", CARS, Some("car"), "Removed a purpose rule"),
    c("recovery-codes-new", A, Some("account"), "Made new recovery codes"),
    c("recovery-key-new", SET, Some("server"), "Made a new recovery key"),
    c("recovery-key-saved", SET, Some("server"), "Said the recovery key is saved"),
    c("recovery-key-shown", SET, Some("server"), "Showed the recovery key"),
    c("release-checked", SET, Some("server"), "Checked GitHub for a newer build"),
    c("role-changed", A, Some("account"), "Changed an account's role"),
    c("session-revoked", S, Some("session"), "Signed a device out"),
    c("sessions-revoked", S, Some("account"), "Signed devices out"),
    c("settings-changed", SET, Some("settings"), "Changed the server's settings"),
    c("setup", S, Some("account"), "Set up the server and its first admin"),
    c("setup-code-made", S, None, "Made a setup code for the first admin"),
    c("setup-code-wrong", S, None, "Typed a wrong setup code"),
    c("setup-move-cancelled", IMP, Some("move"), "Gave up moving the data here"),
    c("setup-move-finished-forced", IMP, Some("move"), "Finished the move with the recovery key"),
    c("setup-move-resumed", IMP, Some("move"), "Carried on moving the data here"),
    c("setup-move-started", IMP, Some("move"), "Started moving the data here"),
    c("setup-restore-cancelled", IMP, Some("move"), "Gave up restoring an export file"),
    c("setup-restore-started", IMP, Some("move"), "Started restoring an export file"),
    c("sign-in", S, Some("session"), "Signed in"),
    c("sign-in-failed", S, None, "Typed a wrong name or password"),
    c("sign-in-refused", S, Some("account"), "Was refused: two-step sign-in is required here"),
    c("sign-out", S, Some("session"), "Signed out"),
    c("signed-out", S, Some("session"), "Was signed out: two-step sign-in is required outside the home network"),
    c("storage-compacted", SET, Some("server"), "Started packing the stored data"),
    c("table-repaired", CARS, Some("car"), "Repaired a car's drives table"),
    c("tesla-account-added", INT, Some("tesla"), "Added a Tesla account"),
    c("tesla-account-changed", INT, Some("tesla"), "Changed a Tesla account"),
    c("tesla-account-error", INT, Some("tesla"), "Tesla refused a Tesla account"),
    c("tesla-account-paused", INT, Some("tesla"), "Paused a Tesla account"),
    c("tesla-account-removed", INT, Some("tesla"), "Removed a Tesla account"),
    c("tesla-account-resumed", INT, Some("tesla"), "Resumed a Tesla account"),
    c("tesla-car-matched", INT, Some("tesla"), "Matched a Tesla car"),
    c("tesla-invoices-changed", INT, Some("settings"), "Turned Supercharger invoices from Tesla on or off"),
    c("tesla-poll-changed", INT, Some("settings"), "Changed how often Tesla is asked"),
    c("tesla-rate-limited", INT, Some("tesla"), "Tesla started limiting requests"),
    c("tesla-sign-in-needed", INT, Some("tesla"), "Tesla needs a new sign-in"),
    c("tesla-token-replaced", INT, Some("tesla"), "Replaced a Tesla account's token"),
    c("teslafi-account-added", INT, Some("teslafi"), "Added a TeslaFi account"),
    c("teslafi-account-changed", INT, Some("teslafi"), "Changed a TeslaFi account"),
    c("teslafi-account-error", INT, Some("teslafi"), "A TeslaFi account is not read"),
    c("teslafi-account-paused", INT, Some("teslafi"), "Paused a TeslaFi account"),
    c("teslafi-account-removed", INT, Some("teslafi"), "Removed a TeslaFi account"),
    c("teslafi-account-resumed", INT, Some("teslafi"), "Resumed a TeslaFi account"),
    c("teslafi-car-matched", INT, Some("teslafi"), "Matched a TeslaFi car"),
    c("teslafi-invoices-changed", INT, Some("settings"), "Turned the charge costs from TeslaFi on or off"),
    c("teslafi-poll-changed", INT, Some("settings"), "Changed how often TeslaFi is asked"),
    c("teslafi-token-refused", INT, Some("teslafi"), "TeslaFi refused a token"),
    c("teslafi-token-replaced", INT, Some("teslafi"), "Replaced a TeslaFi account's token"),
    c("teslamate-local-added", INT, Some("teslamate"), "Added the local TeslaMate"),
    c("teslamate-local-auth-failed", INT, Some("teslamate"), "The local TeslaMate refused the sign-in"),
    c("teslamate-local-car-matched", INT, Some("teslamate"), "Matched a TeslaMate car"),
    c("teslamate-local-catchup-failed", INT, Some("teslamate"), "Catching up from the local TeslaMate failed"),
    c("teslamate-local-changed", INT, Some("teslamate"), "Changed the local TeslaMate"),
    c("teslamate-local-connection-refused", INT, Some("teslamate"), "The local TeslaMate can't be reached"),
    c("teslamate-local-paused", INT, Some("teslamate"), "Paused the local TeslaMate"),
    c("teslamate-local-pin-mismatch", INT, Some("teslamate"), "The local TeslaMate's certificate changed"),
    c("teslamate-local-quiet", INT, Some("teslamate"), "The local TeslaMate went quiet"),
    c("teslamate-local-removed", INT, Some("teslamate"), "Removed the local TeslaMate"),
    c("teslamate-local-resumed", INT, Some("teslamate"), "Resumed the local TeslaMate"),
    c("teslamate-local-tested", INT, Some("teslamate"), "Tested the local TeslaMate"),
    c("token-created", CARS, Some("token"), "Made an upload token"),
    c("token-revoked", CARS, Some("token"), "Revoked an upload token"),
    c("trip-changed", CARS, Some("trip"), "Changed a trip"),
    c("trip-removed", CARS, Some("trip"), "Removed a trip"),
    c("trip-saved", CARS, Some("trip"), "Saved a trip"),
    c("two-step-failed", S, Some("account"), "Typed a wrong two-step code"),
    c("two-step-off", A, Some("account"), "Turned two-step sign-in off"),
    c("two-step-on", A, Some("account"), "Turned two-step sign-in on"),
    c("two-step-policy", A, Some("settings"), "Changed who needs two-step sign-in"),
    c("ui-pack-chosen", SET, Some("ui-pack"), "Chose a design"),
    c("ui-pack-installed", SET, Some("ui-pack"), "Added a design"),
    c("ui-pack-removed", SET, Some("ui-pack"), "Removed a design"),
    c("update-version", CARS, Some("update"), "Named the version of a software update"),
];

/// The table's entry for a code.
pub fn find(code: &str) -> Option<&'static Code> {
    CODES.binary_search_by(|c| c.code.cmp(code)).ok().map(|i| &CODES[i])
}

/// The group of a code (`other` for one the table does not know).
pub fn group_of(code: &str) -> &'static str {
    find(code).map_or(OTHER, |c| c.group)
}

/// The table as `ui/shared/audit-codes.json` holds it: one line per code, sorted by code.
pub fn json() -> String {
    let mut out = String::from("[\n");
    for (i, c) in CODES.iter().enumerate() {
        let item = c.item.map_or("null".to_string(), |k| format!("\"{k}\""));
        let comma = if i + 1 == CODES.len() { "" } else { "," };
        out.push_str(&format!("  {{\"code\":\"{}\",\"group\":\"{}\",\"item\":{item}}}{comma}\n", c.code, c.group));
    }
    out.push_str("]\n");
    out
}

/// A route's words for a sentence: `/api/v1/cars/{car}/rebuild` -> "car rebuild", ids left out.
pub fn route_words(route: &str) -> String {
    let path = route.split('?').next().unwrap_or("");
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    let words: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty() && !s.starts_with('{') && !looks_like_id(s))
        .map(|s| s.replace(['-', '_'], " "))
        .collect();
    if words.is_empty() { "the server".to_string() } else { words.join(" ") }
}

/// `c1`, `p12`, `a3`, `i7`, a number, a long hex or base64 id.
fn looks_like_id(s: &str) -> bool {
    let b = s.as_bytes();
    if s.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    if b.len() >= 2 && b[0].is_ascii_lowercase() && s[1..].chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    s.len() >= 12
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && s.chars().any(|c| c.is_ascii_digit())
}

/// The guard's sentence for a change no handler described: "Changed <route words>." (never a method or a path).
pub fn guard_summary(route: &str) -> String {
    format!("Changed {}.", route_words(route))
}

/// An entry as the audit list answers it, mapped from what was stored: `(action, target, car, summary)`.
pub struct Row {
    pub action: String,
    pub target: Option<String>,
    pub car: Option<String>,
    pub summary: String,
}

/// Old `change` entries whose `METHOD /route` now has a code of its own: `(method, route under /api/v1, code,
/// target)`; a `*` stands for one id, and `{n}` in the target for the route's n-th part (`/imports/i3`: 2 is `i3`).
const LEGACY_ROUTES: &[(&str, &str, &str, &str)] = &[
    ("POST", "/release/check", "release-checked", "server:release"),
    // the chunks of an upload
    ("PATCH", "/imports/*", "import-uploaded", "import:{2}"),
    ("PATCH", "/imports/*/files/*", "import-uploaded", "import:{2}"),
    // a removal or a repricing that turned out to change nothing
    ("DELETE", "/cars/*/charges/*/cost", "charge-cost-removed", "charge:{2}:{4}"),
    ("POST", "/places/*/reprice", "place-repriced", "place:{2}"),
];

/// A legacy target from its template and the route's parts.
fn fill(template: &str, parts: &[&str]) -> String {
    let mut out = template.to_string();
    for (i, p) in parts.iter().enumerate() {
        out = out.replace(&format!("{{{i}}}"), p);
    }
    out
}

fn route_matches(pattern: &str, path: &str) -> bool {
    let (p, q): (Vec<&str>, Vec<&str>) = (pattern.split('/').collect(), path.split('/').collect());
    p.len() == q.len() && p.iter().zip(&q).all(|(a, b)| *a == "*" || a == b)
}

/// The car of an old target whose second part is a car (`tesla:c1:ta1:VIN`), moved out of it
/// (`tesla:ta1:VIN`): for the kinds whose item is not the car's (a car match).
fn move_car_out(target: &str) -> Option<(String, String)> {
    let mut parts = target.splitn(3, ':');
    let (kind, second, rest) = (parts.next()?, parts.next()?, parts.next());
    if !matches!(kind, "tesla" | "teslafi" | "teslamate_local") || ids::parse_car(second).is_none() {
        return None;
    }
    let t = if kind == "teslamate_local" {
        "teslamate:local".to_string()
    } else {
        match rest {
            Some(r) => format!("{kind}:{r}"),
            None => kind.to_string(),
        }
    };
    Some((t, second.to_string()))
}

/// An entry read back: legacy codes and targets mapped, a missing sentence filled in, unknown codes as they are.
pub fn readable(action: &str, target: Option<String>, car: Option<String>, summary: String) -> Row {
    let mut row = Row { action: action.to_string(), target, car, summary };
    if let Some(t) = row.target.take() {
        row.target = Some(match t.as_str() {
            "settings:teslamate_local" => "teslamate:local".to_string(),
            _ => match move_car_out(&t) {
                Some((t, car)) => {
                    row.car = row.car.filter(|c| *c != car).or(Some(car));
                    t
                }
                None if t.starts_with("teslamate_local:") => "teslamate:local".to_string(),
                None => t,
            },
        });
    }
    if row.action == "change" {
        // the guard's old `METHOD /route` (a concrete route: ids in it)
        let said = row.summary.clone();
        let mut words = said.splitn(2, ' ');
        if let (Some(method), Some(route)) = (words.next(), words.next())
            && matches!(method, "POST" | "PUT" | "PATCH" | "DELETE")
            && route.starts_with('/')
        {
            let under = route.strip_prefix("/api/v1").unwrap_or(route);
            match LEGACY_ROUTES.iter().find(|(m, p, _, _)| *m == method && route_matches(p, under)) {
                Some((_, _, code, target)) => {
                    row.action = (*code).to_string();
                    row.summary = find(code).map_or("", |c| c.said).to_string() + ".";
                    let parts: Vec<&str> = under.split('/').collect();
                    row.target = Some(fill(target, &parts));
                }
                None => row.summary = guard_summary(route),
            }
        }
        return row;
    }
    if let Some(c) = find(&row.action) {
        let s = row.summary.trim();
        if s.is_empty() {
            row.summary = format!("{}.", c.said);
        } else if s.starts_with(|ch: char| ch.is_lowercase() || ch.is_ascii_digit()) {
            // an old entry of a few words ("wrong password", "a device of family")
            row.summary = format!("{} ({}).", c.said, s.trim_end_matches('.'));
        }
    }
    row
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn sorted_unique_and_grouped() {
        assert!(CODES.windows(2).all(|w| w[0].code < w[1].code), "sorted, once each");
        assert!(CODES.iter().all(|c| GROUPS.iter().any(|(g, _)| *g == c.group)), "known groups");
        assert_eq!(group_of("sign-in"), "sign-ins");
        assert_eq!(group_of("something-new"), OTHER);
    }

    #[test]
    fn legacy_entries_read_back_readable() {
        let r = readable("change", None, None, "POST /api/v1/release/check".into());
        assert_eq!((r.action.as_str(), r.target.as_deref()), ("release-checked", Some("server:release")));
        assert_eq!(r.summary, "Checked GitHub for a newer build.");
        let r = readable("change", None, None, "PATCH /api/v1/imports/i3/files/1".into());
        assert_eq!((r.action.as_str(), r.target.as_deref()), ("import-uploaded", Some("import:i3")));
        let r = readable(
            "change",
            Some("car:c2".into()),
            Some("c2".into()),
            "DELETE /api/v1/cars/c2/charges/77/cost".into(),
        );
        assert_eq!(
            (r.action.as_str(), r.target.as_deref(), r.car.as_deref()),
            ("charge-cost-removed", Some("charge:c2:77"), Some("c2"))
        );
        let r = readable("change", Some("car:c1".into()), Some("c1".into()), "PATCH /api/v1/cars/c1/rebuild".into());
        assert_eq!((r.action.as_str(), r.summary.as_str()), ("change", "Changed cars rebuild."));
        let r = readable("teslamate-local-changed", Some("settings:teslamate_local".into()), None, "Changed.".into());
        assert_eq!(r.target.as_deref(), Some("teslamate:local"));
        let r =
            readable("teslamate-local-car-matched", Some("teslamate_local:c2:5".into()), Some("c2".into()), "x".into());
        assert_eq!((r.target.as_deref(), r.car.as_deref()), (Some("teslamate:local"), Some("c2")));
        let r = readable("tesla-car-matched", Some("tesla:c2:ta1:VIN1".into()), Some("c2".into()), "X.".into());
        assert_eq!((r.target.as_deref(), r.car.as_deref()), (Some("tesla:ta1:VIN1"), Some("c2")));
        let r = readable("sign-in-failed", None, None, "wrong password".into());
        assert_eq!(r.summary, "Typed a wrong name or password (wrong password).");
        let r = readable("sign-out", None, None, String::new());
        assert_eq!(r.summary, "Signed out.");
        let r = readable("brand-new-code", None, None, "as it is".into());
        assert_eq!((r.action.as_str(), r.summary.as_str()), ("brand-new-code", "as it is"));
        assert_eq!(guard_summary("/api/v1/imports/{import}/files/{index}"), "Changed imports files.");
    }
}
