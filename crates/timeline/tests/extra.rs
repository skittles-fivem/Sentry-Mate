//! The less frequent values of a sample (`model::SampleExtra`), from made-up frames built from the Model 3/Y DBC set.
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

mod common;

use common::Bench;

#[test]
fn body_and_configuration() {
    let mut b = Bench::new();
    // the configuration once: held long after the other values would have gone
    b.send(0.2, 0, 0x7FF, &[("GTW_exteriorColor", 6.0), ("GTW_spoilerType", 0.0), ("GTW_roofType", 1.0)]);
    b.send(0.3, 0, 0x7FF, &[("GTW_wheelType", 7.0)]); // no value name: code_7
    b.send(0.4, 0, 0x7FF, &[("GTW_rightHandDrive", 1.0)]);
    for s in 0..30 {
        let t = s as f64 + 0.5;
        b.send(t, 1, 0x3D9, &[("UI_gpsVehicleHeading", if s < 20 { 123.5 } else { 400.0 })]);
        b.send(t, 0, 0x284, &[("UIsentryMode284", 1.0), ("UIserviceMode284", 0.0)]);
        b.send(t, 0, 0x00C, &[("UI_displayOn", 1.0)]);
        b.send(t, 0, 0x2F3, &[("UI_hvacReqKeepClimateOn", 2.0)]); // DOG
        // left: front LATCH_OPENED, rear LATCH_CLOSED
        b.send(t, 0, 0x102, &[("VCLEFT_frontLatchStatus", 1.0), ("VCLEFT_rearLatchStatus", 2.0)]);
        // right: front LATCH_AJAR, rear LATCH_SNA; the trunk LATCH_CLOSED, then LATCH_SNA (the liftgate decides)
        let trunk = if s < 20 { 2.0 } else { 0.0 };
        b.send(
            t,
            0,
            0x103,
            &[("VCRIGHT_frontLatchStatus", 5.0), ("VCRIGHT_rearLatchStatus", 0.0), ("VCRIGHT_trunkLatchStatus", trunk)],
        );
        b.send(t, 0, 0x142, &[("VCLEFT_liftgateState", 11.0)]); // PLG_STATE_END_OF_TRAVEL
        b.send(t, 0, 0x2E1, &[("VCFRONT_frunkLatchStatus", 4.0)]); // LATCH_OPENING
    }
    let s = b.finish();
    let x = &s[&15].extra;
    assert_eq!(x.paint.as_deref(), Some("pearl_white"));
    assert_eq!(x.wheels.as_deref(), Some("code_7"));
    assert_eq!(x.spoiler.as_deref(), Some("not_installed"));
    assert_eq!(x.roof.as_deref(), Some("fixed_glass"));
    assert_eq!(x.right_hand_drive, Some(true));
    assert_eq!(x.heading_deg, Some(123.5));
    assert_eq!((x.sentry_mode, x.service_mode, x.display_on), (Some(true), Some(false), Some(true)));
    assert_eq!(x.keep_climate.as_deref(), Some("dog"));
    assert_eq!((x.door_fl, x.door_rl, x.door_fr, x.door_rr), (Some(true), Some(false), Some(true), None));
    assert_eq!(x.trunk_open, Some(false), "the trunk latch first");
    assert_eq!(x.frunk_open, Some(true));
    let x = &s[&25].extra;
    assert_eq!(x.heading_deg, None, "past 360");
    assert_eq!(x.trunk_open, Some(true), "latch not available: the liftgate");
    assert_eq!(x.paint.as_deref(), Some("pearl_white"), "still held");
}

