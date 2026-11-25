//! Mending a car's damaged drives table: what can be made again is made again, what is kept somewhere else is put
//! back, and what can't be recovered is said, with the time it covered.
//!
//! The table (`drives.snap` and `drives.log`) holds three kinds of rows. What is not in it is not touched: the
//! owner's purposes, relations and notes live in the key/value store (`drives`), by drive id.
//!
//! 1. **Rows made from the logs and the Tesla API** (source `can`) are made again by a rebuild from the stored logs,
//!    from where the damage begins. The ones that read are kept as they are (and keep their ids).
//! 2. **Imported rows** (TeslaMate, TeslaFi) that read are kept. The rest are read again from the uploads kept
//!    for the imports ([`ImportReplay`], given by the web side), each with the id it had; the ones an import's rows
//!    were moved off this car are held out, and every import whose upload is gone is reported with its span.
//!    Rows that came from a local TeslaMate's catch-up have no kept file: they are reported lost.
//! 3. **Typed values** (purposes and notes) are put back on the drive they belong to: a purpose whose drive has no
//!    id of its own any more goes to the drive that overlaps the second its id names; with none, it is kept under
//!    `car/<id>/purposes-unmatched` and reported, never dropped.
//!
//! The damaged files are put aside (never deleted) in the car's `aside` folder first. The report is kept in the key/value
//! store (`engine`, `car/<id>/repair/drives`) and is what `GET /cars/{car}/repair` gives.

use crate::query::logbook;
use crate::records::{self, NS};
use crate::versions::Cause;
use crate::{Engine, Result, lock, now_ms};
use db::Table;
use model::{CarId, Drive, Source};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

/// What of a car's imports could be read again from the files kept for them.
#[derive(Debug, Clone, Default)]
pub struct Replayed {
    /// The drives the kept uploads give for the car, as an import stores them (their ids are the import's).
    pub drives: Vec<Drive>,
    /// Imports of the car whose rows can't be read again.
    pub gone: Vec<Gone>,
    /// Rows left out because they were moved off the car to another, or to wait for one.
    pub held_out: Vec<HeldOut>,
}

/// An import of the car whose upload can't be read: not kept, or damaged.
#[derive(Debug, Clone)]
pub struct Gone {
    pub source: Source,
    pub name: String,
    pub from_ms: Option<u64>,
    pub to_ms: Option<u64>,
    pub why: String,
}

/// Rows of an import that were moved off the car and so are not put back.
#[derive(Debug, Clone)]
pub struct HeldOut {
    pub source: Source,
    pub name: String,
    pub rows: u64,
    pub from_ms: Option<u64>,
    pub to_ms: Option<u64>,
}

/// Reads the imports' kept uploads again (the web side keeps them; the engine does not know where).
pub trait ImportReplay: Send + Sync {
    /// The imported drives of `car` the kept uploads give, and what they could not.
    fn drives(&self, engine: &Engine, car: CarId) -> Replayed;
}

/// Something a repair could not give back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lost {
    /// What: `imported drives`, `drives made from logs`, `purposes of drives that are gone`.
    pub what: String,
    /// Where it came from: `teslamate`, `teslafi`, `can`, `owner`.
    pub source: String,
    /// The time it covers (`None` at an end: not known, or up to now).
    pub from_ms: Option<u64>,
    pub to_ms: Option<u64>,
    /// How many there should have been (the car's summary counted them), and how many are there.
    pub expected: Option<u64>,
    pub found: Option<u64>,
    pub why: String,
}

/// What a repair of a car's table did and could not do.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RepairReport {
    pub table: String,
    pub started_ms: u64,
    /// When the rebuild and the putting back of purposes that follow were done (`None`: under way).
    pub finished_ms: Option<u64>,
    /// Where the damaged files were put aside, under the data folder.
    pub aside: Option<String>,
    /// What was wrong, as the files read.
    pub damage: Vec<String>,
    /// Rows read from the files as they were.
    pub salvaged: u64,
    /// Imported rows read again from the kept uploads.
    pub reimported: u64,
    /// Rows made from the logs that the rebuild gave (known when it ended).
    pub rebuilt: Option<u64>,
    /// Purposes put on the drive that is theirs now.
    pub relinked: u64,
    /// Purposes kept aside (`car/<id>/purposes-unmatched`) because no drive is theirs.
    pub unmatched: u64,
    /// The rebuild reaches back to here.
    pub rebuild_from_ms: Option<u64>,
    pub lost: Vec<Lost>,
    /// Rows made from the logs the car's summary counted before.
    #[serde(default)]
    pub live_expected: u64,
    /// Of the rows read, those made from logs.
    #[serde(default)]
    pub live_salvaged: u64,
    /// Whether an entry was written to the audit log.
    #[serde(default)]
    pub audited: bool,
}

impl RepairReport {
    /// The rebuild and the putting back are not done yet.
    pub fn running(&self) -> bool {
        self.finished_ms.is_none()
    }
}

