#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Made-up VINs only (full ones end in `PF0000xx`; Shanghai and Berlin cases are 11-character prefixes).

use super::*;

fn vin_only(prefix: &str) -> VehicleInfo {
    vehicle_info(&VehicleInputs { vin: Some(prefix.into()), ..VehicleInputs::default() })
}

#[test]
fn every_table_parses_and_every_row_names_its_source() {
    let t = tables::load().unwrap();
    assert!(!t.wmi.is_empty() && !t.models.is_empty() && !t.body.is_empty() && !t.restraint.is_empty());
    assert!(!t.battery.is_empty() && !t.motor.is_empty() && t.years.len() == 30 && !t.plants.is_empty());
    assert!(t.epa.len() > 150 && !t.packs.is_empty() && !t.pack_codes.is_empty() && !t.wheels.is_empty());
    assert!(!t.can_values.is_empty() && !t.trim_can.is_empty() && !t.tyre_pressure.is_empty());
    for (name, text) in tables::FILES {
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        let head = lines.next().unwrap();
        assert!(head.ends_with("source,confidence"), "{name}: every row has a source and a confidence");
        for l in lines {
            let f: Vec<&str> = l.split(',').collect();
            assert!(!f[f.len() - 2].trim().is_empty(), "{name}: {l}");
        }
    }
    // a code means one thing per model, year and plant
    for (i, a) in t.motor.iter().enumerate() {
        for b in &t.motor[i + 1..] {
            let same_model = a.models.iter().any(|m| b.models.contains(m));
            let years = a.years.from <= b.years.to.unwrap_or(9999) && b.years.from <= a.years.to.unwrap_or(9999);
            let plants = a.plants.is_empty() || b.plants.is_empty() || a.plants.iter().any(|p| b.plants.contains(p));
            assert!(!(same_model && a.code == b.code && years && plants), "overlapping motor rows {a:?} {b:?}");
        }
    }
    assert_eq!(tables().wmi.len(), t.wmi.len(), "the shared copy is the same");
}

