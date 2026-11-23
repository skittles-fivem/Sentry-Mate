#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! One software install whose fingerprints flap while the modules restart, laid down by hand (made-up values, a
//! made-up car): the front body controller's app CRC changes five times in eleven minutes, once back to the old
//! value, and the battery's once. It is one "Tesla update installed" with the settled set; stored the way updates
//! were found before installs were grouped (six of them), with a name typed on one, it is one again after a restart,
//! and the name stays with it.

use engine::query::{self, Paging, Period};
use engine::{Engine, Options};
use std::sync::Arc;

const MIN: u64 = 60_000;
/// 2026-11-22T00:00:00Z.
const NOV22: u64 = 1_795_305_600_000;

fn car() -> model::Car {
    model::Car {
        id: 0,
        name: "Made-up".into(),
        vehicle_keys: Vec::new(),
        preferred_key: None,
        vin: None,
        profile_setting: None,
        time_zone: "UTC".into(),
        time_zone_typed: Some(true),
        efficiency_wh_km: None,
        created_ms: 0,
        inputs: Default::default(),
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

fn updates(e: &Engine, car: u32) -> Vec<query::software::UpdateView> {
    tokio::task::block_in_place(|| query::software::updates(e, car, &Period::all(), &Paging::first(100)))
        .unwrap()
        .body
        .items
}

#[test]
fn a_flapping_install_is_one_update_and_stored_ones_regroup_with_their_name() {
    rt().block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = vault::Vault::create(&data, &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
        let v = Arc::new(v);
        let db = db::Db::open(&data, v.clone(), db::Options::default()).unwrap();
        let store = ingest::Store::with_limits(data.join("can"), v, 0, 200_000, |_| u64::MAX);
        let engine = Engine::new(db.clone(), store.clone(), Options { workers: 1, ..Options::default() });
        engine.start().unwrap();
        let id = db.create_car(car()).unwrap().id;

        // one log a minute long at each of these minutes after 16:00 UTC, with these values
        let (a, b, c) = (0x06ac_6a37, 0x01b6_621d, 0x5e17_c0de);
        let logs: [(u64, u64, u64); 8] = [
            (0, a, 0x11),
            (9, b, 0x11),
            (11, a, 0x11),
            (13, b, 0x11),
            (15, c, 0x11),
            (18, b, 0x11),
            (20, b, 0x22),
            (90, b, 0x22),
        ];
        for (i, &(m, vcfront, bms)) in logs.iter().enumerate() {
            let t = NOV22 + 16 * 60 * MIN + m * MIN;
            let name = format!("mk1_20261122T16{m:02}00Z_{i}.log.xz");
            let vals = [
                ("VCFRONT_infoAppCrc", vcfront, t, t + MIN),
                ("BMS_appCrc", bms, t, t + MIN),
                ("DIR_applicationCrc", 0x99, t, t + MIN),
            ];
            let name = if m >= 60 { format!("mk1_20261122T17{:02}00Z_{i}.log.xz", m - 60) } else { name };
            tokio::task::block_in_place(|| engine.fw_note_values_for_tests(id, &name, &vals)).unwrap();
        }
        let one = updates(&engine, id);
        assert_eq!(one.len(), 1, "{one:?}");
        assert_eq!(one[0].detected_by, "fingerprint");
        assert_eq!(
            (one[0].start_ms, one[0].end_ms),
            (NOV22 + 16 * 60 * MIN + MIN, Some(NOV22 + 16 * 60 * MIN + 20 * MIN))
        );
        let changes: Vec<(&str, &str, &str)> =
            one[0].changes.iter().map(|c| (c.field.as_str(), c.old.as_str(), c.new.as_str())).collect();
        assert_eq!(
            changes,
            vec![("VCFRONT_infoAppCrc", "06ac6a37", "01b6621d"), ("BMS_appCrc", "00000011", "00000022")],
            "the settled set, not the values in between"
        );

        // stored as found before installs were grouped: one update per step
        tokio::task::block_in_place(|| engine.fw_store_ungrouped_for_tests(id)).unwrap();
        let steps = updates(&engine, id);
        assert_eq!(steps.len(), 6, "{steps:?}");
        // the owner typed the version on one in the middle (a made-up version)
        let mid: u64 = steps[2].id.parse().unwrap();
        tokio::task::block_in_place(|| query::software::set_version(&engine, id, mid, Some("2026.44.1"))).unwrap();

        // a restart finds them again: one update, the name with it
        let again = Engine::new(db.clone(), store, Options::default());
        again.start().unwrap();
        again.idle().await;
        let after = updates(&again, id);
        assert_eq!(after.len(), 1, "{after:?}");
        assert_eq!(after[0].version.as_deref(), Some("2026.44.1"));
        assert_eq!(after[0].version_source, Some("owner"));
        assert_eq!(after[0].changes.len(), 2);
        // and stays so
        again.reconcile(Some(id));
        again.idle().await;
        assert_eq!(updates(&again, id).len(), 1);
    });
}
