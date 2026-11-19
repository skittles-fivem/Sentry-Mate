//! The drive and charge events MQTT publishes as the engine records them (the web server's publisher sends them on
//! `<prefix>/cars/c<n>/event`). The same events, rendered the same way, as `/cars/{car}/events` lists them
//! ([`super::software::events`]): a drive or charge starting, and ending once it is no longer the one going on and
//! the car's data goes on at least [`SETTLE_MS`] past its end (so a log that stops mid-drive, or a late upload worked
//! through log by log, never ends a drive early: its end goes out once the data shows it).
//!
//! Bounds: drives and charges are read newest first over the span asked (and as far back as the longest drive, for
//! one that ends in it), and reading stops once `limit` events are found.

use super::drives::each;
use super::system::Shown;
use super::{QResult, car_of};
use crate::Engine;
use model::{CarId, Charge, Drive};

/// The most events one call gives.
pub const MAX_EVENTS: usize = 200;
/// How far the car's data must go on past a drive's or charge's end before the end is published.
pub const SETTLE_MS: u64 = 2 * 60_000;

/// One event, with the key that tells it apart from every other event of the car (`drive_ended:<drive id>`).
#[derive(Debug, Clone, PartialEq)]
pub struct Published {
    pub key: String,
    pub at_ms: u64,
    pub event: mqtt::Event,
}

/// The drive and charge events of `car` that happened in `[from_ms, to_ms)`, oldest first: the newest `limit` of them
/// (at most [`MAX_EVENTS`]). A drive or charge still going, or whose end the data doesn't show yet, gives only its
/// start.
pub fn drive_charge_events(e: &Engine, car: CarId, from_ms: u64, to_ms: u64, limit: usize) -> QResult<Vec<Published>> {
    let limit = limit.min(MAX_EVENTS);
    if from_ms >= to_ms || limit == 0 {
        return Ok(Vec::new());
    }
    let c = car_of(e, car)?;
    let shown = Shown::new(e, car, None)?;
    let places = e.places();
    let name_at = |p: Option<&model::Position>| p.and_then(|p| places.place_at(p.lat, p.lon)).map(|pl| pl.name.clone());
    let within = |t: u64| (from_ms..to_ms).contains(&t);
    let reach = from_ms.saturating_sub(super::reports::LONGEST_DRIVE_MS);
    // the newest moment the car's data reaches
    let newest = e.db().entry(car)?.and_then(|x| x.summary.now).map_or(0, |n| n.at_ms);
    let settled = |end: u64| end.saturating_add(SETTLE_MS) <= newest;
    let mut out: Vec<Published> = Vec::new();

    let live = super::drives::ctx(e, &c)?.live;
    let mut n = 0usize;
    each::<Drive>(e, car, reach, to_ms, None, &shown, true, |d| {
        let (sp, ep) = (name_at(d.start.as_ref()), name_at(d.end.as_ref()));
        if live != Some(d.id) && within(d.end_ms) && settled(d.end_ms) {
            out.push(Published {
                key: format!("drive_ended:{}", d.id),
                at_ms: d.end_ms,
                event: mqtt::Event::DriveEnded { drive: Box::new(d.clone()), start_place: sp.clone(), end_place: ep },
            });
            n += 1;
        }
        if within(d.start_ms) {
            out.push(Published {
                key: format!("drive_started:{}", d.id),
                at_ms: d.start_ms,
                event: mqtt::Event::DriveStarted { drive: Box::new(d.clone()), start_place: sp },
            });
            n += 1;
        }
        // drives don't overlap: every older one's events come before these
        Ok(n < limit && d.end_ms >= from_ms)
    })?;

    let cx = super::charges::ctx(e, &c)?;
    let mut n = 0usize;
    each::<Charge>(e, car, reach, to_ms, None, &shown, true, |mut ch| {
        let place = name_at(ch.position.as_ref());
        if let Some(k) = super::charges::cost_of(e, &c, &ch)? {
            ch.cost = Some(k.amount);
        }
        if cx.live != Some(ch.id) && within(ch.end_ms) && settled(ch.end_ms) {
            out.push(Published {
                key: format!("charge_ended:{}", ch.id),
                at_ms: ch.end_ms,
                event: mqtt::Event::ChargeEnded { charge: Box::new(ch.clone()), place: place.clone() },
            });
            n += 1;
        }
        if within(ch.start_ms) {
            out.push(Published {
                key: format!("charge_started:{}", ch.id),
                at_ms: ch.start_ms,
                event: mqtt::Event::ChargeStarted { charge: Box::new(ch.clone()), place },
            });
            n += 1;
        }
        Ok(n < limit && ch.end_ms >= from_ms)
    })?;

    // a start comes before its own end at the same moment (a row of no length)
    let rank = |p: &Published| u8::from(p.key.contains("_ended:"));
    out.sort_by(|a, b| (a.at_ms, rank(a), &a.key).cmp(&(b.at_ms, rank(b), &b.key)));
    let skip = out.len().saturating_sub(limit);
    out.drain(..skip);
    Ok(out)
}
