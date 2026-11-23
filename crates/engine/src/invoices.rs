//! Supercharger invoices: the paid charging sessions an input read from Tesla's charging history (the Tesla API
//! input) or from TeslaFi, kept as raw input, and the PDF of each invoice.
//!
//! - **Kept as raw input**, sealed in the engine's key/value store (the db seals everything with the vault):
//!   `invoices/sessions` holds every session, one per `key` (`tesla-<session id>`), so the same session read again
//!   (a page read twice, the daily check, a restart in the middle of a backfill) is one session; `invoices/pdf/<key>`
//!   holds an invoice's PDF, one per invoice id. A rebuild of a car's history never touches them.
//! - **Matched when read**, never stored with the charge: a session goes to the car's DC charge that overlaps it in
//!   time the most (within [`TIME_SLACK_MS`] at each end), within [`NEAR_M`] of where the session was when both
//!   positions are known. Charges are rebuilt with new ids from their start, so a match kept with a charge could go
//!   stale; worked out from the sessions and the charges as they are, a full rebuild re-matches by itself. The
//!   matches of a car are kept in memory while its data and the sessions stay the same ([`Engine::memo`]).
//! - **What a charge costs**: a matched session's total (the sum of its sessions, when the car was plugged in twice
//!   in one charge) is the charge's cost, source `tesla_invoice` (or `teslafi`), over a place price or free
//!   Supercharging; a cost an admin typed ([`crate::costs::set_manual`]) wins over it. A session with no charge is
//!   listed ([`Engine::invoices`]), never dropped.
//! - **One visit read by two inputs** (a car with both the Tesla API and TeslaFi) is two sessions of one charge at
//!   the same time: only one counts ([`counted`]), Tesla's own (it has the invoice) before TeslaFi's. The other
//!   stays listed with the charge, its cost not used.

use crate::records::{self, CostEntry, CostPart};
use crate::{Engine, Result, lock, now_ms};
use model::{CarId, Charge, ChargeKind, Source};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// The key/value namespace.
pub const NS: &str = "invoices";
const SESSIONS: &str = "sessions";

/// A session and a charge overlap if they come this close at either end (ms): the car's data and Tesla's clock
/// differ, and Tesla's session ends when the cable is unlatched.
pub const TIME_SLACK_MS: u64 = 10 * 60_000;
/// A session and a charge are at the same site when this close (m), where both positions are known.
pub const NEAR_M: f64 = 400.0;
/// The largest PDF kept (bytes).
pub const MAX_PDF: usize = 4 << 20;
/// The most sessions kept (a long life of Supercharging is a few thousand).
pub const MAX_SESSIONS: usize = 50_000;

/// One fee of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fee {
    /// `charging`, `idle` (parking after the charge) or `other`.
    pub kind: String,
    /// What is owed for it, in the session's currency.
    pub amount: f64,
    /// The price per unit, where given.
    #[serde(default)]
    pub rate: Option<f64>,
    /// How much was billed (`unit`), where given.
    #[serde(default)]
    pub usage: Option<f64>,
    /// `kwh` or `min`, where given.
    #[serde(default)]
    pub unit: Option<String>,
}

/// One paid charging session, as an input read it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// `<source>-<the source's id for it>` ([`key_for`]): one session, however often it is read.
    pub key: String,
    /// `tesla` or `teslafi`.
    pub source: String,
    /// The input's account it was read with (`ta1`, `fa1`).
    pub account: String,
    /// The account's vehicle (`tv1`), where the session named one this server knows.
    #[serde(default)]
    pub vehicle: Option<String>,
    /// The car that vehicle is matched to when the session was read (none: not matched, listed as unmatched).
    #[serde(default)]
    pub car: Option<CarId>,
    /// Tesla's id of the invoice PDF (none: no invoice, or the source gives no PDFs).
    #[serde(default)]
    pub invoice_id: Option<String>,
    /// The invoice's own file name, where given (shown, and used in the zip when it is a plain name).
    #[serde(default)]
    pub invoice_name: Option<String>,
    /// The Supercharger's name.
    #[serde(default)]
    pub site: Option<String>,
    #[serde(default)]
    pub lat: Option<f64>,
    #[serde(default)]
    pub lon: Option<f64>,
    #[serde(default)]
    pub country_code: Option<String>,
    pub start_ms: u64,
    pub end_ms: u64,
    /// The total owed, in `currency`.
    #[serde(default)]
    pub total: Option<f64>,
    #[serde(default)]
    pub currency: Option<String>,
    /// The energy billed (kWh), where billed by energy.
    #[serde(default)]
    pub kwh_billed: Option<f64>,
    /// The minutes billed, where billed by time.
    #[serde(default)]
    pub minutes_billed: Option<f64>,
    #[serde(default)]
    pub fees: Vec<Fee>,
    /// When this server first read it, and last read a change to it.
    pub first_seen_ms: u64,
    pub updated_ms: u64,
    /// The PDF is kept (`invoices/pdf/<key>`).
    #[serde(default)]
    pub pdf: bool,
    /// Its size, bytes.
    #[serde(default)]
    pub pdf_bytes: u64,
}

