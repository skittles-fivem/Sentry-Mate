//! The first read: the cars (each asked about as its row is read), and the links the second read needs (address
//! names, which car each charge is, its AC/DC and top power, and the positions at drive ends and charge places).
//! Nothing is handed over yet, so a backup that turns out broken changes nothing but the cars asked about. The big
//! tables are barely read here: positions only when a wanted one may be among them, charge points for three fields.

use crate::copy::{CopyHeader, Handler, RawRow, Take};
use crate::links::{AddressBook, Ends, Procs, Route, Routes, Slot};
use crate::map;
use crate::report::CarTally;
use crate::rows::{Sink, TmCar};
use crate::schema::{Addr, Car, CarSetting, Chg, Class, ColumnMap, Cp, Drv, Pos, St, Table, classify};
use crate::state::STATE_VERSION;
use crate::value::{Val, timestamp_ms};
use crate::{Error, Options};
use model::CarId;
use std::collections::HashMap;

pub(crate) struct Index<'s> {
    sink: &'s mut dyn Sink,
    pub routes: Routes,
    pub labels: AddressBook,
    pub procs: Procs,
    pub ends: Ends,
    settings: HashMap<i64, (Option<bool>, Option<bool>)>,
    /// While the cars table is read: how many cars have each VIN prefix, and the slot of each Sentry Mate car.
    prefixes: HashMap<String, usize>,
    slot_of: HashMap<CarId, usize>,
    current: Option<ColumnMap>,
}

impl<'s> Index<'s> {
    pub fn new(sink: &'s mut dyn Sink, o: &Options) -> Self {
        Index {
            sink,
            routes: Routes::default(),
            labels: AddressBook::new(o.max_address_bytes),
            procs: Procs::new(o.max_charges),
            ends: Ends::new(o.max_endpoints),
            settings: HashMap::new(),
            prefixes: HashMap::new(),
            slot_of: HashMap::new(),
            current: None,
        }
    }

    pub fn into_parts(mut self) -> (&'s mut dyn Sink, Routes, AddressBook, Procs, Ends) {
        self.procs.settle();
        (self.sink, self.routes, self.labels, self.procs, self.ends)
    }

    /// A car of the cars table: asked about at once, so no list of every car is kept.
    fn ask(&mut self, c: TmCar, unreadable_vin: bool) -> Result<(), Error> {
        if self.routes.by_tm.contains_key(&c.tm_id) {
            return Ok(());
        }
        let report = self.routes.reports.len();
        let mut r = CarTally {
            tm_car_id: c.tm_id,
            vin_prefix: c.vin.as_ref().map(|v| v.vin_prefix.clone()),
            name: c.name.clone(),
            ..CarTally::default()
        };
        if unreadable_vin {
            r.warnings.push("its VIN isn't readable".into());
        }
        if let Some(p) = &r.vin_prefix {
            *self.prefixes.entry(p.clone()).or_default() += 1;
        }
        let route = match self.sink.car(&c).map_err(Error::Sink)? {
            Some(t) => {
                if t.state.version != STATE_VERSION {
                    return Err(Error::StateVersion(t.state.version));
                }
                r.car = Some(t.car);
                let slot = match self.slot_of.get(&t.car) {
                    Some(&s) => s,
                    None => {
                        self.routes.slots.push(Slot { car: t.car, state: t.state, report, work: Default::default() });
                        self.slot_of.insert(t.car, self.routes.slots.len() - 1);
                        self.routes.slots.len() - 1
                    }
                };
                Route::To { slot, report, efficiency: c.efficiency_wh_km.map(|e| e / 1000.0) }
            }
            None => Route::LeftOut { report },
        };
        self.routes.reports.push(r);
        self.routes.by_tm.insert(c.tm_id, route);
        Ok(())
    }

    /// The cars table ended: every car was asked about.
    fn cars_done(&mut self) {
        let prefixes = std::mem::take(&mut self.prefixes);
        for r in &mut self.routes.reports {
            if r.vin_prefix.as_ref().is_some_and(|p| prefixes.get(p).copied().unwrap_or(0) > 1) {
                r.warnings.push("another car of this backup has the same VIN prefix: match it by name".into());
            }
        }
        self.slot_of = HashMap::new();
        self.routes.known = true;
    }

