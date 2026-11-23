//! A made-up car for the tests: it drives, parks, charges and sleeps second by second, and writes the samples and
//! the log facts the CAN Bridge's logs would give (one log a minute, `quiet` before a sleep). Every value is
//! synthetic; positions are in the open sea near 0.5° N 0.5° E, not anywhere a real car was.
#![allow(dead_code, clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use model::{ChargeKind, Gear, Sample};
use timeline::{Closed, Input, LogFacts, NotRecorded, Params, Timeline};

/// 2026-01-01T00:00:00Z.
pub const T0: u64 = 1_767_225_600_000;
pub const KM_PER_DEG_LAT: f64 = 111.195;

pub struct Car {
    pub id: u32,
    /// The next sample's time (ms, a whole second).
    pub t: u64,
    /// `None`: the car sends no odometer.
    pub odo: Option<f64>,
    pub soc: f64,
    pub lat: f64,
    pub lon: f64,
    pub gps: bool,
    /// The pack's lifetime counters (kWh), when the car sends them.
    pub counters: bool,
    pub total_in: f64,
    pub total_out: f64,
    /// What the lifetime charge counter gains per kWh of charge power (1: they agree).
    pub counter_gain: f64,
    pub buses: Vec<String>,
    pub samples: Vec<Sample>,
    pub logs: Vec<LogFacts>,
    open: Option<LogFacts>,
    pause: Option<Option<u64>>,
}

impl Car {
    pub fn new(id: u32, t: u64) -> Car {
        Car {
            id,
            t,
            odo: Some(10_000.0),
            soc: 60.0,
            lat: 0.5,
            lon: 0.5,
            gps: true,
            counters: true,
            total_in: 5_000.0,
            total_out: 4_800.0,
            counter_gain: 1.0,
            buses: vec!["can0".into(), "can1".into()],
            samples: Vec::new(),
            logs: Vec::new(),
            open: None,
            pause: None,
        }
    }

    /// One second, made from a parked car and changed by `f`; `kw` is the battery's power (out of the pack).
    fn second(&mut self, kw: f64, kmh: f64, f: impl FnOnce(&mut Sample)) {
        let km = kmh / 3600.0;
        if let Some(o) = self.odo.as_mut() {
            *o += km;
        }
        self.lat += km / KM_PER_DEG_LAT;
        if kw > 0.0 {
            self.total_out += kw / 3600.0;
        } else {
            self.total_in += -kw / 3600.0 * self.counter_gain;
        }
        self.soc = (self.soc - kw / 3600.0 / 75.0 * 100.0).clamp(0.0, 100.0);
        let mut s = Sample {
            t_ms: self.t,
            lat: self.gps.then_some(self.lat),
            lon: self.gps.then_some(self.lon),
            gps_accuracy_m: self.gps.then_some(3.0),
            elevation_m: Some(50.0),
            speed_kmh: Some(kmh),
            gear: Some(Gear::P),
            odometer_km: self.odo,
            soc: Some(self.soc),
            soc_bms: Some(self.soc + 1.0),
            range_km: Some(self.soc * 5.0),
            battery_voltage_v: Some(400.0),
            battery_current_a: Some(kw * 1000.0 / 400.0),
            battery_power_kw: Some(kw),
            battery_temp_c: Some(25.0),
            outside_temp_c: Some(10.0),
            inside_temp_c: Some(20.0),
            charging: Some(false),
            total_charge_kwh: self.counters.then_some(self.total_in),
            total_discharge_kwh: self.counters.then_some(self.total_out),
            lv_voltage_v: Some(13.5),
            tpms_bar: [Some(2.9); 4],
            ..Sample::default()
        };
        f(&mut s);
        self.record(s);
        self.t += 1000;
    }

    fn record(&mut self, s: Sample) {
        let t = s.t_ms;
        if self.open.as_ref().is_some_and(|l| t >= l.start_ms + 60_000) {
            self.close(Closed::Rotate);
        }
        let buses = self.buses.clone();
        let pause = self.pause.take();
        let l = self.open.get_or_insert_with(|| LogFacts {
            first_frame_ms: Some(t),
            buses,
            not_recorded: pause.map(|p| vec![NotRecorded { at_ms: t, paused_ms: p }]).unwrap_or_default(),
            ..LogFacts::new(t, t, Closed::Rotate)
        });
        l.last_frame_ms = Some(t + 900);
        l.end_ms = t + 900;
        self.samples.push(s);
    }

    fn close(&mut self, why: Closed) {
        if let Some(mut l) = self.open.take() {
            l.closed = why;
            if why == Closed::Quiet {
                l.end_ms += 60_000;
            }
            self.logs.push(l);
        }
    }

    pub fn park(&mut self, secs: u64) {
        for _ in 0..secs {
            self.second(0.4, 0.0, |_| {});
        }
    }

