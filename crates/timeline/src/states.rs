//! Online, asleep or unknown, over a window, from the logs' facts alone.
//!
//! - **Online**: from a log's first frame to its last; logs that follow within [`Params::join_ms`] with the same
//!   buses are one span, and a span ends where the buses change (the vehicle bus awake with the chassis bus asleep is
//!   a parked car kept awake).
//! - **Asleep**: from the last frame of a log closed `quiet` until the next frames.
//!   The end of the window (the newest data) within [`Params::join_ms`] of a log's last frame is still online.
//! - **Unknown**: everything else (no log, a log closed for an update or a stop, the time between logs), and always a
//!   `# not recorded` pause or a hole the caller knows of, even where it would otherwise be asleep. Never sleep.

use crate::facts::{Closed, LogFacts};
use crate::timeline::Params;
use model::{CarId, Source, State, StateSpan};

struct Piece {
    start: u64,
    end: u64,
    buses: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Ev {
    OnlineEnd(usize),
    OnlineStart(usize),
    AsleepEnd,
    AsleepStart,
    UnknownEnd,
    UnknownStart,
}

/// The online spans of the logs, merged, without overlaps, by start.
fn online(logs: &[&LogFacts], join_ms: u64) -> Vec<Piece> {
    let mut pieces: Vec<Piece> = logs
        .iter()
        .filter_map(|l| l.frames().map(|(a, b)| Piece { start: a, end: b.max(a), buses: l.buses.clone() }))
        .collect();
    pieces.sort_by_key(|a| (a.start, a.end));
    let mut out: Vec<Piece> = Vec::with_capacity(pieces.len());
    for p in pieces {
        if let Some(last) = out.last_mut()
            && p.start <= last.end.saturating_add(join_ms)
        {
            if last.buses == p.buses {
                last.end = last.end.max(p.end);
                continue;
            }
            if p.end <= last.end {
                continue; // inside another log's frames (two bridges at once): adds nothing
            }
            // other buses: the earlier one runs until these start
            last.end = p.start;
        }
        out.push(p);
    }
    out.retain(|p| p.end > p.start);
    out
}

/// The state spans of `[from_ms, to_ms)`. `logs` should hold every log that overlaps the window and the last one
/// before it (so a sleep that began before the window is known); `holes` are spans the caller knows have no usable
/// data (a log that failed its check, a gap in the chain).
pub(crate) fn states(
    car: CarId,
    from_ms: u64,
    to_ms: u64,
    logs: &[&LogFacts],
    holes: &[(u64, u64)],
    params: &Params,
) -> Vec<StateSpan> {
    if to_ms <= from_ms {
        return Vec::new();
    }
    let on = online(logs, params.join_ms);
    let mut events: Vec<(u64, Ev)> = Vec::with_capacity(on.len() * 2 + logs.len() * 2 + holes.len() * 2);
    for (i, p) in on.iter().enumerate() {
        events.push((p.start, Ev::OnlineStart(i)));
        events.push((p.end, Ev::OnlineEnd(i)));
    }
    // (a quiet log with no frames and no times says nothing about when)
    for l in logs.iter().filter(|l| l.closed == Closed::Quiet && (l.last_frame_ms.is_some() || l.start_ms > 0)) {
        let from = l.last_frame_ms.unwrap_or(l.start_ms);
        // until the next frames
        let next = on.partition_point(|p| p.start <= from);
        let until = on.get(next).map_or(u64::MAX, |p| p.start);
        events.push((from, Ev::AsleepStart));
        events.push((until, Ev::AsleepEnd));
    }
    for l in logs {
        for n in &l.not_recorded {
            let from = match n.paused_ms {
                Some(p) => n.at_ms.saturating_sub(p),
                // the length isn't known: back to the end of the log before
                None => logs.iter().map(|o| o.span().1).filter(|&e| e <= n.at_ms).max().unwrap_or(from_ms),
            };
            events.push((from, Ev::UnknownStart));
            events.push((n.at_ms, Ev::UnknownEnd));
        }
    }
    for &(a, b) in holes.iter().filter(|(a, b)| b > a) {
        events.push((a, Ev::UnknownStart));
        events.push((b, Ev::UnknownEnd));
    }
    events.sort_unstable();

    let mut spans: Vec<StateSpan> = Vec::new();
    let mut push = |start: u64, end: u64, online: Option<usize>, asleep: i64, unknown: i64| {
        if end <= start {
            return;
        }
        let (state, buses) = match online {
            Some(i) => (State::Online, on.get(i).map(|p| p.buses.clone()).unwrap_or_default()),
            None if unknown > 0 => (State::Unknown, Vec::new()),
            None if asleep > 0 => (State::Asleep, Vec::new()),
            None => (State::Unknown, Vec::new()),
        };
        if let Some(last) = spans.last_mut()
            && last.state == state
            && last.buses == buses
            && last.end_ms == start
        {
            last.end_ms = end;
            return;
        }
        spans.push(StateSpan {
            car,
            state,
            start_ms: start,
            end_ms: end,
            buses,
            source: Source::Live,
            inputs: model::Inputs::SMO,
        });
    };
    let (mut cur, mut online_now, mut asleep, mut unknown) = (from_ms, None, 0i64, 0i64);
    // the online piece that ended last, and when
    let mut ended: Option<(usize, u64)> = None;
    let mut i = 0;
    while i < events.len() {
        let t = events[i].0;
        let end = t.clamp(from_ms, to_ms);
        if end > cur {
            push(cur, end, online_now, asleep, unknown);
            cur = end;
        }
        while let Some(&(_, ev)) = events.get(i).filter(|(et, _)| *et == t) {
            match ev {
                Ev::OnlineStart(k) => online_now = Some(k),
                Ev::OnlineEnd(k) if online_now == Some(k) => {
                    online_now = None;
                    ended = Some((k, t));
                }
                Ev::OnlineEnd(_) => {}
                Ev::AsleepStart => asleep += 1,
                Ev::AsleepEnd => asleep -= 1,
                Ev::UnknownStart => unknown += 1,
                Ev::UnknownEnd => unknown -= 1,
            }
            i += 1;
        }
    }
    if to_ms > cur {
        // the window ends (at the newest data) within `join_ms` of the last frames, with nothing saying otherwise: the
        // car is still online there, as a log that followed this soon would be joined. (Else every window would end in
        // a sliver of unknown between a log's last frame and its end, and the car's state now would read unknown.)
        let tail = match ended {
            Some((k, at)) if online_now.is_none() && asleep == 0 && unknown == 0 && at == cur => {
                (to_ms - cur <= params.join_ms).then_some(k)
            }
            _ => None,
        };
        push(cur, to_ms, online_now.or(tail), asleep, unknown);
    }
    spans
}

/// A point the timeline can be cut at: an asleep span, or an unknown one of at least [`Params::settle_ms`]. Nothing
/// is carried across one (a drive or a charge can't go on through it), so what lies between two is worked out alone.
/// A short unknown span (an app update mid-drive, one lost log) is not one: a drive or a charge may go on across it.
pub fn is_settled(span: &StateSpan, params: &Params) -> bool {
    match span.state {
        State::Asleep => true,
        State::Unknown => span.end_ms.saturating_sub(span.start_ms) >= params.settle_ms,
        State::Online => false,
    }
}

/// What the Tesla API saw over a stretch, strongest first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Seen {
    /// The car's values came (vehicle data, the stream): it was awake.
    Data,
    Online,
    Asleep,
}

