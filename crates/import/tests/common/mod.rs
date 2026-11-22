//! A sink that keeps everything it is given, and the fixtures.
#![allow(dead_code, clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use import::{AddressRow, Batch, CarReport, ImportState, PlaceRow, Sink, SinkError, Target, TmCar};
use model::CarId;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

pub fn read_fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixture(name)).unwrap()
}

/// Matches TeslaMate cars to Sentry Mate cars by VIN prefix, keeps the states between imports, and records rows.
#[derive(Default)]
pub struct Keep {
    /// VIN prefix → Sentry Mate car; a car not listed is left out.
    pub cars: BTreeMap<String, CarId>,
    pub states: BTreeMap<CarId, ImportState>,
    pub asked: Vec<TmCar>,
    pub batches: Vec<(CarId, Batch)>,
    pub places: Vec<PlaceRow>,
    pub addresses: Vec<AddressRow>,
    pub reports: Vec<CarReport>,
    /// Fail on the n-th batch.
    pub fail_at: Option<usize>,
}

impl Keep {
    pub fn new(cars: &[(&str, CarId)]) -> Self {
        Keep { cars: cars.iter().map(|(v, c)| (v.to_string(), *c)).collect(), ..Keep::default() }
    }

    /// Forgets what the last import handed over (the states stay, for the next import).
    pub fn clear(&mut self) {
        self.asked.clear();
        self.batches.clear();
        self.places.clear();
        self.addresses.clear();
        self.reports.clear();
    }

    pub fn report(&self, tm_car_id: i64) -> &CarReport {
        self.reports.iter().find(|r| r.tm_car_id == tm_car_id).unwrap()
    }

    pub fn positions(&self, car: CarId) -> Vec<&import::PositionRow> {
        self.batches
            .iter()
            .filter(|(c, _)| *c == car)
            .flat_map(|(_, b)| match b {
                Batch::Positions(v) => v.iter().collect(),
                _ => Vec::new(),
            })
            .collect()
    }

    pub fn drives(&self, car: CarId) -> Vec<&import::DriveRow> {
        self.batches
            .iter()
            .filter(|(c, _)| *c == car)
            .flat_map(|(_, b)| match b {
                Batch::Drives(v) => v.iter().collect(),
                _ => Vec::new(),
            })
            .collect()
    }

    pub fn charges(&self, car: CarId) -> Vec<&import::ChargeRow> {
        self.batches
            .iter()
            .filter(|(c, _)| *c == car)
            .flat_map(|(_, b)| match b {
                Batch::Charges(v) => v.iter().collect(),
                _ => Vec::new(),
            })
            .collect()
    }

    pub fn points(&self, car: CarId) -> Vec<&import::ChargePointRow> {
        self.batches
            .iter()
            .filter(|(c, _)| *c == car)
            .flat_map(|(_, b)| match b {
                Batch::ChargePoints(v) => v.iter().collect(),
                _ => Vec::new(),
            })
            .collect()
    }

    pub fn states_of(&self, car: CarId) -> Vec<&import::StateRow> {
        self.batches
            .iter()
            .filter(|(c, _)| *c == car)
            .flat_map(|(_, b)| match b {
                Batch::States(v) => v.iter().collect(),
                _ => Vec::new(),
            })
            .collect()
    }

    pub fn updates(&self, car: CarId) -> Vec<&import::UpdateRow> {
        self.batches
            .iter()
            .filter(|(c, _)| *c == car)
            .flat_map(|(_, b)| match b {
                Batch::Updates(v) => v.iter().collect(),
                _ => Vec::new(),
            })
            .collect()
    }

    pub fn rows(&self) -> usize {
        self.batches.iter().map(|(_, b)| b.len()).sum()
    }
}

impl Sink for Keep {
    fn car(&mut self, car: &TmCar) -> Result<Option<Target>, SinkError> {
        self.asked.push(car.clone());
        let Some(id) = car.vin.as_ref().and_then(|v| self.cars.get(&v.vin_prefix)).copied() else { return Ok(None) };
        Ok(Some(Target { car: id, state: self.states.get(&id).cloned().unwrap_or_default() }))
    }