impl Session {
    /// The idle fees (parking after the charge), summed.
    pub fn idle_fee(&self) -> Option<f64> {
        let v: Vec<f64> = self.fees.iter().filter(|f| f.kind == "idle").map(|f| f.amount).collect();
        (!v.is_empty()).then(|| crate::costs::round2(v.iter().sum()))
    }

    /// The cost source a matched charge shows.
    pub fn cost_source(&self) -> &'static str {
        if self.source == "teslafi" { "teslafi" } else { "tesla_invoice" }
    }
}

/// What an upsert did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Upserted {
    pub new: u64,
    pub changed: u64,
    pub same: u64,
}

/// A session and the charge it went to (none: not matched).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Listed {
    pub session: Session,
    pub charge: Option<u64>,
}

fn pdf_key(key: &str) -> String {
    format!("pdf/{key}")
}

/// A key as an input may give it: the source, `-`, and the source's id (letters, digits, `_`, `.`, `-`; at most 120),
/// so it goes in a path as it is.
pub fn key_ok(key: &str) -> bool {
    let id = key.strip_prefix("teslafi-").or_else(|| key.strip_prefix("tesla-"));
    id.is_some_and(|id| {
        !id.is_empty() && id.len() <= 120 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    })
}

/// The key of a session: `tesla-<id>` / `teslafi-<id>`, the id's other characters dropped (none when nothing is
/// left).
pub fn key_for(source: &str, id: &str) -> Option<String> {
    let id: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || "_.-".contains(*c)).take(120).collect();
    let k = format!("{source}-{id}");
    key_ok(&k).then_some(k)
}

/// Whether bytes are a PDF.
pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes.len() > 8 && bytes.starts_with(b"%PDF-")
}

fn read_all(e: &Engine) -> Result<BTreeMap<String, Session>> {
    Ok(records::get(&e.inner.db, NS, SESSIONS)?.unwrap_or_default())
}

/// The same session with nothing new: the times this server read it set aside.
fn same(a: &Session, b: &Session) -> bool {
    let strip = |s: &Session| Session { first_seen_ms: 0, updated_ms: 0, pdf: false, pdf_bytes: 0, ..s.clone() };
    strip(a) == strip(b)
}

impl Engine {
    fn invoice_stamp(&self) -> u64 {
        self.inner.invoice_gen.load(std::sync::atomic::Ordering::Acquire)
    }

    fn invoice_bump(&self) {
        self.inner.invoice_gen.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        *lock(&self.inner.invoice_all) = None;
    }

    /// Every session kept (in memory while none changes).
    pub fn invoice_sessions(&self) -> Result<Arc<BTreeMap<String, Session>>> {
        if let Some(s) = lock(&self.inner.invoice_all).clone() {
            return Ok(s);
        }
        let all = Arc::new(read_all(self)?);
        *lock(&self.inner.invoice_all) = Some(all.clone());
        Ok(all)
    }