/// The states with what the Tesla API saw laid over them. SMO's frames (online) always stand. Where SMO says unknown
/// (no log, a hole, a pause), the API's online gives online and its asleep gives asleep (an offline car stays
/// unknown). Where SMO says asleep only because no frames came after a quiet log, the car's values coming from the API
/// say it was awake after all: online. Spans the API gave name it in their `inputs`. `unknown_inputs`: what an unknown
/// span left names (a car with no SMO data names the API).
pub(crate) fn with_api(spans: Vec<StateSpan>, api: &[crate::ApiSpan], unknown_inputs: model::Inputs) -> Vec<StateSpan> {
    use crate::ApiState;
    let mut api: Vec<&crate::ApiSpan> =
        api.iter().filter(|a| a.end_ms > a.start_ms && a.state != ApiState::Offline).collect();
    if api.is_empty() && unknown_inputs == model::Inputs::SMO {
        return spans;
    }
    api.sort_by_key(|a| (a.start_ms, a.end_ms));
    let mut out: Vec<StateSpan> = Vec::with_capacity(spans.len());
    let mut push = |s: StateSpan| {
        if s.end_ms <= s.start_ms {
            return;
        }
        if let Some(l) = out.last_mut()
            && l.state == s.state
            && l.buses == s.buses
            && l.inputs == s.inputs
            && l.end_ms == s.start_ms
        {
            l.end_ms = s.end_ms;
            return;
        }
        out.push(s);
    };
    for span in spans {
        if span.state == State::Online {
            push(span);
            continue;
        }
        let upto = api.partition_point(|a| a.start_ms < span.end_ms);
        let over: Vec<&crate::ApiSpan> = api[..upto].iter().copied().filter(|a| a.end_ms > span.start_ms).collect();
        let mut pts = vec![span.start_ms, span.end_ms];
        for a in &over {
            pts.push(a.start_ms.max(span.start_ms));
            pts.push(a.end_ms.min(span.end_ms));
        }
        pts.sort_unstable();
        pts.dedup();
        for w in pts.windows(2) {
            let (x, y) = (w[0], w[1]);
            let looks: Vec<(Seen, model::Inputs)> = over
                .iter()
                .filter(|a| a.start_ms <= x && a.end_ms >= y)
                .map(|a| {
                    let seen = match a.state {
                        ApiState::Online if a.data => Seen::Data,
                        ApiState::Online => Seen::Online,
                        _ => Seen::Asleep,
                    };
                    (seen, a.input())
                })
                .collect();
            let seen = looks.iter().map(|l| l.0).min();
            // the API that saw it: of those that saw the same, the first in the order they win (the Tesla API, then
            // TeslaFi's)
            let by = looks.iter().filter(|l| Some(l.0) == seen).fold(model::Inputs::NONE, |a, l| a.union(l.1));
            let api_in = model::Input::apis().find(|i| by.contains(*i)).map_or(by, model::Inputs::from);
            let (state, inputs) = match (span.state, seen) {
                (State::Asleep, Some(Seen::Data)) => (State::Online, api_in),
                (State::Asleep, _) => (State::Asleep, span.inputs),
                (_, Some(Seen::Data | Seen::Online)) => (State::Online, api_in),
                (_, Some(Seen::Asleep)) => (State::Asleep, api_in),
                (_, None) => (State::Unknown, unknown_inputs),
            };
            let buses = if state == span.state { span.buses.clone() } else { Vec::new() };
            push(StateSpan { start_ms: x, end_ms: y, state, inputs, buses, ..span.clone() });
        }
    }
    out
}
