//! Bringing old TeslaMate history into Sentry Mate, from TeslaMate's own backup.
//!
//! The backup is the file TeslaMate's documentation has owners make
//! (`docker compose exec -T database pg_dump -U teslamate teslamate > teslamate.bck`): plain SQL, or the same file
//! gzipped or xz'd. No SQL in it is ever run and no Postgres is needed: only the blocks that hold the tables' rows
//! (`COPY ... FROM stdin;` up to `\.`) are read, as data, matched by column name so any TeslaMate version reads.
//! Tesla's tokens (`private.*`, `tokens`) are passed over without being read.
//!
//! **Any number of cars, any size.** The file is read as a stream, twice (see [`import`]); nothing holds a whole
//! table or every position. Rows are handed to the caller's [`Sink`] per car, in batches, as they are read, and the
//! rows waiting at once are capped ([`Options::max_buffered_rows`]) however many cars are mixed in the backup. A car
//! of the backup costs a fixed 2 KB or so while the import runs (where its rows go and their counts), and its report
//! and state are handed over at the end, one car at a time. What is kept to link the tables (address names, which car
//! a charge is, the positions at drive ends) has ceilings too, past which a link is left out and counted, never
//! guessed.
//!
//! **Re-runnable.** Each car has an [`ImportState`] (a few numbers per table) the caller keeps and gives back: a newer
//! backup then adds only what is new, and rows that were still open (a drive under way) are taken once they ended.
//!
//! Every row comes as Sentry Mate's own type ([`model`]) tagged [`model::Source::Teslamate`], with TeslaMate's id
//! beside it; cars come as [`TmCar`] (the VIN's prefix, never its serial) for the caller to match to its own. The
//! [`Report`] says what was read, taken and left out (with why) per table, and each car's [`CarReport`] the same for
//! that car.

mod copy;
mod emit;
mod index;
mod input;
mod links;
mod map;
pub mod report;
pub mod rows;
mod schema;
pub mod state;
pub mod teslafi;
mod value;
mod vin;
pub mod zip;

pub use input::{Compression, Encoding};
pub use map::CLOSED_AT_LAST;
pub use report::{CarReport, Links, Report, Skip, TableReport};
pub use rows::{
    AddressRow, Batch, ChargePointRow, ChargeRefs, ChargeRow, DriveRefs, DriveRow, MAPPING_VERSION, PlaceRow,
    PositionRow, Sink, SinkError, StateRow, Target, TmCar, UpdateRow, position_of,
};
pub use schema::Table;
pub use state::{ImportState, Mark, STATE_VERSION};
pub use vin::vin_info;

use std::io::{self, Read};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("reading the backup: {0}")]
    Io(#[from] io::Error),
    #[error("the backup doesn't unpack: {0}")]
    Unpack(String),
    #[error(
        "this is a custom-format pg_dump backup (-Fc): make the backup as TeslaMate's documentation says (plain SQL; \
         gzip or xz it if you like)"
    )]
    CustomFormat,
    #[error("{0} files aren't read: give the plain backup, or the backup gzipped or xz'd")]
    Unsupported(&'static str),
    #[error("the backup is UTF-16 big-endian text: save it as UTF-8")]
    Utf16Be,
    #[error("no TeslaMate cars table in this file: is it TeslaMate's backup?")]
    NotTeslamate,
    #[error(
        "this is not TeslaFi's data: in TeslaFi use Settings > Account > Advanced > Download TeslaFi Data and upload \
         the CSV files it gives (one, several in a .zip, or gzipped)"
    )]
    NotTeslafi,
    #[error(
        "this is TeslaFi's list of drives or charges, which has no positions: upload the monthly files of Settings > \
         Account > Advanced > Download TeslaFi Data instead"
    )]
    TeslafiSummary,
    #[error("the backup is incomplete ({0}): it may have been cut short; make it again")]
    Incomplete(&'static str),
    #[error("the backup changed between its two reads")]
    Changed,
    #[error("an import state of version {0} can't be used by this version")]
    StateVersion(u32),
    #[error("storing the imported rows: {0}")]
    Sink(SinkError),
}

