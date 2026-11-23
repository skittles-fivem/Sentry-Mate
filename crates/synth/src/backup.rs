//! Made-up backups of other loggers, as text, the size and texture of real ones: a TeslaMate-like `pg_dump` in plain
//! SQL ([`dump`]) and TeslaFi-like monthly CSV files ([`teslafi_month`]). For measuring how small a kept upload can be
//! packed (docs/plans/storage.md) and for tests of the import; nothing here is anyone's car.
//!
//! The shape follows the made-up fixtures in `crates/import/tests/fixtures` (the tables and columns the importer
//! reads, `COPY ... FROM stdin` blocks with tab-separated rows and `\N` for null) and the TeslaFi columns of
//! `crates/import/tests/tf`. The values are simulated with noise where a real car's have it: a position about every
//! second while driving, its time to the microsecond, a speed that wanders, coordinates that follow the speed and a
//! heading that turns, an odometer in kilometres worked out from miles (a float with any digits), ranges and levels
//! that fall as the car drives, temperatures that drift, and nulls in the columns older rows don't have. The cars
//! live in the open sea (near 0.30° N, 0.20° E) and have the VINs [`vin`] makes.
//!
//! Everything is deterministic: the same options give the same bytes on every machine.

use crate::rng::Rng;
use std::fmt::Write as _;
use std::io::{self, Write};

/// 2024-01-01 00:00:00 UTC, in microseconds.
const BASE_US: i64 = 1_704_067_200_000_000;
const S: i64 = 1_000_000;
const MIN: i64 = 60 * S;
const HOUR: i64 = 60 * MIN;
const DAY: i64 = 24 * HOUR;
/// Any heading, roughly (the exact turn is not needed, and the files measured were made with this).
#[allow(clippy::approx_constant)]
const ABOUT_A_TURN: f64 = 6.28;

/// What a made-up dump holds.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub seed: u64,
    /// Days of history, from 2024-01-01.
    pub days: u32,
    /// Cars (1 or more); each has its own drives and charges, their rows mixed by time as one database writes them.
    pub cars: u32,
}

impl Default for Options {
    fn default() -> Options {
        Options { seed: 1, days: 30, cars: 2 }
    }
}

/// What was written, to check an import against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Written {
    pub positions: u64,
    pub charges: u64,
    pub drives: u64,
    pub charging_processes: u64,
    pub bytes: u64,
}

/// The made-up VIN of car `c` (1-based).
pub fn vin(c: u32) -> String {
    format!("5YJ3E1EA0KF{c:06}")
}

/// `YYYY-MM-DD HH:MM:SS[.ffffff]` (trailing zeros of the fraction left out, as PostgreSQL writes a timestamp).
pub fn time_text(us: i64) -> String {
    let secs = us.div_euclid(S);
    let frac = us.rem_euclid(S);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let mut s = format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60);
    if frac != 0 {
        let f = format!("{frac:06}");
        s.push('.');
        s.push_str(f.trim_end_matches('0'));
    }
    s
}

/// One car as it is simulated.
struct Car {
    id: u32,
    rng: Rng,
    lat: f64,
    lon: f64,
    heading: f64,
    speed: f64,
    odo_mi: f64,
    soc: f64,
    elevation: f64,
    outside: f64,
    inside: f64,
    /// Rows from this time on carry the columns newer versions fill (climate, tyre pressures).
    newer_from: i64,
}

impl Car {
    fn new(seed: u64, id: u32, days: u32) -> Car {
        let mut rng = Rng::new(seed).fork(u64::from(id));
        Car {
            id,
            lat: 0.30 + rng.range(-0.02, 0.02),
            lon: 0.20 + rng.range(-0.02, 0.02),
            heading: rng.range(0.0, ABOUT_A_TURN),
            speed: 0.0,
            odo_mi: 8_000.0 + rng.range(0.0, 20_000.0),
            soc: 78.0,
            elevation: 40.0,
            outside: 9.0,
            inside: 20.0,
            newer_from: BASE_US + i64::from(days) * DAY / 3,
            rng,
        }
    }

    fn range_km(&self, per_pct: f64) -> f64 {
        (self.soc * per_pct * 100.0).round() / 100.0
    }
}

enum Event {
    Drive { secs: i64 },
    Charge { secs: i64, kw: f64, dc: bool },
}

/// One row to write later, in time order across the cars.
struct Row {
    t: i64,
    car: u32,
    text: String,
    /// The drive or charging process it belongs to (0: none), numbered per run of the generator.
    of: u64,
    charge: bool,
}

struct Span {
    car: u32,
    from: i64,
    to: i64,
    drive: bool,
    of: u64,
    start_km: f64,
    end_km: f64,
    start_soc: f64,
    end_soc: f64,
    kwh: f64,
    speed_max: f64,
    first_pos: usize,
    last_pos: usize,
    outside: f64,
}