#[test]
fn charging() {
    let mut b = Bench::new();
    for s in 0..12 {
        let t = s as f64 + 0.5;
        let late = s >= 8;
        b.send(t, 0, 0x25D, &[("CP_chargeCableState", if late { 0.0 } else { 2.0 }), ("CP_chargeDoorOpen", 1.0)]);
        b.send(t, 0, 0x212, &[("BMS_uiChargeStatus", 3.0)]); // BMS_CHARGING
        b.send(t, 0, 0x333, &[("UI_chargeTerminationPct", 80.0), ("UI_acChargeCurrentLimit", 16.0)]);
        b.send(t, 0, 0x204, &[("PCS_chgPHAEnable", 1.0), ("PCS_chgPHBEnable", 1.0), ("PCS_chgPHCEnable", 0.0)]);
        b.send(t, 0, 0x21D, &[("CP_pilotCurrent", 32.0), ("CP_cableCurrentLimit", 20.0)]);
        // the car's own limit: 0 (none given), then 13.5 A
        b.send(t, 0, 0x13D, &[("CP_acChargeCurrentLimit", if late { 13.5 } else { 0.0 })]);
        b.send(t, 0, 0x132, &[("ChargeHoursRemaining132", if late { 4095.0 } else { 95.0 })]);
        b.send(t, 0, 0x33A, &[("UI_uSOE", 72.0)]);
    }
    let s = b.finish();
    let x = &s[&5].extra;
    assert_eq!((x.plugged_in, x.charge_port_open), (Some(true), Some(true)));
    assert_eq!(x.charge_status.as_deref(), Some("charging"));
    assert_eq!(s[&5].charging, Some(true));
    assert_eq!((x.charge_limit_pct, x.charge_current_request_a), (Some(80.0), Some(16.0)));
    assert_eq!(x.charger_phases, Some(2.0));
    assert_eq!((x.charge_current_limit_a, x.charge_limit_source.as_deref()), (Some(20.0), Some("cable")));
    assert_eq!(x.time_to_full_min, Some(95.0));
    assert_eq!(x.usable_soc, Some(72.0));
    let x = &s[&10].extra;
    assert_eq!(x.plugged_in, None, "CHARGE_CABLE_UNKNOWN_SNA");
    assert_eq!((x.charge_current_limit_a, x.charge_limit_source.as_deref()), (Some(13.5), Some("car")));
    assert_eq!(x.time_to_full_min, None, "all bits set");
}

#[test]
fn climate_navigation_update_and_tyres() {
    let mut b = Bench::new();
    for s in 0..12 {
        let t = s as f64 + 0.5;
        let late = s >= 8;
        // PRECONDITION, then OFF
        b.send(t, 0, 0x243, &[("VCRIGHT_hvacPowerState", if late { 0.0 } else { 2.0 })]);
        b.send(t, 0, 0x3B3, &[("UI_userPresent", 1.0), ("UI_batteryPreconditioningRequest", 0.0)]);
        // 327.67 is TRIP_TOO_LONG
        let dest = if late { 327.67 } else { 12.34 };
        b.send(t, 0, 0x082, &[("UI_requestActiveBatteryHeating", 1.0), ("UI_energyAtDestination", dest)]);
        b.send(t, 1, 0x238, &[("UI_navRouteActive", 1.0)]);
        b.send(t, 0, 0x343, &[("VCRIGHT_OTAState", 1.0)]);
        // recommended 2.9 bar front and rear; the rear left at 2.5 bar (more than 10 % low), then 2.8
        b.send(t, 0, 0x219, &[("VCSEC_TPMSRecommendedPressure0", 2.9), ("VCSEC_TPMSRecommendedPressure1", 2.9)]);
        let rl = if late { 2.8 } else { 2.5 };
        let p = [("VCSEC_TPMSPressure0", 2.9), ("VCSEC_TPMSPressure1", 2.9), ("VCSEC_TPMSPressure2", rl)];
        b.send(t + 0.1, 0, 0x219, &[p[0], p[1], p[2], ("VCSEC_TPMSPressure3", 2.875)]);
    }
    let s = b.finish();
    let x = &s[&5].extra;
    assert_eq!((x.climate_on, x.preconditioning_cabin), (Some(true), Some(true)));
    assert_eq!(x.user_present, Some(true));
    assert_eq!(x.preconditioning_battery, Some(true), "the heating request");
    assert_eq!(x.energy_at_destination_kwh, Some(12.34));
    assert_eq!((x.navigating, x.update_installing), (Some(true), Some(true)));
    assert_eq!(s[&5].tpms_bar[2], Some(2.5));
    assert_eq!(x.tire_low, Some(true));
    let x = &s[&10].extra;
    assert_eq!((x.climate_on, x.preconditioning_cabin), (Some(false), Some(false)));
    assert_eq!(x.energy_at_destination_kwh, None, "TRIP_TOO_LONG");
    assert_eq!(x.tire_low, Some(false), "all within 10 %");
}
