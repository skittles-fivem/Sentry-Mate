//! TeslaMate's rows as Sentry Mate's types. Units need no converting: TeslaMate keeps km, km/h, °C, kW, kWh, bar and
//! m whatever it shows, and its times in UTC. What it lacks stays empty.

use crate::links::{AddressBook, Ends, Proc};
use crate::rows::{
    AddressRow, ChargePointRow, ChargeRefs, ChargeRow, DriveRefs, DriveRow, MAPPING_VERSION, PlaceRow, PositionRow,
    StateRow, TmCar, UpdateRow,
};
use crate::schema::{Addr, Car, Chg, Cp, Drv, Geo, Pos, Row, St, Upd};
use crate::value::{Val, timestamp_ms};
use crate::vin::vin_info;
use model::{
    CarId, Charge, ChargeKind, ChargePoint, DEFAULT_RADIUS_M, Drive, MAX_RADIUS_M, MIN_RADIUS_M, Place, PlaceKind,
    Position, PricePlan, Sample, Source, State, StateSpan, Update,
};

/// What became of a row.
pub(crate) enum Mapped<T> {
    Take(T),
    /// No end yet.
    Open,
    /// A value it needs is missing or unreadable.
    Bad(&'static str),
}

impl<T> Mapped<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Mapped<U> {
        match self {
            Mapped::Take(v) => Mapped::Take(f(v)),
            Mapped::Open => Mapped::Open,
            Mapped::Bad(c) => Mapped::Bad(c),
        }
    }
}

macro_rules! need {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(col) => return Mapped::Bad(col),
        }
    };
}

/// The end of a drive, charge, state or update: `Err(true)` when there is none yet (still open), `Err(false)` when
/// it can't be read or comes before the start.
fn end_ms<C: crate::schema::Columns>(row: &Row<'_, '_>, c: C, start: u64) -> Result<u64, bool> {
    match row.val(c, timestamp_ms) {
        Val::Ok(e) if e >= start => Ok(e),
        Val::Null => Err(true),
        _ => Err(false),
    }
}

fn in_range(lat: f64, lon: f64) -> Result<(), &'static str> {
    if !(-90.0..=90.0).contains(&lat) {
        return Err("latitude");
    }
    if !(-180.0..=180.0).contains(&lon) {
        return Err("longitude");
    }
    Ok(())
}

pub(crate) fn car(row: &Row<'_, '_>, id: i64, settings: Option<(Option<bool>, Option<bool>)>) -> (TmCar, bool) {
    let vin_text = row.text(Car::vin);
    let vin = vin_text.as_deref().and_then(vin_info);
    let unreadable_vin = vin_text.is_some() && vin.is_none();
    // The full VIN text is dropped here: only what `vin_info` kept goes on.
    drop(vin_text);
    let (lfp_battery, free_supercharging) = settings.unwrap_or((None, None));
    let car = TmCar {
        tm_id: id,
        vin,
        name: row.text(Car::name),
        model: row.text(Car::model),
        trim_badging: row.text(Car::trim_badging),
        marketing_name: row.text(Car::marketing_name),
        exterior_color: row.text(Car::exterior_color),
        wheel_type: row.text(Car::wheel_type),
        efficiency_wh_km: row.f64(Car::efficiency).filter(|e| *e > 0.0).map(|e| e * 1000.0),
        lfp_battery,
        free_supercharging,
        added_ms: row.ms(Car::inserted_at),
    };
    (car, unreadable_vin)
}

/// A position as kept for a drive's end or a charge's place.
pub(crate) fn endpoint(row: &Row<'_, '_>) -> Option<Position> {
    let t = row.req_ms(Pos::date).ok()?;
    let lat = row.req_f64(Pos::latitude).ok()?;
    let lon = row.req_f64(Pos::longitude).ok()?;
    in_range(lat, lon).ok()?;
    Some(Position {
        t_ms: t,
        lat,
        lon,
        elevation_m: row.f64(Pos::elevation),
        speed_kmh: row.f64(Pos::speed),
        power_kw: row.f64(Pos::power),
        soc: row.f64(Pos::battery_level),
        odometer_km: row.f64(Pos::odometer),
        range_km: row.f64(Pos::rated_battery_range_km),
        outside_temp_c: row.f64(Pos::outside_temp),
        inside_temp_c: row.f64(Pos::inside_temp),
    })
}

