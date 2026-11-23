//! Reading a data folder another process owns (the running server, while a command runs with `docker exec`):
//! nothing is opened for writing, and nothing is created, cleaned up or removed. Journals are read read-only, as
//! the server's own readers read them.
//!
//! Only the history in use is read (`<data>/db`): a full rebuild builds a car's new history in a shadow folder of its
//! own (`<data>/.<name>/db`, [`crate::Db::shadow`]), which is never looked at here, and switches to it with two
//! renames ([`crate::Db::adopt_car`]).
//!
//! The owner keeps writing while this reads: a day's journal grows, a fold replaces the day's snapshot and journal,
//! a compaction moves a finished month into its pack, a switch puts a new car folder in place. Two ways to read
//! through that:
//!
//! - [`Reader::day_points`] and [`Reader::car_settled`] need no write gate: they look at the files they read before
//!   and after (inode, size, modification time) and read again when one changed or the read failed half-way (a
//!   journal entry being appended, a snapshot and a journal of two generations), a few times with growing pauses.
//!   A day that keeps changing is answered [`DayRead::Changing`]: the caller skips it (and says so).
//! - [`Reader::points`] and [`Reader::car`] read once: a caller that needs one moment holds a pause of the folder's
//!   write gate around each read (`gate::Gate::pause`), which a busy server may not give for a long while.

use crate::error::Result;
use crate::file::Ctx;
use crate::fleet::Fleet;
use crate::journal::Files;
use crate::logged::LoggedFile;
use crate::segment::{self, Point};
use model::{Car, CarId, Stream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use vault::Vault;

/// A read-only view of a data folder's db.
pub struct Reader {
    ctx: Ctx,
    dir: PathBuf,
}

/// One day read while its owner may be writing it.
#[derive(Debug, Clone, PartialEq)]
pub enum DayRead<P> {
    /// Its points in the window, in time order, from files that stayed the same while they were read.
    Read(Vec<P>),
    /// It changed (or would not read) every time it was tried: why, the last time.
    Changing(String),
}

/// How many times a day or the fleet is read before it counts as changing, and the pause before the first retry
/// (doubled each time: 20 ms, 40 ms, ... 0.6 s; about 1.3 s in all).
pub const TRIES: u32 = 6;
const FIRST_PAUSE: Duration = Duration::from_millis(20);

/// What a file is now (`None`: not there): its inode, size and modification time.
fn stamp(file: &Path) -> Option<(u64, u64, i128)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(file).ok()?;
    Some((m.ino(), m.len(), i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec())))
}

fn stamps(files: &[PathBuf]) -> Vec<Option<(u64, u64, i128)>> {
    files.iter().map(|f| stamp(f)).collect()
}

/// Reads with `read` until the files `files` are the same before and after a read that worked, at most `tries`
/// times. `Err(why)`: they never were.
fn settled<T>(files: &[PathBuf], tries: u32, read: &mut dyn FnMut() -> Result<T>) -> std::result::Result<T, String> {
    let mut pause = FIRST_PAUSE;
    let mut why = String::from("its files changed while they were read");
    for i in 0..tries.max(1) {
        if i > 0 {
            std::thread::sleep(pause);
            pause = pause.saturating_mul(2);
        }
        let before = stamps(files);
        match read() {
            Ok(t) if stamps(files) == before => return Ok(t),
            Ok(_) => why = "its files changed while they were read".into(),
            Err(e) => why = e.to_string(),
        }
    }
    Err(why)
}

impl Reader {
    /// The db under `data` (`data/db`), sealed with `vault`. Reads nothing yet.
    pub fn new(data: &Path, vault: Arc<Vault>) -> Reader {
        let opts = crate::Options { max_cached_bytes: 0, ..crate::Options::default() };
        Reader { ctx: Ctx::new(vault, data.to_path_buf(), opts), dir: data.join("db") }
    }

    fn car_dir(&self, car: CarId) -> PathBuf {
        self.dir.join("cars").join(car.to_string())
    }

    /// The car's record as the fleet table has it now.
    pub fn car(&self, id: CarId) -> Result<Option<Car>> {
        let fleet = LoggedFile::<Fleet>::inspect_state_or(&self.ctx, Files::named(&self.dir, "fleet"), &|| Ok(None))?;
        Ok(fleet.get(id).map(|e| e.car.clone()))
    }

    /// [`Reader::car`] without the write gate: read again while the fleet's files change under it (up to `tries`
    /// times). `Err`: the last error, when every read failed or saw the files change.
    pub fn car_settled(&self, id: CarId, tries: u32) -> std::result::Result<Option<Car>, String> {
        let f = Files::named(&self.dir, "fleet");
        settled(&[f.snap, f.log], tries, &mut || self.car(id))
    }

    /// Whether the car's folder is there now (a full rebuild's switch takes it away for a moment, between its two
    /// renames).
    pub fn car_folder_exists(&self, car: CarId) -> bool {
        self.car_dir(car).is_dir()
    }

    /// The days a car's series has in `[from_ms, to_ms)` (each day's UTC start).
    pub fn days<P: Point>(&self, car: CarId, stream: Stream, from_ms: u64, to_ms: u64) -> Result<Vec<u64>> {
        let days = segment::days_in::<P>(&self.ctx, &self.car_dir(car), stream, from_ms, to_ms)?;
        Ok(days.into_iter().map(|d| d * segment::DAY_MS).collect())
    }

    /// A car's points of one series in `[from_ms, to_ms)`, in time order.
    pub fn points<P: Point>(&self, car: CarId, stream: Stream, from_ms: u64, to_ms: u64) -> Result<Vec<P>> {
        let car_dir = self.car_dir(car);
        let ctx = &self.ctx;
        let days = segment::days_in::<P>(ctx, &car_dir, stream, from_ms, to_ms)?;
        let days = segment::plains::<P>(ctx, &car_dir, stream, &days, &mut |day| {
            segment::inspected_plain::<P>(ctx, &car_dir, stream, day)
        })?;
        segment::points_of(days, from_ms, to_ms)
    }

    /// The points of `[from_ms, to_ms)` of one day of a car's series (`day_ms`: the day's UTC start), read without
    /// the folder's write gate: the day's snapshot, journal and month's pack, and the car's folder, are looked at
    /// before and after; when one changed meanwhile or the read failed, it is read again (up to `tries` times, with
    /// growing pauses), then [`DayRead::Changing`].
    pub fn day_points<P: Point>(
        &self,
        car: CarId,
        stream: Stream,
        day_ms: u64,
        from_ms: u64,
        to_ms: u64,
        tries: u32,
    ) -> DayRead<P> {
        let car_dir = self.car_dir(car);
        let day = day_ms / segment::DAY_MS;
        let [snap, log, pack] = segment::day_paths::<P>(&car_dir, stream, day);
        let files = [car_dir.clone(), snap, log, pack];
        let (from, to) = (from_ms.max(day_ms), to_ms.min(day_ms + segment::DAY_MS));
        let ctx = &self.ctx;
        let r = settled(&files, tries, &mut || {
            let days = segment::plains::<P>(ctx, &car_dir, stream, &[day], &mut |d| {
                segment::inspected_plain::<P>(ctx, &car_dir, stream, d)
            })?;
            segment::points_of(days, from, to)
        });
        match r {
            Ok(points) => DayRead::Read(points),
            Err(why) => DayRead::Changing(why),
        }
    }
}
