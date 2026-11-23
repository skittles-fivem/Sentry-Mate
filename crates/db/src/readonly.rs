//! Reading a data folder another process owns (the running server, while a command runs with `docker exec`):
//! nothing is opened for writing, and nothing is created, cleaned up or removed. Journals are read read-only, as
//! the server's own readers read them. A caller that needs one moment (no fold of a day between reading its
//! snapshot and its journal) holds a pause of the folder's write gate around each read (`gate::Gate::pause`).

use crate::error::Result;
use crate::file::Ctx;
use crate::fleet::Fleet;
use crate::journal::Files;
use crate::logged::LoggedFile;
use crate::segment::{self, Point};
use model::{Car, CarId, Stream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use vault::Vault;

/// A read-only view of a data folder's db.
pub struct Reader {
    ctx: Ctx,
    dir: PathBuf,
}

impl Reader {
    /// The db under `data` (`data/db`), sealed with `vault`. Reads nothing yet.
    pub fn new(data: &Path, vault: Arc<Vault>) -> Reader {
        let opts = crate::Options { max_cached_bytes: 0, ..crate::Options::default() };
        Reader { ctx: Ctx::new(vault, data.to_path_buf(), opts), dir: data.join("db") }
    }

    /// The car's record as the fleet table has it now.
    pub fn car(&self, id: CarId) -> Result<Option<Car>> {
        let fleet = LoggedFile::<Fleet>::inspect_state(&self.ctx, Files::named(&self.dir, "fleet"))?;
        Ok(fleet.get(id).map(|e| e.car.clone()))
    }

    /// The days a car's series has in `[from_ms, to_ms)` (each day's UTC start).
    pub fn days<P: Point>(&self, car: CarId, stream: Stream, from_ms: u64, to_ms: u64) -> Result<Vec<u64>> {
        let dir = segment::series_dir::<P>(&self.dir.join("cars").join(car.to_string()), stream);
        Ok(segment::days_in(&dir, from_ms, to_ms)?.into_iter().map(|d| d * segment::DAY_MS).collect())
    }

    /// A car's points of one series in `[from_ms, to_ms)`, in time order.
    pub fn points<P: Point>(&self, car: CarId, stream: Stream, from_ms: u64, to_ms: u64) -> Result<Vec<P>> {
        let car_dir = self.dir.join("cars").join(car.to_string());
        let ctx = &self.ctx;
        let days = segment::plains::<P>(ctx, &car_dir, stream, from_ms, to_ms, &mut |files| {
            segment::inspected_plain::<P>(ctx, files)
        })?;
        segment::points_of(days, from_ms, to_ms)
    }
}
