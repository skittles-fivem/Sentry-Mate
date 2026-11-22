//! The made-up car's state and its physics: what it would send on its buses at any moment.

use crate::geo::LatLon;
use model::{ChargeKind, Gear};

/// Which car it is: decides its VIN, its battery and how heavy it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CarModel {
    /// Model Y Long Range.
    Y,
    /// Model 3 Standard Range.
    Three,
}

impl CarModel {
    /// The VIN's 4th character.
    pub fn letter(self) -> char {
        match self {
            CarModel::Y => 'Y',
            CarModel::Three => '3',
        }
    }

    /// Nominal full pack energy, kWh.
    pub fn full_kwh(self) -> f64 {
        match self {
            CarModel::Y => 78.0,
            CarModel::Three => 57.5,
        }
    }

    /// Rated range at 100 %, km.
    pub fn range_km(self) -> f64 {
        match self {
            CarModel::Y => 500.0,
            CarModel::Three => 430.0,
        }
    }

    pub fn mass_kg(self) -> f64 {
        match self {
            CarModel::Y => 2000.0,
            CarModel::Three => 1760.0,
        }
    }

    pub fn cda(self) -> f64 {
        match self {
            CarModel::Y => 0.62,
            CarModel::Three => 0.51,
        }
    }

    /// The most a supercharger gives it, kW.
    pub fn dc_max_kw(self) -> f64 {
        match self {
            CarModel::Y => 250.0,
            CarModel::Three => 170.0,
        }
    }
}

/// The charge port and the charger, as the car reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChargeStatus {
    /// Nothing plugged in.
    Disconnected,
    /// Plugged in, waiting (a scheduled charge).
    Waiting,
    Charging,
    /// Plugged in, paused.
    Stopped,
    /// Plugged in, at its limit.
    Complete,
}

/// Everything the car's frames are made from, in Sentry Mate's units (positive battery current out of the pack).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CarState {
    pub t_ms: u64,
    pub pos: LatLon,
    pub elevation_m: f64,
    pub gps_accuracy_m: f64,
    pub speed_kmh: f64,
    pub gear: Gear,
    pub odometer_km: f64,
    pub full_kwh: f64,
    pub energy_kwh: f64,
    pub range_per_pct_km: f64,
    pub pack_v: f64,
    pub pack_a: f64,
    pub batt_temp_c: f64,
    pub outside_c: f64,
    pub cabin_c: f64,
    pub status: ChargeStatus,
    pub plug: Option<ChargeKind>,
    pub ac_v: f64,
    pub ac_a: f64,
    pub dc_v: f64,
    pub dc_a: f64,
    pub total_charge_kwh: f64,
    pub total_discharge_kwh: f64,
    pub lv_v: f64,
    pub tyres_bar: [f64; 4],
}

impl CarState {
    pub fn new(model: CarModel, t_ms: u64, pos: LatLon, soc: f64, odometer_km: f64) -> CarState {
        let full = model.full_kwh();
        let mut s = CarState {
            t_ms,
            pos,
            elevation_m: elevation_at(pos),
            gps_accuracy_m: 3.0,
            speed_kmh: 0.0,
            gear: Gear::P,
            odometer_km,
            full_kwh: full,
            energy_kwh: full * soc / 100.0,
            range_per_pct_km: model.range_km() / 100.0,
            pack_v: 0.0,
            pack_a: 0.0,
            batt_temp_c: 18.0,
            outside_c: 15.0,
            cabin_c: 15.0,
            status: ChargeStatus::Disconnected,
            plug: None,
            ac_v: 0.0,
            ac_a: 0.0,
            dc_v: 0.0,
            dc_a: 0.0,
            // a car a couple of years old: about what that odometer took
            total_charge_kwh: odometer_km * 0.17 + 300.0,
            total_discharge_kwh: odometer_km * 0.155 + 250.0,
            lv_v: 14.2,
            tyres_bar: [2.90, 2.90, 2.90, 2.90],
        };
        s.set_power(0.0);
        s
    }

    /// Battery %, 0 to 100 (as the screen shows it, before rounding).
    pub fn soc(&self) -> f64 {
        (self.energy_kwh / self.full_kwh * 100.0).clamp(0.0, 100.0)
    }

    pub fn range_km(&self) -> f64 {
        self.soc() * self.range_per_pct_km
    }

    /// Battery power, kW (positive out of the pack).
    pub fn power_kw(&self) -> f64 {
        self.pack_v * self.pack_a / 1000.0
    }

    /// Open-circuit voltage at this charge, then sag (or rise) with the current.
    fn set_power(&mut self, kw: f64) {
        let ocv = 330.0 + 72.0 * self.soc() / 100.0;
        // two passes are plenty for 0.07 Ω
        let mut v = ocv;
        for _ in 0..2 {
            let a = kw * 1000.0 / v;
            v = ocv - a * 0.07;
        }
        self.pack_v = v;
        self.pack_a = kw * 1000.0 / v;
    }

