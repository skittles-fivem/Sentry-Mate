//! Which messages the made-up car sends, on which bus, how often, and what is in them: each frame is encoded with the
//! same DBC set `dbc` decodes (`tesla-3y`), so decoding a made-up log gives back the car's state (rounded to each
//! signal's step).

use crate::car::{CarState, ChargeStatus};
use crate::sim::{CAN0, CAN1, CAN3};
use model::{ChargeKind, Gear};

/// How often messages are sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Rates {
    /// As a Model 3/Y sends them (10 ms to 1 s): about 300 frames a second while driving.
    #[default]
    Realistic,
    /// No message more than once a second (and none less): small, quick logs that still give a value every second.
    Light,
}

type Fill = fn(&CarState, &mut Vec<(&'static str, f64)>);

/// One periodic message (one page of a multiplexed one).
struct Spec {
    id: u32,
    /// The bus bit it goes out on (`CAN0`, `CAN1`, `CAN3`), as `canN`.
    bus: u8,
    period_ms: u64,
    /// Sent only while this holds.
    when: fn(&CarState) -> bool,
    fill: Fill,
}

fn always(_: &CarState) -> bool {
    true
}

fn dc_plugged(s: &CarState) -> bool {
    s.plug == Some(ChargeKind::Dc)
}

fn gear_value(g: Gear) -> f64 {
    match g {
        Gear::P => 1.0,
        Gear::R => 2.0,
        Gear::N => 3.0,
        Gear::D => 4.0,
    }
}

fn charging(s: &CarState) -> bool {
    s.status == ChargeStatus::Charging
}

const SPECS: &[Spec] = &[
    Spec { id: 0x118, bus: CAN0, period_ms: 10, when: always, fill: |s, v| v.push(("DI_gear", gear_value(s.gear))) },
    Spec { id: 0x257, bus: CAN0, period_ms: 20, when: always, fill: |s, v| v.push(("DI_vehicleSpeed", s.speed_kmh)) },
    Spec {
        id: 0x132,
        bus: CAN0,
        period_ms: 10,
        when: always,
        fill: |s, v| {
            v.push(("BattVoltage132", s.pack_v));
            v.push(("SmoothBattCurrent132", s.pack_a));
            v.push(("RawBattCurrent132", s.pack_a));
        },
    },
    Spec { id: 0x3F3, bus: CAN1, period_ms: 100, when: always, fill: |s, v| v.push(("UI_odometer", s.odometer_km)) },
    Spec { id: 0x3B6, bus: CAN0, period_ms: 1000, when: always, fill: |s, v| v.push(("Odometer3B6", s.odometer_km)) },
    Spec {
        id: 0x33A,
        bus: CAN0,
        period_ms: 1000,
        when: always,
        fill: |s, v| {
            let mi = s.range_km() / 1.609_344;
            v.push(("UI_Range", mi.round()));
            v.push(("UI_idealRange", (mi * 1.04).round()));
            v.push(("UI_SOC", s.soc().round()));
        },
    },
    Spec {
        id: 0x292,
        bus: CAN0,
        period_ms: 100,
        when: always,
        fill: |s, v| {
            v.push(("SOCave292", s.soc()));
            v.push(("SOCUI292", s.soc()));
        },
    },
    Spec {
        id: 0x352,
        bus: CAN0,
        period_ms: 1000,
        when: always,
        fill: |s, v| {
            v.push(("BMS_nominalFullPackEnergy", s.full_kwh));
            v.push(("BMS_nominalEnergyRemaining", s.energy_kwh));
            v.push(("BMS_idealEnergyRemaining", s.energy_kwh * 1.02));
        },
    },
    Spec {
        id: 0x332,
        bus: CAN0,
        period_ms: 500,
        when: always,
        fill: |s, v| {
            v.push(("BattBrickTempMax332", s.batt_temp_c + 1.0));
            v.push(("BattBrickTempMin332", s.batt_temp_c - 1.0));
        },
    },
    Spec {
        id: 0x321,
        bus: CAN0,
        period_ms: 1000,
        when: always,
        fill: |s, v| {
            v.push(("VCFRONT_tempAmbientFiltered", s.outside_c));
            v.push(("VCFRONT_tempAmbient", s.outside_c + 0.5));
        },
    },
    Spec {
        id: 0x243,
        bus: CAN0,
        period_ms: 500,
        when: always,
        fill: |s, v| v.push(("VCRIGHT_hvacCabinTempEst", s.cabin_c)),
    },
    Spec {
        id: 0x212,
        bus: CAN0,
        period_ms: 100,
        when: always,
        fill: |s, v| {
            let st = match s.status {
                ChargeStatus::Disconnected => 0.0,
                ChargeStatus::Waiting => 2.0,
                ChargeStatus::Charging => 3.0,
                ChargeStatus::Complete => 4.0,
                ChargeStatus::Stopped => 5.0,
            };
            v.push(("BMS_uiChargeStatus", st));
        },
    },
    Spec {
        id: 0x13D,
        bus: CAN0,
        period_ms: 100,
        when: always,
        fill: |s, v| {
            let st = if charging(s) {
                5.0
            } else if s.plug.is_some() {
                1.0
            } else {
                0.0
            };
            v.push(("CP_hvChargeStatus", st));
            v.push((
                "CP_evseChargeType",
                match s.plug {
                    Some(ChargeKind::Dc) => 1.0,
                    Some(ChargeKind::Ac) => 2.0,
                    None => 0.0,
                },
            ));
        },
    },
    Spec {
        id: 0x204,
        bus: CAN0,
        period_ms: 100,
        when: always,
        fill: |s, v| {
            let on = charging(s) && s.plug == Some(ChargeKind::Ac);
            v.push(("PCS_hvChargeStatus", if on { 2.0 } else { 0.0 }));
        },
    },
    Spec {
        id: 0x264,
        bus: CAN0,
        period_ms: 100,
        when: always,
        fill: |s, v| {
            v.push(("ChargeLineVoltage264", s.ac_v));
            v.push(("ChargeLineCurrent264", s.ac_a));
        },
    },
    Spec {
        id: 0x29D,
        bus: CAN0,
        period_ms: 100,
        when: dc_plugged,
        fill: |s, v| {
            v.push(("CP_evseOutputDcVoltage", s.dc_v));
            v.push(("CP_evseOutputDcCurrent", s.dc_a));
            v.push(("CP_evseOutputDcCurrentStale", 0.0));
        },
    },
    Spec {
        id: 0x3D2,
        bus: CAN0,
        period_ms: 1000,
        when: always,
        fill: |s, v| {
            v.push(("TotalChargeKWh3D2", s.total_charge_kwh));
            v.push(("TotalDischargeKWh3D2", s.total_discharge_kwh));
        },
    },
    Spec { id: 0x261, bus: CAN0, period_ms: 1000, when: always, fill: |s, v| v.push(("v12vBattVoltage261", s.lv_v)) },
    Spec {
        id: 0x219,
        bus: CAN0,
        period_ms: 1000,
        when: always,
        fill: |s, v| {
            v.push(("VCSEC_TPMSPressure0", s.tyres_bar[0]));
            v.push(("VCSEC_TPMSPressure1", s.tyres_bar[1]));
            v.push(("VCSEC_TPMSPressure2", s.tyres_bar[2]));
            v.push(("VCSEC_TPMSPressure3", s.tyres_bar[3]));
        },
    },
    Spec {
        id: 0x3D8,
        bus: CAN0,
        period_ms: 1000,
        when: always,
        fill: |s, v| v.push(("Elevation3D8", s.elevation_m.round())),
    },
    // the Screenmate's own GPS receiver, on its internal bus, under the chassis bus's 0x04F layout
    Spec {
        id: 0x04F,
        bus: CAN3,
        period_ms: 1000,
        when: always,
        fill: |s, v| {
            v.push(("GPSLatitude04F", s.pos.lat));
            v.push(("GPSLongitude04F", s.pos.lon));
            v.push(("GPSAccuracy04F", s.gps_accuracy_m));
        },
    },
];

/// The VIN in 0x405's three pages (bytes 5-7, then 1-7, then 1-7).
pub fn vin_pages(vin: &str) -> [[u8; 8]; 3] {
    let v = vin.as_bytes();
    let mut p = [[0u8; 8]; 3];
    if v.len() == 17 {
        p[0][0] = 0x10;
        p[0][5..8].copy_from_slice(&v[0..3]);
        p[1][0] = 0x11;
        p[1][1..8].copy_from_slice(&v[3..10]);
        p[2][0] = 0x12;
        p[2][1..8].copy_from_slice(&v[10..17]);
    }
    p
}

/// One frame as the bridge records it.
#[derive(Debug, Clone, PartialEq)]
pub struct RawFrame {
    pub t_us: u64,
    /// Interface number (`can0` is 0).
    pub bus: u8,
    pub id: u32,
    pub data: Vec<u8>,
}

/// A frame and the values put in it (each as the decoder reads it back).
#[derive(Debug, Clone, PartialEq)]
pub struct Encoded {
    pub bus: u8,
    pub id: u32,
    pub data: Vec<u8>,
    pub values: Vec<(&'static str, f64)>,
}

fn bus_number(bit: u8) -> u8 {
    match bit {
        CAN0 => 0,
        CAN1 => 1,
        _ => 3,
    }
}

/// The DBC's bounds of a signal, so a value just past them (a hot day, a full pack) is sent as the edge.
fn clamp(sig: &dbc::Signal, v: f64) -> f64 {
    if sig.min < sig.max { v.clamp(sig.min, sig.max) } else { v }
}

/// Encodes one message of the schedule for a state. `None` when the set lacks it (never for `tesla-3y`).
fn encode(spec: &Spec, s: &CarState, scratch: &mut Vec<(&'static str, f64)>) -> Option<Encoded> {
    let msg = dbc::Profile::Tesla3Y.dbc().messages.get(&spec.id)?;
    scratch.clear();
    (spec.fill)(s, scratch);
    for (name, v) in scratch.iter_mut() {
        if let Some(sig) = dbc::encode::signal_of(msg, name) {
            *v = clamp(sig, *v);
        }
    }
    let (data, back) = dbc::encode_message(msg, scratch).ok()?;
    let values = scratch.iter().map(|(n, _)| *n).zip(back).collect();
    Some(Encoded { bus: bus_number(spec.bus), id: spec.id, data, values })
}

/// Every message of the schedule for one state, encoded (for tests: what a decoder must give back).
pub fn frames_for(s: &CarState) -> Vec<Encoded> {
    let mut scratch = Vec::new();
    SPECS.iter().filter(|sp| (sp.when)(s)).filter_map(|sp| encode(sp, s, &mut scratch)).collect()
}

/// Sends the schedule: which frames fall in each step, with their jitter.
pub struct Schedule {
    vin: [[u8; 8]; 3],
    rates: Rates,
    /// Per message (and the VIN last): its next send time (µs), `None` while its bus is quiet.
    next: Vec<Option<u64>>,
    rng: crate::rng::Rng,
    vin_page: usize,
    scratch: Vec<(&'static str, f64)>,
}

impl Schedule {
    pub fn new(vin: &str, rates: Rates, rng: crate::rng::Rng) -> Schedule {
        Schedule {
            vin: vin_pages(vin),
            rates,
            next: vec![None; SPECS.len() + 1],
            rng,
            vin_page: 0,
            scratch: Vec::new(),
        }
    }

    fn period_us(&self, ms: u64) -> u64 {
        match self.rates {
            Rates::Realistic => ms * 1000,
            Rates::Light => 1_000_000,
        }
    }

    /// The frames of `[from_us, to_us)` on these buses, in time order.
    pub fn frames(&mut self, from_us: u64, to_us: u64, buses: u8, s: &CarState, out: &mut Vec<RawFrame>) {
        let start = out.len();
        for i in 0..=SPECS.len() {
            let (bus, period) = match SPECS.get(i) {
                Some(sp) => (sp.bus, self.period_us(sp.period_ms)),
                None => (CAN0, self.period_us(333)),
            };
            if buses & bus == 0 {
                self.next[i] = None;
                continue;
            }
            // a bus waking up: each message starts somewhere in its first period
            let mut due = match self.next[i] {
                Some(d) => d,
                None => from_us + self.rng.below(period.max(1)),
            };
            while due < to_us {
                let jitter = self.rng.below(1_200);
                let t = (due + jitter).min(to_us - 1).max(from_us);
                match SPECS.get(i) {
                    Some(sp) => {
                        if (sp.when)(s) {
                            let mut scratch = std::mem::take(&mut self.scratch);
                            if let Some(e) = encode(sp, s, &mut scratch) {
                                out.push(RawFrame { t_us: t, bus: e.bus, id: e.id, data: e.data });
                            }
                            self.scratch = scratch;
                        }
                    }
                    None => {
                        out.push(RawFrame { t_us: t, bus: 0, id: 0x405, data: self.vin[self.vin_page].to_vec() });
                        self.vin_page = (self.vin_page + 1) % 3;
                    }
                }
                due += period;
            }
            self.next[i] = Some(due);
        }
        out[start..].sort_by_key(|f| f.t_us);
    }
}