/// Writes a made-up TeslaMate-like dump (plain SQL) to `out`.
pub fn dump(o: Options, out: &mut dyn Write) -> io::Result<Written> {
    let cars = o.cars.max(1);
    let mut rows: Vec<Row> = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut of = 0u64;
    for c in 1..=cars {
        let mut car = Car::new(o.seed, c, o.days);
        for day in 0..i64::from(o.days) {
            let midnight = BASE_US + day * DAY;
            let mut events: Vec<(i64, Event)> = Vec::new();
            let drives = 2 + car.rng.below(3) as i64;
            for d in 0..drives {
                let at =
                    midnight + (7 + d * 4) * HOUR + car.rng.below(90 * 60) as i64 * S + car.rng.below(S as u64) as i64;
                events.push((at, Event::Drive { secs: 8 * 60 + car.rng.below(35 * 60) as i64 }));
            }
            if car.rng.chance(0.2) {
                let at = midnight + 13 * HOUR + car.rng.below(3600) as i64 * S;
                events.push((at, Event::Charge { secs: 15 * 60 + car.rng.below(25 * 60) as i64, kw: 150.0, dc: true }));
            }
            if car.soc < 60.0 || car.rng.chance(0.4) {
                let at = midnight + 21 * HOUR + car.rng.below(3600) as i64 * S;
                events
                    .push((at, Event::Charge { secs: 60 * 60 + car.rng.below(100 * 60) as i64, kw: 11.0, dc: false }));
            }
            events.sort_by_key(|e| e.0);
            let mut free_from = midnight;
            for (at, ev) in events {
                let at = at.max(free_from + 5 * MIN);
                of += 1;
                let end = match ev {
                    Event::Drive { secs } => drive(&mut car, at, secs, of, &mut rows, &mut spans),
                    Event::Charge { secs, kw, dc } => charge(&mut car, at, secs, kw, dc, of, &mut rows, &mut spans),
                };
                free_from = end;
            }
            car.outside += car.rng.range(-1.0, 1.0);
        }
    }
    // as one database wrote them: by time, whichever car
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&i| (rows[i].t, rows[i].car));
    let mut pos_id = vec![0u64; rows.len()];
    let (mut np, mut nc) = (0u64, 0u64);
    for &i in &order {
        if rows[i].charge {
            nc += 1;
            pos_id[i] = nc;
        } else {
            np += 1;
            pos_id[i] = np;
        }
    }
    spans.sort_by_key(|s| (s.from, s.car));
    let mut drive_id = std::collections::HashMap::new();
    let mut process_id = std::collections::HashMap::new();
    for s in &spans {
        if s.drive {
            drive_id.insert(s.of, drive_id.len() as u64 + 1);
        } else {
            process_id.insert(s.of, process_id.len() as u64 + 1);
        }
    }
    let mut w = Counting { out, n: 0 };
    w.write_all(PREAMBLE.as_bytes())?;
    let stamp = time_text(BASE_US);
    w.write_all(
        b"--\n-- Data for Name: tokens; Type: TABLE DATA; Schema: private; Owner: teslamate\n--\n\n\
          COPY private.tokens (id, inserted_at, updated_at, refresh, access) FROM stdin;\n",
    )?;
    writeln!(
        w,
        "1\t{stamp}\t{stamp}\t\\\\x4d4144452d55502d4e4f542d412d544f4b454e\t\\\\x4d4144452d55502d4e4f542d412d544f4b454e\n\\.\n\n"
    )?;
    section(
        &mut w,
        "addresses",
        "id, display_name, latitude, longitude, name, house_number, road, neighbourhood, city, county, postcode, state, state_district, country, raw, inserted_at, updated_at, osm_id, osm_type",
    )?;
    let mut rng = Rng::new(o.seed).fork(999);
    for a in 1..=40u32 {
        let (lat, lon) = (0.30 + rng.range(-0.05, 0.05), 0.20 + rng.range(-0.05, 0.05));
        let road = ROADS[a as usize % ROADS.len()];
        let t = time_text(BASE_US + i64::from(a) * 7 * HOUR + 5 * S);
        writeln!(
            w,
            "{a}\t{n}, {road}, Seaville, Nowhere\t{lat:.6}\t{lon:.6}\t\\N\t{n}\t{road}\t\\N\tSeaville\t\\N\t0{pc:04}\t\\N\t\\N\tNowhere\t{{\"city\": \"Seaville\", \"road\": \"{road}\", \"country\": \"Nowhere\", \"postcode\": \"0{pc:04}\", \"house_number\": \"{n}\", \"country_code\": \"xx\"}}\t{t}\t{t}\t{osm}\tway",
            n = 1 + rng.below(120),
            pc = rng.below(10_000),
            osm = 100_000_000 + rng.below(900_000_000),
        )?;
    }
    w.write_all(b"\\.\n\n\n")?;
    section(
        &mut w,
        "car_settings",
        "id, suspend_min, suspend_after_idle_min, req_not_unlocked, free_supercharging, use_streaming_api, enabled, lfp_battery",
    )?;
    for c in 1..=cars {
        writeln!(w, "{c}\t21\t15\tf\tf\tt\tt\tf")?;
    }
    w.write_all(b"\\.\n\n\n")?;
    section(
        &mut w,
        "cars",
        "id, eid, vid, model, efficiency, inserted_at, updated_at, vin, name, trim_badging, settings_id, exterior_color, spoiler_type, wheel_type, display_priority, marketing_name",
    )?;
    for c in 1..=cars {
        writeln!(
            w,
            "{c}\t{}\t{}\t3\t0.153\t{stamp}\t{stamp}\t{}\tMade-up {c}\t74D\t{c}\tMidnightSilver\tNone\tPinwheel18\t{c}\tLR AWD",
            1_000_000 + c,
            2_000_000 + c,
            vin(c)
        )?;
    }
    w.write_all(b"\\.\n\n\n")?;
    section(
        &mut w,
        "charges",
        "id, date, battery_heater_on, battery_level, charge_energy_added, charger_actual_current, charger_phases, charger_pilot_current, charger_power, charger_voltage, fast_charger_present, conn_charge_cable, fast_charger_brand, fast_charger_type, ideal_battery_range_km, not_enough_power_to_heat, outside_temp, charging_process_id, battery_heater, battery_heater_no_power, rated_battery_range_km, usable_battery_level",
    )?;
    let mut line = String::new();
    for &i in order.iter().filter(|&&i| rows[i].charge) {
        line.clear();
        let _ = write!(line, "{}\t", pos_id[i]);
        line.push_str(&rows[i].text.replace("{of}", &process_id.get(&rows[i].of).copied().unwrap_or(0).to_string()));
        line.push('\n');
        w.write_all(line.as_bytes())?;
    }
    w.write_all(b"\\.\n\n\n")?;
    section(
        &mut w,
        "charging_processes",
        "id, start_date, end_date, charge_energy_added, start_ideal_range_km, end_ideal_range_km, start_battery_level, end_battery_level, duration_min, outside_temp_avg, car_id, position_id, address_id, start_rated_range_km, end_rated_range_km, geofence_id, charge_energy_used, cost",
    )?;
    let mut written = Written { positions: np, charges: nc, ..Written::default() };
    for s in spans.iter().filter(|s| !s.drive) {
        written.charging_processes += 1;
        writeln!(
            w,
            "{}\t{}\t{}\t{:.2}\t{:.2}\t{:.2}\t{:.0}\t{:.0}\t{}\t{:.1}\t{}\t{}\t{}\t{:.2}\t{:.2}\t\\N\t{:.2}\t\\N",
            process_id[&s.of],
            time_text(s.from),
            time_text(s.to),
            s.kwh,
            s.start_soc * 5.2,
            s.end_soc * 5.2,
            s.start_soc,
            s.end_soc,
            (s.to - s.from) / MIN,
            s.outside,
            s.car,
            pos_id[s.first_pos],
            1 + s.of % 40,
            s.start_soc * 4.9,
            s.end_soc * 4.9,
            s.kwh * 1.07
        )?;
    }
    w.write_all(b"\\.\n\n\n")?;
    section(
        &mut w,
        "drives",
        "id, start_date, end_date, outside_temp_avg, speed_max, power_max, power_min, start_ideal_range_km, end_ideal_range_km, start_km, end_km, distance, duration_min, car_id, inside_temp_avg, start_address_id, end_address_id, start_rated_range_km, end_rated_range_km, start_position_id, end_position_id, start_geofence_id, end_geofence_id, ascent, descent",
    )?;
    for s in spans.iter().filter(|s| s.drive) {
        written.drives += 1;
        writeln!(
            w,
            "{}\t{}\t{}\t{:.1}\t{:.0}\t{}\t{}\t{:.2}\t{:.2}\t{}\t{}\t{}\t{}\t{}\t{:.1}\t{}\t{}\t{:.2}\t{:.2}\t{}\t{}\t\\N\t\\N\t{}\t{}",
            drive_id[&s.of],
            time_text(s.from),
            time_text(s.to),
            s.outside,
            s.speed_max,
            60 + s.of % 90,
            -(20 + (s.of % 50) as i64),
            s.start_soc * 5.2,
            s.end_soc * 5.2,
            s.start_km,
            s.end_km,
            s.end_km - s.start_km,
            (s.to - s.from) / MIN,
            s.car,
            20.5,
            1 + s.of % 40,
            1 + (s.of + 7) % 40,
            s.start_soc * 4.9,
            s.end_soc * 4.9,
            pos_id[s.first_pos],
            pos_id[s.last_pos],
            s.of % 60,
            (s.of * 7) % 60
        )?;
    }
    w.write_all(b"\\.\n\n\n")?;
    section(
        &mut w,
        "geofences",
        "id, name, latitude, longitude, radius, inserted_at, updated_at, cost_per_unit, session_fee, billing_type",
    )?;
    writeln!(w, "1\tHome (made up)\t0.300000\t0.200000\t25\t{stamp}\t{stamp}\t0.3100\t\\N\tper_kwh\n\\.\n\n")?;
    section(
        &mut w,
        "positions",
        "id, date, latitude, longitude, speed, power, odometer, ideal_battery_range_km, battery_level, outside_temp, elevation, fan_status, driver_temp_setting, passenger_temp_setting, is_climate_on, is_rear_defroster_on, is_front_defroster_on, car_id, drive_id, inside_temp, battery_heater, battery_heater_on, battery_heater_no_power, est_battery_range_km, rated_battery_range_km, usable_battery_level, tpms_pressure_fl, tpms_pressure_fr, tpms_pressure_rl, tpms_pressure_rr",
    )?;
    for &i in order.iter().filter(|&&i| !rows[i].charge) {
        line.clear();
        let _ = write!(line, "{}\t", pos_id[i]);
        let d = match drive_id.get(&rows[i].of) {
            Some(d) => d.to_string(),
            None => "\\N".into(),
        };
        line.push_str(&rows[i].text.replace("{of}", &d));
        line.push('\n');
        w.write_all(line.as_bytes())?;
    }
    w.write_all(b"\\.\n\n\n")?;
    section(&mut w, "schema_migrations", "version, inserted_at")?;
    for k in 0..90i64 {
        writeln!(w, "{}\t{stamp}", 20_190_330_150_000 + k * 9_700_003)?;
    }
    w.write_all(b"\\.\n\n\n")?;
    section(
        &mut w,
        "settings",
        "id, inserted_at, updated_at, unit_of_length, unit_of_temperature, preferred_range, base_url, grafana_url, language, unit_of_pressure, theme_mode",
    )?;
    writeln!(w, "1\t{stamp}\t{stamp}\tkm\tC\trated\t\\N\t\\N\ten\tbar\tsystem\n\\.\n\n")?;
    section(&mut w, "states", "id, state, start_date, end_date, car_id")?;
    // online around each drive and charge, asleep between
    let mut id = 0u64;
    let mut last: Vec<i64> = vec![BASE_US - HOUR; cars as usize + 1];
    for (k, s) in spans.iter().enumerate() {
        let c = s.car as usize;
        let from = (s.from - 3 * MIN).max(last[c]);
        if from > last[c] {
            id += 1;
            writeln!(w, "{id}\tasleep\t{}\t{}\t{}", time_text(last[c]), time_text(from), s.car)?;
        }
        id += 1;
        let later = spans[k + 1..].iter().any(|n| n.car == s.car);
        let to = s.to + 12 * MIN;
        let to = spans[k + 1..].iter().find(|n| n.car == s.car).map_or(to, |n| to.min(n.from - 3 * MIN).max(from));
        writeln!(w, "{id}\tonline\t{}\t{}\t{}", time_text(from), time_text(to), s.car)?;
        last[c] = to;
        if !later {
            id += 1;
            writeln!(w, "{id}\tasleep\t{}\t\\N\t{}", time_text(to), s.car)?;
        }
    }
    w.write_all(b"\\.\n\n\n")?;
    section(&mut w, "updates", "id, start_date, end_date, version, car_id")?;
    for c in 1..=cars {
        writeln!(
            w,
            "{c}\t{}\t{}\t2024.2.7 abc123def456\t{c}",
            time_text(BASE_US - 10 * DAY + 3 * HOUR),
            time_text(BASE_US - 10 * DAY + 3 * HOUR + 40 * MIN)
        )?;
    }
    w.write_all(b"\\.\n\n\n")?;
    w.write_all(POSTAMBLE.as_bytes())?;
    written.bytes = w.n;
    Ok(written)
}

