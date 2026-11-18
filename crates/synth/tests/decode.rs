#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! `dbc` decodes exactly the values the made-up car put in its frames, on their buses, with the bridge's bus map.

use model::{ChargeKind, Gear};
use synth::{CarModel, CarState, ChargeStatus, LatLon, frames_for};

fn states() -> Vec<CarState> {
    let mut v = Vec::new();
    let mut s = CarState::new(CarModel::Y, 1_772_409_600_000, LatLon::new(0.31, 0.21), 63.4, 23_456.7);
    s.gear = Gear::D;
    s.drive(CarModel::Y, 87.3, 0.4, 0.5, 0.1);
    v.push(s.clone());
    s.gear = Gear::R;
    s.drive(CarModel::Y, 3.0, -1.0, 0.5, 0.1);
    v.push(s.clone());
    s.gear = Gear::P;
    s.plug = Some(ChargeKind::Ac);
    s.status = ChargeStatus::Charging;
    s.charge(ChargeKind::Ac, 7.4, 0.1);
    v.push(s.clone());
    s.plug = Some(ChargeKind::Dc);
    s.charge(ChargeKind::Dc, 180.0, 0.1);
    s.outside_c = -12.5;
    s.batt_temp_c = 41.0;
    v.push(s);
    v
}

#[test]
fn every_value_put_in_comes_back() {
    let side = serde_json::json!({ "bus": { "can0": "vehicle", "can1": "chassis", "can3": "screenmate-internal" } });
    for s in states() {
        let frames = frames_for(&s);
        let ids: Vec<u32> = frames.iter().map(|f| f.id).collect();
        // every message of the schedule (0x29D only with a DC charger plugged in)
        let dc = s.plug == Some(ChargeKind::Dc);
        assert_eq!(ids.len(), if dc { 21 } else { 20 }, "{ids:x?}");
        let mut d = dbc::Decoder::new(dbc::Profile::Tesla3Y, &side);
        for (i, e) in frames.iter().enumerate() {
            let mut data = [0u8; 64];
            data[..e.data.len()].copy_from_slice(&e.data);
            let f = logfmt::Frame {
                time_us: 1_772_409_600_000_000 + i as u64 * 1000,
                bus: e.bus,
                id: e.id,
                extended: false,
                len: e.data.len() as u8,
                data,
            };
            let got = d.decode(&f);
            assert!(!got.is_empty(), "0x{:03X} on can{} gave nothing", e.id, e.bus);
            for (name, want) in &e.values {
                let v = got.iter().find(|(s, _)| s.name == *name || s.series == *name).map(|(_, v)| *v);
                assert_eq!(v.map(f64::to_bits), Some(want.to_bits()), "{name} in 0x{:03X}: {v:?} vs {want}", e.id);
            }
        }
        // and they are the car's values, to each signal's step
        let value =
            |name: &str| frames.iter().flat_map(|f| &f.values).find(|(n, _)| *n == name).map(|(_, v)| *v).unwrap();
        assert!((value("DI_vehicleSpeed") - s.speed_kmh).abs() <= 0.04 + 1e-9);
        assert!((value("UI_odometer") - s.odometer_km).abs() <= 0.05 + 1e-9);
        assert!((value("BattVoltage132") - s.pack_v).abs() <= 0.005 + 1e-9);
        assert!((value("SmoothBattCurrent132") - s.pack_a).abs() <= 0.05 + 1e-9);
        assert!((value("GPSLatitude04F") - s.pos.lat).abs() <= 1e-6);
        assert!((value("VCFRONT_tempAmbientFiltered") - s.outside_c).abs() <= 0.25 + 1e-9);
        assert_eq!(value("UI_SOC"), s.soc().round());
    }
}

#[test]
fn the_gps_is_on_the_internal_bus_only() {
    let s = &states()[0];
    let gps: Vec<_> = frames_for(s).into_iter().filter(|f| f.id == 0x04F).collect();
    assert_eq!(gps.len(), 1);
    assert_eq!(gps[0].bus, 3);
    // without the bridge's bus map naming can3 internal, it is not decoded
    let mut d = dbc::Decoder::new(dbc::Profile::Tesla3Y, &serde_json::json!({}));
    let mut data = [0u8; 64];
    data[..8].copy_from_slice(&gps[0].data);
    let f = logfmt::Frame { time_us: 1, bus: 3, id: 0x04F, extended: false, len: 8, data };
    assert!(d.decode(&f).is_empty());
}