    fn row_of(&mut self, map: &ColumnMap, raw: &RawRow<'_>) -> Result<(), Error> {
        let row = map.row(raw);
        match map.table {
            Table::CarSettings => {
                if let Ok(id) = row.req_i64(CarSetting::id) {
                    self.settings
                        .insert(id, (row.bool(CarSetting::lfp_battery), row.bool(CarSetting::free_supercharging)));
                }
            }
            Table::Cars => {
                if let Ok(id) = row.req_i64(Car::id) {
                    let settings = row.i64(Car::settings_id).and_then(|s| self.settings.get(&s).copied());
                    let (car, unreadable_vin) = map::car(&row, id, settings);
                    return self.ask(car, unreadable_vin);
                }
            }
            Table::Addresses => {
                if let Ok(id) = row.req_i64(Addr::id) {
                    let a = map::address(&row, id);
                    self.labels.add(id, &a.label);
                }
            }
            Table::ChargingProcesses => {
                let (Ok(id), Ok(car)) = (row.req_i64(Cp::id), row.req_i64(Cp::car_id)) else { return Ok(()) };
                if let Some(p) = self.procs.entry(id) {
                    p.car = car;
                }
                self.newest(car, &row, &[Cp::start_date, Cp::end_date]);
                let closed = matches!(row.val(Cp::end_date, timestamp_ms), Val::Ok(_));
                if closed
                    && self.routes.may_take(Table::ChargingProcesses, car, id)
                    && let Some(p) = row.i64(Cp::position_id)
                {
                    self.ends.want(p);
                }
            }
            Table::Drives => {
                let (Ok(id), Ok(car)) = (row.req_i64(Drv::id), row.req_i64(Drv::car_id)) else { return Ok(()) };
                self.newest(car, &row, &[Drv::start_date, Drv::end_date]);
                let closed = matches!(row.val(Drv::end_date, timestamp_ms), Val::Ok(_));
                if closed && self.routes.may_take(Table::Drives, car, id) {
                    for c in [Drv::start_position_id, Drv::end_position_id] {
                        if let Some(p) = row.i64(c) {
                            self.ends.want(p);
                        }
                    }
                }
            }
            Table::Charges => {
                let Ok(cp) = row.req_i64(Chg::charging_process_id) else { return Ok(()) };
                let power = row.f64(Chg::charger_power);
                let fast = row.bool(Chg::fast_charger_present);
                let at = row.ms(Chg::date);
                if let Some(p) = self.procs.entry(cp) {
                    p.last_ms = p.last_ms.max(at.unwrap_or(0));
                    if let Some(w) = power {
                        let w = w as f32;
                        if p.power_max.is_nan() || w > p.power_max {
                            p.power_max = w;
                        }
                    }
                    match fast {
                        Some(true) => p.kind = 2,
                        Some(false) => p.kind = p.kind.max(1),
                        None => {}
                    }
                }
            }
            Table::Positions => {
                if let Ok(id) = row.req_i64(Pos::id)
                    && self.ends.wants(id)
                    && let Some(p) = map::endpoint(&row)
                {
                    self.ends.fill(id, &p);
                }
            }
            Table::States => {
                let Ok(car) = row.req_i64(St::car_id) else { return Ok(()) };
                self.newest(car, &row, &[St::start_date, St::end_date]);
                if let Some(t) = row.ms(St::start_date) {
                    let l = self.procs.last_state.entry(car).or_default();
                    *l = (*l).max(t);
                }
            }
            Table::Geofences | Table::Updates => {}
        }
        Ok(())
    }

    /// The newest of these times of the car so far.
    fn newest<C: crate::schema::Columns + Copy>(&mut self, car: i64, row: &crate::schema::Row<'_, '_>, cols: &[C]) {
        for &c in cols {
            if let Some(t) = row.ms(c) {
                self.procs.saw(car, t);
            }
        }
    }
}

impl Handler for Index<'_> {
    type Error = Error;

    fn begin(&mut self, header: &CopyHeader) -> Result<Take, Error> {
        self.current = None;
        let Class::Ours(t) = classify(header) else { return Ok(Take::Skip) };
        let map = ColumnMap::new(t, header);
        if !map.missing_required.is_empty() {
            return Ok(Take::Skip);
        }
        let take = match t {
            Table::Cars => !self.routes.known,
            Table::CarSettings | Table::Addresses | Table::Drives | Table::ChargingProcesses | Table::Charges => true,
            Table::Positions => self.ends.any_wanted(),
            Table::States => true,
            Table::Geofences | Table::Updates => false,
        };
        if !take {
            return Ok(Take::Skip);
        }
        self.current = Some(map);
        Ok(Take::Rows)
    }

    fn row(&mut self, raw: &RawRow<'_>) -> Result<(), Error> {
        let Some(map) = self.current.take() else { return Ok(()) };
        let r = if raw.fields() == map.width { self.row_of(&map, raw) } else { Ok(()) };
        self.current = Some(map);
        r
    }

    fn too_long(&mut self) {}

    fn end(&mut self) -> Result<(), Error> {
        if self.current.take().is_some_and(|m| m.table == Table::Cars) {
            self.cars_done();
        }
        Ok(())
    }
}
