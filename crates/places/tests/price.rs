#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use chrono::{TimeZone, Utc};
use chrono_tz::Tz;
use model::{ChargePoint, PricePlan, Rate};
use places::price::{MAX_RATES, parse_time_zone};
use places::{CostOptions, PriceError, Tariff, validate_plan};
use std::time::Instant;

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> u64 {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap().timestamp_millis() as u64
}

fn local(tz: Tz, y: i32, mo: u32, d: u32, h: u32, mi: u32) -> u64 {
    tz.with_ymd_and_hms(y, mo, d, h, mi, 0).single().unwrap().timestamp_millis() as u64
}

/// Constant power from `start` to `end`, a point every `step` ms (both ends included).
fn flat(start: u64, end: u64, step: u64, kw: f64) -> Vec<ChargePoint> {
    (0..=(end - start) / step).map(|i| point(start + i * step, kw)).collect()
}

fn point(t_ms: u64, kw: f64) -> ChargePoint {
    ChargePoint { t_ms, power_kw: Some(kw), ..ChargePoint::default() }
}

fn rate(name: &str, per_kwh: f64, days: &[u8], from: &str, to: &str) -> Rate {
    Rate { name: name.into(), per_kwh, days: days.to_vec(), from: from.into(), to: to.into() }
}

fn plan(per_kwh: f64, rates: Vec<Rate>) -> PricePlan {
    PricePlan { currency: "USD".into(), per_kwh, rates, session_fee: None }
}

const ALL: &[u8] = &[0, 1, 2, 3, 4, 5, 6];
const HOUR: u64 = 3_600_000;

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

#[test]
fn one_price() {
    let t = Tariff::new(&plan(0.25, vec![]), "America/Chicago").unwrap();
    let start = utc(2026, 11, 16, 1, 0);
    let c = t.charge_cost(&flat(start, start + 2 * HOUR, 1000, 11.0), &CostOptions::default());
    assert!(close(c.kwh, 22.0), "{}", c.kwh);
    assert!(close(c.total, 5.5));
    assert_eq!(c.parts.len(), 1);
    assert_eq!(c.parts[0].rate, None);
    assert_eq!(c.currency, "USD");
    assert_eq!(c.gap_ms, 0);
    assert_eq!(t.rate_name_at(start), None);
    assert_eq!(t.price_at(start), 0.25);
}

#[test]
fn energy_is_split_exactly_at_a_rate_change() {
    // Day rate from 07:00; power ramps from 0 to 12 kW between 06:59 and 07:01 (one stretch, no point at 07:00).
    let t = Tariff::new(&plan(0.10, vec![rate("Day", 0.30, ALL, "07:00", "19:00")]), "UTC").unwrap();
    let pts = [point(utc(2026, 11, 16, 6, 59), 0.0), point(utc(2026, 11, 16, 7, 1), 12.0)];
    let c = t.charge_cost(&pts, &CostOptions::default());
    let day = c.parts.iter().find(|p| p.rate.as_deref() == Some("Day")).unwrap();
    let other = c.parts.iter().find(|p| p.rate.is_none()).unwrap();
    // Before 07:00 the average is 3 kW for a minute, after it 9 kW for a minute.
    assert!(close(other.kwh, 0.05), "{}", other.kwh);
    assert!(close(day.kwh, 0.15), "{}", day.kwh);
    assert!(close(c.total, 0.05 * 0.10 + 0.15 * 0.30));
    // The parts come in plan order, the plan's own price last.
    assert_eq!(c.parts[0].rate.as_deref(), Some("Day"));
}

#[test]
fn a_night_rate_runs_past_midnight_and_belongs_to_the_day_it_starts() {
    // Monday nights only: Monday 21:00 to Tuesday 07:00. 2026-11-16 is a Monday.
    let t = Tariff::new(&plan(0.20, vec![rate("Night", 0.08, &[0], "21:00", "07:00")]), "UTC").unwrap();
    assert_eq!(t.rate_name_at(utc(2026, 11, 16, 21, 0)), Some("Night"));
    assert_eq!(t.rate_name_at(utc(2026, 11, 17, 3, 0)), Some("Night")); // Tuesday early: Monday's night
    assert_eq!(t.rate_name_at(utc(2026, 11, 17, 7, 0)), None);
    assert_eq!(t.rate_name_at(utc(2026, 11, 17, 22, 0)), None); // Tuesday night isn't on the plan
    assert_eq!(t.rate_name_at(utc(2026, 11, 16, 3, 0)), None); // Monday early is Sunday's night
    let c =
        t.charge_cost(&flat(utc(2026, 11, 16, 20, 0), utc(2026, 11, 17, 8, 0), 1000, 10.0), &CostOptions::default());
    assert!(close(c.kwh, 120.0));
    assert!(close(c.parts[0].kwh, 100.0) && close(c.parts[1].kwh, 20.0));
    assert!(close(c.total, 100.0 * 0.08 + 20.0 * 0.20), "{}", c.total);
}