fn report_key(car: CarId) -> String {
    format!("car/{car}/repair/drives")
}

/// Whether `err` is damage to the drives table of `car`.
pub fn is_drives_damage(car: CarId, err: &db::Error) -> bool {
    match err {
        db::Error::Damaged { file, .. } => {
            file == &format!("db/cars/{car}/drives.snap") || file == &format!("db/cars/{car}/drives.log")
        }
        _ => false,
    }
}

impl Engine {
    /// Gives the engine the way to read the imports' kept uploads again (the web server does, at its start).
    pub fn set_import_replay(&self, r: Arc<dyn ImportReplay>) {
        *lock(&self.inner.replay) = Some(r);
    }

    /// The report of the car's last repair of its drives table.
    pub fn repair_report(&self, car: CarId) -> Result<Option<RepairReport>> {
        records::get(&self.inner.db, NS, &report_key(car))
    }

    /// Whether a repair of the car is under way (asked for and not done).
    pub fn repair_running(&self, car: CarId) -> bool {
        lock(&self.inner.repairing).contains(&car)
    }

    /// The cars whose repair ended and has no audit entry yet (the first call looks at every car's report).
    pub fn repairs_to_audit(&self) -> Vec<(CarId, RepairReport)> {
        let mut todo = lock(&self.inner.repair_audit);
        if !todo.scanned {
            todo.scanned = true;
            let keys = self.inner.db.kv().keys(NS).unwrap_or_default();
            for k in keys.iter().filter(|k| k.starts_with("car/") && k.ends_with("/repair/drives")) {
                if let Some(car) = k.split('/').nth(1).and_then(|c| c.parse::<CarId>().ok()) {
                    todo.cars.insert(car);
                }
            }
        }
        let cars: Vec<CarId> = todo.cars.iter().copied().collect();
        drop(todo);
        cars.into_iter()
            .filter_map(|c| match self.repair_report(c) {
                Ok(Some(r)) if !r.running() && !r.audited => Some((c, r)),
                _ => None,
            })
            .collect()
    }

    /// Notes that the audit entry of the car's repair (`started_ms`) is written.
    pub fn repair_audited(&self, car: CarId, started_ms: u64) -> Result<()> {
        if let Some(mut r) = self.repair_report(car)?
            && r.started_ms == started_ms
        {
            r.audited = true;
            records::put(&self.inner.db, NS, &report_key(car), &r)?;
        }
        Ok(())
    }

    /// Claims the car for a repair about to be started with [`Engine::repair_table`] (which releases it when it
    /// returns). `false`: one is under way already.
    pub fn repair_begin(&self, car: CarId) -> bool {
        lock(&self.inner.repairing).insert(car)
    }

    /// [`Engine::repair_table`] of the drives table.
    pub fn repair_drives_table(&self, car: CarId) -> Result<Option<RepairReport>> {
        self.repair_table(car, Table::Drives)
    }

    /// Repairs the car's drives table, if it is damaged: what is lost is read as far as it reads, the files are put
    /// aside, the table is written again, and the rebuild that makes the rest again is started (from where the loss
    /// begins). `Ok(None)`: nothing is damaged. The rebuild and the putting back of purposes finish in the
    /// background; the report says when.
    pub fn repair_table(&self, car: CarId, table: Table) -> Result<Option<RepairReport>> {
        if table != Table::Drives {
            return Err(crate::Error::Other("only the drives table is repaired".into()));
        }
        lock(&self.inner.repairing).insert(car);
        let r = (|| {
            let l = self.car_lock(car);
            let held = lock(&l);
            let done = self.repair_drives_locked(car)?;
            drop(held);
            if let Some(r) = &done {
                self.rebuild_because(
                    crate::Scope { car: Some(car), from_ms: r.rebuild_from_ms, to_ms: None },
                    crate::jobs::What::WHOLE,
                    Cause::new(
                        "table_repaired",
                        "The car's drives table was damaged and repaired: what is made from the logs is made again",
                    ),
                    false,
                )?;
            }
            Ok(done)
        })();
        lock(&self.inner.repairing).remove(&car);
        r
    }

