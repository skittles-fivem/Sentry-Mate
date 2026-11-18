//! What the made-up car does, day by day: drives, charges, a software update, and when the bridge has trouble.
//! Made from the scenario and its seed only, so the same scenario always gives the same plan.

use crate::geo::{LatLon, Route};
use crate::rng::Rng;
use crate::scenario::Scenario;
use model::ChargeKind;

pub const DAY_MS: u64 = 86_400_000;
const MIN_MS: u64 = 60_000;

/// A named place of the made-up town.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Place {
    pub name: String,
    pub at: LatLon,
}

/// When an activity starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum When {
    At(u64),
    /// This long after the activity before it ended.
    After(u64),
}

#[derive(Debug, Clone)]
pub enum Activity {
    Drive {
        when: When,
        from: usize,
        to: usize,
        route: Route,
    },
    /// Charging where the car is. `only_below`: skipped when the battery is at or above this % when it is due.
    Charge {
        when: When,
        kind: ChargeKind,
        kw: f64,
        limit: f64,
        only_below: Option<f64>,
        pause: Option<(u64, u64)>,
    },
    /// A software update: parked and awake, the buses go quiet while modules restart.
    Update {
        when: When,
        len_ms: u64,
        silences: Vec<(u64, u64)>,
    },
}

impl Activity {
    pub fn when(&self) -> When {
        match self {
            Activity::Drive { when, .. } | Activity::Charge { when, .. } | Activity::Update { when, .. } => *when,
        }
    }
}

/// What goes wrong on the bridge's side, at given times (ms).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Trouble {
    /// Recording paused (log folder full): `[from, to)`.
    pub not_recorded: Vec<(u64, u64)>,
    /// The log open at this time is lost before upload (power cut while in RAM): the chain shows a gap.
    pub lose_log_at: Vec<u64>,
    /// Frames lost here (`# dropped`): `(at, how long)`.
    pub drop_at: Vec<(u64, u64)>,
    /// The bridge app updates itself here: its log closes `flush`, it restarts after `len` ms.
    pub app_update_at: Vec<(u64, u64)>,
    /// The Screenmate is off (no logs at all) `[from, to)`: unknown, never asleep.
    pub offline: Vec<(u64, u64)>,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub places: Vec<Place>,
    pub activities: Vec<Activity>,
    pub trouble: Trouble,
}

pub const HOME: usize = 0;
pub const WORK: usize = 1;
pub const SHOP: usize = 2;
pub const SUPERCHARGER: usize = 3;

/// A road between two places: the straight line bent a little at a few corners, with town limits near its ends
/// and faster roads in between, and a stop (lights, a junction) at some corners.
fn route(rng: &mut Rng, a: LatLon, b: LatLon) -> Route {
    let km = a.km_to(b);
    let corners = (km / 1.5).clamp(2.0, 14.0) as usize;
    let mut pts = vec![a];
    for i in 1..corners {
        let f = i as f64 / corners as f64;
        let bend = (km / corners as f64) * rng.range(-0.35, 0.35);
        pts.push(a.lerp(b, f).offset(bend, bend * rng.range(-1.0, 1.0)));
    }
    pts.push(b);
    let legs = pts.len() - 1;
    let mut limits = Vec::with_capacity(legs);
    let mut stops = Vec::with_capacity(legs);
    for i in 0..legs {
        let edge = i == 0 || i + 1 == legs;
        let limit = if edge || km < 4.0 {
            [30.0, 40.0, 50.0][rng.below(3) as usize]
        } else if km > 30.0 {
            [90.0, 110.0, 120.0][rng.below(3) as usize]
        } else {
            [50.0, 70.0, 80.0][rng.below(3) as usize]
        };
        limits.push(limit);
        stops.push(i + 1 == legs || rng.chance(if limit <= 50.0 { 0.4 } else { 0.1 }));
    }
    Route::new(pts, limits, stops)
}

/// Monday 0 … Sunday 6.
pub fn weekday(ms: u64) -> u64 {
    (ms / DAY_MS + 3) % 7
}

