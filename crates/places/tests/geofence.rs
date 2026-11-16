#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

mod support;

use model::{Place, PlaceId, PlaceKind};
use places::{PlaceIndex, distance_m};
use std::time::Instant;
use support::{M_PER_DEG, Rng};

fn place(id: PlaceId, lat: f64, lon: f64, radius_m: f64) -> Place {
    Place { id, name: format!("Place {id}"), kind: PlaceKind::Other, lat, lon, radius_m, price: None }
}

/// A point `m` metres north of (lat, lon) (exact on the sphere along a meridian).
fn north(lat: f64, lon: f64, m: f64) -> (f64, f64) {
    (lat + m / M_PER_DEG, lon)
}

#[test]
fn radius_is_clamped_to_50_m_and_2_km() {
    let ix = PlaceIndex::new([place(1, 10.0, 20.0, 10.0), place(2, -30.0, 40.0, 9_000.0)]);
    assert_eq!(ix.get(1).unwrap().radius_m, 50.0);
    assert_eq!(ix.get(2).unwrap().radius_m, 2000.0);
    let (a, b) = north(10.0, 20.0, 49.0);
    assert_eq!(ix.place_at(a, b).map(|p| p.id), Some(1));
    let (a, b) = north(10.0, 20.0, 51.0);
    assert_eq!(ix.place_at(a, b), None);
    let (a, b) = north(-30.0, 40.0, 1_999.0);
    assert_eq!(ix.place_at(a, b).map(|p| p.id), Some(2));
    let (a, b) = north(-30.0, 40.0, 2_001.0);
    assert_eq!(ix.place_at(a, b), None);
}

#[test]
fn the_edge_of_the_radius_counts_as_inside() {
    let ix = PlaceIndex::new([place(1, 0.0, 0.0, 200.0)]);
    let (a, b) = north(0.0, 0.0, 199.999);
    assert!(ix.place_at(a, b).is_some());
    let (a, b) = north(0.0, 0.0, 200.001);
    assert!(ix.place_at(a, b).is_none());
}

#[test]
fn overlapping_places_nearest_centre_wins() {
    // A: 500 m radius; B: 200 m radius, 300 m north of A.
    let (blat, blon) = north(45.0, 7.0, 300.0);
    let ix = PlaceIndex::new([place(1, 45.0, 7.0, 500.0), place(2, blat, blon, 200.0)]);
    // 200 m from A, 100 m from B: both contain it, B is nearer.
    let (a, b) = north(45.0, 7.0, 200.0);
    assert_eq!(ix.place_at(a, b).unwrap().id, 2);
    let both: Vec<PlaceId> = ix.places_at(a, b).iter().map(|(p, _)| p.id).collect();
    assert_eq!(both, vec![2, 1]);
    // 50 m south of A: 350 m from B, outside its radius.
    let (a, b) = north(45.0, 7.0, -50.0);
    assert_eq!(ix.place_at(a, b).unwrap().id, 1);
    // A big place doesn't swallow a smaller one inside it, and a small one doesn't win outside its radius.
    let (a, b) = north(45.0, 7.0, 520.0); // 220 m from B: outside B, outside A
    assert_eq!(ix.place_at(a, b), None);
}

#[test]
fn equal_distance_goes_to_the_one_listed_first() {
    let (n, _) = north(0.0, 50.0, 100.0);
    let (s, _) = north(0.0, 50.0, -100.0);
    let ix = PlaceIndex::new([place(7, n, 50.0, 300.0), place(3, s, 50.0, 300.0)]);
    assert_eq!(ix.place_at(0.0, 50.0).unwrap().id, 7);
    let ix = PlaceIndex::new([place(3, s, 50.0, 300.0), place(7, n, 50.0, 300.0)]);
    assert_eq!(ix.place_at(0.0, 50.0).unwrap().id, 3);
}

#[test]
fn across_the_antimeridian_and_at_the_poles() {
    let ix =
        PlaceIndex::new([place(1, -16.5, 179.999, 300.0), place(2, 89.9995, 0.0, 200.0), place(3, 0.0, -180.0, 100.0)]);
    // ~213 m away, on the other side of the date line.
    assert_eq!(ix.place_at(-16.5, -179.999).unwrap().id, 1);
    assert!(distance_m(-16.5, 179.999, -16.5, -179.999) < 300.0);
    // Over the pole: the same latitude on the opposite meridian is ~111 m away.
    assert_eq!(ix.place_at(89.9995, 180.0).unwrap().id, 2);
    assert_eq!(ix.place_at(89.9995, -90.0).unwrap().id, 2);
    // -180 and 180 are one meridian.
    assert_eq!(ix.place_at(0.0, 180.0).unwrap().id, 3);
    assert_eq!(ix.place_at(0.0005, 179.9995).unwrap().id, 3);
}