    /// In D at a steady speed, the pack giving `kw` (negative: regenerating).
    pub fn drive(&mut self, secs: u64, kmh: f64, kw: f64) {
        for _ in 0..secs {
            self.second(kw, kmh, |s| s.gear = Some(Gear::D));
        }
    }

    /// In D, standing (a red light).
    pub fn stand(&mut self, secs: u64) {
        self.drive(secs, 0.0, 1.0);
    }

    /// Charging at `kw` into the pack.
    pub fn charge(&mut self, secs: u64, kw: f64, kind: ChargeKind) {
        for _ in 0..secs {
            self.second(-kw, 0.0, |s| {
                s.charging = Some(true);
                s.charge_kind = Some(kind);
                s.charge_power_kw = Some(kw);
                // AC: a 240 V line, the charger losing 10 %
                if kind == ChargeKind::Ac {
                    s.charger_voltage_v = Some(240.0);
                    s.charger_current_a = Some(kw * 1.1 * 1000.0 / 240.0);
                }
            });
        }
    }

    /// Plugged in, not charging.
    pub fn plugged(&mut self, secs: u64, kind: ChargeKind) {
        for _ in 0..secs {
            self.second(0.3, 0.0, |s| s.charge_kind = Some(kind));
        }
    }

    /// The car goes to sleep: the log is closed `quiet` and nothing comes for `secs`.
    pub fn sleep(&mut self, secs: u64) {
        self.close(Closed::Quiet);
        self.t += secs * 1000;
    }

    /// No log for `secs` (an app update, a log lost), the car awake.
    pub fn gap(&mut self, secs: u64) {
        self.close(Closed::Flush);
        self.t += secs * 1000;
    }

    /// No log for `secs` while the car drives on (a log lost mid-drive): the odometer, the position and the
    /// counters move on as if it drove at `kmh`, the pack giving `kw`.
    pub fn gap_driving(&mut self, secs: u64, kmh: f64, kw: f64) {
        self.close(Closed::Flush);
        for _ in 0..secs {
            let km = kmh / 3600.0;
            if let Some(o) = self.odo.as_mut() {
                *o += km;
            }
            self.lat += km / KM_PER_DEG_LAT;
            self.total_out += kw / 3600.0;
            self.t += 1000;
        }
    }

    /// Recording paused for `secs` (the bridge's folder full): the next log starts with `# not recorded`.
    pub fn pause(&mut self, secs: u64, said: bool) {
        self.close(Closed::Rotate);
        self.t += secs * 1000;
        self.pause = Some(said.then_some(secs * 1000));
    }

    /// Closes the open log (rotate) and returns everything.
    pub fn finish(mut self) -> (Vec<Sample>, Vec<LogFacts>) {
        self.close(Closed::Rotate);
        (self.samples, self.logs)
    }
}

/// The timeline of `[from, to)`.
pub fn build(car: u32, from: u64, to: u64, samples: &[Sample], logs: &[LogFacts]) -> Timeline {
    timeline::build(
        &Input {
            car,
            from_ms: from,
            to_ms: to,
            samples,
            logs,
            holes: &[],
            api: &[],
            marks: &[],
            order: Default::default(),
        },
        &Params::default(),
    )
}

pub fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

/// Made-up frames of the Model 3/Y set, built from the DBC definitions (`dbc::encode_message`), run through the
/// decoder (default buses: can0 vehicle, can1 chassis) and the sample builder.
pub struct Bench {
    pub decoder: dbc::Decoder,
    pub builder: timeline::SampleBuilder,
}

impl Bench {
    pub fn new() -> Bench {
        Bench {
            decoder: dbc::Decoder::new(dbc::Profile::Tesla3Y, &serde_json::json!({})),
            builder: timeline::SampleBuilder::new(dbc::Profile::Tesla3Y, timeline::SampleParams::default()),
        }
    }

    /// One frame of message `id` on `can<bus>` at `t_s` seconds after [`T0`], carrying these values (by series or
    /// plain name; the rest of its bits 0).
    pub fn send(&mut self, t_s: f64, bus: u8, id: u32, values: &[(&str, f64)]) {
        let msg = &dbc::Profile::Tesla3Y.dbc().messages[&id];
        let (bytes, back) = dbc::encode_message(msg, values).unwrap();
        for ((name, want), got) in values.iter().zip(&back) {
            assert!((want - got).abs() < 1e-6, "{name}: {want} encodes as {got}");
        }
        let mut data = [0u8; 64];
        data[..bytes.len()].copy_from_slice(&bytes);
        let frame = logfmt::Frame {
            time_us: T0 * 1000 + (t_s * 1e6) as u64,
            bus,
            id,
            extended: false,
            len: bytes.len() as u8,
            data,
        };
        self.builder.feed(&mut self.decoder, &frame);
    }

    /// The samples, by whole second after [`T0`].
    pub fn finish(self) -> std::collections::BTreeMap<u64, Sample> {
        let (samples, _) = self.builder.finish();
        samples.into_iter().map(|s| ((s.t_ms - T0) / 1000, s)).collect()
    }
}
