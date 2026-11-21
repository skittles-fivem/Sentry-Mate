//! The read side: what `docs/ui-api.md` needs, as plain Rust functions over the db (no HTTP), one module per resource.
//!
//! Answers are `Serialize` types whose field names, units and shapes are the contract's: metric units named in the
//! field (`_km`, `_kwh`, `_kpa`, ...), times in ms UTC, ids as strings (cars `c<n>`, places `p<n>`, drives and charges
//! decimal), `null` for unknown (never 0), `source: "can" | "tesla_api" | "teslamate" | "teslafi"`. Car-scoped answers carry `car` and
//! `time_zone` ([`Scoped`]). Lists are paged by an opaque cursor ([`Paging`], [`List`]) that is tied to the filters
//! and sort it was made with: sent with others it is refused (`400` on `after`).
//!
//! Every function here blocks while it reads the db's files (and a few read trust records through the engine's
//! judge, on the engine's runtime): call them from a blocking thread (`tokio::task::spawn_blocking`), never straight
//! from an async task.
//!
//! Access (which cars an account sees) is the web server's: it passes only cars the account may see, and maps
//! [`QueryError`] to the contract's error answers ([`QueryError::status`], [`QueryError::code`]).

pub mod awake;
pub mod battery;
pub mod can_import;
pub mod charges;
pub mod decoding;
pub mod drives;
pub mod fleet;
pub mod imports;
pub mod keys;
pub mod logbook;
pub mod now;
pub mod parked;
pub mod places;
pub mod published;
pub mod reports;
pub mod series;
pub mod signals;
pub mod software;
pub mod system;
pub mod timeline;
pub mod travel;
pub mod trips;
pub mod vehicle;

use crate::Engine;
use base64::Engine as _;
use model::{Car, CarId, PlaceId};
use serde::{Deserialize, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// A car's id as the API writes it: `c<n>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CarRef(pub CarId);

impl Serialize for CarRef {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("c{}", self.0))
    }
}

/// `c7` → 7 (also a plain `7`).
pub fn parse_car(s: &str) -> Option<CarId> {
    s.strip_prefix('c').unwrap_or(s).parse().ok()
}

pub fn car_ref(id: CarId) -> String {
    format!("c{id}")
}

/// A place's id as the API writes it: `p<n>`.
pub fn place_ref(id: PlaceId) -> String {
    format!("p{id}")
}

pub fn parse_place(s: &str) -> Option<PlaceId> {
    s.strip_prefix('p').unwrap_or(s).parse().ok()
}

/// A drive, charge or update id (a decimal string: they pass 2^53).
pub fn parse_id(s: &str) -> Option<u64> {
    s.parse().ok()
}

/// An amount in one currency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Money {
    pub currency: String,
    pub amount: f64,
}

/// Why a query could not be answered, with the contract's status and code.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("no such car")]
    NoSuchCar,
    /// `no_such_drive`, `no_such_charge`, `no_such_place`, `no_such_update`, ...
    #[error("{0}")]
    NotFound(&'static str),
    /// A wrong type or a bad query value (`400 bad_input`), by field.
    #[error("bad input: {fields:?}")]
    BadInput { fields: BTreeMap<String, String> },
    /// A well-formed value that breaks a rule (`422 invalid`), by field.
    #[error("invalid: {fields:?}")]
    Invalid { fields: BTreeMap<String, String> },
    #[error(transparent)]
    Db(#[from] db::Error),
    #[error(transparent)]
    Engine(#[from] crate::Error),
}

impl QueryError {
    pub fn bad(field: &str, why: &str) -> QueryError {
        QueryError::BadInput { fields: BTreeMap::from([(field.to_string(), why.to_string())]) }
    }

    pub fn invalid(field: &str, why: &str) -> QueryError {
        QueryError::Invalid { fields: BTreeMap::from([(field.to_string(), why.to_string())]) }
    }

    /// The HTTP status the contract gives it.
    pub fn status(&self) -> u16 {
        match self {
            QueryError::NoSuchCar | QueryError::NotFound(_) => 404,
            QueryError::BadInput { .. } => 400,
            QueryError::Invalid { .. } => 422,
            QueryError::Db(_) | QueryError::Engine(_) => 500,
        }
    }

    /// The contract's error code.
    pub fn code(&self) -> &'static str {
        match self {
            QueryError::NoSuchCar => "no_such_car",
            QueryError::NotFound(c) => c,
            QueryError::BadInput { .. } => "bad_input",
            QueryError::Invalid { .. } => "invalid",
            QueryError::Db(_) | QueryError::Engine(_) => "server_error",
        }
    }

    /// The fields named, for `{ error: { fields } }`.
    pub fn fields(&self) -> Option<&BTreeMap<String, String>> {
        match self {
            QueryError::BadInput { fields } | QueryError::Invalid { fields } => Some(fields),
            _ => None,
        }
    }
}

pub type QResult<T> = Result<T, QueryError>;

/// `limit` (1 to 500, default 50) and `after` (the `next` of the page before).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Paging {
    pub limit: Option<usize>,
    pub after: Option<String>,
}

pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 500;

impl Paging {
    pub fn first(limit: usize) -> Paging {
        Paging { limit: Some(limit), after: None }
    }

    pub fn limit(&self) -> QResult<usize> {
        match self.limit {
            None => Ok(DEFAULT_LIMIT),
            Some(n) if (1..=MAX_LIMIT).contains(&n) => Ok(n),
            Some(_) => Err(QueryError::bad("limit", "1 to 500")),
        }
    }
}

/// A page of a list: `{ items, next, total? }`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct List<T> {
    pub items: Vec<T>,
    pub next: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct CursorBody {
    /// The sort key's bits ([`f64::to_bits`]): an unknown value sorts as -inf, which JSON has no number for.
    k: u64,
    id: String,
    h: String,
}

fn sig_hash(sig: &str) -> String {
    let d = Sha256::digest(sig.as_bytes());
    d.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Sorts `rows` by `key` (then by id, both one way), and gives the page after `paging.after`. `sig` names the filters
/// and the sort; a cursor made with another is refused.
pub(crate) fn paginate<T>(
    mut rows: Vec<T>,
    key: impl Fn(&T) -> (f64, String),
    desc: bool,
    sig: &str,
    paging: &Paging,
    with_total: bool,
) -> QResult<List<T>> {
    let limit = paging.limit()?;
    let h = sig_hash(sig);
    let cmp = |a: &(f64, String), b: &(f64, String)| {
        let o = a.0.total_cmp(&b.0).then_with(|| cmp_ids(&a.1, &b.1));
        if desc { o.reverse() } else { o }
    };
    rows.sort_by(|a, b| cmp(&key(a), &key(b)));
    let total = rows.len() as u64;
    let start = match &paging.after {
        None => 0,
        Some(c) => {
            let bad = || QueryError::bad("after", "not a cursor of this list: start again without it");
            let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(c).map_err(|_| bad())?;
            let body: CursorBody = serde_json::from_slice(&raw).map_err(|_| bad())?;
            if body.h != h {
                return Err(bad());
            }
            let at = (f64::from_bits(body.k), body.id);
            rows.partition_point(|r| cmp(&key(r), &at) != std::cmp::Ordering::Greater)
        }
    };
    let mut rest: Vec<T> = rows.into_iter().skip(start).collect();
    let more = rest.len() > limit;
    rest.truncate(limit);
    let next = match rest.last() {
        Some(last) if more => {
            let (k, id) = key(last);
            let body = CursorBody { k: k.to_bits(), id, h };
            let raw =
                serde_json::to_vec(&body).map_err(|e| QueryError::Engine(crate::Error::Damaged(e.to_string())))?;
            Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw))
        }
        _ => None,
    };
    Ok(List { items: rest, next, total: with_total.then_some(total) })
}

/// The key a list's cursor holds (`None`: the first page), for lists that page without [`paginate`] (they keep only
/// a page of rows while they read): `400` on `after` when it was made for other filters or by someone else.
pub(crate) fn read_after(paging: &Paging, sig: &str) -> QResult<Option<(f64, String)>> {
    let Some(c) = &paging.after else { return Ok(None) };
    let bad = || QueryError::bad("after", "not a cursor of this list: start again without it");
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(c).map_err(|_| bad())?;
    let body: CursorBody = serde_json::from_slice(&raw).map_err(|_| bad())?;
    if body.h != sig_hash(sig) {
        return Err(bad());
    }
    Ok(Some((f64::from_bits(body.k), body.id)))
}

/// The cursor after the row whose key is `key`, read back by [`read_after`] (and by [`paginate`]).
pub(crate) fn make_after(key: &(f64, String), sig: &str) -> QResult<String> {
    let body = CursorBody { k: key.0.to_bits(), id: key.1.clone(), h: sig_hash(sig) };
    let raw = serde_json::to_vec(&body).map_err(|e| QueryError::Engine(crate::Error::Damaged(e.to_string())))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw))
}