#[test]
fn first_matching_rate_wins() {
    let p =
        plan(0.30, vec![rate("Weekend", 0.12, &[5, 6], "00:00", "23:59"), rate("Night", 0.15, ALL, "22:00", "06:00")]);
    let t = Tariff::new(&p, "UTC").unwrap();
    // Saturday 2026-11-21 23:00: both match, the first listed wins.
    assert_eq!(t.rate_name_at(utc(2026, 11, 21, 23, 0)), Some("Weekend"));
    assert_eq!(t.rate_name_at(utc(2026, 11, 20, 23, 0)), Some("Night"));
    assert_eq!(t.price_at(utc(2026, 11, 20, 12, 0)), 0.30);
}

/// Chicago, spring forward (2026-03-08, 02:00 → 03:00): the night rate 00:00-06:00 lasts five real hours.
#[test]
fn dst_spring_forward_in_chicago() {
    let tz: Tz = "America/Chicago".parse().unwrap();
    let t = Tariff::with_tz(&plan(0.20, vec![rate("Night", 0.10, ALL, "00:00", "06:00")]), tz).unwrap();
    let start = local(tz, 2026, 3, 7, 22, 0);
    let end = local(tz, 2026, 3, 8, 8, 0);
    assert_eq!((start, end), (utc(2026, 3, 8, 4, 0), utc(2026, 3, 8, 13, 0)));
    let c = t.charge_cost(&flat(start, end, 1000, 6.0), &CostOptions::default());
    assert!(close(c.kwh, 54.0), "{}", c.kwh);
    assert!(close(c.parts[0].kwh, 30.0), "night {}", c.parts[0].kwh);
    assert!(close(c.parts[1].kwh, 24.0));
    assert!(close(c.total, 3.0 + 4.8));
    assert_eq!(t.rate_name_at(utc(2026, 3, 8, 10, 59)), Some("Night")); // 05:59 CDT
    assert_eq!(t.rate_name_at(utc(2026, 3, 8, 11, 0)), None); // 06:00 CDT
}

/// Chicago, fall back (2026-11-01, 02:00 → 01:00): the night rate lasts seven real hours.
#[test]
fn dst_fall_back_in_chicago() {
    let tz: Tz = "America/Chicago".parse().unwrap();
    let t = Tariff::with_tz(&plan(0.20, vec![rate("Night", 0.10, ALL, "00:00", "06:00")]), tz).unwrap();
    let start = local(tz, 2026, 10, 31, 22, 0);
    assert_eq!(start, utc(2026, 11, 1, 3, 0));
    let end = utc(2026, 11, 1, 14, 0); // 08:00 CST
    let c = t.charge_cost(&flat(start, end, 1000, 6.0), &CostOptions::default());
    assert!(close(c.kwh, 66.0));
    assert!(close(c.parts[0].kwh, 42.0), "night {}", c.parts[0].kwh);
    assert!(close(c.total, 4.2 + 4.8));
}

/// Berlin: a rate for 02:00-03:00 never happens on the spring day (clocks jump from 02:00 to 03:00) and happens
/// twice on the autumn day (03:00 back to 02:00).
#[test]
fn dst_in_berlin_skipped_and_repeated_hours() {
    let tz: Tz = "Europe/Berlin".parse().unwrap();
    let t = Tariff::with_tz(&plan(0.30, vec![rate("Late", 0.50, ALL, "02:00", "03:00")]), tz).unwrap();
    // 2026-03-29, 01:00 CET to 04:00 CEST: two real hours, none of them 02:xx.
    let spring =
        t.charge_cost(&flat(utc(2026, 3, 29, 0, 0), utc(2026, 3, 29, 2, 0), 1000, 3.0), &CostOptions::default());
    assert!(close(spring.kwh, 6.0));
    assert_eq!(spring.parts.len(), 1);
    assert!(close(spring.total, 1.8));
    // 2026-10-25, 01:00 CEST to 04:00 CET: four real hours, two of them 02:xx.
    let start = local(tz, 2026, 10, 25, 1, 0);
    assert_eq!(start, utc(2026, 10, 24, 23, 0));
    let autumn = t.charge_cost(&flat(start, utc(2026, 10, 25, 3, 0), 1000, 3.0), &CostOptions::default());
    assert!(close(autumn.kwh, 12.0));
    assert!(close(autumn.parts[0].kwh, 6.0), "late {}", autumn.parts[0].kwh);
    assert!(close(autumn.total, 3.0 + 1.8));
}