    /// Keeps sessions an input read: new ones added, changed ones replaced (what this server knows of the PDF kept),
    /// the same ones left. Sessions with a bad key or no time are left out.
    pub fn invoice_upsert(&self, sessions: &[Session]) -> Result<Upserted> {
        let _one = lock(&self.inner.invoice_write);
        let mut all = read_all(self)?;
        let mut out = Upserted::default();
        let now = now_ms();
        let mut touched: Vec<(CarId, u64, u64)> = Vec::new();
        for s in sessions {
            if !key_ok(&s.key) || s.start_ms == 0 || s.end_ms < s.start_ms {
                continue;
            }
            match all.get_mut(&s.key) {
                Some(old) if same(old, s) => out.same += 1,
                Some(old) => {
                    let (first, pdf, bytes) = (old.first_seen_ms, old.pdf, old.pdf_bytes);
                    let kept_pdf = pdf && old.invoice_id == s.invoice_id;
                    touched.extend(old.car.map(|c| (c, old.start_ms, old.end_ms)));
                    *old = Session {
                        first_seen_ms: first,
                        updated_ms: now,
                        pdf: kept_pdf,
                        pdf_bytes: if kept_pdf { bytes } else { 0 },
                        ..s.clone()
                    };
                    touched.extend(s.car.map(|c| (c, s.start_ms, s.end_ms)));
                    out.changed += 1;
                }
                None => {
                    if all.len() >= MAX_SESSIONS {
                        continue;
                    }
                    all.insert(
                        s.key.clone(),
                        Session { first_seen_ms: now, updated_ms: now, pdf: false, pdf_bytes: 0, ..s.clone() },
                    );
                    touched.extend(s.car.map(|c| (c, s.start_ms, s.end_ms)));
                    out.new += 1;
                }
            }
        }
        if out.new + out.changed > 0 {
            records::put(&self.inner.db, NS, SESSIONS, &all)?;
            self.invoice_bump();
            for (car, from, to) in touched {
                self.emit(crate::Event::Rows {
                    car,
                    kinds: vec!["charges"],
                    from_ms: from.saturating_sub(TIME_SLACK_MS),
                    to_ms: to + TIME_SLACK_MS,
                });
            }
            // the places' prices from invoices, with these sessions
            self.invoice_prices_soon();
        }
        Ok(out)
    }

    /// Keeps a session's invoice PDF. `false`: no such session, or not a PDF (nothing kept).
    pub fn invoice_put_pdf(&self, key: &str, bytes: &[u8]) -> Result<bool> {
        if !is_pdf(bytes) || bytes.len() > MAX_PDF {
            return Ok(false);
        }
        let _one = lock(&self.inner.invoice_write);
        let mut all = read_all(self)?;
        let Some(s) = all.get_mut(key) else { return Ok(false) };
        self.inner.db.kv().put(NS, &pdf_key(key), bytes)?;
        s.pdf = true;
        s.pdf_bytes = bytes.len() as u64;
        records::put(&self.inner.db, NS, SESSIONS, &all)?;
        self.invoice_bump();
        Ok(true)
    }

    /// A session's PDF, when kept.
    pub fn invoice_pdf(&self, key: &str) -> Result<Option<Vec<u8>>> {
        if !key_ok(key) {
            return Ok(None);
        }
        Ok(self.inner.db.kv().get(NS, &pdf_key(key))?.filter(|b| is_pdf(b)))
    }

    /// The sessions of an input's account whose invoice is not kept yet: `(key, invoice id, vehicle)`, oldest first.
    pub fn invoice_pdfs_wanted(&self, source: &str, account: &str) -> Result<Vec<(String, String, Option<String>)>> {
        let all = self.invoice_sessions()?;
        let mut v: Vec<&Session> = all
            .values()
            .filter(|s| s.source == source && s.account == account && !s.pdf && s.invoice_id.is_some())
            .collect();
        v.sort_by_key(|s| s.start_ms);
        Ok(v.into_iter().filter_map(|s| Some((s.key.clone(), s.invoice_id.clone()?, s.vehicle.clone()))).collect())
    }

    /// Whether a session is kept already (an input reading new sessions stops at the first page it knows whole).
    pub fn invoice_known(&self, key: &str) -> Result<bool> {
        Ok(self.invoice_sessions()?.contains_key(key))
    }