    /// Runs `dt_s` with the battery giving `kw` (negative: taking it in).
    pub fn spend(&mut self, kw: f64, dt_s: f64) {
        let kwh = kw * dt_s / 3600.0;
        self.energy_kwh = (self.energy_kwh - kwh).clamp(0.0, self.full_kwh);
        if kwh > 0.0 {
            self.total_discharge_kwh += kwh;
        } else {
            self.total_charge_kwh += -kwh;
        }
        self.set_power(kw);
    }

    /// Temperatures drift toward where they are headed (battery warmed by its power, the cabin by the climate).
    pub fn temps(&mut self, dt_s: f64, cabin_target: Option<f64>) {
        let p = self.power_kw().abs();
        let heat = p * 0.0009 + if self.status == ChargeStatus::Charging { 0.004 } else { 0.0 };
        self.batt_temp_c += (heat - (self.batt_temp_c - self.outside_c) * 0.00012) * dt_s;
        self.batt_temp_c = self.batt_temp_c.clamp(-30.0, 55.0);
        let target = cabin_target.unwrap_or(self.outside_c);
        let rate = if cabin_target.is_some() { 0.006 } else { 0.0008 };
        self.cabin_c += (target - self.cabin_c) * (rate * dt_s).min(1.0);
        let warm = if self.speed_kmh > 1.0 { 0.12 } else { 0.0 };
        for (i, t) in self.tyres_bar.iter_mut().enumerate() {
            let base = 2.90 + 0.008 * (self.outside_c - 15.0) - 0.02 * (i as f64 % 2.0);
            *t += (base + warm - *t) * (0.0015 * dt_s).min(1.0);
        }
    }

    /// One step of driving: speed to `kmh` with acceleration `accel` (m/s²).
    pub fn drive(&mut self, model: CarModel, kmh: f64, accel: f64, aux_kw: f64, dt_s: f64) {
        let v = kmh / 3.6;
        let m = model.mass_kg();
        let wheel = 0.5 * 1.2 * model.cda() * v * v * v + 0.009 * m * 9.81 * v + m * accel * v;
        let kw = if wheel >= 0.0 {
            wheel / 1000.0 / 0.88
        } else if kmh > 5.0 {
            (wheel / 1000.0 * 0.68).max(-70.0)
        } else {
            0.0
        } + aux_kw;
        self.speed_kmh = kmh;
        self.odometer_km += kmh * dt_s / 3600.0;
        self.spend(kw, dt_s);
    }

    /// Parked with the car awake (screen off, systems on).
    pub fn idle(&mut self, aux_kw: f64, dt_s: f64) {
        self.speed_kmh = 0.0;
        self.spend(aux_kw, dt_s);
    }

    /// Asleep for `dt_s`: a small drain, temperatures settle.
    pub fn sleep(&mut self, dt_s: f64) {
        self.speed_kmh = 0.0;
        let kwh = 0.018 * dt_s / 3600.0;
        self.energy_kwh = (self.energy_kwh - kwh).max(0.0);
        self.total_discharge_kwh += kwh;
        self.set_power(0.0);
        let f = libm::exp(-dt_s / 7200.0);
        self.batt_temp_c = self.outside_c + (self.batt_temp_c - self.outside_c) * f;
        self.cabin_c = self.outside_c + (self.cabin_c - self.outside_c) * f;
    }

    /// One step of charging at `kw` into the charger (the pack takes a little less).
    pub fn charge(&mut self, kind: ChargeKind, charger_kw: f64, dt_s: f64) {
        self.speed_kmh = 0.0;
        let into_pack = match kind {
            ChargeKind::Ac => charger_kw * 0.92 - 0.25,
            ChargeKind::Dc => charger_kw * 0.985,
        }
        .max(0.0);
        self.spend(-into_pack, dt_s);
        match kind {
            ChargeKind::Ac => {
                self.ac_v = 230.0;
                self.ac_a = charger_kw * 1000.0 / self.ac_v;
                (self.dc_v, self.dc_a) = (0.0, 0.0);
            }
            ChargeKind::Dc => {
                self.dc_v = self.pack_v;
                self.dc_a = charger_kw * 1000.0 / self.pack_v;
                (self.ac_v, self.ac_a) = (0.0, 0.0);
            }
        }
    }

    /// No current from the charger (plugged or not).
    pub fn no_charger_current(&mut self) {
        (self.ac_v, self.ac_a, self.dc_v, self.dc_a) =
            (if self.plug == Some(ChargeKind::Ac) { 230.0 } else { 0.0 }, 0.0, 0.0, 0.0);
        if self.plug == Some(ChargeKind::Dc) {
            self.dc_v = self.pack_v;
        }
    }
}

/// A made-up landscape: gentle hills from the position.
pub fn elevation_at(p: LatLon) -> f64 {
    45.0 + 30.0 * libm::sin(p.lat * 37.0) * libm::cos(p.lon * 29.0) + 12.0 * libm::sin(p.lon * 113.0)
}

/// The DC charge curve: the most the car takes at this battery %, kW.
pub fn dc_curve_kw(model: CarModel, soc: f64) -> f64 {
    let max = model.dc_max_kw();
    if soc < 15.0 {
        max
    } else if soc < 80.0 {
        max * (1.0 - (soc - 15.0) / 65.0 * 0.72)
    } else {
        (max * 0.28 * (1.0 - (soc - 80.0) / 20.0)).max(8.0)
    }
}