#[test]
fn session_fee_is_added_once() {
    let mut p = plan(0.40, vec![]);
    p.session_fee = Some(2.5);
    let t = Tariff::new(&p, "Europe/Berlin").unwrap();
    let start = utc(2026, 6, 1, 12, 0);
    let c = t.charge_cost(&flat(start, start + HOUR / 2, 1000, 100.0), &CostOptions::default());
    assert!(close(c.energy_cost, 20.0));
    assert!(close(c.session_fee, 2.5));
    assert!(close(c.total, 22.5));
    let none = t.charge_cost(&[], &CostOptions::default());
    assert!(close(none.total, 2.5) && none.kwh == 0.0 && none.parts.is_empty());
    assert!(close(t.cost_of(10.0, start), 4.0));
}

#[test]
fn grid_energy_is_shared_out_by_the_curve() {
    let t = Tariff::new(&plan(0.10, vec![rate("Day", 0.30, ALL, "07:00", "19:00")]), "UTC").unwrap();
    // 5 kWh before 07:00 and 5 kWh after; the grid gave 11 kWh (10 % losses).
    let pts = flat(utc(2026, 11, 16, 6, 0), utc(2026, 11, 16, 8, 0), 1000, 5.0);
    let c = t.charge_cost(&pts, &CostOptions { grid_kwh: Some(11.0), ..CostOptions::default() });
    assert!(close(c.curve_kwh, 10.0));
    assert!(close(c.kwh, 11.0));
    assert!(close(c.parts[0].kwh, 5.5) && close(c.parts[1].kwh, 5.5));
    assert!(close(c.total, 5.5 * 0.30 + 5.5 * 0.10));
}

#[test]
fn gaps_unsorted_points_and_odd_values() {
    let t = Tariff::new(&plan(1.0, vec![]), "UTC").unwrap();
    let s = utc(2026, 11, 16, 0, 0);
    let mut pts = flat(s, s + HOUR, 60_000, 6.0);
    // A 20-minute hole, then another hour.
    pts.extend(flat(s + HOUR + 20 * 60_000, s + 2 * HOUR + 20 * 60_000, 60_000, 6.0));
    let c = t.charge_cost(&pts, &CostOptions::default());
    assert_eq!(c.gap_ms, 20 * 60_000);
    assert!(close(c.kwh, 12.0), "{}", c.kwh);
    // Order doesn't matter.
    let mut shuffled = pts.clone();
    shuffled.reverse();
    shuffled.swap(3, 40);
    assert_eq!(t.charge_cost(&shuffled, &CostOptions::default()), c);
    // Points without power are skipped; negative power (the pack giving power) costs nothing; of two points at
    // the same moment the later one counts.
    let pts = vec![
        point(s, 6.0),
        ChargePoint { t_ms: s + 30_000, power_kw: None, ..ChargePoint::default() },
        point(s + 60_000, 6.0),
        point(s + 120_000, -50.0),
        point(s + 180_000, 0.0),
        point(s + 180_000, 6.0),
        ChargePoint { t_ms: s + 240_000, power_kw: Some(f64::NAN), ..ChargePoint::default() },
    ];
    let c = t.charge_cost(&pts, &CostOptions::default());
    // 6 kW for a minute (0.1 kWh), a ramp 6 → 0 (0.05 kWh), then a ramp 0 → 6 kW (0.05 kWh).
    assert!(close(c.kwh, 0.1 + 0.05 + 0.05), "{}", c.kwh);
}