    fn batch(&mut self, car: CarId, batch: Batch) -> Result<(), SinkError> {
        assert!(!batch.is_empty());
        if self.fail_at == Some(self.batches.len()) {
            return Err("the disk is full".into());
        }
        self.batches.push((car, batch));
        Ok(())
    }

    fn places(&mut self, rows: Vec<PlaceRow>) -> Result<(), SinkError> {
        self.places.extend(rows);
        Ok(())
    }

    fn addresses(&mut self, rows: Vec<AddressRow>) -> Result<(), SinkError> {
        self.addresses.extend(rows);
        Ok(())
    }

    fn car_done(&mut self, report: CarReport, state: Option<ImportState>) -> Result<(), SinkError> {
        if let (Some(car), Some(state)) = (report.car, state) {
            self.states.insert(car, state);
        }
        self.reports.push(report);
        Ok(())
    }
}

/// `YYYY-MM-DD HH:MM:SS` (UTC) for ms since 1970.
pub fn time_text(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Days to a civil date (the well-known era arithmetic).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// 2024-01-01 00:00:00 UTC.
pub const BASE_MS: u64 = 1_704_067_200_000;

/// A TeslaMate-like backup made up as it is read, never held whole, in pg_dump's table order: `cars` cars, each with
/// `positions` positions (ids mixed across the cars, as TeslaMate's one counter makes them, a point every
/// `step_s` seconds), `drives` drives of 10 positions each, one charge with 3 points, 3 states (the last still open)
/// and an update. Coordinates are made up around a spot in the Atlantic.
#[derive(Debug, Clone, Copy)]
pub struct Synth {
    pub cars: u64,
    pub positions: u64,
    pub drives: u64,
    pub step_s: u64,
}

impl Synth {
    pub fn reader(self) -> SynthReader {
        SynthReader { s: self, stage: 0, next: 0, buf: Vec::new(), at: 0 }
    }

    pub fn vin(c: u64) -> String {
        format!("5YJ3E1EA0KF{c:06}")
    }

    fn position_time(&self, j: u64) -> u64 {
        BASE_MS + j * self.step_s * 1000
    }

    /// The car and per-car index of position `k` (1-based ids).
    fn position_of(&self, k: u64) -> (u64, u64) {
        ((k - 1) % self.cars + 1, (k - 1) / self.cars)
    }

    fn position_id(&self, car: u64, j: u64) -> u64 {
        j * self.cars + car
    }
}

fn copy_header(o: &mut String, table: &str, columns: &[&str]) {
    o.push_str(&format!("COPY public.{table} ({}) FROM stdin;\n", columns.join(", ")));
}

pub struct SynthReader {
    s: Synth,
    stage: u32,
    next: u64,
    buf: Vec<u8>,
    at: usize,
}

impl SynthReader {
    /// Writes the next piece; false at the end.
    fn fill(&mut self) -> bool {
        use std::fmt::Write;
        let s = self.s;
        let mut o = String::new();
        match self.stage {
            0 => {
                o.push_str("--\n-- PostgreSQL database dump\n--\n\nSET client_encoding = 'UTF8';\n\n");
                o.push_str("COPY private.tokens (id, refresh, access) FROM stdin;\n1\t\\\\x00\t\\\\x00\n\\.\n\n");
                o.push_str(
                    "COPY public.addresses (id, display_name, latitude, longitude, name, road, city) FROM stdin;\n",
                );
                o.push_str("1\tMade-up Road, Testville\t12.300000\t-45.600000\t\\N\tMade-up Road\tTestville\n\\.\n\n");
                o.push_str("COPY public.car_settings (id, lfp_battery, free_supercharging) FROM stdin;\n");
                for c in 1..=s.cars {
                    writeln!(o, "{c}\tf\tf").unwrap();
                }
                o.push_str("\\.\n\nCOPY public.cars (id, vin, name, model, efficiency, settings_id) FROM stdin;\n");
                for c in 1..=s.cars {
                    writeln!(o, "{c}\t{}\tCar {c}\t3\t0.15\t{c}", Synth::vin(c)).unwrap();
                }
                o.push_str("\\.\n\n");
                copy_header(
                    &mut o,
                    "charges",
                    &[
                        "id",
                        "date",
                        "battery_level",
                        "charge_energy_added",
                        "charger_power",
                        "charger_voltage",
                        "charger_actual_current",
                        "fast_charger_present",
                        "charging_process_id",
                    ],
                );
                for c in 1..=s.cars {
                    let t = s.position_time(s.positions) + 60_000;
                    for i in 0..3u64 {
                        let id = (c - 1) * 3 + i + 1;
                        writeln!(
                            o,
                            "{id}\t{}\t{}\t{}\t{}\t230\t16\tf\t{c}",
                            time_text(t + i * 60_000),
                            50 + i,
                            i * 2,
                            11 - i
                        )
                        .unwrap();
                    }
                }
                o.push_str("\\.\n\n");
                copy_header(
                    &mut o,
                    "charging_processes",
                    &[
                        "id",
                        "start_date",
                        "end_date",
                        "charge_energy_added",
                        "start_battery_level",
                        "end_battery_level",
                        "car_id",
                        "position_id",
                    ],
                );
                for c in 1..=s.cars {
                    let t = s.position_time(s.positions) + 60_000;
                    let p = s.position_id(c, s.positions - 1);
                    writeln!(o, "{c}\t{}\t{}\t4.00\t50\t52\t{c}\t{p}", time_text(t), time_text(t + 180_000)).unwrap();
                }
                o.push_str("\\.\n\n");
                copy_header(
                    &mut o,
                    "drives",
                    &[
                        "id",
                        "start_date",
                        "end_date",
                        "distance",
                        "car_id",
                        "start_position_id",
                        "end_position_id",
                        "start_rated_range_km",
                        "end_rated_range_km",
                    ],
                );
                for c in 1..=s.cars {
                    for d in 0..s.drives {
                        let id = (c - 1) * s.drives + d + 1;
                        let (j0, j1) = (d * 10, d * 10 + 9);
                        writeln!(
                            o,
                            "{id}\t{}\t{}\t5.0\t{c}\t{}\t{}\t300.00\t295.00",
                            time_text(s.position_time(j0)),
                            time_text(s.position_time(j1)),
                            s.position_id(c, j0),
                            s.position_id(c, j1)
                        )
                        .unwrap();
                    }
                }
                o.push_str("\\.\n\n");
                copy_header(
                    &mut o,
                    "positions",
                    &[
                        "id",
                        "date",
                        "latitude",
                        "longitude",
                        "speed",
                        "power",
                        "odometer",
                        "battery_level",
                        "car_id",
                        "drive_id",
                        "rated_battery_range_km",
                    ],
                );
                self.stage = 1;
                self.next = 1;
            }
            1 => {
                let total = s.cars * s.positions;
                let end = (self.next + 2000).min(total + 1);
                for k in self.next..end {
                    let (c, j) = s.position_of(k);
                    let drive =
                        if j < s.drives * 10 { format!("{}", (c - 1) * s.drives + j / 10 + 1) } else { "\\N".into() };
                    writeln!(
                        o,
                        "{k}\t{}\t{:.6}\t{:.6}\t{}\t{}\t{:.1}\t{}\t{c}\t{drive}\t{:.2}",
                        time_text(s.position_time(j)),
                        12.3 + (j % 1000) as f64 * 1e-5,
                        -45.6 - (c % 1000) as f64 * 1e-5,
                        j % 120,
                        (j % 50) as i64 - 10,
                        10_000.0 + j as f64 * 0.5,
                        80 - (j % 60),
                        300.0 - (j % 100) as f64 * 0.5
                    )
                    .unwrap();
                }
                self.next = end;
                if end > total {
                    o.push_str("\\.\n\n");
                    self.stage = 2;
                }
            }
            2 => {
                o.push_str("COPY public.settings (id, unit_of_length) FROM stdin;\n1\tkm\n\\.\n\n");
                o.push_str("COPY public.states (id, state, start_date, end_date, car_id) FROM stdin;\n");
                for c in 1..=s.cars {
                    let t0 = BASE_MS - 3_600_000;
                    let t1 = s.position_time(s.positions) + 3_600_000;
                    let base = (c - 1) * 3;
                    writeln!(o, "{}\tonline\t{}\t{}\t{c}", base + 1, time_text(t0), time_text(t1)).unwrap();
                    writeln!(o, "{}\tasleep\t{}\t{}\t{c}", base + 2, time_text(t1), time_text(t1 + 3_600_000)).unwrap();
                    writeln!(o, "{}\tonline\t{}\t\\N\t{c}", base + 3, time_text(t1 + 3_600_000)).unwrap();
                }
                o.push_str("\\.\n\nCOPY public.updates (id, start_date, end_date, version, car_id) FROM stdin;\n");
                for c in 1..=s.cars {
                    writeln!(
                        o,
                        "{c}\t{}\t{}\t2024.2.7 abc\t{c}",
                        time_text(BASE_MS - 86_400_000),
                        time_text(BASE_MS - 84_000_000)
                    )
                    .unwrap();
                }
                o.push_str("\\.\n\n--\n-- PostgreSQL database dump complete\n--\n\n");
                self.stage = 3;
            }
            _ => return false,
        }
        self.buf = o.into_bytes();
        self.at = 0;
        true
    }
}

impl std::io::Read for SynthReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.at == self.buf.len() {
            if !self.fill() {
                return Ok(0);
            }
        }
        let n = out.len().min(self.buf.len() - self.at);
        out[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// Counts what it is given and keeps nothing (for the scale tests), checking batches stay within their size.
#[derive(Default)]
pub struct Count {
    pub batch_rows: usize,
    pub per_car: BTreeMap<CarId, [u64; 6]>,
    pub batches: u64,
    pub largest_batch: usize,
    pub places: u64,
    pub addresses: u64,
    pub reports: u64,
    pub states: BTreeMap<CarId, ImportState>,
    /// First and last position time per car, to check order.
    pub last_position_ms: BTreeMap<CarId, u64>,
    pub out_of_order: u64,
}

impl Sink for Count {
    fn car(&mut self, car: &TmCar) -> Result<Option<Target>, SinkError> {
        let id = CarId::try_from(car.tm_id).unwrap() + 1000;
        Ok(Some(Target { car: id, state: self.states.get(&id).cloned().unwrap_or_default() }))
    }

    fn batch(&mut self, car: CarId, batch: Batch) -> Result<(), SinkError> {
        assert!(!batch.is_empty() && (self.batch_rows == 0 || batch.len() <= self.batch_rows));
        self.batches += 1;
        self.largest_batch = self.largest_batch.max(batch.len());
        let c = self.per_car.entry(car).or_default();
        let i = match &batch {
            Batch::Positions(v) => {
                let last = self.last_position_ms.entry(car).or_default();
                for p in v {
                    if p.sample.t_ms < *last {
                        self.out_of_order += 1;
                    }
                    *last = p.sample.t_ms;
                }
                0
            }
            Batch::Drives(_) => 1,
            Batch::Charges(_) => 2,
            Batch::ChargePoints(_) => 3,
            Batch::States(_) => 4,
            Batch::Updates(_) => 5,
            Batch::Removed(_) | Batch::Cleared(_) => return Ok(()),
        };
        c[i] += batch.len() as u64;
        Ok(())
    }

    fn places(&mut self, rows: Vec<PlaceRow>) -> Result<(), SinkError> {
        self.places += rows.len() as u64;
        Ok(())
    }

    fn addresses(&mut self, rows: Vec<AddressRow>) -> Result<(), SinkError> {
        self.addresses += rows.len() as u64;
        Ok(())
    }

    fn car_done(&mut self, report: CarReport, state: Option<ImportState>) -> Result<(), SinkError> {
        self.reports += 1;
        if let (Some(car), Some(state)) = (report.car, state) {
            self.states.insert(car, state);
        }
        Ok(())
    }
}

/// The process's peak memory so far (Linux), bytes.
pub fn peak_rss() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let l = s.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kb: u64 = l.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// The process's memory now (Linux), bytes.
pub fn rss() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let l = s.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: u64 = l.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// Starts measuring the process's peak memory afresh (Linux): the peak becomes what is in use now. False where that
/// can't be done (then the peak counts what came before too).
pub fn reset_peak() -> bool {
    std::fs::write("/proc/self/clear_refs", "5").is_ok()
}
