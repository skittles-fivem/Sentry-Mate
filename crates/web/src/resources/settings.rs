//! Server settings: `GET`/`PATCH /api/v1/settings` (docs/ui-api.md, "Server, settings and health").
//!
//! The server's defaults (units, range, car order) that everyone's preferences start from, where addresses and map
//! tiles come from, the release check (off by default; [`super::release`]),
//! links to other sites, the proxy for what the server fetches itself, and the host names the next certificate is
//! made for (an empty list: the running certificate's names). Kept in the history's database (sealed with the vault)
//! under the `server` setting; until a first change, every value is its default.
//!
//! The frame follows the settings at once: [`apply`] sets the preference defaults ([`AppState::set_defaults`],
//! which pushes a `prefs` event to every open stream when they change) and the tile origins of the packs' CSP
//! ([`AppState::set_tiles`]), at start ([`init`]) and after every change. A change of units also goes to the MQTT
//! settings, so MQTT publishes (and `/cars/{car}/events` shows) values in the server's units.
//!
//! A PATCH carries `version` and only the parts that change (`{ units: { pressure: "psi" } }` keeps the other
//! units); a wrong type is `400 bad_input`, a value that breaks a rule `422 invalid`, both by field (`units.distance`,
//! `host_names.2`), a stale version `409 stale_version`. `car_order` only ever shows the cars the account sees; a
//! change keeps the cars it doesn't see where they were (after the ones it sent).

use crate::ctx::target;
use crate::units::{self, Defaults, Units};
use crate::{ApiError, ApiResult, AppState, Fields, JsonBody, Viewer, ids};
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use engine::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::sync::Mutex;

/// The database setting the server settings are kept under.
pub const KEY: &str = "server";

/// One change of the server's or the MQTT settings at a time, so of two changes with the same version one wins and
/// the other gets `409`.
pub(crate) static CHANGES: Mutex<()> = Mutex::new(());

pub const GEOCODE_MODES: &[&str] = &["offline", "online"];
pub const TILE_MODES: &[&str] = &["offline", "online"];
pub const DATASETS: &[&str] = &["cities1000", "cities500"];
pub const MAX_HOST_NAMES: usize = 10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Geocode {
    /// `offline` (the GeoNames towns in the data folder) or `online` (an address service at `url`).
    pub mode: String,
    pub dataset: String,
    pub url: Option<String>,
    pub language: String,
    /// Positions are rounded to this before an online lookup.
    pub rounding_m: u32,
}