/// Limits and sizes. Besides about 2 KB per car of the backup, the defaults cap what an import holds at about 120 MB
/// however big the backup (the ceilings only fill with thousands of cars' history), plus xz's window when the backup
/// is xz'd (8 MB at xz's default level, 64 MB at -9). A backup of a few cars takes a few MB.
///
/// With many cars mixed in the backup, batches get smaller rather than memory larger: [`Options::max_buffered_rows`]
/// shared by all the cars whose rows are waiting.
#[derive(Debug, Clone)]
pub struct Options {
    /// Rows per batch handed to the sink (per car and table).
    pub batch_rows: usize,
    /// Rows waiting across all cars before every car's batch is handed over.
    pub max_buffered_rows: usize,
    /// A row (one line of the backup) longer than this is left out and counted. TeslaMate's longest are addresses.
    pub max_line: usize,
    /// Bytes of address names kept to name drive and charge ends.
    pub max_address_bytes: usize,
    /// Charges whose car, AC/DC and top power are kept (charge points of others are left out and counted).
    pub max_charges: usize,
    /// Positions kept to be drive ends and charge places.
    pub max_endpoints: usize,
    /// Rows still open (no end yet) remembered per car and table (TeslaMate has one or two: the drive, charge and
    /// state under way when the backup was made); past it the oldest are no longer waited for, with a warning.
    pub max_open: usize,
    /// The same, for all cars of one import together.
    pub max_open_total: usize,
    /// Refuse a backup without pg_dump's closing line (one cut short).
    pub require_complete: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            batch_rows: 2048,
            max_buffered_rows: 16_384,
            max_line: 1 << 20,
            max_address_bytes: 32 << 20,
            max_charges: 1_000_000,
            max_endpoints: 300_000,
            max_open: 1000,
            max_open_total: 100_000,
            require_complete: true,
        }
    }
}

/// Imports a backup. `open` gives the backup from its start; it is called twice and must give the same bytes both
/// times (a file, opened again): the first read finds the cars and what links the tables (which car a charge point
/// belongs to is only in a table pg_dump writes after the points), the second hands the rows over. Nothing is handed
/// over before the first read has checked the whole file, so a broken backup imports nothing.
///
/// Each car's report and new state reach the sink at the end ([`Sink::car_done`]). If the import stops part-way (an
/// error from the sink, the disk), rows already handed over stay with the caller and no state is given: running it
/// again hands those rows over again, so store rows keyed by car, table and `tm_id`, replacing.
pub fn import<R: Read>(
    mut open: impl FnMut() -> io::Result<R>,
    sink: &mut dyn Sink,
    options: &Options,
) -> Result<Report, Error> {
    let mut idx = index::Index::new(sink, options);
    let mut reader = copy::CopyReader::new(options.max_line);
    let first = input::read(open()?, &mut reader, &mut idx)?;
    if reader.inside_block() {
        return Err(Error::Incomplete("it ends inside a table"));
    }
    if options.require_complete && !reader.saw_end() {
        return Err(Error::Incomplete("pg_dump's closing line is missing"));
    }
    if !idx.routes.known {
        return Err(Error::NotTeslamate);
    }
    let (sink, routes, labels, procs, ends) = idx.into_parts();
    let mut emit = emit::Emit::new(sink, options, routes, labels, procs, ends);
    let mut reader = copy::CopyReader::new(options.max_line);
    let second = input::read(open()?, &mut reader, &mut emit)?;
    if (second.unpacked_bytes, second.crc32) != (first.unpacked_bytes, first.crc32) {
        return Err(Error::Changed);
    }
    let mut report = emit.finish()?;
    report.compression = first.compression;
    report.encoding = first.encoding;
    report.packed_bytes = first.packed_bytes;
    report.unpacked_bytes = first.unpacked_bytes;
    Ok(report)
}

/// [`import`] from a file.
pub fn import_file(path: &Path, sink: &mut dyn Sink, options: &Options) -> Result<Report, Error> {
    import(|| std::fs::File::open(path), sink, options)
}