/// The examples of the research (section 4.5) and more, one row per model, era and plant.
#[test]
fn decodes_each_model_year_and_plant() {
    struct Case {
        prefix: &'static str,
        model: &'static str,
        year: u16,
        year_kind: &'static str,
        plant: &'static str,
        steering: &'static str,
        motors: Option<u8>,
        drive: Option<&'static str>,
        performance: Option<bool>,
        platform: Option<&'static str>,
    }
    let c = |prefix, model, year, year_kind, plant, steering, motors, drive, performance, platform| Case {
        prefix,
        model,
        year,
        year_kind,
        plant,
        steering,
        motors,
        drive,
        performance,
        platform,
    };
    let cases = [
        c(
            "7SAYGDEE1PF",
            "Model Y",
            2023,
            "model_year",
            "Fremont; California",
            "left",
            Some(2),
            Some("AWD"),
            Some(false),
            None,
        ),
        c(
            "5YJ3E1EA2PF",
            "Model 3",
            2023,
            "model_year",
            "Fremont; California",
            "left",
            Some(1),
            Some("RWD"),
            Some(false),
            None,
        ),
        c(
            "LRW3F7FS3PC",
            "Model 3",
            2023,
            "build_year",
            "Shanghai (Giga Shanghai)",
            "right",
            Some(1),
            Some("RWD"),
            Some(false),
            None,
        ),
        c(
            "XP7YGCEK4PB",
            "Model Y",
            2023,
            "build_year",
            "Berlin-Brandenburg (Giga Berlin)",
            "left",
            Some(2),
            Some("AWD"),
            Some(false),
            None,
        ),
        c(
            "5YJSA1E26HF",
            "Model S",
            2017,
            "model_year",
            "Fremont; California",
            "left",
            Some(2),
            Some("AWD"),
            Some(false),
            Some("legacy"),
        ),
        c(
            "5YJSA1E57MF",
            "Model S",
            2021,
            "model_year",
            "Fremont; California",
            "left",
            Some(2),
            Some("AWD"),
            Some(false),
            Some("refresh"),
        ),
        c(
            "5YJSA1E2XMF",
            "Model S",
            2021,
            "model_year",
            "Fremont; California",
            "left",
            Some(2),
            Some("AWD"),
            Some(false),
            Some("legacy"),
        ),
        c(
            "5YJXCAE60NF",
            "Model X",
            2022,
            "model_year",
            "Fremont; California",
            "left",
            Some(3),
            Some("AWD"),
            Some(true),
            Some("refresh"),
        ),
        c(
            "7G2CEHEE8RA",
            "Cybertruck",
            2024,
            "model_year",
            "Austin; Texas (Giga Texas)",
            "left",
            Some(3),
            Some("AWD"),
            Some(true),
            None,
        ),
        c(
            "7G2CEHEC0SA",
            "Cybertruck",
            2025,
            "model_year",
            "Austin; Texas (Giga Texas)",
            "left",
            Some(1),
            Some("RWD"),
            Some(false),
            None,
        ),
        c(
            "5YJ3E1EC0PF",
            "Model 3",
            2023,
            "model_year",
            "Fremont; California",
            "left",
            Some(2),
            Some("AWD"),
            Some(true),
            None,
        ),
        c(
            "5YJ3E1ET0SF",
            "Model 3",
            2025,
            "model_year",
            "Fremont; California",
            "left",
            Some(2),
            Some("AWD"),
            Some(true),
            None,
        ),
        c(
            "7SAYGDET0TA",
            "Model Y",
            2026,
            "model_year",
            "Austin; Texas (Giga Texas)",
            "left",
            Some(2),
            Some("AWD"),
            Some(true),
            None,
        ),
        c(
            "7SAYGDEF0RF",
            "Model Y",
            2024,
            "model_year",
            "Fremont; California",
            "left",
            Some(2),
            Some("AWD"),
            Some(true),
            None,
        ),
        c(
            "LRWYGCFS0PC",
            "Model Y",
            2023,
            "build_year",
            "Shanghai (Giga Shanghai)",
            "left",
            Some(1),
            Some("RWD"),
            Some(false),
            None,
        ),
        c(
            "5YJSA1DN0DF",
            "Model S",
            2013,
            "model_year",
            "Fremont; California",
            "left",
            Some(1),
            Some("RWD"),
            Some(false),
            Some("legacy"),
        ),
        c(
            "5YJ3E1EB0KF",
            "Model 3",
            2019,
            "model_year",
            "Fremont; California",
            "left",
            Some(2),
            Some("AWD"),
            None,
            None,
        ),
        c(
            "7G2TBEEB0PN",
            "Semi",
            2023,
            "model_year",
            "Reno; Nevada (Giga Nevada)",
            "left",
            None,
            Some("RWD"),
            Some(false),
            None,
        ),
        c(
            "5YJAJEEU0TA",
            "Cybercab",
            2026,
            "model_year",
            "Austin; Texas (Giga Texas)",
            "left",
            Some(1),
            None,
            Some(false),
            None,
        ),
    ];
    for k in &cases {
        let v = vin_only(k.prefix);
        let p = k.prefix;
        assert_eq!(v.model.value.as_deref(), Some(k.model), "{p}");
        assert_eq!(v.model.source, Some(FactSource::Vin), "{p}");
        assert_eq!(v.model_year.value, Some(k.year), "{p}");
        assert_eq!(v.year_kind, Some(k.year_kind), "{p}");
        assert_eq!(v.plant.value.as_deref(), Some(k.plant), "{p}");
        assert_eq!(v.steering.value.as_deref(), Some(k.steering), "{p}");
        assert_eq!(v.motors.value, k.motors, "{p}");
        assert_eq!(v.drive.value.as_deref(), k.drive, "{p}");
        assert_eq!(v.performance.value, k.performance, "{p}");
        assert_eq!(v.platform.value.as_deref(), k.platform, "{p}");
        assert!(v.body.is_known() && v.restraint.is_known() && v.maker.is_known(), "{p}: {v:?}");
        assert!(v.disagreements.is_empty(), "{p}: {:?}", v.disagreements);
        assert!(v.unknown_codes.iter().all(|u| u.field != "motors" && u.field != "body"), "{p}: {:?}", v.unknown_codes);
    }
}

#[test]
fn chemistry_from_the_vin_only_where_tesla_says_it() {
    // Shanghai: F is LFP, from Tesla's manual
    let v = vin_only("LRW3F7FS3PC");
    assert_eq!(v.chemistry.value.as_deref(), Some("lfp"));
    assert_eq!((v.chemistry.source, v.chemistry.confidence), (Some(FactSource::Vin), Some(Confidence::High)));
    assert!(v.chemistry.note.as_deref().unwrap().starts_with("vin_pos7"));
    // Shanghai E: ternary
    assert_eq!(vin_only("LRW3E7EB0PC").chemistry.value.as_deref(), Some("nca_nmc"));
    // a US car says E whatever its pack: the chemistry is only the usual pack for the build, never high
    let us = vin_only("5YJ3E1EA2PF");
    assert_eq!(us.chemistry.value.as_deref(), Some("lfp"));
    assert!(us.chemistry.confidence < Some(Confidence::High));
    assert_eq!(us.trim.value.as_deref(), Some("Model 3 RWD"));
    assert_eq!(us.epa_range_km.value, Some(438.0));
    assert_eq!(us.epa_id, Some(46206));
    // a US VIN with F in position 7 (only in a withdrawn filing): unknown, never LFP
    let f = vin_only("5YJ3E1FA0PF");
    assert_eq!(f.chemistry.value, None);
    assert_eq!(f.battery_type.value, None);
    assert!(
        f.unknown_codes.iter().any(|u| u.field == "battery_type" && u.at == "vin:7" && u.code == "F"),
        "{:?}",
        f.unknown_codes
    );
}