/// The order [`paginate`] gives rows: by key, then by id, both one way.
pub(crate) fn cmp_key(a: &(f64, String), b: &(f64, String), desc: bool) -> std::cmp::Ordering {
    let o = a.0.total_cmp(&b.0).then_with(|| cmp_ids(&a.1, &b.1));
    if desc { o.reverse() } else { o }
}

/// Ids compare as numbers when both are (ids pass 2^53, so not through f64), else as text.
fn cmp_ids(a: &str, b: &str) -> std::cmp::Ordering {
    let num = |s: &str| s.trim_start_matches(|c: char| c.is_ascii_alphabetic() || c == ':').parse::<u128>().ok();
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        _ => a.cmp(b),
    }
}

/// `from_ms` (inclusive) and `to_ms` (exclusive); a missing edge is the start of the history, or now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Period {
    pub from_ms: Option<u64>,
    pub to_ms: Option<u64>,
}

impl Period {
    pub fn new(from_ms: Option<u64>, to_ms: Option<u64>) -> Period {
        Period { from_ms, to_ms }
    }

    pub fn all() -> Period {
        Period::default()
    }

    /// Both edges, `from_ms` defaulting to the history's start.
    pub fn bounds(&self) -> QResult<(u64, u64)> {
        let (a, b) = (self.from_ms.unwrap_or(0), self.to_ms.unwrap_or(u64::MAX));
        if a > b {
            return Err(QueryError::bad("from_ms", "after to_ms"));
        }
        Ok((a, b))
    }

    /// Both edges, `from_ms` defaulting to `default_ms` before the end (the end defaults to `now_ms`).
    pub fn window(&self, default_ms: u64, now_ms: u64) -> QResult<(u64, u64)> {
        let b = self.to_ms.unwrap_or(now_ms);
        let a = self.from_ms.unwrap_or(b.saturating_sub(default_ms));
        if a > b {
            return Err(QueryError::bad("from_ms", "after to_ms"));
        }
        Ok((a, b))
    }

    pub(crate) fn sig(&self) -> String {
        format!("{:?}-{:?}", self.from_ms, self.to_ms)
    }
}

/// A car-scoped answer: the car and its time zone at the top, the body beside them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Scoped<T> {
    pub car: CarRef,
    pub time_zone: String,
    #[serde(flatten)]
    pub body: T,
}

/// The car, or `no_such_car`.
pub(crate) fn car_of(e: &Engine, car: CarId) -> QResult<Car> {
    e.db().car(car)?.ok_or(QueryError::NoSuchCar)
}

pub(crate) fn scoped<T>(c: &Car, body: T) -> Scoped<T> {
    Scoped { car: CarRef(c.id), time_zone: c.time_zone.clone(), body }
}

/// Rounds to `d` decimals, keeping unknown unknown.
pub(crate) fn r(v: Option<f64>, d: i32) -> Option<f64> {
    let m = 10f64.powi(d);
    v.filter(|x| x.is_finite()).map(|x| (x * m).round() / m)
}

/// Tyre pressure: bar → kPa.
pub(crate) fn kpa(bar: Option<f64>) -> Option<f64> {
    r(bar.map(|b| b * 100.0), 1)
}

/// One origin as the API writes it: `{ kind: "smo" | "tesla_api" | "import", site?, label }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceRef {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<&'static str>,
    pub label: String,
}

impl From<model::Origin> for SourceRef {
    fn from(o: model::Origin) -> SourceRef {
        SourceRef { kind: o.kind(), site: o.site().map(model::ImportSite::word), label: o.label() }
    }
}

/// The list of origins (`sources`), for a value or a car.
pub(crate) fn source_refs(origins: &[model::Origin]) -> Vec<SourceRef> {
    origins.iter().copied().map(SourceRef::from).collect()
}

/// Where a row came from, flattened into the row: `source` (the old word), `sources` (never empty) and
/// `source_label` ("Data from SMO and the Tesla API").
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Sources {
    pub source: &'static str,
    pub sources: Vec<SourceRef>,
    pub source_label: String,
}

impl Sources {
    /// A row of `source` from `inputs` (empty: an older live row, SMO).
    pub(crate) fn of(source: model::Source, inputs: model::Inputs) -> Sources {
        Sources::from_origins(&model::Origin::of_row(source, inputs))
    }