struct Counting<'a> {
    out: &'a mut dyn Write,
    n: u64,
}

impl Write for Counting<'_> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        let n = self.out.write(b)?;
        self.n += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

fn section(w: &mut dyn Write, table: &str, columns: &str) -> io::Result<()> {
    write!(
        w,
        "--\n-- Data for Name: {table}; Type: TABLE DATA; Schema: public; Owner: teslamate\n--\n\nCOPY public.{table} ({columns}) FROM stdin;\n"
    )
}

/// A float as PostgreSQL writes a `double precision`: the shortest text that reads back the same.
fn float(v: f64) -> String {
    format!("{v}")
}

/// A drive from `at` for `secs` seconds: a position about every second. The end time.
fn drive(car: &mut Car, at: i64, secs: i64, of: u64, rows: &mut Vec<Row>, spans: &mut Vec<Span>) -> i64 {
    let first = rows.len();
    let (start_km, start_soc) = (car.odo_mi * 1.609_344, car.soc);
    let mut t = at;
    let mut target = car.rng.range(30.0, 110.0);
    let mut speed_max = 0.0f64;
    let newer = at >= car.newer_from;
    let tpms = [2.9, 2.9, 2.8, 2.8];
    for k in 0..secs {
        // the logger's clock: a second, give or take
        t += S + car.rng.below(60_000) as i64 - 30_000;
        if car.rng.chance(0.02) {
            target = if car.rng.chance(0.25) { 0.0 } else { car.rng.range(20.0, 130.0) };
        }
        let left = secs - k;
        let want = if left < 25 { 0.0 } else { target };
        let accel = ((want - car.speed) * 0.15).clamp(-9.0, 6.0) + car.rng.range(-0.8, 0.8);
        car.speed = (car.speed + accel).max(0.0);
        speed_max = speed_max.max(car.speed);
        car.heading += car.rng.range(-0.03, 0.03) + if car.rng.chance(0.01) { car.rng.range(-1.5, 1.5) } else { 0.0 };
        let km = car.speed / 3600.0;
        car.lat += km / 111.0 * car.heading.cos();
        car.lon += km / 111.0 * car.heading.sin();
        // kept in the open sea
        if (car.lat - 0.30).abs() > 0.25 || (car.lon - 0.20).abs() > 0.25 {
            car.heading += ABOUT_A_TURN / 2.0;
        }
        car.odo_mi += km / 1.609_344;
        car.soc = (car.soc - km * 0.16 / 0.75).max(3.0);
        car.elevation += car.rng.range(-0.6, 0.6);
        if car.rng.chance(0.01) {
            car.inside += car.rng.range(-0.2, 0.2);
        }
        let power = (accel * car.speed * 0.25 + car.speed * 0.12 + car.rng.range(-2.0, 2.0)).round() as i64;
        let odo_km = (car.odo_mi * 1_000_000.0).round() / 1_000_000.0 * 1.609_344;
        let (ideal, rated) = (car.range_km(5.2), car.range_km(4.9));
        let mut s = String::with_capacity(200);
        let _ = write!(
            s,
            "{}\t{:.6}\t{:.6}\t{:.0}\t{power}\t{}\t{ideal:.2}\t{:.0}\t{:.1}\t{:.0}\t",
            time_text(t),
            car.lat,
            car.lon,
            car.speed,
            float(odo_km),
            car.soc,
            (car.outside * 2.0).round() / 2.0,
            car.elevation
        );
        if newer {
            let _ = write!(s, "{}\t21.0\t21.0\tt\tf\tf\t", 2 + (k / 300) % 3);
        } else {
            s.push_str("\\N\t\\N\t\\N\t\\N\t\\N\t\\N\t");
        }
        let _ = write!(
            s,
            "{}\t{{of}}\t{:.1}\t\\N\t\\N\t\\N\t{:.2}\t{rated:.2}\t{:.0}\t",
            car.id,
            car.inside,
            rated * 0.93,
            car.soc - 1.0
        );
        if newer {
            let _ = write!(s, "{}\t{}\t{}\t{}", tpms[0], tpms[1], tpms[2], tpms[3]);
        } else {
            s.push_str("\\N\t\\N\t\\N\t\\N");
        }
        rows.push(Row { t, car: car.id, text: s, of, charge: false });
    }
    car.speed = 0.0;
    spans.push(Span {
        car: car.id,
        from: at,
        to: t,
        drive: true,
        of,
        start_km,
        end_km: car.odo_mi * 1.609_344,
        start_soc,
        end_soc: car.soc,
        kwh: 0.0,
        speed_max,
        first_pos: first,
        last_pos: rows.len() - 1,
        outside: car.outside,
    });
    t
}

