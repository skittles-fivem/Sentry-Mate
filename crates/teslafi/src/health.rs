//! How old TeslaFi's data is, and whose problem it is when it is old.
//!
//! The poller reads TeslaFi every few seconds to minutes without errors, so a connection that "works" says nothing
//! about the data: TeslaFi's polls of the car carry TeslaFi's own time (`Date`), the time of its last data from the
//! car, and every value read from it is dated by that time (never by when this server read it). Two things make that
//! time old while the connection is fine, and each is shown to the admins as a notice that names whose problem it is:
//!
//! - **TeslaFi has no new data** ([`DataHealth::NoData`]): the car is awake as TeslaFi's newest poll says, and no poll
//!   TeslaFi had not given before has come for [`NO_DATA_AFTER_MS`] (2 hours). TeslaFi writes a poll about every
//!   minute while a car is awake, and a car falling asleep leaves a gap of under two hours (the history treats a
//!   TeslaFi gap that short as covered), so a longer silence from an awake car is TeslaFi not hearing from the car:
//!   the problem is on TeslaFi's side or between TeslaFi and the car, not here. A car TeslaFi says is asleep or
//!   offline is silent as expected, however long: no notice.
//! - **TeslaFi's times are read in the wrong zone** ([`DataHealth::TimeOffset`]): polls keep coming (a new one in the
//!   last [`FRESH_MS`], 10 minutes), and the two newest were each dated as far before (or after) the moment they were
//!   read, within [`STEADY_SLACK_MS`] (TeslaFi's delay), at least [`TIME_OFFSET_MIN_MS`] (1 hour) and close to a
//!   whole quarter of an hour (zones differ by whole hours, a few by half or three quarters). TeslaFi writes local
//!   time without a zone (`time_zone` of the account, else the server's default), so a zone read wrong shifts every
//!   poll by the same whole offset, while TeslaFi's time moves on with the clock. Data that is just old (a parked
//!   car TeslaFi heard from hours ago) does not keep a steady distance from the clock, nor one of whole hours: no
//!   notice. After a change of the account's zone nothing is said until two polls have been read in the new one. A
//!   zone that differs by under an hour is not seen.

use crate::poller::AccountStatus;

/// An awake car with no new poll from TeslaFi for this long: TeslaFi has had no new data from the car (2 hours).
pub const NO_DATA_AFTER_MS: u64 = 2 * 3_600_000;
/// A poll this recent (this server's clock) is "polls keep coming" (10 minutes).
pub const FRESH_MS: u64 = 10 * 60_000;
/// A fresh poll dated this far from the moment it was read: the account's time zone is wrong (1 hour).
pub const TIME_OFFSET_MIN_MS: u64 = 3_600_000;
/// Two polls dated this close to the same distance from the moment each was read keep a steady distance (5 minutes:
/// TeslaFi's delay varies by a few), and a distance this close to a whole quarter of an hour is a zone's.
pub const STEADY_SLACK_MS: u64 = 5 * 60_000;
const QUARTER_HOUR_MS: u64 = 15 * 60_000;

/// What the age of TeslaFi's data says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataHealth {
    /// Nothing to say: the data is current, the car sleeps (as TeslaFi says), or there is no poll yet.
    Fine,
    /// TeslaFi has had no new data from the (awake) car for `age_ms`; its last data is from `last_data_ms` (TeslaFi's
    /// own time).
    NoData { age_ms: u64, last_data_ms: u64 },
    /// New polls come, but TeslaFi's times are `skew_ms` before (`behind`) or after the moment they were read.
    TimeOffset { skew_ms: u64, behind: bool },
}

