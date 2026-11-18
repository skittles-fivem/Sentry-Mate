//! `/cars/{car}/awake-reasons`: the spans the car was awake while not driving, with the buses that were awake, why it
//! was awake (as far as the decoded signals tell) and the pack's average power over the span.

use super::{List, Paging, Period, QResult, Scoped, car_of, paginate, r, scoped, source_word};
use crate::Engine;
use crate::pipeline::all_rows;
use db::StateRow;
use model::{CarId, Sample, Source, State, StateSpan};
use serde::Serialize;

const DAY: u64 = 86_400_000;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AwakeSpan {
    pub start_ms: u64,
    pub end_ms: u64,
    /// `vehicle`, `chassis`.
    pub buses: Vec<&'static str>,
    /// `charging`, ... (only those the decoded signals show).
    pub reasons: Vec<&'static str>,
    /// The pack's mean power over the span, kW (positive out of the pack); null without samples.
    pub avg_power_kw: Option<f64>,
    pub source: &'static str,
}

/// The awake (online) spans of `spans` with `busy` (drives: start, end) taken out, joined where they touch.
pub(crate) fn awake_pieces(spans: &[StateSpan], busy: &[(u64, u64)]) -> Vec<(u64, u64, Vec<String>, Source)> {
    let mut out: Vec<(u64, u64, Vec<String>, Source)> = Vec::new();
    let mut push = |x: u64, y: u64, buses: &[String], src: Source| {
        if y <= x {
            return;
        }
        if let Some(l) = out.last_mut()
            && l.1 == x
            && l.3 == src
        {
            l.1 = y;
            for b in buses {
                if !l.2.contains(b) {
                    l.2.push(b.clone());
                }
            }
            return;
        }
        out.push((x, y, buses.to_vec(), src));
    };
    let mut bi = 0usize;
    for s in spans.iter().filter(|s| s.state == State::Online) {
        while busy.get(bi).is_some_and(|b| b.1 <= s.start_ms) {
            bi += 1;
        }
        let mut cur = s.start_ms;
        for &(x, y) in busy[bi.min(busy.len())..].iter().take_while(|b| b.0 < s.end_ms) {
            if x > cur {
                push(cur, x.min(s.end_ms), &s.buses, s.source);
            }
            cur = cur.max(y);
        }
        if s.end_ms > cur {
            push(cur, s.end_ms, &s.buses, s.source);
        }
    }
    out
}

pub(crate) fn bus_words(b: &[String]) -> Vec<&'static str> {
    let mut v = Vec::new();
    if b.iter().any(|x| x == "can0") {
        v.push("vehicle");
    }
    if b.iter().any(|x| x == "can1") {
        v.push("chassis");
    }
    v
}

/// The mean of `f` over the car's samples in `[a, b)`, read a day at a time (never the whole span at once).
pub(crate) fn mean_of(
    e: &Engine,
    car: CarId,
    source: Source,
    a: u64,
    b: u64,
    f: impl Fn(&Sample) -> Option<f64>,
) -> QResult<Option<f64>> {
    let (mut sum, mut n) = (0.0, 0u64);
    let mut x = a;
    while x < b {
        let y = (x / DAY + 1).saturating_mul(DAY).min(b);
        for s in e.db().read_points::<Sample>(car, source, x, y)? {
            if let Some(v) = f(&s).filter(|v| v.is_finite()) {
                sum += v;
                n += 1;
            }
        }
        x = y;
    }
    Ok((n > 0).then(|| sum / n as f64))
}

/// `/cars/{car}/awake-reasons`, newest first. A span belongs to the period by its start.
pub fn awake_reasons(e: &Engine, car: CarId, period: &Period, paging: &Paging) -> QResult<Scoped<List<AwakeSpan>>> {
    let c = car_of(e, car)?;
    let (a, b) = period.bounds()?;
    let b = b.min(crate::now_ms().saturating_add(1));
    // from a little earlier: a span that began before the period and goes on into it is that span, not a new one
    let from = a.saturating_sub(2 * DAY);
    let spans = super::timeline::joined(
        all_rows::<StateRow>(e.db(), car, from, b, Some(Source::Can))?.into_iter().map(|s| s.span).collect(),
    );
    let drives: Vec<(u64, u64)> = super::drives::rows(e, car, &Period::new(Some(from), Some(b)), Some(Source::Can))?
        .iter()
        .map(|d| (d.start_ms, d.end_ms))
        .collect();
    let pieces: Vec<(u64, u64, Vec<String>, Source)> =
        awake_pieces(&spans, &drives).into_iter().filter(|p| p.0 >= a && p.0 < b).collect();
    let sig = format!("awake|{car}|{}", period.sig());
    let page = paginate(pieces, |p| (p.0 as f64, p.0.to_string()), true, &sig, paging, false)?;
    let charges: Vec<(u64, u64)> =
        super::charges::rows(e, car, &Period::new(Some(from.saturating_sub(5 * DAY)), Some(b)), Some(Source::Can))?
            .iter()
            .map(|ch| (ch.start_ms, ch.end_ms))
            .collect();
    let mut items = Vec::with_capacity(page.items.len());
    for (x, y, buses, src) in page.items {
        let mut reasons = Vec::new();
        if charges.iter().any(|&(p, q)| p < y && q > x) {
            reasons.push("charging");
        }
        items.push(AwakeSpan {
            start_ms: x,
            end_ms: y,
            buses: bus_words(&buses),
            reasons,
            avg_power_kw: r(mean_of(e, car, src, x, y, |s| s.battery_power_kw)?, 3),
            source: source_word(src),
        });
    }
    Ok(scoped(&c, List { items, next: page.next, total: None }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(state: State, a: u64, b: u64, buses: &[&str]) -> StateSpan {
        StateSpan {
            car: 1,
            state,
            start_ms: a,
            end_ms: b,
            buses: buses.iter().map(|s| s.to_string()).collect(),
            source: Source::Can,
        }
    }

    #[test]
    fn drives_are_taken_out_and_touching_spans_join() {
        let spans = vec![
            span(State::Online, 0, 100, &["can0"]),
            span(State::Online, 100, 200, &["can0", "can1"]),
            span(State::Asleep, 200, 300, &[]),
            span(State::Online, 300, 400, &["can0"]),
        ];
        let p = awake_pieces(&spans, &[(150, 170), (350, 500)]);
        assert_eq!(p.len(), 3, "{p:?}");
        assert_eq!((p[0].0, p[0].1), (0, 150));
        assert_eq!(bus_words(&p[0].2), vec!["vehicle", "chassis"]);
        assert_eq!((p[1].0, p[1].1), (170, 200));
        assert_eq!((p[2].0, p[2].1), (300, 350));
    }
}