/// A charge from `at`: a position at its start, then a row of `charges` about every five seconds. The end time.
#[allow(clippy::too_many_arguments)]
fn charge(
    car: &mut Car,
    at: i64,
    secs: i64,
    kw: f64,
    dc: bool,
    of: u64,
    rows: &mut Vec<Row>,
    spans: &mut Vec<Span>,
) -> i64 {
    let first = rows.len();
    let start_soc = car.soc;
    let odo_km = (car.odo_mi * 1_000_000.0).round() / 1_000_000.0 * 1.609_344;
    let mut s = String::new();
    let _ = write!(
        s,
        "{}\t{:.6}\t{:.6}\t\\N\t{:.0}\t{}\t{:.2}\t{:.0}\t{:.1}\t{:.0}\t\\N\t\\N\t\\N\t\\N\t\\N\t\\N\t{}\t\\N\t\\N\t\\N\t\\N\t\\N\t\\N\t{:.2}\t{:.0}\t\\N\t\\N\t\\N\t\\N",
        time_text(at),
        car.lat,
        car.lon,
        -kw,
        float(odo_km),
        car.range_km(5.2),
        car.soc,
        (car.outside * 2.0).round() / 2.0,
        car.elevation,
        car.id,
        car.range_km(4.9),
        car.soc
    );
    rows.push(Row { t: at, car: car.id, text: s, of: 0, charge: false });
    let mut t = at;
    let mut kwh = 0.0;
    while t < at + secs * S && car.soc < 99.0 {
        t += 5 * S + car.rng.below(400_000) as i64 - 200_000;
        // a DC charge tapers as the battery fills
        let now_kw = if dc { kw * (1.15 - car.soc / 100.0).clamp(0.2, 1.0) } else { kw } + car.rng.range(-0.4, 0.4);
        kwh += now_kw * 5.0 / 3600.0;
        car.soc = (car.soc + now_kw * 5.0 / 3600.0 / 0.75).min(100.0);
        let volts = if dc { 380.0 + car.soc * 0.2 } else { 230.0 } + car.rng.range(-1.5, 1.5);
        let amps = if dc { now_kw * 1000.0 / volts } else { 16.0 };
        let mut s = String::with_capacity(160);
        let _ = write!(
            s,
            "{}\tf\t{:.0}\t{kwh:.2}\t{amps:.0}\t{}\t{}\t{:.0}\t{volts:.0}\t{}\tIEC\t{}\t{}\t{:.2}\tf\t{:.1}\t{{of}}\tf\t\\N\t{:.2}\t{:.0}",
            time_text(t),
            car.soc,
            if dc { "\\N" } else { "3" },
            if dc { "500" } else { "16" },
            now_kw,
            if dc { "t" } else { "f" },
            if dc { "Tesla" } else { "\\N" },
            if dc { "Tesla" } else { "<invalid>" },
            car.range_km(5.2),
            (car.outside * 2.0).round() / 2.0,
            car.range_km(4.9),
            car.soc
        );
        rows.push(Row { t, car: car.id, text: s, of, charge: true });
    }
    spans.push(Span {
        car: car.id,
        from: at,
        to: t,
        drive: false,
        of,
        start_km: 0.0,
        end_km: 0.0,
        start_soc,
        end_soc: car.soc,
        kwh,
        speed_max: 0.0,
        first_pos: first,
        last_pos: first,
        outside: car.outside,
    });
    t
}

