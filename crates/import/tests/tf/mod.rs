//! Made-up TeslaFi monthly files (the columns TeslaFi's raw data download has, values chosen), and a sink that keeps
//! rows as a store would: by car, table and id, replacing, with removals applied. Positions are in the open ocean;
//! VINs end in PF0000xx.
#![allow(dead_code, clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use chrono::{Duration, NaiveDate, NaiveDateTime};
use import::{Batch, CarReport, ImportState, Sink, SinkError, Table, Target, TmCar};
use model::CarId;
use std::collections::BTreeMap;

pub const VIN1: &str = "5YJ3E1EA1PF000001";
pub const VIN2: &str = "7SAYGDEE5PF000002";

/// TeslaFi's columns, in an order like its own (more columns than the importer reads, as TeslaFi's files have).
pub const HEADER: &str = "data_id,Date,calendar_enabled,remote_start_enabled,state,carState,shift_state,\
charging_state,latitude,longitude,heading,elevation,speed,power,odometer,odometerF,battery_level,\
usable_battery_level,battery_range,ideal_battery_range,est_battery_range,charge_energy_added,charger_power,\
charger_voltage,charger_actual_current,charger_phases,fast_charger_present,fast_charger_type,inside_temp,\
outside_temp,inside_tempF,outside_tempF,car_version,vin,display_name,vehicle_id,measure,gui_distance_units,\
gui_temperature_units,idleNumber,sleepNumber,driveNumber,chargeNumber";

/// How the made-up account shows units.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shows {
    /// km and °C: `odometerF` is km beside the base miles, `inside_tempF` °C beside the base °C.
    Metric,
    /// miles and °F: `odometerF` is miles too (the same), `inside_tempF` °F.
    Imperial,
}

/// What the car does at a poll.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Doing {
    Asleep,
    Parked,
    /// mph.
    Drive(f64),
    /// kW, DC?
    Charge(f64, bool),
    ChargeDone,
}

pub struct Gen {
    pub lines: Vec<String>,
    pub id: u64,
    pub odo_mi: f64,
    pub soc: f64,
    pub added: f64,
    pub version: String,
    pub vin: String,
    pub name: String,
    pub shows: Shows,
    pub lat: f64,
    pub lon: f64,
    /// Extra empty columns per row (to make big files).
    pub pad: usize,
}

impl Gen {
    pub fn new(vin: &str) -> Gen {
        Gen {
            lines: Vec::new(),
            id: 91_000_000,
            odo_mi: 12_345.0,
            soc: 80.0,
            added: 0.0,
            version: "2023.2.10 ac3b2a1c".into(),
            vin: vin.into(),
            name: "Blue Whale".into(),
            shows: Shows::Metric,
            lat: -30.5,
            lon: -140.2,
            pad: 0,
        }
    }

    pub fn header(&self) -> String {
        let mut h = HEADER.to_string();
        for i in 0..self.pad {
            h.push_str(&format!(",extra_{i}"));
        }
        h
    }

    /// One poll at local wall time `t`.
    pub fn poll(&mut self, t: NaiveDateTime, doing: Doing) {
        let l = self.line(t, doing);
        self.lines.push(l);
    }

    pub fn line(&mut self, t: NaiveDateTime, doing: Doing) -> String {
        self.id += 1;
        let (state, car_state, shift, charging) = match doing {
            Doing::Asleep => ("asleep", "Sleeping", "None", "Disconnected"),
            Doing::Parked => ("online", "Idling", "P", "Disconnected"),
            Doing::Drive(_) => ("online", "Driving", "D", "Disconnected"),
            Doing::Charge(..) => ("online", "Charging", "P", "Charging"),
            Doing::ChargeDone => ("online", "Idling", "P", "Complete"),
        };
        let mut speed = "None".to_string();
        let mut power = "0".to_string();
        let (mut cpower, mut volts, mut amps, mut phases, mut fast, mut ftype) =
            ("0".to_string(), "0".to_string(), "0".to_string(), "None", "False", "<invalid>");
        match doing {
            Doing::Drive(mph) => {
                speed = format!("{mph:.0}");
                power = "18".into();
                self.odo_mi += mph / 60.0;
                self.soc -= 0.2;
                self.lat += 0.005;
                self.added = 0.0;
            }
            Doing::Charge(kw, dc) => {
                cpower = format!("{kw:.0}");
                self.added += kw / 60.0;
                self.soc = (self.soc + kw / 60.0 / 0.75).min(100.0);
                power = format!("{:.0}", -kw);
                if dc {
                    fast = "True";
                    ftype = "Supercharger";
                    volts = "390.0".into();
                    amps = format!("{:.1}", kw * 1000.0 / 390.0);
                } else {
                    volts = "235.4".into();
                    amps = format!("{:.1}", kw * 1000.0 / 235.4 / 3.0);
                    phases = "3";
                }
            }
            _ => {}
        }
        let date = t.format("%Y-%m-%d %H:%M:%S").to_string();
        let odo_f = match self.shows {
            Shows::Metric => self.odo_mi * 1.609_344,
            Shows::Imperial => self.odo_mi,
        };
        let (in_c, out_c) = (21.0, 12.0);
        let (in_f, out_f, measure, gui_d, gui_t) = match self.shows {
            Shows::Metric => (in_c, out_c, "metric", "km/hr", "C"),
            Shows::Imperial => (in_c * 9.0 / 5.0 + 32.0, out_c * 9.0 / 5.0 + 32.0, "US", "mi/hr", "F"),
        };
        let range = self.soc * 3.1;
        let mut s = format!(
            "{id},{date},False,False,{state},{car_state},{shift},{charging},{lat:.6},{lon:.6},90,12,{speed},{power},\
             {odo:.3},{odo_f:.3},{soc:.0},{soc:.0},{range:.2},999.0,{range:.2},{added:.2},{cpower},{volts},{amps},\
             {phases},{fast},{ftype},{in_c},{out_c},{in_f},{out_f},{version},{vin},\"{name}\",1234567,{measure},{gui_d},\
             {gui_t},0,0,0,0",
            id = self.id,
            lat = self.lat,
            lon = self.lon,
            odo = self.odo_mi,
            soc = self.soc,
            added = self.added,
            version = self.version,
            vin = self.vin,
            name = self.name,
        );
        for _ in 0..self.pad {
            s.push(',');
        }
        s
    }