#[test]
fn plans_are_checked_and_tidied() {
    let p = PricePlan {
        currency: " eur ".into(),
        per_kwh: 0.123_456_789,
        rates: vec![
            rate("", 0.2, &[3, 1, 1], "22:00", "06:00"),
            rate("A very long name for a rate indeed", 0.1, &[0], "00:00", "05:00"),
        ],
        session_fee: Some(1.234_567_8),
    };
    let ok = validate_plan(&p).unwrap();
    assert_eq!(ok.currency, "EUR");
    assert_eq!(ok.per_kwh, 0.12346);
    assert_eq!(ok.rates[0].name, "Rate 1");
    assert_eq!(ok.rates[0].days, vec![1, 3]);
    assert_eq!(ok.rates[1].name.chars().count(), 24);
    assert_eq!(ok.session_fee, Some(1.23457));

    let with = |f: &dyn Fn(&mut PricePlan)| {
        let mut p = plan(0.2, vec![rate("Peak", 0.4, ALL, "16:00", "21:00")]);
        f(&mut p);
        validate_plan(&p)
    };
    assert_eq!(with(&|_| {}).map(|_| ()), Ok(()));
    assert_eq!(with(&|p| p.currency = "US".into()), Err(PriceError::Currency));
    assert_eq!(with(&|p| p.currency = "U$D".into()), Err(PriceError::Currency));
    assert!(matches!(with(&|p| p.per_kwh = 10.5), Err(PriceError::Price { .. })));
    assert!(matches!(with(&|p| p.per_kwh = -0.01), Err(PriceError::Price { .. })));
    assert!(matches!(with(&|p| p.per_kwh = f64::NAN), Err(PriceError::Price { .. })));
    assert_eq!(with(&|p| p.per_kwh = 10.0).map(|p| p.per_kwh), Ok(10.0));
    assert!(matches!(with(&|p| p.rates[0].per_kwh = 11.0), Err(PriceError::Price { what }) if what == "Peak"));
    assert_eq!(
        with(&|p| p.rates = vec![rate("R", 0.1, ALL, "01:00", "02:00"); MAX_RATES + 1]),
        Err(PriceError::TooManyRates)
    );
    assert!(with(&|p| p.rates = vec![rate("R", 0.1, ALL, "01:00", "02:00"); MAX_RATES]).is_ok());
    for (from, to) in [("7:00", "09:00"), ("24:00", "01:00"), ("07:00", "07:60"), ("07.00", "08:00")] {
        assert_eq!(
            with(&|p| {
                p.rates[0].from = from.into();
                p.rates[0].to = to.into();
            }),
            Err(PriceError::Time { rate: "Peak".into() }),
            "{from}-{to}"
        );
    }
    assert_eq!(with(&|p| p.rates[0].to = "16:00".into()), Err(PriceError::SameTimes { rate: "Peak".into() }));
    assert_eq!(with(&|p| p.rates[0].days = vec![]), Err(PriceError::Days { rate: "Peak".into() }));
    assert_eq!(with(&|p| p.rates[0].days = vec![1, 7]), Err(PriceError::Days { rate: "Peak".into() }));
    assert!(matches!(with(&|p| p.rates[0].name = "<b>x</b>".into()), Err(PriceError::Name { .. })));
    assert_eq!(with(&|p| p.session_fee = Some(-1.0)), Err(PriceError::SessionFee));
    assert_eq!(with(&|p| p.session_fee = Some(1000.5)), Err(PriceError::SessionFee));
    assert!(matches!(Tariff::new(&plan(0.2, vec![]), "Mars/Olympus"), Err(PriceError::TimeZone(_))));
    assert!(parse_time_zone(" Europe/Berlin ").is_ok());
    // Errors read as sentences for the settings form.
    assert_eq!(PriceError::Time { rate: "Peak".into() }.to_string(), "Peak: times are HH:MM, like 07:00 and 21:30.");
}

/// Many cars charging at once, each in its own time zone, priced with tariffs shared between threads.
#[test]
fn two_thousand_cars_priced_in_parallel() {
    let zones = ["America/Chicago", "Europe/Berlin", "Asia/Kathmandu", "Australia/Adelaide", "Pacific/Chatham", "UTC"];
    let p = plan(
        0.25,
        vec![rate("Night", 0.09, ALL, "23:00", "06:00"), rate("Peak", 0.45, &[0, 1, 2, 3, 4], "16:00", "21:00")],
    );
    let tariffs: Vec<Tariff> = zones.iter().map(|z| Tariff::new(&p, z).unwrap()).collect();
    let t0 = Instant::now();
    let totals: Vec<f64> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..8u64)
            .map(|w| {
                let tariffs = &tariffs;
                s.spawn(move || {
                    let mut sum = 0.0;
                    for car in (w..2_000).step_by(8) {
                        let start = utc(2026, 11, 2, 0, 0) + car * 7 * 60_000;
                        let pts = flat(start, start + 3 * HOUR, 10_000, 7.0 + (car % 5) as f64);
                        let c = tariffs[car as usize % tariffs.len()].charge_cost(&pts, &CostOptions::default());
                        assert!(close(c.kwh, 3.0 * (7.0 + (car % 5) as f64)));
                        sum += c.total;
                    }
                    sum
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let took = t0.elapsed();
    println!("2,000 charges (3 h, a point every 10 s) priced in {took:?}");
    assert!(totals.iter().all(|t| *t > 0.0));
    assert!(took.as_secs() < 30);
}