/// Judges an account's data at `now_ms`. Paused accounts, accounts that are not read (a refused token, errors, TeslaFi
/// limiting) and cars not matched are not judged: they have their own notices.
pub fn data_health(st: &AccountStatus, now_ms: u64) -> DataHealth {
    use crate::AccountState;
    if st.state != AccountState::Ok || st.car.is_none() || st.rate_limited_until_ms.is_some() {
        return DataHealth::Fine;
    }
    let Some(data_ms) = st.last_data_ms else { return DataHealth::Fine };
    // when a poll TeslaFi had not given before last came, on this server's clock (before the first one after a
    // restart that is not known: TeslaFi's own time stands for it)
    let seen_ms = st.last_new_ms;
    if seen_ms.is_some_and(|s| now_ms.saturating_sub(s) <= FRESH_MS) {
        let Some(offset) = st.steady_offset_ms else { return DataHealth::Fine };
        let skew = offset.unsigned_abs();
        let off_grid = (skew % QUARTER_HOUR_MS).min(QUARTER_HOUR_MS - skew % QUARTER_HOUR_MS);
        return if skew >= TIME_OFFSET_MIN_MS && off_grid <= STEADY_SLACK_MS {
            DataHealth::TimeOffset { skew_ms: skew, behind: offset > 0 }
        } else {
            DataHealth::Fine
        };
    }
    let age = now_ms.saturating_sub(seen_ms.unwrap_or(data_ms));
    if st.awake && age >= NO_DATA_AFTER_MS {
        return DataHealth::NoData { age_ms: age, last_data_ms: data_ms };
    }
    DataHealth::Fine
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poller::Mode;
    use crate::store::AccountState;

    const NOW: u64 = 1_800_000_000_000;
    const MIN: u64 = 60_000;
    const HOUR: u64 = 3_600_000;

    fn status() -> AccountStatus {
        AccountStatus {
            account: "fa1".into(),
            state: AccountState::Ok,
            mode: Mode::Idle,
            vehicle: Some("fv1".into()),
            car: Some(7),
            car_state: Some("Idling".into()),
            polls_today: 0,
            rate_limited_until_ms: None,
            last_request_ms: Some(NOW),
            next_request_ms: None,
            last_data_ms: Some(NOW - MIN),
            last_new_ms: Some(NOW - MIN),
            steady_offset_ms: Some(MIN as i64),
            awake: true,
            last_ok_ms: Some(NOW),
            last_error: None,
            tls: None,
        }
    }

    #[test]
    fn current_data_is_fine() {
        assert_eq!(data_health(&status(), NOW), DataHealth::Fine);
    }

    #[test]
    fn an_awake_car_with_no_new_poll_for_two_hours_is_teslafis_silence() {
        let mut s = status();
        s.last_data_ms = Some(NOW - 6 * HOUR);
        s.last_new_ms = Some(NOW - 6 * HOUR + MIN);
        assert_eq!(
            data_health(&s, NOW),
            DataHealth::NoData { age_ms: 6 * HOUR - MIN, last_data_ms: NOW - 6 * HOUR },
            "the age is from TeslaFi's data, the connection polling every 15 s does not renew it"
        );
        // just under the threshold: nothing yet
        s.last_data_ms = Some(NOW - 2 * HOUR + 2 * MIN);
        s.last_new_ms = Some(NOW - 2 * HOUR + MIN);
        assert_eq!(data_health(&s, NOW), DataHealth::Fine);
        // a restart forgot when the poll came: TeslaFi's own time stands for it
        s.last_new_ms = None;
        s.last_data_ms = Some(NOW - 3 * HOUR);
        assert_eq!(data_health(&s, NOW), DataHealth::NoData { age_ms: 3 * HOUR, last_data_ms: NOW - 3 * HOUR });
    }

    #[test]
    fn a_car_teslafi_says_sleeps_is_silent_as_expected() {
        let mut s = status();
        s.awake = false;
        s.mode = Mode::Asleep;
        s.last_data_ms = Some(NOW - 30 * HOUR);
        s.last_new_ms = Some(NOW - 30 * HOUR);
        assert_eq!(data_health(&s, NOW), DataHealth::Fine);
    }

    #[test]
    fn polls_that_come_fresh_but_are_dated_hours_off_are_a_wrong_zone() {
        let mut s = status();
        // read a minute ago, dated six hours before it, as the poll before: the zone is read six hours behind
        s.last_new_ms = Some(NOW - MIN);
        s.last_data_ms = Some(NOW - MIN - 6 * HOUR);
        s.steady_offset_ms = Some((6 * HOUR + 2 * MIN) as i64);
        assert_eq!(data_health(&s, NOW), DataHealth::TimeOffset { skew_ms: 6 * HOUR + 2 * MIN, behind: true });
        // dated hours after the moment it was read: read ahead
        s.steady_offset_ms = Some(-((5 * HOUR) as i64));
        assert_eq!(data_health(&s, NOW), DataHealth::TimeOffset { skew_ms: 5 * HOUR, behind: false });
        // a half-hour zone
        s.steady_offset_ms = Some((5 * HOUR + 30 * MIN + MIN) as i64);
        assert_eq!(data_health(&s, NOW), DataHealth::TimeOffset { skew_ms: 5 * HOUR + 31 * MIN, behind: true });
        // minutes apart is just TeslaFi's delay
        s.steady_offset_ms = Some((20 * MIN) as i64);
        assert_eq!(data_health(&s, NOW), DataHealth::Fine);
        // not said while the car sleeps either: a fresh poll is a fresh poll
        s.awake = false;
        s.steady_offset_ms = Some((6 * HOUR) as i64);
        assert_eq!(data_health(&s, NOW), DataHealth::TimeOffset { skew_ms: 6 * HOUR, behind: true });
    }

    #[test]
    fn old_data_that_keeps_no_steady_whole_offset_is_no_wrong_zone() {
        let mut s = status();
        s.awake = false;
        s.last_new_ms = Some(NOW - MIN);
        s.last_data_ms = Some(NOW - 6 * HOUR - 7 * MIN);
        // the two newest polls were not dated alike (a parked car's old data, TeslaFi catching up): nothing to say
        s.steady_offset_ms = None;
        assert_eq!(data_health(&s, NOW), DataHealth::Fine);
        // steady, but far from any zone's offset
        s.steady_offset_ms = Some((6 * HOUR + 7 * MIN) as i64);
        assert_eq!(data_health(&s, NOW), DataHealth::Fine);
    }

    #[test]
    fn an_account_not_read_has_its_own_notices() {
        let mut s = status();
        s.last_data_ms = Some(NOW - 6 * HOUR);
        s.last_new_ms = Some(NOW - 6 * HOUR);
        s.state = AccountState::Refused;
        assert_eq!(data_health(&s, NOW), DataHealth::Fine);
        s.state = AccountState::Ok;
        s.rate_limited_until_ms = Some(NOW + HOUR);
        assert_eq!(data_health(&s, NOW), DataHealth::Fine);
        s.rate_limited_until_ms = None;
        s.car = None;
        assert_eq!(data_health(&s, NOW), DataHealth::Fine);
    }
}