    /// One made-up day from local midnight: asleep until 07:00, a drive at 08:00 (30 min, 40 mph), an AC charge at
    /// 09:00 (90 min, 11 kW, then Complete), a drive at 17:00 (20 min, 60 mph), a DC charge at 17:30 (30 min,
    /// 120 kW), asleep from 20:00. Polls once a minute while awake, every 30 min asleep.
    pub fn day(&mut self, d: NaiveDate) {
        let at = |h: u32, m: u32| d.and_hms_opt(h, m, 0).unwrap();
        for h in 0..7 {
            self.poll(at(h, 0), Doing::Asleep);
            self.poll(at(h, 30), Doing::Asleep);
        }
        for m in (0..60).step_by(5) {
            self.poll(at(7, m), Doing::Parked);
        }
        for m in 0..30 {
            self.poll(at(8, m), Doing::Drive(40.0));
        }
        for m in 30..60 {
            self.poll(at(8, m), Doing::Parked);
        }
        for i in 0..90 {
            self.poll(at(9, 0) + Duration::minutes(i), Doing::Charge(11.0, false));
        }
        self.poll(at(10, 30), Doing::ChargeDone);
        self.added = 0.0;
        for h in 11..17 {
            self.poll(at(h, 0), Doing::Parked);
            self.poll(at(h, 30), Doing::Parked);
        }
        for m in 0..20 {
            self.poll(at(17, m), Doing::Drive(60.0));
        }
        for m in 20..30 {
            self.poll(at(17, m), Doing::Parked);
        }
        for m in 30..60 {
            self.poll(at(17, m), Doing::Charge(120.0, true));
        }
        self.poll(at(18, 0), Doing::ChargeDone);
        self.added = 0.0;
        self.poll(at(19, 0), Doing::Parked);
        for h in 20..24 {
            self.poll(at(h, 0), Doing::Asleep);
            self.poll(at(h, 30), Doing::Asleep);
        }
    }

    /// The file so far (header and rows).
    pub fn csv(&self) -> String {
        let mut s = self.header();
        s.push_str("\r\n");
        for l in &self.lines {
            s.push_str(l);
            s.push_str("\r\n");
        }
        s
    }

    /// Takes the rows so far as a file and starts the next one.
    pub fn take_csv(&mut self) -> String {
        let s = self.csv();
        self.lines.clear();
        s
    }
}

/// A month of days, as TeslaFi's `TeslaFi<M><YYYY>.csv`.
pub fn month(g: &mut Gen, y: i32, m: u32) -> String {
    let mut d = NaiveDate::from_ymd_opt(y, m, 1).unwrap();
    while d.month0() + 1 == m {
        if d.day() == 15 {
            g.version = "2023.6.9 9f3e2d1b".into();
        }
        g.day(d);
        d = d.succ_opt().unwrap();
    }
    g.take_csv()
}

use chrono::Datelike;

/// Keeps rows as a store would (by car, table and id; removals applied) and the states between imports.
#[derive(Default)]
pub struct Store {
    /// VIN prefix → car; others are left out.
    pub cars: BTreeMap<String, CarId>,
    /// Where a car without a VIN goes.
    pub no_vin: Option<CarId>,
    /// Where a car the file asks for by number goes, whatever its VIN (a check's plan, as `/imports` keeps it).
    pub plan: BTreeMap<i64, Option<CarId>>,
    pub zones: BTreeMap<CarId, String>,
    pub states: BTreeMap<CarId, ImportState>,
    pub drives: BTreeMap<(CarId, i64), model::Drive>,
    pub charges: BTreeMap<(CarId, i64), model::Charge>,
    pub spans: BTreeMap<(CarId, i64), (model::StateSpan, bool)>,
    pub updates: BTreeMap<(CarId, i64), model::Update>,
    pub positions: BTreeMap<(CarId, u64), model::Sample>,
    pub points: BTreeMap<(CarId, u64), model::ChargePoint>,
    /// Rows handed over by the last import, by kind.
    pub handed: BTreeMap<&'static str, u64>,
    pub reports: Vec<CarReport>,
    pub asked: Vec<TmCar>,
}