const ROADS: [&str; 8] = [
    "Made-up Road",
    "Second Street",
    "Harbour Lane",
    "Kelp Avenue",
    "Lighthouse Way",
    "Gull Close",
    "Tide Street",
    "Anchor Row",
];

/// What a plain dump starts with (generic statements, as any `pg_dump` of a database with these tables writes them).
const PREAMBLE: &str = "--\n-- PostgreSQL database dump\n--\n\n-- Dumped from database version 16.16 (Debian 16.16-1.pgdg13+1)\n-- Dumped by pg_dump version 16.16 (Debian 16.16-1.pgdg13+1)\n\nSET statement_timeout = 0;\nSET lock_timeout = 0;\nSET idle_in_transaction_session_timeout = 0;\nSET client_encoding = 'UTF8';\nSET standard_conforming_strings = on;\nSELECT pg_catalog.set_config('search_path', '', false);\nSET check_function_bodies = false;\nSET xmloption = content;\nSET client_min_messages = warning;\nSET row_security = off;\n\n--\n-- Name: private; Type: SCHEMA; Schema: -; Owner: teslamate\n--\n\nCREATE SCHEMA private;\n\n\nALTER SCHEMA private OWNER TO teslamate;\n\nSET default_tablespace = '';\n\nSET default_table_access_method = heap;\n\n--\n-- Name: positions; Type: TABLE; Schema: public; Owner: teslamate\n--\n\nCREATE TABLE public.positions (\n    id bigint NOT NULL,\n    date timestamp(6) without time zone NOT NULL,\n    latitude numeric(8,6) NOT NULL,\n    longitude numeric(9,6) NOT NULL,\n    speed smallint,\n    power smallint,\n    odometer double precision,\n    battery_level smallint,\n    car_id smallint NOT NULL,\n    drive_id integer\n);\n\n\nALTER TABLE public.positions OWNER TO teslamate;\n\n";