pub fn make(s: &Scenario) -> Plan {
    let mut rng = Rng::new(s.seed).fork(1);
    let places = vec![
        Place { name: "Home".into(), at: s.home },
        Place { name: "Work".into(), at: s.work },
        Place { name: "Shop".into(), at: s.shop },
        Place { name: "Supercharger".into(), at: s.supercharger },
    ];
    let routes = |rng: &mut Rng, a: usize, b: usize| route(rng, places[a].at, places[b].at);
    let offset = s.utc_offset_min as i64 * MIN_MS as i64;
    let local_midnight = |d: u64| ((s.start_ms + d * DAY_MS) as i64 - offset).max(0) as u64;
    let minutes = |m: f64| (m * MIN_MS as f64) as u64;
    let mut acts: Vec<Activity> = Vec::new();
    let days = u64::from(s.days);
    // the long trip: the first Saturday, else the last day (of two or more)
    let trip_day = s
        .supercharger_trip
        .then(|| (0..days).find(|&d| weekday(local_midnight(d) + DAY_MS / 2) == 5).or((days >= 2).then(|| days - 1)));
    let trip_day = trip_day.flatten();
    let mut update_done = !s.software_update;
    let mut first_home_charge: Option<u64> = None;
    for d in 0..days {
        let m = local_midnight(d);
        let wd = weekday(m + DAY_MS / 2);
        let mut drove = false;
        if Some(d) == trip_day {
            let at = m + minutes(10.0 * 60.0 + rng.range(-20.0, 20.0));
            acts.push(Activity::Drive {
                when: When::At(at),
                from: HOME,
                to: SUPERCHARGER,
                route: routes(&mut rng, HOME, SUPERCHARGER),
            });
            acts.push(Activity::Charge {
                when: When::After(minutes(rng.range(2.0, 4.0))),
                kind: ChargeKind::Dc,
                kw: s.model.dc_max_kw(),
                limit: 80.0,
                only_below: None,
                pause: None,
            });
            acts.push(Activity::Drive {
                when: When::After(minutes(rng.range(5.0, 12.0))),
                from: SUPERCHARGER,
                to: HOME,
                route: routes(&mut rng, SUPERCHARGER, HOME),
            });
            drove = true;
        } else if s.commute && wd < 5 {
            let leave = s.leave_min as f64 + rng.range(-10.0, 15.0);
            acts.push(Activity::Drive {
                when: When::At(m + minutes(leave)),
                from: HOME,
                to: WORK,
                route: routes(&mut rng, HOME, WORK),
            });
            if !update_done {
                acts.push(Activity::Update {
                    when: When::At(m + minutes(12.5 * 60.0)),
                    len_ms: minutes(24.0),
                    silences: vec![(minutes(5.0), 45_000), (minutes(14.0), 40_000)],
                });
                update_done = true;
            }
            let back = s.back_min as f64 + rng.range(-15.0, 30.0);
            acts.push(Activity::Drive {
                when: When::At(m + minutes(back)),
                from: WORK,
                to: HOME,
                route: routes(&mut rng, WORK, HOME),
            });
            drove = true;
        }
        if s.errands && Some(d) != trip_day && rng.chance(if wd < 5 { 0.35 } else { 0.6 }) {
            let at = if wd < 5 { 19.5 * 60.0 } else { 14.0 * 60.0 } + rng.range(-20.0, 20.0);
            acts.push(Activity::Drive {
                when: When::At(m + minutes(at)),
                from: HOME,
                to: SHOP,
                route: routes(&mut rng, HOME, SHOP),
            });
            acts.push(Activity::Drive {
                when: When::After(minutes(rng.range(18.0, 35.0))),
                from: SHOP,
                to: HOME,
                route: routes(&mut rng, SHOP, HOME),
            });
            drove = true;
        }
        if s.home_charging && drove {
            let at = m + minutes(s.charge_start_min as f64);
            first_home_charge.get_or_insert(at);
            let pause = (first_home_charge == Some(at)).then_some((minutes(25.0), minutes(2.0)));
            acts.push(Activity::Charge {
                when: When::At(at),
                kind: ChargeKind::Ac,
                kw: s.home_charger_kw,
                limit: s.charge_limit,
                only_below: Some(s.charge_below),
                pause,
            });
        }
    }
    let mut trouble = s.trouble.clone();
    if s.with_trouble
        && let Some(at) = first_home_charge
    {
        trouble.lose_log_at.push(at + minutes(40.0));
        trouble.drop_at.push((at + minutes(50.0) + 20_000, 400));
        trouble.not_recorded.push((at + minutes(60.0), at + minutes(66.0)));
        trouble.app_update_at.push((at + minutes(90.0), 25_000));
    }
    // in time order (an `After` keeps its place behind the one it follows)
    let mut keyed: Vec<(u64, usize, Activity)> = Vec::new();
    let mut last = 0u64;
    for (i, a) in acts.into_iter().enumerate() {
        let k = match a.when() {
            When::At(t) => t,
            When::After(_) => last,
        };
        last = k;
        keyed.push((k, i, a));
    }
    keyed.sort_by_key(|(k, i, _)| (*k, *i));
    Plan { places, activities: keyed.into_iter().map(|(_, _, a)| a).collect(), trouble }
}