    /// The sessions of a car matched to its charges: charge id → the sessions' keys (in time order).
    pub(crate) fn invoice_matches(&self, car: CarId) -> Result<Arc<HashMap<u64, Vec<String>>>> {
        let stamp = self.invoice_stamp();
        self.memo(car, "invoice-matches", crate::memo::On::All, stamp, || match_car(self, car))
    }

    /// The sessions matched to a charge (none when it has none).
    pub fn invoice_sessions_of(&self, car: CarId, charge: u64) -> Result<Vec<Session>> {
        let m = self.invoice_matches(car)?;
        let Some(keys) = m.get(&charge) else { return Ok(Vec::new()) };
        let all = self.invoice_sessions()?;
        Ok(keys.iter().filter_map(|k| all.get(k).cloned()).collect())
    }

    /// The cost a charge has from its invoices: the sum of its sessions in their currency (sessions in another
    /// currency than the first, or without a total, are left out of it).
    pub fn invoice_cost(&self, car: CarId, ch: &Charge) -> Result<Option<CostEntry>> {
        let mut sessions = self.invoice_sessions_of(car, ch.id)?;
        if sessions.iter().any(|s| s.currency.is_none() && s.total.is_some()) {
            // TeslaFi's log names no currency: the place's price's, else the first priced place's
            let places = self.places();
            let here = ch.position.as_ref().and_then(|p| places.place_at(p.lat, p.lon)).and_then(|p| p.price.as_ref());
            let cur = here.or_else(|| places.all().iter().find_map(|p| p.price.as_ref())).map(|p| p.currency.clone());
            for s in sessions.iter_mut().filter(|s| s.currency.is_none()) {
                s.currency.clone_from(&cur);
            }
        }
        Ok(cost_of(&sessions))
    }

    /// Every session kept with the charge it went to, newest first, of the cars given (`None`: all, unmatched ones
    /// included), starting in `[from_ms, to_ms)`.
    pub fn invoices(&self, cars: Option<&[CarId]>, from_ms: u64, to_ms: u64) -> Result<Vec<Listed>> {
        let all = self.invoice_sessions()?;
        let mut by_car: HashMap<CarId, HashMap<String, u64>> = HashMap::new();
        let mut out = Vec::new();
        for s in all.values() {
            if s.start_ms < from_ms || s.start_ms >= to_ms {
                continue;
            }
            if let Some(cs) = cars
                && !s.car.is_some_and(|c| cs.contains(&c))
            {
                continue;
            }
            let charge = match s.car {
                Some(car) => {
                    if let std::collections::hash_map::Entry::Vacant(v) = by_car.entry(car) {
                        let m = self.invoice_matches(car)?;
                        v.insert(m.iter().flat_map(|(ch, keys)| keys.iter().map(|k| (k.clone(), *ch))).collect());
                    }
                    by_car.get(&car).and_then(|m| m.get(&s.key)).copied()
                }
                None => None,
            };
            out.push(Listed { session: s.clone(), charge });
        }
        out.sort_by(|a, b| b.session.start_ms.cmp(&a.session.start_ms).then_with(|| a.session.key.cmp(&b.session.key)));
        Ok(out)
    }
}

/// Whose word on a visit counts first: Tesla's own charging history (the invoice), then any other input's.
fn rank(s: &Session) -> u8 {
    u8::from(s.source != "tesla")
}

/// The sessions of one charge that count toward its cost: a session at the same time as one of an input that ranks
/// before it (Tesla's own before TeslaFi's) is the same visit read twice, and is left out. Sessions of one input
/// all count (the car plugged in twice in one charge), as do sessions of two inputs at different times.
pub fn counted(sessions: &[Session]) -> Vec<&Session> {
    let meet = |a: &Session, b: &Session| {
        a.start_ms <= b.end_ms.max(b.start_ms + 1) && b.start_ms <= a.end_ms.max(a.start_ms + 1)
    };
    sessions.iter().filter(|s| !sessions.iter().any(|o| rank(o) < rank(s) && meet(o, s))).collect()
}

