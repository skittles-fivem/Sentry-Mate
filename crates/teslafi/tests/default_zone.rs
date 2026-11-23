//! TeslaFi writes each poll's time in the account's wall time. An account with no zone of its own is read in the
//! server's default zone, and that is the zone an admin saved, not the one the server started in: read in UTC, a
//! poll a car in Chicago just made looks 6 h old (in winter), and so do its lock and windows. Made-up data only.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{MemSink, vault};
use std::collections::BTreeMap;
use std::sync::Arc;
use teslafi::{Options, Service};

#[test]
fn the_default_zone_follows_the_saved_one() {
    let (_t, data, v) = vault();
    let s = Service::new(&data, v, Arc::new(MemSink::default()), Options::default());
    assert_eq!(s.default_time_zone(), chrono_tz::Tz::UTC, "the zone the server started in");
    s.set_default_time_zone("America/Chicago");
    assert_eq!(s.default_time_zone(), chrono_tz::Tz::America__Chicago);
    s.set_default_time_zone("Mars/Base");
    assert_eq!(s.default_time_zone(), chrono_tz::Tz::America__Chicago, "a zone that is none is not taken");
}

#[test]
fn a_poll_read_in_the_wrong_zone_is_hours_off() {
    let fields: BTreeMap<String, String> = [("Date", "2026-11-23 08:00:00"), ("fd_window", "0"), ("locked", "1")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let utc = import::teslafi::feed::read(&fields, chrono_tz::Tz::UTC, None).unwrap();
    let chicago = import::teslafi::feed::read(&fields, chrono_tz::Tz::America__Chicago, None).unwrap();
    assert_eq!(chicago.t_ms - utc.t_ms, 6 * 3_600_000);
    assert_eq!(chicago.off_can.windows[0], Some(false), "fd_window: the driver front window");
    assert_eq!(chicago.off_can.locked, Some(true));
}
