//! Which window of a car's timeline to rebuild when a log arrives, in order or late.
//!
//! A log can only change what lies between the settled spans around it ([`is_settled`]): back to the last asleep (or
//! long unknown) span that ended before the log, forward to the first that starts after it. The window runs from the
//! start of the one to the end of the other, so both are rebuilt whole and nothing outside changes.
//!
//! A car that stays awake (Sentry mode, camping) has no settled span for days, so the idle cut points of the existing
//! timeline ([`crate::Timeline::cuts`]) bound the window too: the last one far enough before what the log can change
//! (the state spans it falls in, and the samples a cut point looks at), the first one far enough after. Nothing is
//! carried across a cut point either, and one that far from the log is still one once the log is in.
//!
//! The caller replaces the states, drives, charges, positions and charge points in `[from_ms, to_ms)`, and the cut
//! points strictly between `from_ms` and `to_ms`, with [`crate::build`]'s for that window.
//!
//! It looks only at the spans and cut points near the log (binary searches and a walk to the nearest settled spans),
//! so a car with years of history costs no more than a new one.

use crate::states::is_settled;
use crate::timeline::{Params, cut_margin_ms};
use model::StateSpan;

/// A window to rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// From here (0: no settled span or cut point before the log, from the car's first data).
    pub from_ms: u64,
    /// To here; `None`: no settled span or cut point after the log, to the newest data (now).
    pub to_ms: Option<u64>,
}

/// The window to rebuild for a log whose data spans `[log_from_ms, log_to_ms]` (its frames and its own start and end:
/// [`crate::LogFacts::span`]), given the car's existing state spans around it (sorted by start) and its idle cut points
/// (sorted; `&[]` when none are kept, then only settled spans bound the window).
pub fn rebuild_window(states: &[StateSpan], cuts: &[u64], log_from_ms: u64, log_to_ms: u64, params: &Params) -> Window {
    let log_to_ms = log_to_ms.max(log_from_ms);

    // what the log can change: the spans it falls in (or the last one before it and the first one after, when it
    // falls between them), whose ends move when its frames join them; a cut point counts only if the samples it
    // looked at are clear of that
    let margin = cut_margin_ms(params);
    let first_touched = states.partition_point(|s| s.start_ms <= log_from_ms).checked_sub(1);
    let a = first_touched.map_or(log_from_ms, |i| states[i].start_ms.min(log_from_ms));
    let last_touched = states.partition_point(|s| s.end_ms <= log_to_ms);
    let b = states.get(last_touched).map_or(log_to_ms, |s| s.end_ms.max(log_to_ms));
    let cut_from = cuts.partition_point(|&c| c.saturating_add(margin) <= a).checked_sub(1).map(|i| cuts[i]);
    let cut_to = cuts.get(cuts.partition_point(|&c| c < b.saturating_add(margin))).copied();

    // the last settled span that ends at or before the log starts, and the first that starts at or after it ends;
    // looked for only between the cut points (no settled span is inside one's reach), so the walk is short even when
    // the car hasn't slept for months
    let before = states.partition_point(|s| s.end_ms <= log_from_ms);
    let lo = cut_from.map_or(0, |c| states.partition_point(|s| s.start_ms < c)).min(before);
    let settled_from = states[lo..before].iter().rev().find(|s| is_settled(s, params)).map(|s| s.start_ms);
    let after = states.partition_point(|s| s.start_ms < log_to_ms);
    let hi = cut_to.map_or(states.len(), |c| states.partition_point(|s| s.end_ms <= c)).max(after);
    let settled_to = states[after..hi].iter().find(|s| is_settled(s, params)).map(|s| s.end_ms);

    Window { from_ms: settled_from.or(cut_from).unwrap_or(0), to_ms: settled_to.or(cut_to) }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use model::{Source, State};

    fn spans(list: &[(State, u64, u64)]) -> Vec<StateSpan> {
        list.iter()
            .map(|&(state, a, b)| StateSpan {
                car: 1,
                state,
                start_ms: a * 60_000,
                end_ms: b * 60_000,
                buses: Vec::new(),
                source: Source::Live,
                inputs: model::Inputs::SMO,
            })
            .collect()
    }

    const M: u64 = 60_000;

    #[test]
    fn back_to_the_last_settled_span_and_on_to_the_next() {
        let s = spans(&[
            (State::Online, 0, 30),
            (State::Asleep, 30, 90),
            (State::Online, 90, 120),
            (State::Unknown, 120, 125), // five minutes: not settled, a drive may go on across it
            (State::Online, 125, 150),
            (State::Unknown, 150, 200), // long: settled
            (State::Online, 200, 220),
        ]);
        let p = Params::default();
        assert_eq!(rebuild_window(&s, &[], 130 * M, 131 * M, &p), Window { from_ms: 30 * M, to_ms: Some(200 * M) });
        // a log inside a settled span: that span may stop being one, so both neighbours are taken
        assert_eq!(rebuild_window(&s, &[], 160 * M, 161 * M, &p), Window { from_ms: 30 * M, to_ms: None });
        // before anything settled, after everything
        assert_eq!(rebuild_window(&s, &[], 10 * M, 11 * M, &p), Window { from_ms: 0, to_ms: Some(90 * M) });
        assert_eq!(rebuild_window(&s, &[], 230 * M, 231 * M, &p), Window { from_ms: 150 * M, to_ms: None });
        assert_eq!(rebuild_window(&[], &[], 5, 6, &p), Window { from_ms: 0, to_ms: None });
    }

    #[test]
    fn idle_cut_points_bound_the_window_where_nothing_settled() {
        let p = Params::default();
        // online all along, split at cut points every five minutes, but for an unknown minute (a log not here yet)
        let s = spans(&[
            (State::Online, 0, 5),
            (State::Online, 5, 10),
            (State::Online, 10, 14),
            (State::Unknown, 14, 15),
            (State::Online, 15, 20),
            (State::Online, 20, 25),
            (State::Online, 25, 30),
        ]);
        let cuts = [5 * M, 10 * M, 20 * M, 25 * M];
        // the late log fills the unknown minute: from the last cut point a margin before it (the online span before,
        // whose end moves, starts there), to the first a margin after the span after it (whose start moves)
        assert_eq!(rebuild_window(&s, &cuts, 14 * M, 15 * M, &p), Window { from_ms: 10 * M, to_ms: Some(25 * M) });
        // a cut point closer to the log than the margin is not used: the samples it looked at may change
        assert_eq!(rebuild_window(&s, &cuts, 11 * M, 12 * M, &p), Window { from_ms: 5 * M, to_ms: Some(20 * M) });
        // a cut point and a settled span: the nearer one wins on each side
        let mut with_sleep = s.clone();
        with_sleep.push(spans(&[(State::Asleep, 30, 90)])[0].clone());
        assert_eq!(
            rebuild_window(&with_sleep, &cuts, 26 * M, 27 * M, &p),
            Window { from_ms: 20 * M, to_ms: Some(90 * M) }
        );
        // a new log after everything: from the last cut point clear of the last span
        assert_eq!(rebuild_window(&s, &cuts, 31 * M, 32 * M, &p), Window { from_ms: 20 * M, to_ms: None });
    }
}