#[test]
fn unknown_codes_give_no_value() {
    // a position-8 code no table names
    let v = vin_only("7SAYGDEZ0TF");
    assert_eq!(v.motors.value, None);
    assert_eq!(v.performance.value, None);
    assert!(v.unknown_codes.iter().any(|u| u.at == "vin:8" && u.code == "Z"));
    // a model letter the tables don't know for its maker, a maker that isn't Tesla
    let x = vin_only("5YJZE1EA0PF");
    assert_eq!(x.model.value, None);
    assert!(x.unknown_codes.iter().any(|u| u.field == "model"));
    assert_eq!(vin_only("XP7SA1E26PF").model.value, None, "Berlin builds no Model S");
    assert_eq!(model_code("SYNCFAKE0RX"), None);
    // a maker code that is not Tesla's names no model, whatever its 4th character
    assert_eq!(model_code("SYN3FAKE0RX"), None);
    let vw = vin_only("WVW3E1EA0PF");
    assert_eq!((vw.model.value.as_deref(), vw.epa_range_km.value, vw.body.value.as_deref()), (None, None, None));
    assert!(vw.unknown_codes.iter().any(|u| u.field == "maker") && vw.unknown_codes.iter().any(|u| u.field == "model"));
    let w = vin_only("WVWZZZ1KZPF");
    assert_eq!((w.model.value.clone(), w.maker.value.clone()), (None, None));
    // CAN codes beyond the public DBC's tables
    let can = CanBuild { wheel_type: Some(99), pack_energy: Some(9), exterior_color: Some(7), ..CanBuild::default() };
    let v = vehicle_info(&VehicleInputs { vin: Some("7SAYGDEE1PF".into()), can, ..VehicleInputs::default() });
    assert_eq!(v.wheel.value, None);
    assert_eq!(v.paint.value, None);
    assert_eq!(v.pack_code.value.as_deref(), Some("9"), "the raw code is kept, never turned into kWh");
    assert_eq!(v.pack.value.as_deref(), Some("Long Range / Performance (2170), 2170 cells"), "the VIN's pack stays");
    for f in ["wheel", "paint", "pack_code"] {
        assert!(
            v.unknown_codes.iter().any(|u| u.field == f && u.source == FactSource::Can),
            "{f}: {:?}",
            v.unknown_codes
        );
    }
    // nothing at all
    let none = vehicle_info(&VehicleInputs::default());
    assert_eq!(none.model.value, None);
    assert!(none.unknown_codes.is_empty() && none.disagreements.is_empty());
}

#[test]
fn can_wins_over_the_vin_and_disagreements_are_listed() {
    let can = CanBuild {
        chassis_type: Some(3),
        drivetrain_type: Some(0),
        performance_package: Some(1),
        right_hand_drive: Some(0),
        wheel_type: Some(16),
        exterior_color: Some(6),
        map_region: Some(1),
        full_pack_kwh: Some(76.2),
        buffer_kwh: Some(3.4),
        brick_count: Some(96),
        ..CanBuild::default()
    };
    let v = vehicle_info(&VehicleInputs { vin: Some("7SAYGDEE1PF".into()), can, ..VehicleInputs::default() });
    assert_eq!((v.model.value.as_deref(), v.model.source), (Some("Model Y"), Some(FactSource::Can)));
    assert_eq!((v.drive.value.as_deref(), v.drive.source), (Some("RWD"), Some(FactSource::Can)));
    assert_eq!((v.performance.value, v.performance.source), (Some(true), Some(FactSource::Can)));
    assert_eq!((v.trim.value.as_deref(), v.trim.source), (Some("Model Y Performance"), Some(FactSource::Can)));
    assert_eq!(v.wheel.value.as_deref(), Some("Uberturbine 21 Black"));
    assert_eq!((v.wheel_in.value, v.wheel_in.source), (Some(21), Some(FactSource::Can)));
    assert_eq!(v.paint.value.as_deref(), Some("Pearl White"));
    assert_eq!(v.region.value.as_deref(), Some("EU"));
    assert_eq!(v.nominal_kwh.value, Some(KwhRange { min: 76.2, max: 76.2 }));
    assert_eq!(v.usable_kwh.value, Some(KwhRange { min: 72.8, max: 72.8 }));
    assert_eq!(v.nominal_kwh.source, Some(FactSource::Can));
    let fields: Vec<&str> = v.disagreements.iter().map(|d| d.field).collect();
    assert!(fields.contains(&"drive") && fields.contains(&"performance"), "{fields:?}");
    assert!(!fields.contains(&"model") && !fields.contains(&"nominal_kwh"), "the same model, an aged pack: {fields:?}");
    let d = v.disagreements.iter().find(|d| d.field == "drive").unwrap();
    assert_eq!(d.values[0], SaidBy { value: "RWD".into(), source: FactSource::Can });
    assert_eq!(d.values[1], SaidBy { value: "AWD".into(), source: FactSource::Vin });
    // a model CAN and the VIN disagree on
    let other = CanBuild { chassis_type: Some(2), ..CanBuild::default() };
    let v = vehicle_info(&VehicleInputs { vin: Some("7SAYGDEE1PF".into()), can: other, ..VehicleInputs::default() });
    assert_eq!(v.model.value.as_deref(), Some("Model 3"));
    assert!(v.disagreements.iter().any(|d| d.field == "model"));
}