/// The cost of a charge's sessions: those that count ([`counted`]), added up.
pub fn cost_of(sessions: &[Session]) -> Option<CostEntry> {
    let sessions = counted(sessions);
    let first = sessions.iter().find(|s| s.total.is_some() && s.currency.is_some())?;
    let currency = first.currency.clone()?;
    let mine: Vec<&Session> =
        sessions.iter().copied().filter(|s| s.total.is_some() && s.currency.as_deref() == Some(&currency)).collect();
    let amount: f64 = mine.iter().filter_map(|s| s.total).sum();
    if !amount.is_finite() {
        return None;
    }
    let parts: Vec<CostPart> = mine
        .iter()
        .flat_map(|s| s.fees.iter())
        .map(|f| CostPart {
            rate: Some(match f.kind.as_str() {
                "charging" => "Charging".to_string(),
                "idle" => "Idle fee".to_string(),
                _ => "Other fee".to_string(),
            }),
            per_kwh: if f.unit.as_deref() == Some("kwh") { f.rate.unwrap_or(0.0) } else { 0.0 },
            kwh: if f.unit.as_deref() == Some("kwh") { f.usage.unwrap_or(0.0) } else { 0.0 },
            cost: f.amount,
            from_kwh: None,
            to_kwh: None,
        })
        .collect();
    Some(CostEntry {
        amount: crate::costs::round2(amount),
        currency,
        source: first.cost_source().to_string(),
        mode: None,
        entered: None,
        energy_basis: None,
        place: None,
        parts,
        session_fee: None,
        gap_ms: 0,
        cycle: None,
        of: None,
        at_ms: mine.iter().map(|s| s.updated_ms).max().unwrap_or(0),
    })
}

/// How much `[a0, a1]` and `[b0, b1]` overlap, each widened by the slack (ms; 0 when they do not meet).
fn overlap(a0: u64, a1: u64, b0: u64, b1: u64) -> u64 {
    let lo = a0.max(b0.saturating_sub(TIME_SLACK_MS));
    let hi = a1.max(a0 + 1).min(b1.max(b0 + 1) + TIME_SLACK_MS);
    hi.saturating_sub(lo)
}

/// A charge a session can be: DC (or of no known kind, at DC power), near enough where both positions are known.
fn could_be(s: &Session, ch: &Charge) -> bool {
    let dc = match ch.kind {
        Some(ChargeKind::Dc) => true,
        Some(_) => false,
        None => ch.power_max_kw.is_some_and(|p| p >= 25.0),
    };
    if !dc {
        return false;
    }
    match (s.lat.zip(s.lon), ch.position.as_ref()) {
        (Some((lat, lon)), Some(p)) => places::geo::distance_m(lat, lon, p.lat, p.lon) <= NEAR_M,
        _ => true,
    }
}

