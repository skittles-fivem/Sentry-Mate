//! Reading what can be read of a table whose files are damaged, and putting it aside.
//!
//! A table is a snapshot (sealed in 64 KiB chunks, compressed JSON of its rows in start order) and a journal of the
//! changes since (one sealed record per piece of an entry). Nothing in the normal read goes on past a bad chunk or
//! record, and nothing damaged is ever read as empty. Salvage reads the other way: as far as the bytes are good.
//!
//! - **Snapshot.** The chunks before the first bad one open, a streaming decompressor gives the output up to the
//!   break, and the rows are parsed one by one until one does not parse. The rows read are whole rows; the ones after
//!   them are lost, and so are any others that start as late as the last one read (the table is in start order, so
//!   [`SnapshotPart::lost_from_ms`] is where the loss begins).
//! - **Journal.** Each record opens or not on its own; the entries that read whole are applied in order, the ones
//!   that did not are counted. What a lost entry held is not known (a batch of rows put or removed at some time), so
//!   a journal's loss has no span.
//!
//! Nothing here deletes anything: [`Db::set_aside_table`] moves the files into `cars/<id>/aside/<time>/`.

use crate::error::{Error, Result};
use crate::file::{Ctx, Kind, check_header, sync_dir};
use crate::journal::{Files, Journal};
use crate::logged::{Logged, parse_gen_level};
use crate::rows::{Item, Table};
use crate::table::{Batch, TimeTable};
use crate::{Db, codec};
use model::CarId;
use std::io::Read;
use std::path::Path;

/// What of a table's snapshot could be read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotPart {
    /// There is a snapshot file.
    pub found: bool,
    /// It read to its end: nothing in it is lost.
    pub whole: bool,
    /// Rows read from it.
    pub rows: usize,
    /// Rows that start from here on may be lost (`None`: nothing of the snapshot is lost). `Some(0)`: all of it.
    pub lost_from_ms: Option<u64>,
    pub why: Option<String>,
}

/// What of a table's journal could be read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JournalPart {
    pub found: bool,
    /// Entries read and applied.
    pub applied: u64,
    /// Entries that did not read (what they held is lost; when and what is not known).
    pub lost_entries: u64,
    /// Records after the last one read can't be reached.
    pub lost_tail: bool,
    /// It follows an older snapshot than the one read: its changes were folded in already, and it was left out.
    pub stale: bool,
    pub why: Option<String>,
}

/// What reading a table as far as it reads gave.
#[derive(Debug, Clone)]
pub struct Salvage<T> {
    /// The rows: the snapshot's rows that read, with every journal entry that read applied, in start order.
    pub rows: Vec<T>,
    pub snapshot: SnapshotPart,
    pub journal: JournalPart,
}

impl<T> Salvage<T> {
    /// Whether something of the table is lost.
    pub fn lost_something(&self) -> bool {
        (self.snapshot.found && !self.snapshot.whole)
            || self.journal.lost_entries > 0
            || self.journal.lost_tail
            || (self.journal.found && self.journal.why.is_some())
    }

    /// Where the loss begins in time, when it can be told: `Some(0)` for a snapshot lost whole or a journal's loss
    /// (unknown when), the last row's start for a snapshot cut.
    pub fn lost_from_ms(&self) -> Option<u64> {
        if self.snapshot.found && !self.snapshot.whole {
            return self.snapshot.lost_from_ms;
        }
        (self.journal.lost_entries > 0 || self.journal.lost_tail || self.journal.why.is_some()).then_some(0)
    }

    /// The loss in words, for a report and the log.
    pub fn why(&self) -> Vec<String> {
        let mut w = Vec::new();
        if self.snapshot.found && !self.snapshot.whole {
            let r = self.snapshot.why.clone().unwrap_or_else(|| "it does not read to its end".into());
            w.push(format!("the snapshot: {r}; {} rows read from it", self.snapshot.rows));
        }
        if self.journal.lost_entries > 0 {
            w.push(format!("the journal: {} changes did not read", self.journal.lost_entries));
        }
        if self.journal.lost_tail {
            w.push("the journal: the records after a damaged one can't be reached".into());
        }
        if let Some(r) = &self.journal.why {
            w.push(format!("the journal: {r}"));
        }
        w
    }
}