    /// A row of `source` whose inputs are not kept (spans made from several rows): SMO for live rows.
    pub(crate) fn family(source: model::Source) -> Sources {
        Sources::of(source, model::Inputs::NONE)
    }

    pub(crate) fn from_origins(origins: &[model::Origin]) -> Sources {
        let imported = origins.iter().any(|o| o.site().is_some());
        let smo = origins.contains(&model::Origin::Smo);
        let site = origins.iter().find_map(|o| o.site());
        let source = if smo {
            "can"
        } else if let Some(site) = site.filter(|_| imported) {
            site.word()
        } else if origins.contains(&model::Origin::TeslaApi) {
            "tesla_api"
        } else {
            "can"
        };
        Sources { source, sources: source_refs(origins), source_label: model::Origin::label_of(origins) }
    }

    /// The row family these sources belong to (imports apart from live rows).
    pub(crate) fn is_imported(&self) -> bool {
        self.sources.iter().any(|s| s.kind == "import")
    }
}

/// What `source=` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFilter {
    /// Every live row, whatever input gave it (not a `source=` word: used inside the engine).
    Live,
    /// Live rows with SMO data (`smo`, old word `can`).
    Smo,
    /// Live rows with Tesla API data (`tesla_api`).
    TeslaApi,
    /// Imported rows, from any site or one (`import`, `import:teslamate`, `import:teslafi`, old word `teslamate`).
    Import(Option<model::ImportSite>),
}

impl SourceFilter {
    /// The row family the filter reads.
    pub fn family(self) -> model::Source {
        match self {
            SourceFilter::Live | SourceFilter::Smo | SourceFilter::TeslaApi => model::Source::Live,
            SourceFilter::Import(_) => model::Source::Teslamate,
        }
    }

    /// Whether a row of `source` from `inputs` matches.
    pub fn keeps(self, source: model::Source, inputs: model::Inputs) -> bool {
        match self {
            SourceFilter::Import(site) => source.is_imported() && site.is_none_or(|s| source.site() == Some(s)),
            SourceFilter::Live => source == model::Source::Live,
            SourceFilter::Smo => {
                source == model::Source::Live && (inputs.is_empty() || inputs.contains(model::Input::Smo))
            }
            SourceFilter::TeslaApi => source == model::Source::Live && inputs.contains(model::Input::TeslaApi),
        }
    }

    /// The streams a series reads.
    pub fn streams(self) -> Vec<model::Stream> {
        match self {
            SourceFilter::Live => vec![model::Stream::Smo, model::Stream::TeslaApi],
            SourceFilter::Smo => vec![model::Stream::Smo],
            SourceFilter::TeslaApi => vec![model::Stream::TeslaApi],
            SourceFilter::Import(None) => model::Stream::IMPORTED.to_vec(),
            SourceFilter::Import(Some(site)) => vec![model::Stream::of_site(site)],
        }
    }
}

/// A filter as answers echo it: its word and the origins it reads.
pub(crate) fn filter_words(f: Option<SourceFilter>) -> (String, Vec<SourceRef>) {
    use model::{ImportSite, Origin};
    let (word, origins) = match f {
        None => (
            "all".to_string(),
            vec![
                Origin::Smo,
                Origin::TeslaApi,
                Origin::Import(ImportSite::Teslamate),
                Origin::Import(ImportSite::Teslafi),
            ],
        ),
        Some(SourceFilter::Live) => ("live".into(), vec![Origin::Smo, Origin::TeslaApi]),
        Some(SourceFilter::Smo) => ("smo".into(), vec![Origin::Smo]),
        Some(SourceFilter::TeslaApi) => ("tesla_api".into(), vec![Origin::TeslaApi]),
        Some(SourceFilter::Import(None)) => {
            ("import".into(), ImportSite::ALL.into_iter().map(Origin::Import).collect())
        }
        Some(SourceFilter::Import(Some(s))) => (Origin::Import(s).word(), vec![Origin::Import(s)]),
    };
    (word, source_refs(&origins))
}

/// `source=smo|tesla_api|import|import:teslamate|import:teslafi|all`, and the old words `can` (SMO) and `teslamate` (absent: all).
pub fn parse_source(s: Option<&str>) -> QResult<Option<SourceFilter>> {
    match s {
        None | Some("all") | Some("") => Ok(None),
        Some("smo") | Some("can") => Ok(Some(SourceFilter::Smo)),
        Some("tesla_api") => Ok(Some(SourceFilter::TeslaApi)),
        Some("import") => Ok(Some(SourceFilter::Import(None))),
        Some("teslamate") => Ok(Some(SourceFilter::Import(Some(model::ImportSite::Teslamate)))),
        Some(w) => match w.strip_prefix("import:").and_then(model::ImportSite::from_word) {
            Some(site) => Ok(Some(SourceFilter::Import(Some(site)))),
            None => Err(QueryError::bad("source", "smo, tesla_api, import, import:teslamate, import:teslafi or all")),
        },
    }
}