#[test]
fn can_trim_rules_and_the_legacy_message() {
    let sr =
        CanBuild { chassis_type: Some(2), performance_package: Some(3), soft_range: Some(1), ..CanBuild::default() };
    let v = vehicle_info(&VehicleInputs { vin: Some("5YJ3E1EA0KF".into()), can: sr, ..VehicleInputs::default() });
    assert_eq!(v.trim.value.as_deref(), Some("Model 3 Standard Range"));
    let early = CanBuild { performance_package: Some(1), ..CanBuild::default() };
    let v = vehicle_info(&VehicleInputs { vin: Some("5YJ3E1EB0KF".into()), can: early, ..VehicleInputs::default() });
    assert_eq!(
        (v.performance.value, v.performance.source),
        (Some(true), Some(FactSource::Can)),
        "CAN tells what B can't"
    );
    assert!(v.disagreements.is_empty());
    // a 2021 Model S sending its VIN in 0x508 is a legacy car, whatever else
    let legacy = CanBuild { vin_via: Some(0x508), ..CanBuild::default() };
    let v = vehicle_info(&VehicleInputs { vin: Some("5YJSA1E2XMF".into()), can: legacy, ..VehicleInputs::default() });
    assert_eq!((v.platform.value.as_deref(), v.platform.source), (Some("legacy"), Some(FactSource::Can)));
}

#[test]
fn the_owner_overrides_any_field() {
    let mut owner = BTreeMap::new();
    owner.insert("trim".to_string(), OwnerValue::Text("Model Y Long Range AWD".into()));
    owner.insert("drivetrain".to_string(), OwnerValue::Text("awd".into()));
    owner.insert("model_year".to_string(), OwnerValue::Number(2024.0));
    owner.insert("right_hand_drive".to_string(), OwnerValue::Bool(true));
    owner.insert("chemistry".to_string(), OwnerValue::Text("lfp".into()));
    owner.insert("nominal_kwh".to_string(), OwnerValue::Number(80.5));
    owner.insert("wheel_in".to_string(), OwnerValue::Number(20.0));
    let can = CanBuild { drivetrain_type: Some(1), full_pack_kwh: Some(78.0), ..CanBuild::default() };
    let v = vehicle_info(&VehicleInputs { vin: Some("7SAYGDEE1PF".into()), can, owner, ..VehicleInputs::default() });
    assert_eq!((v.trim.value.as_deref(), v.trim.source), (Some("Model Y Long Range AWD"), Some(FactSource::Owner)));
    assert_eq!((v.drive.value.as_deref(), v.drive.source), (Some("AWD"), Some(FactSource::Owner)));
    assert_eq!((v.model_year.value, v.model_year.source), (Some(2024), Some(FactSource::Owner)));
    assert_eq!(v.year_kind, Some("model_year"));
    assert_eq!((v.steering.value.as_deref(), v.steering.source), (Some("right"), Some(FactSource::Owner)));
    assert_eq!(v.chemistry.source, Some(FactSource::Owner));
    assert_eq!(v.nominal_kwh.value, Some(KwhRange { min: 80.5, max: 80.5 }));
    assert_eq!(v.wheel_in.value, Some(20));
    // the typed trim picks the EPA row
    assert_eq!(v.epa_name.as_deref(), Some("Model Y Long Range AWD"));
    assert!(v.trim_candidates.is_empty());
    // the typed year and steering differ from the VIN: listed
    let fields: Vec<&str> = v.disagreements.iter().map(|d| d.field).collect();
    assert!(fields.contains(&"model_year") && fields.contains(&"steering"), "{fields:?}");
}