/// Works a car's matches out: each of its sessions to the charge it overlaps most (a live charge before an imported
/// one of the same overlap).
fn match_car(e: &Engine, car: CarId) -> Result<HashMap<u64, Vec<String>>> {
    let all = e.invoice_sessions()?;
    let mut mine: Vec<&Session> = all.values().filter(|s| s.car == Some(car)).collect();
    mine.sort_by_key(|s| (s.start_ms, s.key.clone()));
    let mut out: HashMap<u64, Vec<String>> = HashMap::new();
    if mine.is_empty() {
        return Ok(out);
    }
    // the car's charges around its sessions, read once per stretch of sessions close together
    let mut i = 0;
    while i < mine.len() {
        let from = mine[i].start_ms.saturating_sub(24 * 3_600_000);
        let mut j = i;
        let mut to = mine[i].end_ms;
        while j + 1 < mine.len() && mine[j + 1].start_ms <= to + 7 * 86_400_000 {
            j += 1;
            to = to.max(mine[j].end_ms);
        }
        let charges: Vec<Charge> = crate::pipeline::all_rows(&e.inner.db, car, from, to + TIME_SLACK_MS + 1, None)?;
        for s in &mine[i..=j] {
            let best = charges
                .iter()
                .filter(|ch| could_be(s, ch))
                .map(|ch| (overlap(ch.start_ms, ch.end_ms, s.start_ms, s.end_ms), ch.source == Source::Live, ch))
                .filter(|(v, _, _)| *v > 0)
                .max_by_key(|(v, live, ch)| (*v, *live, std::cmp::Reverse(ch.start_ms)));
            if let Some((_, _, ch)) = best {
                out.entry(ch.id).or_default().push(s.key.clone());
            }
        }
        i = j + 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(key: &str, total: Option<f64>, cur: &str) -> Session {
        Session {
            key: key.into(),
            source: "tesla".into(),
            account: "ta1".into(),
            vehicle: None,
            car: None,
            invoice_id: None,
            invoice_name: None,
            site: None,
            lat: None,
            lon: None,
            country_code: None,
            start_ms: 1,
            end_ms: 2,
            total,
            currency: Some(cur.into()),
            kwh_billed: Some(10.0),
            minutes_billed: None,
            fees: vec![Fee {
                kind: "idle".into(),
                amount: 1.5,
                rate: Some(0.5),
                usage: Some(3.0),
                unit: Some("min".into()),
            }],
            first_seen_ms: 0,
            updated_ms: 0,
            pdf: false,
            pdf_bytes: 0,
        }
    }

    #[test]
    fn keys_and_pdfs() {
        assert!(key_ok("tesla-123"));
        assert!(key_ok("teslafi-a_b.c-9"));
        assert!(!key_ok("tesla-"));
        assert!(!key_ok("123"));
        assert!(!key_ok("tesla-1 2"));
        assert!(!key_ok("other-1"));
        assert_eq!(key_for("tesla", "12/34"), Some("tesla-1234".into()));
        assert_eq!(key_for("tesla", "//"), None);
        assert!(is_pdf(b"%PDF-1.4\n%made up"));
        assert!(!is_pdf(b"<html>"));
    }

    #[test]
    fn a_charges_sessions_add_up_in_one_currency() {
        let c = cost_of(&[
            s("tesla-1", Some(10.0), "EUR"),
            s("tesla-2", Some(2.255), "EUR"),
            s("tesla-3", Some(9.0), "USD"),
        ])
        .unwrap_or_else(|| panic!("a cost"));
        assert_eq!(c.amount, 12.26);
        assert_eq!(c.currency, "EUR");
        assert_eq!(c.source, "tesla_invoice");
        assert_eq!(c.parts.len(), 2);
        assert!(cost_of(&[s("tesla-1", None, "EUR")]).is_none());
        assert_eq!(s("tesla-1", None, "EUR").idle_fee(), Some(1.5));
    }

    #[test]
    fn one_visit_read_by_two_inputs_counts_once() {
        let at = |key: &str, source: &str, start: u64, end: u64, total: f64| Session {
            source: source.into(),
            start_ms: start,
            end_ms: end,
            ..s(key, Some(total), "USD")
        };
        // the same visit from Tesla and from TeslaFi: Tesla's counts, whichever was read first
        let both = [at("teslafi-9", "teslafi", 1000, 9000, 20.5), at("tesla-1", "tesla", 2000, 9500, 20.0)];
        let c = cost_of(&both).unwrap_or_else(|| panic!("a cost"));
        assert_eq!((c.amount, c.source.as_str()), (20.0, "tesla_invoice"));
        assert_eq!(counted(&both).iter().map(|s| s.key.as_str()).collect::<Vec<_>>(), vec!["tesla-1"]);
        // plugged in twice, the second visit read by TeslaFi alone: both count
        let twice = [at("tesla-1", "tesla", 1000, 9000, 20.0), at("teslafi-9", "teslafi", 20_000, 30_000, 5.0)];
        assert_eq!(cost_of(&twice).map(|c| c.amount), Some(25.0));
        // TeslaFi alone
        let one = [at("teslafi-9", "teslafi", 1000, 9000, 20.5)];
        assert_eq!(cost_of(&one).map(|c| (c.amount, c.source)), Some((20.5, "teslafi".into())));
    }

    #[test]
    fn overlaps_with_slack() {
        assert!(overlap(0, 1000, 500, 2000) > 0);
        assert!(overlap(15 * 60_000, 40 * 60_000, 0, 10 * 60_000) > 0);
        assert_eq!(overlap(10 * 60_000 * 3, 10 * 60_000 * 4, 0, 60_000), 0);
    }
}