const POSTAMBLE: &str = "--\n-- Name: positions_id_seq; Type: SEQUENCE SET; Schema: public; Owner: teslamate\n--\n\nSELECT pg_catalog.setval('public.positions_id_seq', 1, true);\n\n\n--\n-- Name: positions positions_pkey; Type: CONSTRAINT; Schema: public; Owner: teslamate\n--\n\nALTER TABLE ONLY public.positions\n    ADD CONSTRAINT positions_pkey PRIMARY KEY (id);\n\n\n--\n-- PostgreSQL database dump complete\n--\n\n";

/// TeslaFi's columns, in an order like its own (as `crates/import/tests/tf` has them).
pub const TESLAFI_HEADER: &str = "data_id,Date,calendar_enabled,remote_start_enabled,state,carState,shift_state,\
charging_state,latitude,longitude,heading,elevation,speed,power,odometer,odometerF,battery_level,\
usable_battery_level,battery_range,ideal_battery_range,est_battery_range,charge_energy_added,charger_power,\
charger_voltage,charger_actual_current,charger_phases,fast_charger_present,fast_charger_type,inside_temp,\
outside_temp,inside_tempF,outside_tempF,car_version,vin,display_name,vehicle_id,measure,gui_distance_units,\
gui_temperature_units,idleNumber,sleepNumber,driveNumber,chargeNumber";