#[test]
fn the_tesla_api_and_an_import_fill_what_is_left() {
    let api = TextBuild {
        car_type: Some("modely".into()),
        trim_badging: Some("74d".into()),
        exterior_color: Some("MidnightSilver".into()),
        wheel_type: Some("Apollo19".into()),
        right_hand_drive: Some(true),
        ..TextBuild::default()
    };
    let tm = TextBuild { lfp_battery: Some(false), trim_badging: Some("74D".into()), ..TextBuild::default() };
    let v = vehicle_info(&VehicleInputs {
        vin: Some("7SAYGDEE1PF".into()),
        tesla_api: api,
        import: Some((FactSource::Teslamate, tm)),
        ..VehicleInputs::default()
    });
    assert_eq!((v.trim.value.as_deref(), v.trim.source), (Some("74d"), Some(FactSource::TeslaApi)));
    assert_eq!((v.paint.value.as_deref(), v.paint.source), (Some("MidnightSilver"), Some(FactSource::TeslaApi)));
    assert_eq!((v.wheel_in.value, v.wheel_in.source), (Some(19), Some(FactSource::TeslaApi)));
    assert_eq!(v.steering.value.as_deref(), Some("left"), "the VIN wins over the API");
    assert!(v.disagreements.iter().any(|d| d.field == "steering"));
    assert_eq!(v.chemistry.source, Some(FactSource::Vin));
    assert!(v.disagreements.iter().all(|d| d.field != "trim"), "API words are not compared");
    // with no VIN, the API and the import still say something
    let v = vehicle_info(&VehicleInputs {
        tesla_api: TextBuild { car_type: Some("model3".into()), ..TextBuild::default() },
        import: Some((FactSource::Teslamate, TextBuild { lfp_battery: Some(true), ..TextBuild::default() })),
        ..VehicleInputs::default()
    });
    assert_eq!((v.model.value.as_deref(), v.model.source), (Some("Model 3"), Some(FactSource::TeslaApi)));
    assert_eq!((v.chemistry.value.as_deref(), v.chemistry.source), (Some("lfp"), Some(FactSource::Teslamate)));
}

#[test]
fn trims_and_ranges_from_the_epa_table() {
    // two EPA trims fit a 2023 Model Y AWD: none is chosen, both are named
    let v = vin_only("7SAYGDEE1PF");
    assert_eq!(v.trim.value, None);
    assert_eq!(v.trim_candidates, vec!["Model Y AWD".to_string(), "Model Y Long Range AWD".to_string()]);
    assert_eq!(v.epa_range_km.value, None);
    // Plaid by position 8; the range waits for the wheel size
    let plaid = vin_only("5YJSA1E60NF");
    assert_eq!(plaid.trim.value.as_deref(), Some("Model S Plaid"));
    assert_eq!(plaid.epa_range_km.value, None);
    assert_eq!(plaid.wheel_options_in, vec![19, 21]);
    let can = CanBuild { tire_type: Some(18), ..CanBuild::default() };
    let v = vehicle_info(&VehicleInputs { vin: Some("5YJSA1E60NF".into()), can, ..VehicleInputs::default() });
    assert_eq!(v.wheel_in.value, Some(21));
    assert_eq!(v.epa_range_km.value, Some(560.0));
    assert_eq!(v.epa_name.as_deref(), Some("Model S Plaid (21 inch wheels)"));
    // 2014 Model S with an 85 kWh class pack in position 7, a 2013 S by its battery tier
    let s85 = vin_only("5YJSA1H10EF");
    assert_eq!(s85.trim.value.as_deref(), Some("Model S (85 kW-hr battery pack)"));
    assert_eq!(s85.nominal_kwh.value, Some(KwhRange { min: 81.0, max: 82.0 }));
    let s13 = vin_only("5YJSA1DN0DF");
    assert_eq!(s13.trim.value.as_deref(), Some("Model S 85"));
    // a Berlin car has no EPA rating
    let b = vin_only("XP7YGCEK4PB");
    assert_eq!(b.epa_range_km.value, None);
    assert!(b.notes.iter().any(|n| n.contains("EPA")));
    assert_eq!(epa_trim("Model 3 Long Range Performance AWD (18in)"), "Model 3 Long Range Performance AWD");
    assert_eq!(epa_trim("Model S AWD - P85D"), "Model S AWD - P85D");
    assert_eq!(epa_trim("Model S (60 kW-hr battery pack)"), "Model S (60 kW-hr battery pack)");
    assert!(epa_performance("Model S AWD - P90D") && !epa_performance("Model S AWD - 90D"));
}