/// `dir=asc|desc`.
pub fn parse_dir(s: Option<&str>, default_desc: bool) -> QResult<bool> {
    match s {
        None | Some("") => Ok(default_desc),
        Some("desc") => Ok(true),
        Some("asc") => Ok(false),
        Some(_) => Err(QueryError::bad("dir", "asc or desc")),
    }
}

/// A place reference on a row: `{ id, name }`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlaceBrief {
    pub id: String,
    pub name: String,
}

/// An address as rows carry it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Address {
    pub short: Option<String>,
    pub locality: Option<String>,
    pub region: Option<String>,
    pub country_code: Option<String>,
}

/// Where a drive started or ended, a charge was, a stop is.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Where {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub place: Option<PlaceBrief>,
    pub address: Address,
    /// What to show: the place's name, else the short address.
    pub label: Option<String>,
}

pub(crate) fn where_of(
    places: &crate::placing::Places,
    lat: Option<f64>,
    lon: Option<f64>,
    address: Option<&str>,
    label_if_none: Option<&str>,
) -> Where {
    let place = match (lat, lon) {
        (Some(a), Some(b)) => places.place_at(a, b),
        _ => None,
    };
    let short = address.map(str::to_string);
    let label = place.map(|p| p.name.clone()).or_else(|| short.clone()).or_else(|| label_if_none.map(str::to_string));
    Where {
        lat: r(lat, 6),
        lon: r(lon, 6),
        place: place.map(|p| PlaceBrief { id: place_ref(p.id), name: p.name.clone() }),
        address: Address { short, ..Address::default() },
        label,
    }
}