/// The plaintext of a sealed file as far as it opens, and why it stopped if it did not read to its end. `None`: no
/// such file.
fn read_prefix(ctx: &Ctx, file: &Path) -> Result<Option<(Vec<u8>, Option<String>)>> {
    let label = ctx.label(file);
    let f = match std::fs::File::open(file) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let len = f.metadata()?.len();
    let mut r = match ctx.vault.open_reader(&label, std::io::BufReader::new(f), len) {
        Ok(r) => r,
        Err(e) => return Ok(Some((Vec::new(), Some(Error::from(e).to_string())))),
    };
    let mut out = Vec::new();
    let mut buf = vec![0u8; vault::stream::CHUNK];
    loop {
        match r.read(&mut buf) {
            Ok(0) => return Ok(Some((out, None))),
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) => {
                let why = match vault::unwrap_io(e) {
                    vault::Error::Damaged => "a chunk does not open (changed, or the file is cut short)".to_string(),
                    other => other.to_string(),
                };
                return Ok(Some((out, Some(why))));
            }
        }
    }
}

/// The whole rows at the start of a snapshot's JSON (`{"rows":[...]}`), however it ends; whether it ended as it
/// should.
fn parse_rows<T: Item>(json: &[u8]) -> (Vec<T>, bool) {
    let Some(mut pos) = json.windows(8).position(|w| w == b"\"rows\":[").map(|p| p + 8) else {
        return (Vec::new(), false);
    };
    let mut rows = Vec::new();
    loop {
        while json.get(pos).is_some_and(|b| b.is_ascii_whitespace() || *b == b',') {
            pos += 1;
        }
        match json.get(pos) {
            None => return (rows, false),
            Some(b']') => {
                let rest = &json[pos + 1..];
                return (rows, rest.iter().all(|b| b.is_ascii_whitespace() || *b == b'}'));
            }
            Some(_) => {}
        }
        let mut it = serde_json::Deserializer::from_slice(&json[pos..]).into_iter::<T>();
        match it.next() {
            Some(Ok(r)) => {
                pos += it.byte_offset();
                rows.push(r);
            }
            _ => return (rows, false),
        }
    }
}

/// Reads a table's files as far as they read.
pub(crate) fn salvage_files<T: Item>(ctx: &Ctx, files: &Files) -> Result<Salvage<T>> {
    let snap_label = ctx.label(&files.snap);
    let mut snapshot = SnapshotPart::default();
    let mut table = TimeTable::<T>::empty();
    let mut generation = None;
    if let Some((bytes, stopped)) = read_prefix(ctx, &files.snap)? {
        snapshot.found = true;
        let mut why = stopped;
        let mut rows = Vec::new();
        let mut whole = false;
        match check_header(&bytes, Kind::Snapshot, &snap_label) {
            Err(e) => why = why.or_else(|| Some(e.to_string())),
            Ok(body) => match parse_gen_level(body, &snap_label) {
                Err(e) => why = why.or_else(|| Some(e.to_string())),
                Ok((g, _)) => {
                    generation = Some(g);
                    let (json, unpacked) = codec::decompress_prefix(&body[9..]);
                    let (r, closed) = parse_rows::<T>(&json);
                    rows = r;
                    whole = why.is_none() && unpacked && closed;
                    if !whole && why.is_none() {
                        why = Some("its rows do not read to their end".into());
                    }
                }
            },
        }
        snapshot.rows = rows.len();
        snapshot.whole = whole;
        if !whole {
            snapshot.lost_from_ms = Some(rows.last().map_or(0, |r| r.start_ms()));
            snapshot.why = why;
        }
        table.apply(Batch { del: Vec::new(), put: rows });
    }
    let mut journal = JournalPart::default();
    if let Some(j) = Journal::salvage(ctx, &files.log)? {
        journal.found = true;
        journal.lost_entries = j.lost_entries;
        journal.lost_tail = j.tail_lost;
        journal.why = j.unreadable.clone();
        let stale = matches!((j.generation, generation), (Some(a), Some(b)) if a < b);
        if stale {
            // folded into the snapshot already, a crash came before the journal was removed
            journal.stale = true;
        } else {
            if let (Some(a), Some(b)) = (j.generation, generation)
                && a > b
            {
                journal.why.get_or_insert_with(|| {
                    format!("it follows snapshot {a} but the snapshot read is {b}: changes folded in since are missing")
                });
            }
            for e in &j.entries {
                match TimeTable::<T>::decode_op(e) {
                    Ok(op) => {
                        table.apply(op);
                        journal.applied += 1;
                    }
                    Err(_) => journal.lost_entries += 1,
                }
            }
        }
    }
    Ok(Salvage { rows: table.all().cloned().collect(), snapshot, journal })
}