    /// [`Engine::repair_table`] without the rebuild, for a caller that rebuilds anyway, with the car's lock held.
    /// `Ok(None)`: the table reads, nothing was done.
    pub(crate) fn repair_drives_locked(&self, car: CarId) -> Result<Option<RepairReport>> {
        let db = &self.inner.db;
        let salvage = db.salvage_table::<Drive>(car)?;
        if !salvage.lost_something() {
            return Ok(None);
        }
        let started_ms = now_ms();
        let tally = db.entry(car)?.map(|e| e.summary.tally).unwrap_or_default();
        let damage = salvage.why();
        let cut_from = salvage.snapshot.found.then_some(salvage.snapshot.lost_from_ms).flatten();
        let from_ms = salvage.lost_from_ms();
        tracing::warn!(
            "car {car}: its drives table is damaged ({}): {} rows read, the files are put aside and the table is \
             written again",
            damage.join("; "),
            salvage.rows.len()
        );
        let aside = db.set_aside_table(car, Table::Drives, &damage.join("; "))?;
        let mut rows = salvage.rows;
        let mut have: HashSet<u64> = rows.iter().map(|d| d.id).collect();
        let salvaged = rows.len() as u64;
        let live_salvaged = rows.iter().filter(|d| !d.source.is_imported()).count() as u64;
        // the imports' own files, for the imported rows that did not read
        let replay = lock(&self.inner.replay).clone();
        let replayed = replay.map(|r| r.drives(self, car)).unwrap_or_default();
        let mut reimported = 0u64;
        for d in replayed.drives {
            if have.insert(d.id) {
                rows.push(d);
                reimported += 1;
            }
        }
        db.restore_table(car, &rows)?;
        let mut lost = Vec::new();
        for site in [model::ImportSite::Teslamate, model::ImportSite::Teslafi] {
            let source = Source::of_site(site);
            let expected = u64::try_from(tally.of_site(site).drives).unwrap_or(0);
            let found = rows.iter().filter(|d| d.source == source).count() as u64;
            if found >= expected {
                continue;
            }
            let mine = |s: Source| s == source;
            let gone: Vec<&Gone> = replayed.gone.iter().filter(|g| mine(g.source)).collect();
            let held: Vec<&HeldOut> = replayed.held_out.iter().filter(|g| mine(g.source)).collect();
            let mut why = Vec::new();
            for g in &gone {
                why.push(format!("the upload of the import {} can't be read again: {}", g.name, g.why));
            }
            for h in &held {
                why.push(format!(
                    "{} rows of the import {} were moved off this car and are not put back",
                    h.rows, h.name
                ));
            }
            if gone.is_empty() && held.is_empty() {
                why.push("no kept upload holds them (rows read from a local TeslaMate, or an import discarded)".into());
            }
            let froms = gone.iter().filter_map(|g| g.from_ms).chain(held.iter().filter_map(|h| h.from_ms));
            let tos = gone.iter().filter_map(|g| g.to_ms).chain(held.iter().filter_map(|h| h.to_ms));
            let (a, b) = (froms.min(), tos.max());
            // a snapshot cut says from where; the imports' own spans say more where they are known
            let from = match (cut_from, a) {
                (Some(c), Some(a)) => Some(c.min(a)),
                (c, a) => c.or(a),
            };
            lost.push(Lost {
                what: "imported drives".into(),
                source: site.word().into(),
                from_ms: from,
                to_ms: b,
                expected: Some(expected),
                found: Some(found),
                why: why.join("; "),
            });
        }
        let report = RepairReport {
            table: "drives".into(),
            started_ms,
            aside,
            damage,
            salvaged,
            reimported,
            rebuild_from_ms: Some(from_ms.unwrap_or(0)),
            lost,
            live_expected: u64::try_from(tally.can.drives).unwrap_or(0),
            live_salvaged,
            ..Default::default()
        };
        records::put(db, NS, &report_key(car), &report)?;
        lock(&self.inner.repair_audit).cars.insert(car);
        Ok(Some(report))
    }

    /// The repair of the car's table is done once its rows made from the logs are back: counts them, puts the owner's
    /// purposes on the drives that are theirs now, and says what is lost. Does nothing when no repair is under way.
    pub(crate) fn finish_repair(&self, car: CarId) -> Result<()> {
        let db = &self.inner.db;
        let Some(mut r) = self.repair_report(car)?.filter(RepairReport::running) else { return Ok(()) };
        let live = crate::pipeline::all_rows::<Drive>(db, car, 0, u64::MAX, Some(Source::Live))?.len() as u64;
        let moved = logbook::relink_purposes(db, car)?;
        r.relinked += moved.relinked;
        r.unmatched = moved.unmatched;
        r.rebuilt = Some(live.saturating_sub(r.live_salvaged));
        if live < r.live_expected {
            r.lost.push(Lost {
                what: "drives made from logs".into(),
                source: "can".into(),
                from_ms: r.rebuild_from_ms,
                to_ms: None,
                expected: Some(r.live_expected),
                found: Some(live),
                why: "the stored logs make fewer drives than the table held (a log that held one is gone or can't be \
                      read)"
                    .into(),
            });
        }
        if moved.unmatched > 0 {
            r.lost.push(Lost {
                what: "purposes of drives that are gone".into(),
                source: "owner".into(),
                from_ms: None,
                to_ms: None,
                expected: Some(moved.unmatched),
                found: Some(0),
                why: "no drive is theirs now; they are kept (car/<id>/purposes-unmatched) and go back on a drive that \
                      has their id again"
                    .replace("<id>", &car.to_string()),
            });
        }
        r.finished_ms = Some(now_ms());
        records::put(db, NS, &report_key(car), &r)?;
        lock(&self.inner.repair_audit).cars.insert(car);
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct AuditList {
    scanned: bool,
    cars: std::collections::BTreeSet<CarId>,
}