#[test]
fn a_filter_limits_places_to_some_cars() {
    let (blat, blon) = north(45.0, 7.0, 50.0);
    let ix = PlaceIndex::new([place(1, 45.0, 7.0, 500.0), place(2, blat, blon, 200.0)]);
    let only_place_1 = |p: &Place| p.id == 1;
    assert_eq!(ix.place_at(blat, blon).unwrap().id, 2);
    assert_eq!(ix.place_at_where(blat, blon, only_place_1).unwrap().id, 1);
    assert_eq!(ix.place_at_where(blat, blon, |_| false), None);
}

#[test]
fn bad_positions_are_nowhere() {
    let ix = PlaceIndex::new([place(1, 0.0, 0.0, 2000.0)]);
    for (a, b) in [(f64::NAN, 0.0), (0.0, f64::INFINITY), (91.0, 0.0), (0.0, 181.0)] {
        assert_eq!(ix.place_at(a, b), None);
        assert!(ix.places_at(a, b).is_empty());
    }
}

#[test]
fn visits_per_place() {
    let ix = PlaceIndex::new([place(1, 10.0, 10.0, 200.0), place(2, 20.0, 20.0, 200.0)]);
    let counts = ix.count_at([(10.0, 10.0), (10.0001, 10.0), (20.0, 20.0), (30.0, 30.0)]);
    assert_eq!(counts[&1], 2);
    assert_eq!(counts[&2], 1);
    assert_eq!(counts.len(), 2);
}

/// Thousands of places (a fleet's chargers, homes and workplaces): lookups give the same answer as checking every
/// place, and stay fast because they look only at the places nearby.
#[test]
fn many_places_same_answers_as_checking_all_and_fast() {
    let mut rng = Rng(0x5EED_0001);
    let mut list = Vec::new();
    // 40,000 places over the whole globe, 10,000 crowded into one made-up city 20 km across.
    for id in 0..40_000u32 {
        list.push(place(id, rng.range(-89.0, 89.0), rng.range(-180.0, 180.0), rng.range(50.0, 2000.0)));
    }
    for id in 40_000..50_000u32 {
        list.push(place(id, rng.range(10.0, 10.18), rng.range(20.0, 20.18), rng.range(50.0, 400.0)));
    }
    let t = Instant::now();
    let ix = PlaceIndex::new(list.clone());
    let built = t.elapsed();
    assert_eq!(ix.len(), 50_000);

    let brute = |lat: f64, lon: f64| {
        list.iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let m = distance_m(lat, lon, p.lat, p.lon);
                (m <= p.radius_m).then_some((m, i, p.id))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)))
            .map(|(_, _, id)| id)
    };
    let mut queries = Vec::new();
    for i in 0..800 {
        if i % 2 == 0 {
            // Near a place (some inside, some just outside).
            let p = &list[(rng.next_f64() * list.len() as f64) as usize % list.len()];
            let (a, b) = north(p.lat, p.lon, rng.range(-2_100.0, 2_100.0));
            queries.push((a.clamp(-90.0, 90.0), b));
        } else {
            queries.push((rng.range(-90.0, 90.0), rng.range(-180.0, 180.0)));
        }
    }
    let mut found = 0;
    for &(a, b) in &queries {
        let got = ix.place_at(a, b).map(|p| p.id);
        assert_eq!(got, brute(a, b), "at query {a},{b}");
        found += usize::from(got.is_some());
    }
    assert!(found > 200, "the test should hit places often ({found})");

    let n = 200_000;
    let t = Instant::now();
    let mut hits = 0usize;
    for i in 0..n {
        let (a, b) = queries[i % queries.len()];
        hits += usize::from(ix.place_at(a, b).is_some());
    }
    let per = t.elapsed() / n as u32;
    println!("50,000 places: built in {built:?}, {per:?} a lookup ({hits} hits)");
    assert!(per.as_micros() < 200, "{per:?} a lookup");
}