/// A made-up TeslaFi car across its monthly files.
pub struct Teslafi {
    rng: Rng,
    id: u64,
    odo_mi: f64,
    soc: f64,
    lat: f64,
    lon: f64,
    heading: f64,
    inside: f64,
    outside: f64,
    numbers: [u64; 4],
    vin: String,
}

impl Teslafi {
    pub fn new(seed: u64) -> Teslafi {
        Teslafi {
            rng: Rng::new(seed).fork(77),
            id: 91_000_000,
            odo_mi: 12_345.0,
            soc: 80.0,
            lat: 0.30,
            lon: 0.20,
            heading: 1.0,
            inside: 21.0,
            outside: 12.0,
            numbers: [1, 1, 1, 1],
            vin: "5YJ3E1EA1PF000001".into(),
        }
    }

    /// The file of month `m` (1 to 12) of year `y`, as TeslaFi's `TeslaFi<M><YYYY>.csv`: a row a minute while the
    /// car is awake, one every half hour asleep; local wall time without a zone; CRLF line ends.
    pub fn month(&mut self, y: i32, m: u32) -> String {
        let days = match m {
            4 | 6 | 9 | 11 => 30,
            2 if y % 4 == 0 => 29,
            2 => 28,
            _ => 31,
        };
        let mut s = String::with_capacity(days as usize * 200_000);
        s.push_str(TESLAFI_HEADER);
        s.push_str("\r\n");
        for d in 1..=days {
            self.day(&mut s, &format!("{y:04}-{m:02}-{d:02}"));
        }
        s
    }

    #[allow(clippy::too_many_arguments)]
    fn row(&mut self, s: &mut String, date: &str, h: u32, min: u32, doing: u8, speed: f64, kw: f64, added: f64) {
        self.id += 1 + self.rng.below(3);
        let sec = self.rng.below(8);
        let (state, car_state, shift, charging) = match doing {
            0 => ("asleep", "Sleeping", "None", "Disconnected"),
            1 => ("online", "Idling", "P", "Disconnected"),
            2 => ("online", "Driving", "D", "Disconnected"),
            _ => ("online", "Charging", "P", "Charging"),
        };
        let (speed_t, power) = match doing {
            2 => (format!("{speed:.0}"), format!("{:.0}", speed * 0.3 + self.rng.range(-8.0, 12.0))),
            3 => ("None".into(), format!("{:.0}", -kw)),
            _ => ("None".into(), "0".into()),
        };
        let (cp, volts, amps, phases, fast, ftype) = if doing == 3 && kw > 30.0 {
            (
                format!("{kw:.0}"),
                format!("{:.1}", 388.0 + self.rng.range(-3.0, 3.0)),
                format!("{:.1}", kw * 1000.0 / 390.0),
                "None",
                "True",
                "Supercharger",
            )
        } else if doing == 3 {
            (
                format!("{kw:.0}"),
                format!("{:.1}", 235.0 + self.rng.range(-2.0, 2.0)),
                "16.0".into(),
                "3",
                "False",
                "<invalid>",
            )
        } else {
            ("0".into(), "0".into(), "0".into(), "None", "False", "<invalid>")
        };
        let range = self.soc * 3.1;
        let _ = write!(
            s,
            "{},{date} {h:02}:{min:02}:{sec:02},False,False,{state},{car_state},{shift},{charging},{:.6},{:.6},{:.0},{:.0},{speed_t},{power},{:.6},{:.6},{:.0},{:.0},{range:.2},999.0,{:.2},{added:.2},{cp},{volts},{amps},{phases},{fast},{ftype},{:.1},{:.1},{:.1},{:.1},2023.44.30.8 06f534d46010,{},\"Made-up, the Whale\",1234567,metric,km/hr,C,{},{},{},{}\r\n",
            self.id,
            self.lat,
            self.lon,
            (self.heading.to_degrees()).rem_euclid(360.0),
            30.0 + (self.lat * 4000.0).sin() * 25.0,
            self.odo_mi,
            self.odo_mi * 1.609_344,
            self.soc,
            self.soc - 1.0,
            range * 0.94,
            self.inside,
            (self.outside * 2.0).round() / 2.0,
            self.inside,
            (self.outside * 2.0).round() / 2.0,
            self.vin,
            self.numbers[0],
            self.numbers[1],
            self.numbers[2],
            self.numbers[3]
        );
    }

