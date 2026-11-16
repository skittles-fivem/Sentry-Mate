#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

mod support;

use model::{CarId, Place, PlaceKind};
use places::{PlaceIndex, Spot, SuggestOptions, Suggester, distance_m, suggest};
use std::time::Instant;
use support::{M_PER_DEG, Rng};

fn spot(lat: f64, lon: f64, t_ms: u64, car: CarId) -> Spot {
    Spot { lat, lon, t_ms, car }
}

fn saved(id: u32, lat: f64, lon: f64, r: f64) -> Place {
    Place { id, name: "Saved".into(), kind: PlaceKind::Home, lat, lon, radius_m: r, price: None }
}

#[test]
fn busiest_spots_not_yet_saved_come_first() {
    let home = saved(1, 30.0, 30.0, 200.0);
    let ix = PlaceIndex::new([home]);
    let mut spots = Vec::new();
    let mut t = 0;
    let mut add = |lat: f64, lon: f64, n: usize, spots: &mut Vec<Spot>| {
        for i in 0..n {
            t += 1;
            // Within ~30 m of the spot.
            spots.push(spot(lat + (i as f64 % 3.0 - 1.0) * 0.0002, lon, t, 1));
        }
    };
    add(30.0, 30.0, 6, &mut spots); // at home: saved already
    add(31.0, 31.0, 5, &mut spots); // work
    add(32.0, 32.0, 3, &mut spots); // a charger
    add(33.0, 33.0, 1, &mut spots); // once only
    let out = suggest(spots.clone(), &ix, SuggestOptions::default());
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].visits, 5);
    assert!(distance_m(out[0].lat, out[0].lon, 31.0, 31.0) < 30.0);
    assert_eq!(out[1].visits, 3);
    assert_eq!(out[0].cars, vec![1]);

    let mut s = Suggester::new(&ix, SuggestOptions::default());
    spots.iter().for_each(|p| s.add(p));
    assert_eq!(s.at_saved_places(), 6);
    assert_eq!(s.invalid(), 0);

    let one = suggest(spots, &ix, SuggestOptions { max: 1, ..SuggestOptions::default() });
    assert_eq!(one.len(), 1);
}

#[test]
fn equal_visits_most_recent_first() {
    let ix = PlaceIndex::new([]);
    let spots = vec![spot(1.0, 1.0, 10, 1), spot(1.0, 1.0, 11, 1), spot(2.0, 2.0, 50, 2), spot(2.0, 2.0, 51, 2)];
    let out = suggest(spots, &ix, SuggestOptions::default());
    assert_eq!(out.len(), 2);
    assert_eq!((out[0].lat, out[0].last_ms, out[0].first_ms), (2.0, 51, 50));
    assert_eq!(out[1].lat, 1.0);
}

#[test]
fn groups_are_150_m_around_their_middle() {
    let ix = PlaceIndex::new([]);
    let one = SuggestOptions { min_visits: 1, ..SuggestOptions::default() };
    let near = suggest([spot(0.0, 0.0, 1, 1), spot(140.0 / M_PER_DEG, 0.0, 2, 1)], &ix, one);
    assert_eq!(near.len(), 1);
    assert_eq!(near[0].visits, 2);
    let far = suggest([spot(0.0, 0.0, 1, 1), spot(160.0 / M_PER_DEG, 0.0, 2, 1)], &ix, one);
    assert_eq!(far.len(), 2);
    // The middle follows the group: a stop 140 m beyond the first is near the new middle (70 m away) and joins.
    let chain = suggest(
        [spot(0.0, 0.0, 3, 1), spot(140.0 / M_PER_DEG, 0.0, 2, 1), spot(210.0 / M_PER_DEG, 0.0, 1, 1)],
        &ix,
        one,
    );
    assert_eq!(chain.len(), 1);
    assert_eq!(chain[0].visits, 3);
}

#[test]
fn a_group_across_the_antimeridian_stays_there() {
    let ix = PlaceIndex::new([]);
    let out = suggest(
        [spot(5.0, 179.9996, 1, 1), spot(5.0, -179.9996, 2, 2), spot(5.0, 179.9998, 3, 1)],
        &ix,
        SuggestOptions::default(),
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].visits, 3);
    assert!(out[0].lon.abs() > 179.99, "middle {}", out[0].lon);
    assert_eq!(out[0].cars, vec![1, 2]);
}

#[test]
fn bad_positions_and_middles_inside_saved_places_are_left_out() {
    let ix = PlaceIndex::new([saved(1, 0.0, 0.0, 100.0)]);
    let mut s = Suggester::new(&ix, SuggestOptions::default());
    s.add(&spot(f64::NAN, 0.0, 1, 1));
    s.add(&spot(95.0, 0.0, 1, 1));
    // Two stops just outside the place on either side: their middle is inside it.
    s.add(&spot(110.0 / M_PER_DEG, 0.0, 2, 1));
    s.add(&spot(-110.0 / M_PER_DEG, 0.0, 3, 1));
    assert_eq!(s.invalid(), 2);
    assert!(s.finish().is_empty());
}

/// 2,000 cars, a year of stops each fed one at a time (never a list of every stop in memory): each car's home and
/// work, a fast charger every car uses, and stops scattered elsewhere. The shared charger comes out on top with
/// every car in it; the build takes well under a second per hundred thousand stops.
#[test]
fn two_thousand_cars_streamed() {
    const CARS: u32 = 2_000;
    let charger = (40.0, -100.0);
    let ix = PlaceIndex::new([]);
    let mut s = Suggester::new(&ix, SuggestOptions { max: 10, ..SuggestOptions::default() });
    let mut rng = Rng(0xCA25);
    let t = Instant::now();
    let mut stops = 0u64;
    for car in 0..CARS {
        let home = (rng.range(35.0, 45.0), rng.range(-110.0, -90.0));
        let work = (home.0 + rng.range(-0.2, 0.2), home.1 + rng.range(-0.2, 0.2));
        for day in 0..60u64 {
            let t_ms = day * 86_400_000 + u64::from(car);
            let jitter = |r: &mut Rng| r.range(-0.0003, 0.0003);
            for (lat, lon) in [home, work] {
                s.add(&spot(lat + jitter(&mut rng), lon + jitter(&mut rng), t_ms, car));
                stops += 1;
            }
            if day % 30 == 0 {
                s.add(&spot(charger.0 + jitter(&mut rng), charger.1 + jitter(&mut rng), t_ms, car));
                s.add(&spot(rng.range(30.0, 50.0), rng.range(-120.0, -80.0), t_ms, car));
                stops += 2;
            }
        }
    }
    let out = s.finish();
    let took = t.elapsed();
    println!("{CARS} cars, {stops} stops: suggestions in {took:?}");
    assert_eq!(out[0].visits, (CARS * 2) as usize);
    assert_eq!(out[0].cars.len(), CARS as usize);
    assert!(distance_m(out[0].lat, out[0].lon, charger.0, charger.1) < 20.0);
    assert_eq!(out.len(), 10);
    assert!(out[1..].iter().all(|g| g.cars.len() == 1 && g.visits >= 60));
    assert!(took.as_secs() < 30, "{took:?}");

    // One car's suggestions come from its own stops only (its home and work, 60 visits each).
    let mut one = Suggester::new(&ix, SuggestOptions::default());
    let mut rng = Rng(0xCA25);
    let home = (rng.range(35.0, 45.0), rng.range(-110.0, -90.0));
    for day in 0..60u64 {
        one.add(&spot(home.0, home.1, day, 0));
    }
    let mine = one.finish();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].visits, 60);
}