pub(crate) fn position(row: &Row<'_, '_>, id: i64) -> Mapped<PositionRow> {
    let t = need!(row.req_ms(Pos::date));
    let lat = need!(row.req_f64(Pos::latitude));
    let lon = need!(row.req_f64(Pos::longitude));
    need!(in_range(lat, lon));
    let sample = Sample {
        t_ms: t,
        lat: Some(lat),
        lon: Some(lon),
        elevation_m: row.f64(Pos::elevation),
        speed_kmh: row.f64(Pos::speed),
        odometer_km: row.f64(Pos::odometer),
        soc: row.f64(Pos::battery_level),
        range_km: row.f64(Pos::rated_battery_range_km),
        ideal_range_km: row.f64(Pos::ideal_battery_range_km),
        battery_power_kw: row.f64(Pos::power),
        outside_temp_c: row.f64(Pos::outside_temp),
        inside_temp_c: row.f64(Pos::inside_temp),
        tpms_bar: [
            row.f64(Pos::tpms_pressure_fl),
            row.f64(Pos::tpms_pressure_fr),
            row.f64(Pos::tpms_pressure_rl),
            row.f64(Pos::tpms_pressure_rr),
        ],
        ..Sample::default()
    };
    Mapped::Take(PositionRow {
        tm_id: id,
        tm_drive_id: row.i64(Pos::drive_id),
        sample,
        usable_soc: row.f64(Pos::usable_battery_level),
    })
}

pub(crate) struct DriveCtx<'a> {
    pub labels: &'a AddressBook,
    pub ends: &'a Ends,
    /// The car's kWh per km.
    pub efficiency: Option<f64>,
}

pub(crate) fn drive(
    row: &Row<'_, '_>,
    id: i64,
    car: CarId,
    cx: &DriveCtx<'_>,
    notes: &mut Vec<&'static str>,
) -> Mapped<DriveRow> {
    let start_ms = need!(row.req_ms(Drv::start_date));
    let end_ms = match end_ms(row, Drv::end_date, start_ms) {
        Ok(e) => e,
        Err(true) => return Mapped::Open,
        Err(false) => return Mapped::Bad("end_date"),
    };
    let tm = DriveRefs {
        start_position: row.i64(Drv::start_position_id),
        end_position: row.i64(Drv::end_position_id),
        start_address: row.i64(Drv::start_address_id),
        end_address: row.i64(Drv::end_address_id),
        start_geofence: row.i64(Drv::start_geofence_id),
        end_geofence: row.i64(Drv::end_geofence_id),
    };
    let start = cx.ends.get(tm.start_position);
    let end = cx.ends.get(tm.end_position);
    if (tm.start_position.is_some() && start.is_none()) || (tm.end_position.is_some() && end.is_none()) {
        notes.push("end positions not looked up (find them by time)");
    }
    let start_range_km = row.f64(Drv::start_rated_range_km);
    let end_range_km = row.f64(Drv::end_rated_range_km);
    let energy_used_kwh = match (start_range_km, end_range_km, cx.efficiency) {
        (Some(s), Some(e), Some(eff)) => {
            notes.push("energy estimated from rated range and efficiency");
            Some((s - e) * eff)
        }
        _ => None,
    };
    let drive = Drive {
        id: 0,
        car,
        start_ms,
        end_ms,
        start_soc: start.as_ref().and_then(|p| p.soc),
        end_soc: end.as_ref().and_then(|p| p.soc),
        start,
        end,
        start_place: None,
        end_place: None,
        start_address: cx.labels.get(tm.start_address),
        end_address: cx.labels.get(tm.end_address),
        start_odometer_km: row.f64(Drv::start_km),
        end_odometer_km: row.f64(Drv::end_km),
        distance_km: row.f64(Drv::distance),
        speed_max_kmh: row.f64(Drv::speed_max),
        power_max_kw: row.f64(Drv::power_max),
        power_min_kw: row.f64(Drv::power_min),
        energy_used_kwh,
        start_range_km,
        end_range_km,
        outside_temp_avg_c: row.f64(Drv::outside_temp_avg),
        inside_temp_avg_c: row.f64(Drv::inside_temp_avg),
        ascent_m: row.f64(Drv::ascent),
        descent_m: row.f64(Drv::descent),
        source: Source::Teslamate,
        rules_version: MAPPING_VERSION,
    };
    Mapped::Take(DriveRow { tm_id: id, drive, tm })
}