impl Store {
    pub fn new(cars: &[(&str, CarId)]) -> Store {
        Store { cars: cars.iter().map(|(v, c)| (v[..11].to_string(), *c)).collect(), ..Store::default() }
    }

    pub fn clear(&mut self) {
        self.handed.clear();
        self.reports.clear();
        self.asked.clear();
    }

    pub fn handed(&self) -> u64 {
        self.handed.values().sum()
    }

    pub fn drives_of(&self, car: CarId) -> Vec<&model::Drive> {
        self.drives.iter().filter(|((c, _), _)| *c == car).map(|(_, d)| d).collect()
    }

    pub fn charges_of(&self, car: CarId) -> Vec<&model::Charge> {
        self.charges.iter().filter(|((c, _), _)| *c == car).map(|(_, d)| d).collect()
    }
}

impl Sink for Store {
    fn car(&mut self, car: &TmCar) -> Result<Option<Target>, SinkError> {
        self.asked.push(car.clone());
        if let Some(to) = self.plan.get(&car.tm_id) {
            let Some(id) = *to else { return Ok(None) };
            return Ok(Some(Target { car: id, state: self.states.get(&id).cloned().unwrap_or_default() }));
        }
        let id = match &car.vin {
            Some(v) => self.cars.get(&v.vin_prefix).copied(),
            None => self.no_vin,
        };
        let Some(id) = id else { return Ok(None) };
        Ok(Some(Target { car: id, state: self.states.get(&id).cloned().unwrap_or_default() }))
    }

    fn batch(&mut self, car: CarId, batch: Batch) -> Result<(), SinkError> {
        assert!(!batch.is_empty());
        let kind = match &batch {
            Batch::Positions(_) => "positions",
            Batch::Drives(_) => "drives",
            Batch::Charges(_) => "charges",
            Batch::ChargePoints(_) => "charge_points",
            Batch::States(_) => "states",
            Batch::Updates(_) => "updates",
            Batch::Removed(_) => "removed",
            Batch::Cleared(_) => "cleared",
        };
        *self.handed.entry(kind).or_default() += batch.len() as u64;
        match batch {
            Batch::Positions(v) => {
                for p in v {
                    self.positions.insert((car, p.sample.t_ms), p.sample);
                }
            }
            Batch::ChargePoints(v) => {
                for p in v {
                    self.points.insert((car, p.point.t_ms), p.point);
                }
            }
            Batch::Drives(v) => {
                for d in v {
                    self.drives.insert((car, d.tm_id), d.drive);
                }
            }
            Batch::Charges(v) => {
                for c in v {
                    self.charges.insert((car, c.tm_id), c.charge);
                }
            }
            Batch::States(v) => {
                for s in v {
                    self.spans.insert((car, s.tm_id), (s.span, s.offline));
                }
            }
            Batch::Updates(v) => {
                for u in v {
                    self.updates.insert((car, u.tm_id), u.update);
                }
            }
            Batch::Removed(v) => {
                for (t, id) in v {
                    let gone = match t {
                        Table::Drives => self.drives.remove(&(car, id)).is_some(),
                        Table::ChargingProcesses => self.charges.remove(&(car, id)).is_some(),
                        Table::States => self.spans.remove(&(car, id)).is_some(),
                        Table::Updates => self.updates.remove(&(car, id)).is_some(),
                        _ => false,
                    };
                    assert!(gone, "removed {t:?} {id}, which was not stored");
                }
            }
            Batch::Cleared(v) => {
                for (a, b) in v {
                    let at = |t: u64| t >= a && t <= b;
                    self.positions.retain(|(c, t), _| *c != car || !at(*t));
                    self.points.retain(|(c, t), _| *c != car || !at(*t));
                    self.drives.retain(|(c, _), d| *c != car || !at(d.start_ms));
                    self.charges.retain(|(c, _), d| *c != car || !at(d.start_ms));
                    self.spans.retain(|(c, _), (d, _)| *c != car || !at(d.start_ms));
                    self.updates.retain(|(c, _), d| *c != car || !at(d.start_ms));
                }
            }
        }
        Ok(())
    }

    fn places(&mut self, _rows: Vec<import::PlaceRow>) -> Result<(), SinkError> {
        Ok(())
    }

    fn addresses(&mut self, _rows: Vec<import::AddressRow>) -> Result<(), SinkError> {
        Ok(())
    }

    fn car_done(&mut self, report: CarReport, state: Option<ImportState>) -> Result<(), SinkError> {
        if let (Some(car), Some(state)) = (report.car, state) {
            // the state goes through JSON, as a store keeps it
            let j = serde_json::to_string(&state).unwrap();
            self.states.insert(car, serde_json::from_str(&j).unwrap());
        }
        self.reports.push(report);
        Ok(())
    }

    fn time_zone(&mut self, car: CarId) -> Option<String> {
        self.zones.get(&car).cloned()
    }
}

pub fn local(s: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
}