#[test]
fn vin_info_keeps_eleven_characters_and_knows_every_model() {
    let v = vin_info("7G2CEHEE8PF000004", 0).unwrap();
    assert_eq!((v.vin_prefix.as_str(), v.model, v.year, v.plant), ("7G2CEHEE8PF", Some('C'), Some(2023), Some('F')));
    assert_eq!(vin_info("5YJ3E1EA2PF000002", 0x405).unwrap().model, Some('3'));
    assert_eq!(vin_info("5YJRE1EA0PF000003", 0).unwrap().model, None, "the Roadster is not shown");
    assert_eq!(vin_info("5YJZE1EA2PF000002", 0).unwrap().model, None);
    assert!(vin_info("5YJ3E1EA2PF00000I", 0).is_none());
    assert!(vin_info("5YJ3E1EA2PF00000", 0).is_none());
    assert_eq!(vin_prefix("5yj3e1ea2pf000002").as_deref(), Some("5YJ3E1EA2PF"));
    assert_eq!(year_of('8', Some('R')), Some(2008));
    assert_eq!(year_of('8', Some('Y')), Some(2038));
    assert_eq!(platform_of("5YJSA1E2XMF"), Some("legacy"));
    assert_eq!(platform_of("5YJSA1E57MF"), Some("refresh"));
    assert_eq!(platform_of("7SAYGDEE1PF"), None);
    assert_eq!(platform_of("5YJSA1E26PF"), Some("legacy"), "position 8 tells, outside the table's years too");
    assert_eq!(model_name('C'), Some("Cybertruck"));
    // only the first 11 characters are read: a full VIN and its prefix decode the same
    let full = vin_only("5YJ3E1EA2PF000002");
    assert_eq!(full, vin_only("5YJ3E1EA2PF"));
    assert_eq!(full.vin_prefix.as_deref(), Some("5YJ3E1EA2PF"));
    assert!(!serde_json_free_text(&full).contains("000002"));
}

fn serde_json_free_text(v: &VehicleInfo) -> String {
    format!("{v:?}")
}

#[test]
fn pure_and_offline() {
    let inputs = VehicleInputs {
        vin: Some("7SAYGDEE1PF".into()),
        can: CanBuild { drivetrain_type: Some(1), ..CanBuild::default() },
        ..VehicleInputs::default()
    };
    assert_eq!(vehicle_info(&inputs), vehicle_info(&inputs), "the same inputs give the same answer");
    // the code path cannot reach the network: the crate depends on serde alone, and its code names no socket,
    // process, file or clock
    let manifest = include_str!("../../Cargo.toml");
    let deps = manifest.split("[dependencies]").nth(1).unwrap().split("[dev-dependencies]").next().unwrap();
    let names: Vec<&str> =
        deps.lines().filter_map(|l| l.split('=').next()).map(str::trim).filter(|n| !n.is_empty()).collect();
    assert_eq!(names, vec!["serde"]);
    for (name, code) in [("mod.rs", include_str!("mod.rs")), ("tables.rs", include_str!("tables.rs"))] {
        for word in ["std::net", "TcpStream", "UdpSocket", "std::process", "std::fs", "SystemTime", "env::var"] {
            assert!(!code.contains(word), "{name} names {word}");
        }
    }
}

#[test]
fn can_build_takes_decoded_signals() {
    let mut c = CanBuild::default();
    assert!(c.take("GTW_chassisType", 3.0));
    assert!(c.take("GTW_wheelType", 16.0));
    assert!(!c.take("GTW_wheelType", 1.5), "not a code");
    assert!(c.take("BMS_nominalFullPackEnergy", 78.4));
    assert!(!c.take("BMS_nominalFullPackEnergy", 400.0));
    assert!(c.take("BattBrickVoltageMaxNum332", 41.0));
    assert!(c.take("BattBrickVoltageMinNum332", 96.0));
    assert!(c.take("BattBrickVoltageMaxNum332", 12.0));
    assert!(!c.take("DI_vehicleSpeed", 50.0));
    assert_eq!(
        (c.chassis_type, c.wheel_type, c.full_pack_kwh, c.brick_count),
        (Some(3), Some(16), Some(78.4), Some(96))
    );
    let mut newer = CanBuild { wheel_type: Some(17), ..CanBuild::default() };
    newer.fill_from(&c);
    assert_eq!((newer.wheel_type, newer.chassis_type, newer.brick_count), (Some(17), Some(3), Some(96)));
    assert!(CanBuild::default().is_empty() && !newer.is_empty());
}