    fn day(&mut self, s: &mut String, date: &str) {
        self.outside += self.rng.range(-1.5, 1.5);
        let d1 = 7 * 60 + self.rng.below(120) as u32;
        let len1 = 15 + self.rng.below(40) as u32;
        let d2 = 16 * 60 + self.rng.below(120) as u32;
        let len2 = 15 + self.rng.below(40) as u32;
        let charge_at = 20 * 60 + self.rng.below(60) as u32;
        let charges = self.soc < 65.0 || self.rng.chance(0.3);
        let mut added = 0.0;
        let mut m = 0u32;
        while m < 24 * 60 {
            let driving = (d1..d1 + len1).contains(&m) || (d2..d2 + len2).contains(&m);
            let charging = charges && m >= charge_at && self.soc < 90.0;
            let awake = driving
                || charging
                || (d1.saturating_sub(10)..d1 + len1 + 20).contains(&m)
                || (d2.saturating_sub(10)..d2 + len2 + 20).contains(&m);
            if driving {
                if m == d1 || m == d2 {
                    self.numbers[2] += 1;
                }
                let speed = self.rng.range(0.0, 75.0);
                let mi = speed / 60.0;
                self.odo_mi += mi;
                self.soc = (self.soc - mi * 0.35).max(5.0);
                self.heading += self.rng.range(-0.4, 0.4);
                self.lat = (self.lat + mi * 1.609 / 111.0 * self.heading.cos()).clamp(0.05, 0.55);
                self.lon = (self.lon + mi * 1.609 / 111.0 * self.heading.sin()).clamp(-0.05, 0.45);
                self.inside += self.rng.range(-0.3, 0.3);
                self.row(s, date, m / 60, m % 60, 2, speed, 0.0, 0.0);
            } else if charging {
                if added == 0.0 {
                    self.numbers[3] += 1;
                }
                let kw = 11.0;
                added += kw / 60.0;
                self.soc = (self.soc + kw / 60.0 / 0.75).min(100.0);
                self.row(s, date, m / 60, m % 60, 3, 0.0, kw, added);
            } else if awake {
                self.row(s, date, m / 60, m % 60, 1, 0.0, 0.0, 0.0);
            } else if m.is_multiple_of(30) {
                self.row(s, date, m / 60, m % 60, 0, 0.0, 0.0, 0.0);
            }
            m += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_same_options_give_the_same_dump_and_its_rows_have_their_columns() {
        let o = Options { seed: 3, days: 2, cars: 2 };
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let w = dump(o, &mut a).unwrap();
        dump(o, &mut b).unwrap();
        assert_eq!(a, b);
        assert_eq!(w.bytes, a.len() as u64);
        assert!(w.positions > 5_000 && w.drives >= 8, "{w:?}");
        let text = String::from_utf8(a).unwrap();
        let mut columns = 0;
        let mut seen = 0;
        for l in text.lines() {
            if let Some(rest) = l.strip_prefix("COPY ") {
                columns = rest.split(',').count();
            } else if l == "\\." {
                columns = 0;
            } else if columns > 0 {
                assert_eq!(l.split('\t').count(), columns, "{l}");
                seen += 1;
            }
        }
        assert!(seen as u64 > w.positions + w.charges);
        assert_eq!(time_text(BASE_US + 1_500_000), "2024-01-01 00:00:01.5");
        assert_eq!(time_text(BASE_US + 61 * S + 123_456), "2024-01-01 00:01:01.123456");
    }

    #[test]
    fn a_teslafi_month_has_a_row_for_each_poll_with_every_column() {
        let mut t = Teslafi::new(5);
        let csv = t.month(2024, 2);
        let columns = TESLAFI_HEADER.split(',').count();
        let mut rows = 0;
        for l in csv.split("\r\n").skip(1).filter(|l| !l.is_empty()) {
            // the name is quoted and holds a comma
            assert_eq!(l.split(',').count(), columns + 1, "{l}");
            rows += 1;
        }
        assert!(rows > 29 * 100, "{rows}");
        assert_eq!(csv, Teslafi::new(5).month(2024, 2));
    }
}