impl Db {
    /// Reads a car's table as far as its files read (see the module docs), under the car's lock. Changes nothing.
    pub fn salvage_table<T: Item>(&self, car: CarId) -> Result<Salvage<T>> {
        self.with_car(car, |db, _, dir| salvage_files::<T>(&db.inner.ctx, &Files::named(dir, T::TABLE.name())))
    }

    /// Moves a table's files out of the way into `cars/<id>/aside/<time>/` (with a note of why), so the table starts
    /// empty and the files stay, never deleted. What the files held is read first, with [`Db::salvage_table`]. Returns
    /// the folder (its path under the data folder), `None` when there were no files. A table put aside is no longer
    /// listed in [`Db::damaged`].
    pub fn set_aside_table(&self, car: CarId, table: Table, why: &str) -> Result<Option<String>> {
        let moved = self.with_car(car, |db, data, dir| {
            let files = Files::named(dir, table.name());
            let present: Vec<&Path> =
                [&files.snap, &files.log].into_iter().map(|p| p.as_path()).filter(|p| p.exists()).collect();
            if present.is_empty() {
                return Ok(None);
            }
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |t| u64::try_from(t.as_millis()).unwrap_or(0));
            let base = dir.join("aside");
            let mut folder = base.join(stamp.to_string());
            let mut n = 1;
            while folder.exists() {
                folder = base.join(format!("{stamp}-{n}"));
                n += 1;
            }
            std::fs::create_dir_all(&folder)?;
            for p in present {
                let name = p.file_name().unwrap_or_default();
                std::fs::rename(p, folder.join(name))?;
            }
            let note = format!(
                "These files of the {} table were set aside because they were damaged: {why}.\n\
                 Nothing in them was deleted. They open only under their own names: to look at them, stop the \
                 server and put them back in the folder above, as {0}.snap and {0}.log.\n",
                table.name()
            );
            std::fs::write(folder.join("NOTE.txt"), note)?;
            sync_dir(&folder);
            sync_dir(dir);
            data.unload();
            let rel = db.inner.ctx.label(&folder);
            db.forget_damage(car, &[files.snap.as_path(), files.log.as_path()]);
            Ok(Some(rel))
        })?;
        self.inner.versions.bump(car, true, true);
        Ok(moved)
    }

    /// Writes the table of a car whose table was put aside ([`Db::set_aside_table`]) with `rows`, as its snapshot, and
    /// takes the car's summary to count them. The table must have no files.
    pub fn restore_table<T: Item>(&self, car: CarId, rows: &[T]) -> Result<()> {
        crate::check_rows(car, rows)?;
        let rows = crate::dedup(rows);
        let imported = rows.iter().any(|r| r.source().is_imported());
        let live = rows.iter().any(|r| !r.source().is_imported());
        let r = self.with_car(car, |db, data, dir| {
            let files = Files::named(dir, T::TABLE.name());
            if files.snap.exists() || files.log.exists() {
                return Err(Error::invalid("a table is written this way only where none is kept"));
            }
            data.unload();
            let (t, _) = data.table::<T>(&db.inner.ctx, dir)?;
            t.state.apply(Batch { del: Vec::new(), put: rows });
            t.fold(&db.inner.ctx, crate::file::Which::Warm)?;
            let w = t.weight();
            let (mut part, mut last) = (crate::Tally::default(), None);
            for r in t.state.all() {
                r.tally(&mut part, 1);
                last = last.max(Some(r.end_ms()));
            }
            data.set_weight(T::TABLE, w);
            db.mend_summary(car, T::TABLE, &part, last)
        });
        self.inner.versions.bump(car, live, imported);
        r
    }
}