#[test]
fn base_trims_are_not_told_apart_by_can() {
    // GTW_performancePackage 0 with GTW_drivetrainType 0 or 1 is every base car: RWD and Long Range RWD, the 4680
    // AWD and the Long Range AWD look the same; the VIN's candidates stay and no range is chosen
    let base = |vin: &str, drive: u32| {
        let can = CanBuild { performance_package: Some(0), drivetrain_type: Some(drive), ..CanBuild::default() };
        vehicle_info(&VehicleInputs { vin: Some(vin.into()), can, ..VehicleInputs::default() })
    };
    for (vin, drive, both) in [
        ("5YJ3E1EA0RF", 0, ["Model 3 RWD", "Model 3 Long Range RWD"]),
        ("7SAYGDED0RF", 0, ["Model Y RWD", "Model Y Long Range RWD"]),
        ("7SAYGDEE0PA", 1, ["Model Y AWD", "Model Y Long Range AWD"]),
        ("7SAYGDEE0TF", 1, ["Model Y Standard AWD", "Model Y Long Range AWD"]),
    ] {
        let v = base(vin, drive);
        assert_eq!(v.trim.value, None, "{vin}");
        for n in both {
            assert!(v.trim_candidates.iter().any(|c| c == n), "{vin}: {n} in {:?}", v.trim_candidates);
        }
        assert_eq!((v.epa_range_km.value, v.epa_name.as_deref()), (None, None), "{vin}");
    }
    // a 2027 Model 3 RWD can be the RWD or the Premium RWD
    let v = base("5YJ3E1EA0VF", 0);
    assert_eq!((v.trim.value.as_deref(), v.epa_range_km.value), (None, None), "{:?}", v.trim_candidates);
    // the rules that do tell a trim still do
    let can = CanBuild { performance_package: Some(1), ..CanBuild::default() };
    let v = vehicle_info(&VehicleInputs { vin: Some("7SAYGDEE0PA".into()), can, ..VehicleInputs::default() });
    assert_eq!((v.trim.value.as_deref(), v.trim.source), (Some("Model Y Performance"), Some(FactSource::Can)));
    assert!(t_has_no_base_rule());
}

fn t_has_no_base_rule() -> bool {
    tables().trim_can.iter().all(|r| r.performance_package != Some(0))
}

#[test]
fn the_vin_outranks_the_brick_count_for_the_chemistry() {
    // Shanghai, position 7 E: NCA/NMC by Tesla's manual; 106 bricks would say LFP, but that rule is not checked yet
    let can = CanBuild { brick_count: Some(106), ..CanBuild::default() };
    let v = vehicle_info(&VehicleInputs { vin: Some("LRW3E7EB0PC".into()), can, ..VehicleInputs::default() });
    assert_eq!((v.chemistry.value.as_deref(), v.chemistry.source), (Some("nca_nmc"), Some(FactSource::Vin)));
    assert_eq!(v.chemistry.confidence, Some(Confidence::High));
    // the brick count alone says nothing of it either
    let can = CanBuild { brick_count: Some(106), ..CanBuild::default() };
    let v = vehicle_info(&VehicleInputs { can, ..VehicleInputs::default() });
    assert_eq!(v.chemistry.value, None);
}

#[test]
fn a_us_vin_only_hints_at_the_chemistry() {
    for vin in ["5YJ3E1EA0PF", "7SAYGDED0PF"] {
        let v = vin_only(vin);
        assert_eq!((v.chemistry.value.as_deref(), v.chemistry.source), (Some("lfp"), Some(FactSource::Vin)), "{vin}");
        assert_eq!(v.chemistry.confidence, Some(Confidence::Low), "{vin}: the usual pack for the build, no more");
        assert!(v.chemistry.note.as_deref().is_some_and(|n| n.contains("does not tell")), "{vin}");
    }
}