pub(crate) struct ChargeCtx<'a> {
    pub labels: &'a AddressBook,
    pub ends: &'a Ends,
    pub proc: Option<Proc>,
}

pub(crate) fn charge(
    row: &Row<'_, '_>,
    id: i64,
    car: CarId,
    cx: &ChargeCtx<'_>,
    notes: &mut Vec<&'static str>,
) -> Mapped<ChargeRow> {
    let start_ms = need!(row.req_ms(Cp::start_date));
    let end_ms = match end_ms(row, Cp::end_date, start_ms) {
        Ok(e) => e,
        Err(true) => return Mapped::Open,
        Err(false) => return Mapped::Bad("end_date"),
    };
    let tm = ChargeRefs {
        position: row.i64(Cp::position_id),
        address: row.i64(Cp::address_id),
        geofence: row.i64(Cp::geofence_id),
    };
    let position = cx.ends.get(tm.position);
    if tm.position.is_some() && position.is_none() {
        notes.push("place position not looked up (find it by time)");
    }
    let kind = match cx.proc.map(|p| p.kind) {
        Some(2) => Some(ChargeKind::Dc),
        Some(1) => Some(ChargeKind::Ac),
        _ => None,
    };
    let charge = Charge {
        id: 0,
        car,
        start_ms,
        end_ms,
        position,
        place: None,
        address: cx.labels.get(tm.address),
        kind,
        energy_added_kwh: row.f64(Cp::charge_energy_added),
        energy_used_kwh: row.f64(Cp::charge_energy_used),
        start_soc: row.f64(Cp::start_battery_level),
        end_soc: row.f64(Cp::end_battery_level),
        start_range_km: row.f64(Cp::start_rated_range_km),
        end_range_km: row.f64(Cp::end_rated_range_km),
        power_max_kw: cx.proc.and_then(|p| p.power_max_kw()),
        outside_temp_avg_c: row.f64(Cp::outside_temp_avg),
        battery_temp_avg_c: None,
        cost: row.f64(Cp::cost),
        source: Source::Teslamate,
        rules_version: MAPPING_VERSION,
    };
    Mapped::Take(ChargeRow { tm_id: id, charge, tm })
}

pub(crate) fn charge_point(row: &Row<'_, '_>, id: i64, charge_id: i64) -> Mapped<ChargePointRow> {
    let t = need!(row.req_ms(Chg::date));
    let point = ChargePoint {
        t_ms: t,
        power_kw: row.f64(Chg::charger_power),
        voltage_v: row.f64(Chg::charger_voltage),
        current_a: row.f64(Chg::charger_actual_current),
        soc: row.f64(Chg::battery_level),
        battery_temp_c: None,
        range_km: row.f64(Chg::rated_battery_range_km),
    };
    Mapped::Take(ChargePointRow {
        tm_id: id,
        tm_charge_id: charge_id,
        point,
        fast_charger: row.bool(Chg::fast_charger_present),
        phases: row.i64(Chg::charger_phases).and_then(|p| u8::try_from(p).ok()),
        energy_added_kwh: row.f64(Chg::charge_energy_added),
        outside_temp_c: row.f64(Chg::outside_temp),
        ideal_range_km: row.f64(Chg::ideal_battery_range_km),
        usable_soc: row.f64(Chg::usable_battery_level),
    })
}

pub(crate) fn state(row: &Row<'_, '_>, id: i64, car: CarId) -> Mapped<StateRow> {
    let start_ms = need!(row.req_ms(St::start_date));
    let (state, offline) = match row.text(St::state).as_deref() {
        Some("online") => (State::Online, false),
        Some("asleep") => (State::Asleep, false),
        Some("offline") => (State::Unknown, true),
        _ => return Mapped::Bad("state"),
    };
    let end_ms = match end_ms(row, St::end_date, start_ms) {
        Ok(e) => e,
        Err(true) => return Mapped::Open,
        Err(false) => return Mapped::Bad("end_date"),
    };
    let span = StateSpan { car, state, start_ms, end_ms, buses: Vec::new(), source: Source::Teslamate };
    Mapped::Take(StateRow { tm_id: id, span, offline })
}