impl Default for Geocode {
    fn default() -> Geocode {
        Geocode {
            mode: "offline".into(),
            dataset: places::geocode::Dataset::default().name().into(),
            url: None,
            language: "en".into(),
            rounding_m: 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tiles {
    /// `online` (the browsers load tiles from `online_url`) or `offline` (tiles from this server). `offline` until an
    /// admin turns online tiles on: no browser asks an outside tile server before that.
    pub mode: String,
    /// The outside tile server online mode uses (filled in as a suggestion only).
    pub online_url: Option<String>,
    pub attribution: String,
    pub max_zoom: u8,
    /// The offline map file to use, a `.pmtiles` name in the data folder's `maps/` (none: the largest there).
    pub offline_file: Option<String>,
    /// The online preset an admin picked (`places::tiles::PRESETS` id, or `custom` for `online_url`); none: never
    /// picked, so [`places::tiles::DEFAULT_PRESET`] applies (also over the old implicit OpenStreetMap default).
    pub preset: Option<String>,
    /// Place names and roads drawn over satellite pictures (presets that have such an overlay).
    pub labels: bool,
    /// The presets viewers may switch to on a map (Map / Satellite), besides the chosen one.
    pub styles: Vec<String>,
}

/// One online map style the pages may draw: a preset, or the admin's own address (`custom`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TileStyle {
    pub id: String,
    pub name: String,
    pub short: String,
    /// `map` or `satellite`.
    pub kind: String,
    pub template: String,
    pub attribution: String,
    pub max_zoom: u8,
    /// Transparent layers drawn on top (place names and roads), in order.
    pub overlays: Vec<String>,
    /// Aerial photos: pages never dim these in the dark theme.
    pub imagery: bool,
    pub region: Option<String>,
}

impl Tiles {
    /// The style id in use: a preset's, or `custom`.
    pub fn chosen_id(&self) -> String {
        use places::tiles::{DEFAULT_PRESET, preset, preset_for_url};
        match self.preset.as_deref() {
            Some(id) if id == "custom" || preset(id).is_some() => id.into(),
            _ => match self.online_url.as_deref() {
                // never picked as a preset (also the old implicit OpenStreetMap or OpenTopoMap default): the street map
                None => DEFAULT_PRESET.into(),
                Some(u) => match preset_for_url(u) {
                    Some(p) if p.id == "osm" || p.id == "opentopomap" => DEFAULT_PRESET.into(),
                    Some(p) => p.id.into(),
                    None => "custom".into(),
                },
            },
        }
    }

    /// The style for an id (`custom`: the admin's own address, when set).
    pub fn style(&self, id: &str) -> Option<TileStyle> {
        if id == "custom" {
            let url = self.online_url.clone()?;
            return Some(TileStyle {
                id: "custom".into(),
                name: "Custom tile server".into(),
                short: "Map".into(),
                kind: "map".into(),
                template: url,
                attribution: self.attribution.clone(),
                max_zoom: self.max_zoom,
                overlays: Vec::new(),
                imagery: false,
                region: None,
            });
        }
        let p = places::tiles::preset(id)?;
        Some(TileStyle {
            id: p.id.into(),
            name: p.name.into(),
            short: p.short.into(),
            kind: p.kind.into(),
            template: p.url.into(),
            attribution: p.attribution.into(),
            max_zoom: p.max_zoom,
            overlays: if self.labels { p.overlays.iter().map(|o| (*o).to_string()).collect() } else { Vec::new() },
            imagery: p.kind == "satellite",
            region: p.region.map(Into::into),
        })
    }

    /// The style in use.
    pub fn chosen(&self) -> Option<TileStyle> {
        self.style(&self.chosen_id())
    }

    /// The styles a viewer may switch between: the chosen one first, then the other allowed presets.
    pub fn viewer_styles(&self) -> Vec<TileStyle> {
        let first = self.chosen_id();
        let mut out: Vec<TileStyle> = self.chosen().into_iter().collect();
        for id in &self.styles {
            if *id != first
                && let Some(s) = self.style(id)
            {
                out.push(s);
            }
        }
        out
    }

    /// Every host the browsers load map pictures from while tiles are online (the chosen style first).
    pub fn hosts(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in self.viewer_styles() {
            for u in std::iter::once(&s.template).chain(s.overlays.iter()) {
                if let Some(h) = host_of(u)
                    && !out.contains(&h)
                {
                    out.push(h);
                }
            }
        }
        out
    }
}

impl Default for Tiles {
    fn default() -> Tiles {
        Tiles {
            mode: "offline".into(),
            online_url: Some(places::tiles::OSM_TILE_URL.into()),
            attribution: places::tiles::OSM_TILE_CREDIT.into(),
            max_zoom: 19,
            offline_file: None,
            preset: None,
            labels: true,
            styles: vec![places::tiles::DEFAULT_PRESET.into(), "satellite".into()],
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReleaseCheck {
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutsideLinks {
    pub maps: bool,
    pub release_notes: bool,
}

/// Whether an import fills the values live history lacks, server-wide (on by default; a car's own setting wins).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportGapFill {
    pub enabled: bool,
}

impl Default for ImportGapFill {
    fn default() -> ImportGapFill {
        ImportGapFill { enabled: true }
    }
}

/// A new server version that decodes the stored logs differently rebuilds the cars made with the old one by itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FullRebuild {
    pub auto: bool,
}

impl Default for FullRebuild {
    fn default() -> FullRebuild {
        FullRebuild { auto: true }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Outbound {
    pub proxy: Option<String>,
}

/// The server settings as stored and answered (`car_order` filtered per account on the way out).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerSettings {
    pub units: Units,
    pub range: units::Range,
    /// Car ids (`c<n>`).
    pub car_order: Vec<String>,
    pub geocode: Geocode,
    pub tiles: Tiles,
    pub release_check: ReleaseCheck,
    pub outside_links: OutsideLinks,
    pub outbound: Outbound,
    /// The names the next certificate is made for (the one running now keeps its own: `/network`).
    pub host_names: Vec<String>,
    /// The IANA time zone a new car starts in (Settings: Cars changes each car's own). None until it is set, at
    /// first-run setup from the admin's browser or here: then new cars start in the container's `TZ`, else UTC.
    pub default_time_zone: Option<String>,
    pub import_gap_fill: ImportGapFill,
    pub full_rebuild: FullRebuild,
    pub version: u64,
}

impl Default for ServerSettings {
    fn default() -> ServerSettings {
        ServerSettings {
            units: Units::default(),
            range: units::Range::default(),
            car_order: Vec::new(),
            geocode: Geocode::default(),
            tiles: Tiles::default(),
            release_check: ReleaseCheck::default(),
            outside_links: OutsideLinks::default(),
            outbound: Outbound::default(),
            host_names: Vec::new(),
            default_time_zone: None,
            import_gap_fill: ImportGapFill::default(),
            full_rebuild: FullRebuild::default(),
            version: 1,
        }
    }
}

impl ServerSettings {
    /// The preference defaults these give.
    pub fn defaults(&self) -> Defaults {
        Defaults { units: self.units, range: self.range, car_order: self.car_order.clone() }
    }

    /// The origins the pages may load map tiles from: the online tile server's (none while tiles are offline).
    pub fn tile_origins(&self) -> Vec<String> {
        if self.tiles.mode != "online" {
            return Vec::new();
        }
        let mut out: Vec<String> = Vec::new();
        for s in self.tiles.viewer_styles() {
            for u in std::iter::once(&s.template).chain(s.overlays.iter()) {
                if let Some(o) = origin_of(u)
                    && !out.contains(&o)
                {
                    out.push(o);
                }
            }
        }
        out
    }
}

fn preset_ids() -> Vec<&'static str> {
    places::tiles::PRESETS.iter().map(|p| p.id).collect()
}

/// `https://host[:port]` of an https address.
pub fn origin_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let auth = &rest[..end];
    (!auth.is_empty() && !auth.contains('@')).then(|| format!("https://{}", auth.to_ascii_lowercase()))
}

/// The host of an http(s) address.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let auth = &rest[..end];
    let host = if let Some(v6) = auth.strip_prefix('[') {
        v6.split_once(']').map_or(v6, |(h, _)| h)
    } else {
        auth.rsplit_once(':').map_or(auth, |(h, _)| h)
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// The saved settings (the defaults until a first change; `host_names` defaults to the names of the certificate
/// the server runs with, so a first save keeps them).
pub fn load(e: &Engine, cert_names: &[String]) -> ApiResult<ServerSettings> {
    let saved = e.db().setting(KEY).map_err(|err| ApiError::server(format!("reading the server settings: {err}")))?;
    match saved {
        Some(v) => Ok(serde_json::from_value(v).unwrap_or_else(|err| {
            tracing::error!("the server settings can't be read ({err}); the defaults apply until they are saved again");
            ServerSettings::default()
        })),
        None => Ok(ServerSettings { host_names: default_host_names(cert_names), ..ServerSettings::default() }),
    }
    .map(|mut s| {
        // none saved: the default names
        if s.host_names.is_empty() {
            s.host_names = default_host_names(cert_names);
        }
        s
    })
}

/// The names the next certificate is made for when none are saved: those of the running one, but `localhost`, and
/// only the ones that are host names or addresses ([`cert_host_name`], as a saved name must be: the running one's
/// may come from `SENTRY_MATE_NAMES` as written, spaces and all), each once.
fn default_host_names(cert_names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in cert_names.iter().filter_map(|n| cert_host_name(n)) {
        if n != "localhost" && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// A host name for the certificate: a DNS name, an IPv4 address or an IPv6 address (with or without brackets),
/// written the way it is kept (lower case, IPv6 without brackets and compressed).
fn cert_host_name(n: &str) -> Option<String> {
    let n = n.trim().to_ascii_lowercase();
    let bare = n.strip_prefix('[').and_then(|r| r.strip_suffix(']')).unwrap_or(&n);
    if let Ok(v6) = bare.parse::<std::net::Ipv6Addr>() {
        return Some(v6.to_string());
    }
    is_host_name(&n).then_some(n)
}

/// Makes the frame follow `s`: the preference defaults (pushing `prefs` events only when they changed), the tile
/// origins, the proxy, and the release check's switch.
pub fn apply(app: &AppState, s: &ServerSettings) {
    let d = s.defaults();
    if app.defaults() != d {
        app.set_defaults(d);
    }
    app.set_tiles(s.tile_origins());
    if let Some(z) = s.default_time_zone.as_deref()
        && let Ok(e) = app.service::<Engine>()
    {
        e.set_default_time_zone(z);
        super::teslafi::follow_time_zone(app, &e.default_time_zone());
    }
    super::tesla::follow_proxy(app, s.outbound.proxy.as_deref());
    super::teslafi::follow_proxy(app, s.outbound.proxy.as_deref());
    app.release.follow(s.release_check.enabled, s.outbound.proxy.as_deref().and_then(tesla::Proxy::parse));
    if let Ok(e) = app.service::<Engine>() {
        e.set_import_gap_fill(s.import_gap_fill.enabled);
        e.set_full_rebuild_auto(s.full_rebuild.auto);
    }
}

/// First-run setup: the time zone of the first admin's browser becomes the one new cars start in, unless one is
/// saved already (a restore or move brings its own) or `zone` is not an IANA zone. Quietly does nothing without an
/// engine.
pub fn first_time_zone(app: &AppState, zone: &str) -> ApiResult<()> {
    let zone = zone.trim();
    let Ok(e) = app.service::<Engine>() else { return Ok(()) };
    if !engine::tz::is_zone(zone) {
        return Ok(());
    }
    let _one = CHANGES.lock().unwrap_or_else(|p| p.into_inner());
    let mut s = load(&e, &cert_names(app))?;
    if s.default_time_zone.is_some() {
        return Ok(());
    }
    s.default_time_zone = Some(zone.to_string());
    s.version += 1;
    let value = serde_json::to_value(&s)?;
    e.db().set_setting(KEY, value).map_err(|err| ApiError::server(format!("saving the server settings: {err}")))?;
    apply(app, &s);
    Ok(())
}

/// At start: the frame follows the saved settings (else the defaults).
pub fn init(app: &AppState, e: &Engine) -> ApiResult<()> {
    let s = load(e, &cert_names(app))?;
    apply(app, &s);
    Ok(())
}

/// The names in the certificate the server runs with now (empty when unknown).
pub fn cert_names(app: &AppState) -> Vec<String> {
    let Ok(src) = app.service::<super::notices::Sources>() else { return Vec::new() };
    let Some(path) = src.certificate.as_ref() else { return Vec::new() };
    std::fs::read_to_string(path).ok().map(|pem| san_names(&pem)).unwrap_or_default()
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/settings", get(read).patch(change))
}

/// The settings as one account sees them: `car_order` lists only the cars it sees, and `default_time_zone` is the
/// zone new cars start in now (the container's `TZ`, else UTC, until one is saved).
fn view(app: &AppState, v: &Viewer, s: &ServerSettings) -> Value {
    let mut out = serde_json::to_value(s).unwrap_or_else(|_| json!({}));
    if s.default_time_zone.is_none()
        && let Ok(e) = app.service::<Engine>()
    {
        out["default_time_zone"] = Value::from(e.default_time_zone());
    }
    let order: Vec<Value> = s
        .car_order
        .iter()
        .filter(|c| ids::parse_car(c).is_some_and(|id| v.sees_existing(app, id)))
        .map(|c| Value::from(c.as_str()))
        .collect();
    out["car_order"] = Value::Array(order);
    out
}

async fn read(State(app): State<AppState>, v: Viewer) -> ApiResult<Json<Value>> {
    let e = app.service::<Engine>()?;
    let s = app.blocking(move |app| load(&e, &cert_names(app))).await?;
    Ok(Json(view(&app, &v, &s)))
}

async fn change(State(app): State<AppState>, v: Viewer, body: JsonBody) -> ApiResult<Json<Value>> {
    let e = app.service::<Engine>()?;
    let who = v.clone();
    let (before, after, zone_moved) = app
        .blocking(move |app| {
            let _one = CHANGES.lock().unwrap_or_else(|p| p.into_inner());
            let cur = load(&e, &cert_names(app))?;
            if body.version()? != cur.version {
                return Err(ApiError::stale_version(cur.version));
            }
            let mut new = patched(app, &who, &cur, &body)?;
            new.version = cur.version + 1;
            let value = serde_json::to_value(&new)?;
            e.db()
                .set_setting(KEY, value)
                .map_err(|err| ApiError::server(format!("saving the server settings: {err}")))?;
            if new.units != cur.units {
                super::mqtt::follow_units(app, &e, new.units)?;
            }
            // still under the lock, so the frame follows the changes in the order they were saved
            let zone_was = app.service::<super::teslafi::TeslafiService>().ok().map(|s| s.default_time_zone());
            apply(app, &new);
            let zone_is = app.service::<super::teslafi::TeslafiService>().ok().map(|s| s.default_time_zone());
            let zone_moved = zone_was.zip(zone_is).filter(|(a, b)| a != b);
            Ok((cur, new, zone_moved))
        })
        .await?;
    let mut changed: Vec<&str> = Vec::new();
    for (k, same) in [
        ("units", before.units == after.units),
        ("range", before.range == after.range),
        ("car order", before.car_order == after.car_order),
        ("address lookup", before.geocode == after.geocode),
        ("map tiles", before.tiles == after.tiles),
        ("release check", before.release_check == after.release_check),
        ("links to other sites", before.outside_links == after.outside_links),
        ("proxy", before.outbound == after.outbound),
        ("host names", before.host_names == after.host_names),
        ("default time zone", before.default_time_zone == after.default_time_zone),
        ("import gap filling", before.import_gap_fill == after.import_gap_fill),
        ("automatic full rebuilds", before.full_rebuild == after.full_rebuild),
    ] {
        if !same {
            changed.push(k);
        }
    }
    let summary = if changed.is_empty() {
        "Saved the server settings unchanged.".to_string()
    } else {
        format!("Changed the server settings: {}.", changed.join(", "))
    };
    v.audit(&app, "settings-changed", Some(&target("settings", None, "server")), &summary).await?;
    // the TeslaFi accounts with no zone of their own read their times in the new default from now on: each says so
    // as a change of its zone would (what a re-dating of the data read before starts from)
    if let (Some((was, is)), Ok(svc)) = (zone_moved, app.service::<super::teslafi::TeslafiService>()) {
        for a in svc.accounts().into_iter().filter(|a| a.time_zone.is_none()) {
            let summary = format!(
                "Changed the TeslaFi account {}: set its time zone to {is} (from {was}), the server's default.",
                a.label
            );
            v.audit(&app, "teslafi-account-changed", Some(&target("teslafi", None, &a.id)), &summary).await?;
        }
    }
    Ok(Json(view(&app, &v, &after)))
}

// ---- reading a PATCH

fn is_https_url(s: &str) -> bool {
    s.len() <= 2048
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
        && origin_of(s).is_some_and(|o| host_ok(o.trim_start_matches("https://")))
}

/// `host[:port]`: a host name, an IPv4 address or a bracketed IPv6 address, and a port.
fn host_ok(auth: &str) -> bool {
    let (host, port) = if let Some(v6) = auth.strip_prefix('[') {
        match v6.split_once(']') {
            Some((h, rest)) => {
                if h.parse::<std::net::Ipv6Addr>().is_err() {
                    return false;
                }
                (None, rest.strip_prefix(':'))
            }
            None => return false,
        }
    } else {
        match auth.rsplit_once(':') {
            Some((h, p)) => (Some(h), Some(p)),
            None => (Some(auth), None),
        }
    };
    if let Some(p) = port
        && !p.parse::<u16>().is_ok_and(|n| n > 0)
    {
        return false;
    }
    host.is_none_or(is_host_name)
}

/// A DNS name (labels of letters, digits and `-`, not at a label's ends) or an IPv4 address.
pub fn is_host_name(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !l.starts_with('-')
                && !l.ends_with('-')
        })
}

/// `http://`, `https://` or `socks5://` with a host and an optional port, no name or password.
fn is_proxy(s: &str) -> bool {
    let Some((scheme, rest)) = s.split_once("://") else { return false };
    if !matches!(scheme, "http" | "https" | "socks5") {
        return false;
    }
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    !rest.is_empty() && !rest.contains(['@', '/', '?', '#']) && host_ok(rest)
}

/// The sub-object `name` of the body (`400` on its unknown keys, and when it is not an object).
fn part<'a>(body: &'a JsonBody, name: &str, keys: &[&str], bad: &mut Fields) -> Option<&'a Map<String, Value>> {
    match body.get(name) {
        None => None,
        Some(Value::Object(m)) => {
            for k in m.keys() {
                if !keys.contains(&k.as_str()) {
                    bad.add(&format!("{name}.{k}"), "Unknown field.");
                }
            }
            Some(m)
        }
        Some(_) => {
            bad.add(name, "Must be an object.");
            None
        }
    }
}

fn word(
    m: &Map<String, Value>,
    at: &str,
    k: &str,
    allowed: &[&str],
    bad: &mut Fields,
    rule: &mut Fields,
) -> Option<String> {
    match m.get(k)? {
        Value::String(s) if allowed.contains(&s.as_str()) => Some(s.clone()),
        Value::String(_) => {
            rule.add(&format!("{at}.{k}"), &format!("Must be one of: {}.", allowed.join(", ")));
            None
        }
        _ => {
            bad.add(&format!("{at}.{k}"), "Must be text.");
            None
        }
    }
}

fn text(m: &Map<String, Value>, at: &str, k: &str, bad: &mut Fields) -> Option<String> {
    match m.get(k)? {
        Value::String(s) => Some(s.clone()),
        _ => {
            bad.add(&format!("{at}.{k}"), "Must be text.");
            None
        }
    }
}

/// `Some(None)` for null.
fn text_or_null(m: &Map<String, Value>, at: &str, k: &str, bad: &mut Fields) -> Option<Option<String>> {
    match m.get(k)? {
        Value::Null => Some(None),
        Value::String(s) => Some(Some(s.clone())),
        _ => {
            bad.add(&format!("{at}.{k}"), "Must be text or null.");
            None
        }
    }
}

fn flag(m: &Map<String, Value>, at: &str, k: &str, bad: &mut Fields) -> Option<bool> {
    match m.get(k)? {
        Value::Bool(b) => Some(*b),
        _ => {
            bad.add(&format!("{at}.{k}"), "Must be true or false.");
            None
        }
    }
}

fn whole(m: &Map<String, Value>, at: &str, k: &str, bad: &mut Fields) -> Option<u64> {
    let v = m.get(k)?;
    let n = v.as_u64();
    if n.is_none() {
        bad.add(&format!("{at}.{k}"), "Must be a whole number.");
    }
    n
}

/// `cur` with the body's changes, every rule checked (`400`, then `422`, each naming its fields).
fn patched(app: &AppState, v: &Viewer, cur: &ServerSettings, body: &JsonBody) -> ApiResult<ServerSettings> {
    let mut bad = Fields::new();
    let mut rule = Fields::new();
    let known = [
        "version",
        "units",
        "range",
        "car_order",
        "geocode",
        "tiles",
        "release_check",
        "outside_links",
        "outbound",
        "host_names",
        "default_time_zone",
        "import_gap_fill",
        "full_rebuild",
    ];
    for k in body.0.keys() {
        if !known.contains(&k.as_str()) {
            bad.add(k, "Unknown field.");
        }
    }
    let mut new = cur.clone();
    if let Some(m) = part(body, "units", &["distance", "temperature", "pressure"], &mut bad) {
        if let Some(w) = word(m, "units", "distance", units::DISTANCE, &mut bad, &mut rule) {
            new.units.distance = if w == "mi" { units::Distance::Mi } else { units::Distance::Km };
        }
        if let Some(w) = word(m, "units", "temperature", units::TEMPERATURE, &mut bad, &mut rule) {
            new.units.temperature = if w == "f" { units::Temperature::F } else { units::Temperature::C };
        }
        if let Some(w) = word(m, "units", "pressure", units::PRESSURE, &mut bad, &mut rule) {
            new.units.pressure = if w == "psi" { units::Pressure::Psi } else { units::Pressure::Bar };
        }
    }
    match body.get("range") {
        None => {}
        Some(Value::String(s)) if s == "rated" => new.range = units::Range::Rated,
        Some(Value::String(s)) if s == "ideal" => new.range = units::Range::Ideal,
        Some(Value::String(_)) => rule.add("range", "Must be rated or ideal."),
        Some(_) => bad.add("range", "Must be text."),
    }
    if let Some(order) = body.get("car_order") {
        match order
            .as_array()
            .and_then(|a| a.iter().map(|x| x.as_str().and_then(ids::parse_car)).collect::<Option<Vec<_>>>())
        {
            None => bad.add("car_order", "Must be a list of car ids."),
            Some(list) if list.len() > crate::prefs::MAX_CAR_ORDER => {
                rule.add("car_order", &format!("At most {} cars.", crate::prefs::MAX_CAR_ORDER));
            }
            Some(list) => {
                // (the same answer for a hidden car and an unknown one)
                if !list.iter().all(|c| v.sees_existing(app, *c)) {
                    rule.add("car_order", "No such car.");
                } else {
                    let mut seen = std::collections::HashSet::new();
                    let mut out: Vec<String> = list.into_iter().filter(|c| seen.insert(*c)).map(ids::car).collect();
                    // the cars this account doesn't see keep their places after the ones it sent
                    for c in &cur.car_order {
                        if ids::parse_car(c).is_some_and(|id| !v.sees(id) && app.car_exists(id)) && !out.contains(c) {
                            out.push(c.clone());
                        }
                    }
                    new.car_order = out;
                }
            }
        }
    }
    if let Some(m) = part(body, "geocode", &["mode", "dataset", "url", "language", "rounding_m"], &mut bad) {
        if let Some(w) = word(m, "geocode", "mode", GEOCODE_MODES, &mut bad, &mut rule) {
            new.geocode.mode = w;
        }
        if let Some(w) = word(m, "geocode", "dataset", DATASETS, &mut bad, &mut rule) {
            new.geocode.dataset = w;
        }
        if let Some(u) = text_or_null(m, "geocode", "url", &mut bad) {
            new.geocode.url = u.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        }
        if let Some(l) = text(m, "geocode", "language", &mut bad) {
            new.geocode.language = l.trim().to_string();
        }
        if let Some(n) = whole(m, "geocode", "rounding_m", &mut bad) {
            new.geocode.rounding_m = u32::try_from(n.min(u64::from(u32::MAX))).unwrap_or(u32::MAX);
        }
    }
    let tile_keys = ["mode", "online_url", "attribution", "max_zoom", "offline_file", "preset", "labels", "styles"];
    if let Some(m) = part(body, "tiles", &tile_keys, &mut bad) {
        if let Some(w) = word(m, "tiles", "mode", TILE_MODES, &mut bad, &mut rule) {
            new.tiles.mode = w;
        }
        if let Some(u) = text_or_null(m, "tiles", "online_url", &mut bad) {
            new.tiles.online_url = u.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        }
        if let Some(a) = text(m, "tiles", "attribution", &mut bad) {
            new.tiles.attribution = a.trim().to_string();
        }
        if let Some(n) = whole(m, "tiles", "max_zoom", &mut bad) {
            new.tiles.max_zoom = u8::try_from(n).unwrap_or(u8::MAX);
        }
        if let Some(f) = text_or_null(m, "tiles", "offline_file", &mut bad) {
            new.tiles.offline_file = f.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        }
        if m.contains_key("online_url") && !m.contains_key("preset") {
            // an address without a preset: the preset it is, else the admin's own
            let id = new.tiles.online_url.as_deref().and_then(places::tiles::preset_for_url).map(|p| p.id);
            new.tiles.preset = Some(id.unwrap_or("custom").into());
        }
        if let Some(p) = text_or_null(m, "tiles", "preset", &mut bad) {
            match p.as_deref() {
                None => new.tiles.preset = None,
                Some("custom") => new.tiles.preset = Some("custom".into()),
                Some(id) => match places::tiles::preset(id) {
                    Some(pr) => {
                        new.tiles.preset = Some(pr.id.into());
                        new.tiles.online_url = Some(pr.url.into());
                        new.tiles.attribution = pr.attribution.into();
                        new.tiles.max_zoom = pr.max_zoom;
                    }
                    None => rule.add("tiles.preset", &format!("One of: {}, custom, or null.", preset_ids().join(", "))),
                },
            }
        }
        if let Some(b) = flag(m, "tiles", "labels", &mut bad) {
            new.tiles.labels = b;
        }
        if let Some(v) = m.get("styles") {
            match v.as_array().and_then(|a| a.iter().map(Value::as_str).collect::<Option<Vec<_>>>()) {
                None => bad.add("tiles.styles", "Must be a list of preset ids."),
                Some(list) => {
                    let mut out: Vec<String> = Vec::new();
                    for id in list {
                        if places::tiles::preset(id).is_none() {
                            rule.add("tiles.styles", &format!("Preset ids only: {}.", preset_ids().join(", ")));
                        } else if !out.iter().any(|o| o == id) {
                            out.push(id.into());
                        }
                    }
                    new.tiles.styles = out;
                }
            }
        }
    }
    if let Some(m) = part(body, "import_gap_fill", &["enabled"], &mut bad)
        && let Some(b) = flag(m, "import_gap_fill", "enabled", &mut bad)
    {
        new.import_gap_fill.enabled = b;
    }
    if let Some(m) = part(body, "full_rebuild", &["auto"], &mut bad)
        && let Some(b) = flag(m, "full_rebuild", "auto", &mut bad)
    {
        new.full_rebuild.auto = b;
    }
    if let Some(m) = part(body, "release_check", &["enabled"], &mut bad)
        && let Some(b) = flag(m, "release_check", "enabled", &mut bad)
    {
        new.release_check.enabled = b;
    }
    if let Some(m) = part(body, "outside_links", &["maps", "release_notes"], &mut bad) {
        if let Some(b) = flag(m, "outside_links", "maps", &mut bad) {
            new.outside_links.maps = b;
        }
        if let Some(b) = flag(m, "outside_links", "release_notes", &mut bad) {
            new.outside_links.release_notes = b;
        }
    }
    if let Some(m) = part(body, "outbound", &["proxy"], &mut bad)
        && let Some(p) = text_or_null(m, "outbound", "proxy", &mut bad)
    {
        new.outbound.proxy = p.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    }
    if let Some(h) = body.get("host_names") {
        match h.as_array().and_then(|a| a.iter().map(|x| x.as_str().map(str::to_string)).collect::<Option<Vec<_>>>()) {
            None => bad.add("host_names", "Must be a list of host names."),
            Some(list) if list.len() > MAX_HOST_NAMES => {
                rule.add("host_names", &format!("At most {MAX_HOST_NAMES} host names."));
            }
            Some(list) => {
                // an empty list: the default names (those of the running certificate)
                let mut out: Vec<String> = Vec::new();
                for (i, n) in list.iter().enumerate() {
                    match cert_host_name(n) {
                        None => rule.add(
                            &format!("host_names.{i}"),
                            "A host name such as sentry-mate.local, or an IP address.",
                        ),
                        Some(n) if !out.contains(&n) => out.push(n),
                        Some(_) => {}
                    }
                }
                new.host_names = if out.is_empty() { default_host_names(&cert_names(app)) } else { out };
            }
        }
    }
    match body.get("default_time_zone") {
        None => {}
        Some(Value::String(z)) if engine::tz::is_zone(z.trim()) => new.default_time_zone = Some(z.trim().to_string()),
        Some(Value::String(_)) => rule.add("default_time_zone", "An IANA time zone such as America/Chicago."),
        Some(_) => bad.add("default_time_zone", "Must be text."),
    }
    bad.bad_input()?;
    // the rules of the whole, after the parts
    let g = &new.geocode;
    if let Some(u) = &g.url
        && !is_https_url(u)
    {
        rule.add("geocode.url", "An https:// address, or null.");
    } else if g.mode == "online" && g.url.is_none() {
        rule.add("geocode.url", "Online address lookup needs the https:// address of the service.");
    }
    if g.rounding_m > 1000 {
        rule.add("geocode.rounding_m", "Must be between 0 and 1000 m.");
    }
    if !(2..=10).contains(&g.language.chars().count())
        || !g.language.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        rule.add("geocode.language", "A language code such as en or de.");
    }
    let t = &new.tiles;
    let template_ok = |u: &str| is_https_url(u) && ["{z}", "{x}", "{y}"].iter().all(|p| u.contains(p));
    if let Some(u) = &t.online_url
        && !template_ok(u)
    {
        rule.add("tiles.online_url", "An https:// tile address with {z}, {x} and {y}, or null.");
    } else if t.mode == "online" && t.online_url.is_none() {
        rule.add("tiles.online_url", "Online tiles need an https:// tile address with {z}, {x} and {y}.");
    }
    if !(1..=22).contains(&t.max_zoom) {
        rule.add("tiles.max_zoom", "Must be between 1 and 22.");
    }
    if t.attribution.chars().count() > 300 {
        rule.add("tiles.attribution", "At most 300 characters.");
    }
    if let Some(f) = &t.offline_file
        && !places::pmtiles::name_ok(f)
    {
        rule.add("tiles.offline_file", "A .pmtiles file name in the data folder's maps folder (no folders), or null.");
    }
    if let Some(p) = &new.outbound.proxy
        && !is_proxy(p)
    {
        rule.add(
            "outbound.proxy",
            "A proxy address such as http://proxy.lan:3128 (no name or password in it), or null.",
        );
    }
    rule.invalid()?;
    Ok(new)
}

// ---- the certificate's names

fn tlv(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = b.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        (rest[..n].iter().fold(0usize, |a, &x| (a << 8) | usize::from(x)), &rest[n..])
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

/// The DNS names and IP addresses of the first certificate of a PEM file (its subject alternative names).
pub fn san_names(pem: &str) -> Vec<String> {
    use base64::Engine as _;
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    let Some(start) = pem.find(BEGIN).map(|i| i + BEGIN.len()) else { return Vec::new() };
    let Some(end) = pem[start..].find("-----END CERTIFICATE-----").map(|i| start + i) else { return Vec::new() };
    let b64: String = pem[start..end].chars().filter(|c| !c.is_whitespace()).collect();
    let Ok(der) = base64::engine::general_purpose::STANDARD.decode(b64) else { return Vec::new() };
    // the extension 2.5.29.17 (subjectAltName): its OID, an optional `critical`, then an OCTET STRING holding the
    // SEQUENCE of names
    const OID: &[u8] = &[0x06, 0x03, 0x55, 0x1d, 0x11];
    let Some(at) = der.windows(OID.len()).position(|w| w == OID) else { return Vec::new() };
    let mut rest = &der[at + OID.len()..];
    if rest.first() == Some(&0x01) {
        let Some((_, _, r)) = tlv(rest) else { return Vec::new() };
        rest = r;
    }
    let Some((0x04, octets, _)) = tlv(rest) else { return Vec::new() };
    let Some((0x30, mut names, _)) = tlv(octets) else { return Vec::new() };
    let mut out = Vec::new();
    while let Some((tag, v, r)) = tlv(names) {
        names = r;
        match tag {
            0x82 => {
                if let Ok(s) = std::str::from_utf8(v) {
                    out.push(s.to_string());
                }
            }
            0x87 if v.len() == 4 => out.push(std::net::Ipv4Addr::new(v[0], v[1], v[2], v[3]).to_string()),
            0x87 if v.len() == 16 => {
                let mut a = [0u8; 16];
                a.copy_from_slice(v);
                out.push(std::net::Ipv6Addr::from(a).to_string());
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn origins_hosts_and_proxies() {
        assert_eq!(origin_of("https://Tile.Example.org/{z}/{x}/{y}.png").as_deref(), Some("https://tile.example.org"));
        assert_eq!(origin_of("https://t.example:8443/a").as_deref(), Some("https://t.example:8443"));
        assert_eq!(origin_of("http://t.example/a"), None);
        assert_eq!(host_of("https://geo.example:443/x").as_deref(), Some("geo.example"));
        assert_eq!(host_of("https://[::1]:8443/").as_deref(), Some("::1"));
        assert!(is_proxy("http://proxy.lan:3128") && is_proxy("socks5://192.0.2.2") && is_proxy("https://p.example/"));
        assert!(!is_proxy("ftp://p") && !is_proxy("http://user:pw@p") && !is_proxy("http://p/path"));
        assert!(is_host_name("sentry-mate.local") && is_host_name("192.0.2.10"));
        assert!(!is_host_name("-a.local") && !is_host_name("a..b") && !is_host_name("a b"));
        assert_eq!(cert_host_name(" Sentry-Mate.LOCAL ").as_deref(), Some("sentry-mate.local"));
        assert_eq!(cert_host_name("[::1]").as_deref(), Some("::1"));
        assert_eq!(cert_host_name("FD00:0:0::0001").as_deref(), Some("fd00::1"));
        assert_eq!(cert_host_name("[::1"), None);
        assert_eq!(cert_host_name("a b"), None);
        // the running certificate's names, from SENTRY_MATE_NAMES as written: only real names are offered
        let running: Vec<String> = ["localhost", "nas.lan", " 192.0.2.5", "bad name", "*.lan", "NAS.lan"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(default_host_names(&running), ["nas.lan", "192.0.2.5"]);
        // nothing outside until an admin turns online tiles on
        let mut s = ServerSettings::default();
        assert_eq!(s.tiles.mode, "offline");
        assert!(s.tile_origins().is_empty());
        s.tiles.mode = "online".into();
        // the OpenStreetMap street map by default, satellite (with names and roads) to switch to
        assert_eq!(s.tiles.chosen_id(), "osm");
        assert_eq!(
            s.tile_origins(),
            vec!["https://tile.openstreetmap.org".to_string(), "https://server.arcgisonline.com".to_string()]
        );
        // a saved server from before (OpenStreetMap, never picked as a preset) gets the default; a picked one stays
        let old: ServerSettings = serde_json::from_value(
            serde_json::json!({ "tiles": { "mode": "online", "online_url": places::tiles::OSM_TILE_URL } }),
        )
        .unwrap();
        assert_eq!(old.tiles.chosen_id(), "osm");
        s.tiles.preset = Some("satellite".into());
        assert_eq!(s.tiles.chosen().unwrap().template, places::tiles::ESRI_IMAGERY_URL);
        s.tiles.preset = None;
        s.tiles.online_url = Some("https://tiles.example.org/{z}/{x}/{y}.png".into());
        assert_eq!(s.tiles.chosen_id(), "custom");
        s.tiles.preset = Some("satellite_us".into());
        s.tiles.styles = vec![];
        assert_eq!(s.tiles.hosts(), vec!["basemap.nationalmap.gov".to_string()]);
        assert_eq!(s.tiles.chosen().unwrap().max_zoom, 16);
    }
}
