//! Which window of a car's timeline to rebuild when a log arrives, in order or late.
//!
//! A log can only change what lies between the settled spans around it ([`is_settled`]): back to the last asleep (or
//! long unknown) span that ended before the log, forward to the first that starts after it. The window runs from the
//! start of the one to the end of the other, so both are rebuilt whole and nothing outside changes: the caller
//! replaces the states, drives, charges, positions and charge points in `[from_ms, to_ms)` with [`crate::build`]'s.
//!
//! It looks only at the spans near the log (two binary searches and a walk to the nearest settled spans), so a car
//! with years of history costs no more than a new one, as long as it sleeps now and then.

use crate::states::is_settled;
use crate::timeline::Params;
use model::StateSpan;

/// A window to rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// From here (0: no settled span before the log, from the car's first data).
    pub from_ms: u64,
    /// To here; `None`: no settled span after the log, to the newest data (now).
    pub to_ms: Option<u64>,
}

/// The window to rebuild for a log whose data spans `[log_from_ms, log_to_ms]` (its frames and its own start and end:
/// [`crate::LogFacts::span`]), given the car's existing state spans around it, sorted by start.
pub fn rebuild_window(states: &[StateSpan], log_from_ms: u64, log_to_ms: u64, params: &Params) -> Window {
    let log_to_ms = log_to_ms.max(log_from_ms);
    // the last settled span that ends at or before the log starts
    let before = states.partition_point(|s| s.end_ms <= log_from_ms);
    let from_ms = states[..before].iter().rev().find(|s| is_settled(s, params)).map_or(0, |s| s.start_ms);
    // the first settled span that starts at or after the log ends
    let after = states.partition_point(|s| s.start_ms < log_to_ms);
    let to_ms = states[after..].iter().find(|s| is_settled(s, params)).map(|s| s.end_ms);
    Window { from_ms, to_ms }
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
                source: Source::Can,
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
        assert_eq!(rebuild_window(&s, 130 * M, 131 * M, &p), Window { from_ms: 30 * M, to_ms: Some(200 * M) });
        // a log inside a settled span: that span may stop being one, so both neighbours are taken
        assert_eq!(rebuild_window(&s, 160 * M, 161 * M, &p), Window { from_ms: 30 * M, to_ms: None });
        // before anything settled, after everything
        assert_eq!(rebuild_window(&s, 10 * M, 11 * M, &p), Window { from_ms: 0, to_ms: Some(90 * M) });
        assert_eq!(rebuild_window(&s, 230 * M, 231 * M, &p), Window { from_ms: 150 * M, to_ms: None });
        assert_eq!(rebuild_window(&[], 5, 6, &p), Window { from_ms: 0, to_ms: None });
    }
}