/// Every car the db has, by id (for `cars=all`).
pub fn all_cars(e: &Engine) -> QResult<Vec<CarId>> {
    let mut out = Vec::new();
    let mut q = db::FleetQuery { search: None, limit: 500, after: None };
    loop {
        let page = e.db().fleet(&q)?;
        out.extend(page.cars.iter().map(|c| c.car.id));
        match page.next {
            Some(n) => q.after = Some(n),
            None => break,
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// `cars=all` or `cars=c1,c7`, checked against the cars the caller may see (`visible`): an unknown or hidden car is
/// `no_such_car`.
pub fn parse_cars(s: Option<&str>, visible: &[CarId]) -> QResult<Vec<CarId>> {
    match s {
        None | Some("all") | Some("") => Ok(visible.to_vec()),
        Some(list) => {
            let mut out = Vec::new();
            for part in list.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                let id = parse_car(part).ok_or(QueryError::NoSuchCar)?;
                if !visible.contains(&id) {
                    return Err(QueryError::NoSuchCar);
                }
                if !out.contains(&id) {
                    out.push(id);
                }
            }
            Ok(out)
        }
    }
}

/// Sums money by currency, rounded to cents.
pub(crate) fn money(by: BTreeMap<String, f64>) -> Vec<Money> {
    by.into_iter().map(|(currency, amount)| Money { currency, amount: (amount * 100.0).round() / 100.0 }).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    use model::{ImportSite, Input, Inputs, Source};

    #[test]
    fn source_words_and_their_old_spellings() {
        assert_eq!(super::parse_source(Some("can")).unwrap(), super::parse_source(Some("smo")).unwrap());
        assert_eq!(
            super::parse_source(Some("teslamate")).unwrap(),
            Some(super::SourceFilter::Import(Some(ImportSite::Teslamate)))
        );
        assert_eq!(super::parse_source(Some("import")).unwrap(), Some(super::SourceFilter::Import(None)));
        assert_eq!(super::parse_source(Some("all")).unwrap(), None);
        assert!(super::parse_source(Some("import:tessie")).is_err());
        assert!(super::parse_source(Some("fleet_api")).is_err());
        let api = super::SourceFilter::TeslaApi;
        assert!(api.keeps(Source::Live, Inputs::BOTH));
        assert!(!api.keeps(Source::Live, Inputs::NONE), "an older live row is SMO's");
        assert!(super::SourceFilter::Smo.keeps(Source::Live, Inputs::NONE));
        assert!(!super::SourceFilter::Smo.keeps(Source::Live, Inputs::from(Input::TeslaApi)));
        assert!(!super::SourceFilter::Smo.keeps(Source::Teslamate, Inputs::NONE));
    }

    #[test]
    fn a_row_names_its_sources_and_keeps_the_old_word() {
        let both = super::Sources::of(Source::Live, Inputs::BOTH);
        assert_eq!(both.source, "can");
        assert_eq!(both.source_label, "Data from SMO and the Tesla API");
        let v = serde_json::to_value(&both).unwrap();
        assert_eq!(
            v["sources"],
            serde_json::json!([
                { "kind": "smo", "label": "Data from SMO" },
                { "kind": "tesla_api", "label": "Data from the Tesla API" },
            ])
        );
        assert_eq!(super::Sources::of(Source::Live, Inputs::TESLA_API).source, "tesla_api");
        let tm = serde_json::to_value(super::Sources::family(Source::Teslamate)).unwrap();
        assert_eq!(tm["source"], "teslamate");
        assert_eq!(
            tm["sources"][0],
            serde_json::json!({ "kind": "import", "site": "teslamate", "label": "Imported from TeslaMate" })
        );
        assert_eq!(super::filter_words(None).0, "all");
        assert_eq!(
            super::filter_words(Some(super::SourceFilter::Import(Some(ImportSite::Teslamate)))).0,
            "import:teslamate"
        );
    }

    #[test]
    fn pages_follow_the_cursor_and_refuse_one_from_other_filters() {
        let rows: Vec<(f64, String)> = (0..7).map(|i| (f64::from(i % 3), format!("{i}"))).collect();
        let key = |r: &(f64, String)| r.clone();
        let p1 = paginate(rows.clone(), key, true, "a", &Paging::first(3), true).unwrap();
        assert_eq!(p1.items.len(), 3);
        assert_eq!(p1.total, Some(7));
        let p2 =
            paginate(rows.clone(), key, true, "a", &Paging { limit: Some(3), after: p1.next.clone() }, false).unwrap();
        let p3 =
            paginate(rows.clone(), key, true, "a", &Paging { limit: Some(3), after: p2.next.clone() }, false).unwrap();
        assert!(p3.next.is_none());
        let mut seen: Vec<String> = p1.items.into_iter().chain(p2.items).chain(p3.items).map(|r| r.1).collect();
        assert_eq!(seen.len(), 7);
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 7, "no row twice, none missed");
        let other = paginate(rows, key, true, "b", &Paging { limit: Some(3), after: p2.next }, false);
        assert_eq!(other.unwrap_err().code(), "bad_input");
        assert_eq!(Paging { limit: Some(501), after: None }.limit().unwrap_err().code(), "bad_input");
    }

    #[test]
    fn pages_go_on_past_rows_with_unknown_or_odd_keys() {
        let keys = [f64::NEG_INFINITY, f64::NEG_INFINITY, 1.5, f64::INFINITY, -0.0, f64::NEG_INFINITY, 3.0];
        let rows: Vec<(f64, String)> = keys.iter().enumerate().map(|(i, &k)| (k, format!("{i}"))).collect();
        let key = |r: &(f64, String)| r.clone();
        for desc in [true, false] {
            let mut seen = Vec::new();
            let mut after = None;
            loop {
                let p = paginate(rows.clone(), key, desc, "a", &Paging { limit: Some(2), after }, false).unwrap();
                seen.extend(p.items.into_iter().map(|r| r.1));
                match p.next {
                    Some(n) => after = Some(n),
                    None => break,
                }
            }
            seen.sort();
            assert_eq!(seen, (0..7).map(|i| i.to_string()).collect::<Vec<_>>(), "desc={desc}");
        }
    }

    #[test]
    fn ids_and_cars() {
        assert_eq!(parse_car("c12"), Some(12));
        assert_eq!(car_ref(3), "c3");
        assert_eq!(serde_json::to_string(&CarRef(4)).unwrap(), "\"c4\"");
        assert_eq!(parse_cars(Some("c1,c2"), &[1, 2, 3]).unwrap(), vec![1, 2]);
        assert_eq!(parse_cars(Some("c9"), &[1]).unwrap_err().code(), "no_such_car");
        assert_eq!(parse_cars(None, &[1, 2]).unwrap(), vec![1, 2]);
        assert_eq!(cmp_ids("10", "9"), std::cmp::Ordering::Greater);
    }
}