pub(crate) fn update(row: &Row<'_, '_>, id: i64, car: CarId) -> Mapped<UpdateRow> {
    let start_ms = need!(row.req_ms(Upd::start_date));
    let end_ms = match end_ms(row, Upd::end_date, start_ms) {
        Ok(e) => e,
        Err(true) => return Mapped::Open,
        Err(false) => return Mapped::Bad("end_date"),
    };
    let Some(version) = row.text(Upd::version) else { return Mapped::Bad("version") };
    Mapped::Take(UpdateRow {
        tm_id: id,
        update: Update { car, start_ms, end_ms: Some(end_ms), version, source: Source::Teslamate },
    })
}

pub(crate) fn place(row: &Row<'_, '_>, id: i64, notes: &mut Vec<&'static str>) -> Result<PlaceRow, &'static str> {
    let lat = row.req_f64(Geo::latitude)?;
    let lon = row.req_f64(Geo::longitude)?;
    in_range(lat, lon)?;
    let name = row.text(Geo::name).unwrap_or_default();
    let kind = match name.trim().to_lowercase().as_str() {
        "home" => PlaceKind::Home,
        "work" => PlaceKind::Work,
        _ => PlaceKind::Other,
    };
    let tm_radius_m = row.f64(Geo::radius);
    let radius_m = match tm_radius_m {
        Some(r) if r < MIN_RADIUS_M => {
            notes.push("radius raised to 50 m");
            MIN_RADIUS_M
        }
        Some(r) if r > MAX_RADIUS_M => {
            notes.push("radius lowered to 2 km");
            MAX_RADIUS_M
        }
        Some(r) => r,
        None => DEFAULT_RADIUS_M,
    };
    let cost = row.f64(Geo::cost_per_unit);
    let session_fee = row.f64(Geo::session_fee);
    let per_minute = row.text(Geo::billing_type).as_deref() == Some("per_minute");
    let (price, per_minute_price) = match (cost, per_minute) {
        (Some(c), false) => {
            (Some(PricePlan { currency: String::new(), per_kwh: c, rates: Vec::new(), session_fee }), None)
        }
        (Some(c), true) => {
            notes.push("per-minute price kept beside the place");
            (None, Some(c))
        }
        (None, _) => (None, None),
    };
    Ok(PlaceRow {
        tm_id: id,
        place: Place { id: 0, name, kind, lat, lon, radius_m, price },
        per_minute_price,
        session_fee,
        tm_radius_m,
    })
}

pub(crate) fn address(row: &Row<'_, '_>, id: i64) -> AddressRow {
    let mut a = AddressRow {
        tm_id: id,
        label: String::new(),
        display_name: row.text(Addr::display_name),
        lat: row.f64(Addr::latitude),
        lon: row.f64(Addr::longitude),
        name: row.text(Addr::name),
        house_number: row.text(Addr::house_number),
        road: row.text(Addr::road),
        neighbourhood: row.text(Addr::neighbourhood),
        city: row.text(Addr::city),
        county: row.text(Addr::county),
        postcode: row.text(Addr::postcode),
        state: row.text(Addr::state),
        state_district: row.text(Addr::state_district),
        country: row.text(Addr::country),
        osm_id: row.i64(Addr::osm_id),
        osm_type: row.text(Addr::osm_type),
    };
    a.label = label(&a);
    a
}

/// A short name for an address: its name, else road and number (else the neighbourhood), then the city.
pub(crate) fn label(a: &AddressRow) -> String {
    let first = a.name.clone().or_else(|| match (&a.road, &a.house_number) {
        (Some(r), Some(n)) => Some(format!("{r} {n}")),
        (Some(r), None) => Some(r.clone()),
        _ => a.neighbourhood.clone(),
    });
    let place = a.city.clone().or_else(|| a.county.clone());
    match (first, place) {
        (Some(f), Some(p)) if f != p => format!("{f}, {p}"),
        (Some(f), _) => f,
        (None, Some(p)) => p,
        (None, None) => a.display_name.clone().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        let mut a =
            AddressRow { name: Some("Test Works".into()), city: Some("Testville".into()), ..Default::default() };
        assert_eq!(label(&a), "Test Works, Testville");
        a.name = None;
        a.road = Some("Made-up Road".into());
        a.house_number = Some("1".into());
        assert_eq!(label(&a), "Made-up Road 1, Testville");
        a.city = None;
        assert_eq!(label(&a), "Made-up Road 1");
        a.road = None;
        a.display_name = Some("Somewhere".into());
        assert_eq!(label(&a), "Somewhere");
        a.city = Some("Testville".into());
        a.neighbourhood = Some("Testville".into());
        assert_eq!(label(&a), "Testville");
    }
}