#[test]
fn trims_with_their_own_factory_wheels() {
    // a 2026 Model Y RWD can be a Long Range (19/20) or a Standard (18/19, EPA 50040 and 50041)
    assert_eq!(vin_only("7SAYGDED0TF").wheel_options_in, vec![18, 19, 20]);
    let owner = |trim: &str| {
        let mut o = BTreeMap::new();
        o.insert("trim".to_string(), OwnerValue::Text(trim.into()));
        o
    };
    let v = vehicle_info(&VehicleInputs {
        vin: Some("7SAYGDED0TF".into()),
        owner: owner("Model Y Standard RWD"),
        ..VehicleInputs::default()
    });
    assert_eq!((v.wheel_options_in.clone(), v.wheel_in.value), (vec![18, 19], None));
    let v = vehicle_info(&VehicleInputs {
        vin: Some("7SAYGDED0TF".into()),
        owner: owner("Model Y Long Range RWD"),
        ..VehicleInputs::default()
    });
    assert_eq!(v.wheel_options_in, vec![19, 20]);
    // the 2027 Model Y L comes on 20 in only
    let v = vehicle_info(&VehicleInputs {
        vin: Some("7SAYGDEE0VF".into()),
        owner: owner("Model Y L"),
        ..VehicleInputs::default()
    });
    assert_eq!((v.wheel_in.value, v.wheel_in.source), (Some(20), Some(FactSource::Vin)));
    assert!(v.wheel_options_in.is_empty());
    assert_eq!(v.epa_id, Some(50862));
}

#[test]
fn performance_codes_of_the_current_manuals() {
    // Model Y 2025+ manual: F is Dual Motor - Performance for model years S/T/V
    let v = vin_only("7SAYGFEF0TF");
    assert_eq!((v.motors.value, v.drive.value.as_deref(), v.performance.value), (Some(2), Some("AWD"), Some(true)));
    assert!(v.unknown_codes.iter().all(|u| u.field != "motors"));
    // Model 3 2024+ manual: T is Dual Motor Performance at plants C and F
    let v = vin_only("LRW3E7ET0RC");
    assert_eq!((v.motors.value, v.drive.value.as_deref(), v.performance.value), (Some(2), Some("AWD"), Some(true)));
    assert!(v.unknown_codes.iter().all(|u| u.field != "motors"));
}

#[test]
fn full_test_vins_are_made_up() {
    // the repo's rule: a whole VIN in a test ends in PF0000 and two digits
    for src in [include_str!("tests.rs"), include_str!("../lib.rs")] {
        let b = src.as_bytes();
        for i in 0..b.len().saturating_sub(16) {
            let w = &b[i..i + 17];
            let vin_char = |c: &u8| matches!(c, b'A'..=b'H' | b'J'..=b'N' | b'P' | b'R'..=b'Z' | b'0'..=b'9');
            let edge = |j: Option<&u8>| j.is_none_or(|c| !c.is_ascii_alphanumeric());
            if w.iter().all(vin_char)
                && w.iter().any(u8::is_ascii_uppercase)
                && edge(i.checked_sub(1).and_then(|j| b.get(j)))
                && edge(b.get(i + 17))
            {
                let s = std::str::from_utf8(w).unwrap();
                assert!(s[9..15] == *"PF0000" && s[15..].bytes().all(|c| c.is_ascii_digit()), "{s}");
            }
        }
    }
}

#[test]
fn only_sources_that_exist_are_named() {
    // no TeslaFi import exists: no fact can say it came from one
    let words: Vec<&str> =
        [FactSource::Owner, FactSource::Can, FactSource::Vin, FactSource::TeslaApi, FactSource::Teslamate]
            .iter()
            .map(|s| s.word())
            .collect();
    assert_eq!(words, ["owner", "can", "vin", "tesla_api", "teslamate"]);
    assert!(serde_json::from_str::<FactSource>("\"teslafi\"").is_err());
}

#[test]
fn the_door_placard_gives_the_tyre_pressures_for_a_known_wheel_size() {
    // a made-up Model Y prefix, model year 2023 (the VIN's)
    let with_wheel = |w: Option<f64>| {
        let mut owner = BTreeMap::new();
        if let Some(w) = w {
            owner.insert("wheel_in".to_string(), OwnerValue::Number(w));
        }
        vehicle_info(&VehicleInputs { vin: Some("7SAYGDEE1PF".into()), owner, ..VehicleInputs::default() })
    };
    let p = placard_pressure(&with_wheel(Some(20.0))).unwrap();
    assert_eq!((p.front_kpa, p.rear_kpa, p.wheel_in), (290.0, 290.0, 20));
    assert!(p.source.contains("Owner's Manual"), "{}", p.source);
    // a size the table has no row for (a Model Y on 18 in wheels in 2023): nothing guessed
    assert_eq!(placard_pressure(&with_wheel(Some(18.0))), None);
    // the size not known (this build came with 19 or 20 in): nothing guessed either
    let v = with_wheel(None);
    assert_eq!(v.wheel_in.value, None, "{:?}", v.wheel_options_in);
    assert_eq!(placard_pressure(&v), None);
    // no model known
    assert_eq!(placard_pressure(&vehicle_info(&VehicleInputs::default())), None);
}
