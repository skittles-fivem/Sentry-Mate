//! TeslaFi's own export brought in: the raw data TeslaFi lets an owner download (Settings > Account > Advanced >
//! "Download TeslaFi Data"), one CSV per car and calendar month, each row a poll of the car. Any number of those
//! files come at once: one CSV, several in a `.zip`, or one gzipped or xz'd. Each is recognised by its header (TeslaFi
//! writes no column list: the columns read are named in [`poll::Col`]), never by its name, and read as a stream, a
//! line at a time: a few years of history (hundreds of MB) never sits in memory.
//!
//! TeslaFi keeps polls, not drives or charges, so those are worked out here: a drive from the gear (`shift_state`),
//! a charge from `charging_state`, the car's state (online, asleep, offline) from `state`, software updates from
//! changes of `car_version` ([`seg`]). Polls become positions and charge curve points. Everything reaches the same
//! [`Sink`] a TeslaMate import fills, in Sentry Mate's types and units: miles and Fahrenheit are converted by what the
//! file shows (`odometerF` beside `odometer`, `inside_tempF` beside `inside_temp`, the `gui_*_units` columns) or by
//! what the owner says ([`Options`]). TeslaFi writes local wall time without a zone: [`Options::time_zone`], else the
//! car's own time zone ([`Sink::time_zone`]), else [`Options::default_time_zone`] turns it into UTC, the repeated
//! hour of autumn read in the order of the rows.
//!
//! **Re-runnable.** Each car's [`State`] (kept inside its [`ImportState`]) lists the stretches of time already
//! imported, with what was still under way at their edges. A row inside a stretch is passed over, so importing the
//! same files again adds nothing, and a month imported part way (downloaded before it ended) adds only the rest. A
//! drive or charge over the end of a month is joined with its other half when both months are in, whichever came
//! first: the joined row replaces the halves (the later half is removed, [`Batch::Removed`]). Rows carry ids made from
//! their start, so a run cut short and started again replaces what it stored, never doubles it.

mod csv;
pub mod feed;
mod poll;
mod seg;

pub use poll::CarState;
pub use seg::{Agg, Kind, Snap};

use crate::report::CarReport;
use crate::rows::{Batch, ChargePointRow, ChargeRow, DriveRow, PositionRow, Sink, StateRow, TmCar, UpdateRow};
use crate::schema::Table;
use crate::state::ImportState;
use crate::zip::{ZipError, ZipStream, is_zip};
use crate::{Error, input::XZ_MAX_WINDOW_KIB, report::TableReport, vin_info};
use csv::{Fields, Line, Lines};
use model::{CarId, ChargeKind, ChargePoint, Update};
use poll::{Col, Header, HeaderKind, Row, Utc};
use seg::{Out, Run};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{self, BufRead, BufReader, Read};

/// The kind of import, as `/imports` names it.
pub const KIND: &str = "teslafi";
/// The layout of [`State`]; another one is refused rather than misread.
pub const STATE_VERSION: u32 = 1;
const BUF: usize = 256 * 1024;

/// What the base columns' distances (odometer, ranges, speed) are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Distance {
    Mi,
    Km,
}

/// What the base columns' temperatures are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Temperature {
    C,
    F,
}

impl Distance {
    pub fn from_word(w: &str) -> Option<Distance> {
        match w.trim().to_ascii_lowercase().as_str() {
            "mi" | "miles" | "mile" => Some(Distance::Mi),
            "km" | "kilometers" | "kilometres" => Some(Distance::Km),
            _ => None,
        }
    }
}

impl Temperature {
    pub fn from_word(w: &str) -> Option<Temperature> {
        match w.trim().to_ascii_lowercase().as_str() {
            "c" | "celsius" => Some(Temperature::C),
            "f" | "fahrenheit" => Some(Temperature::F),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// The base columns' distance unit; `None`: worked out from each file (miles, as Tesla gives them, when it can't
    /// tell).
    pub distance: Option<Distance>,
    /// The base columns' temperature unit; `None`: worked out from each file (°C when it can't tell).
    pub temperature: Option<Temperature>,
    /// The time zone TeslaFi's times are in (IANA, e.g. `America/Chicago`); `None`: the car's.
    pub time_zone: Option<String>,
    /// When neither the owner nor the car says.
    pub default_time_zone: String,
    /// Rows per batch handed to the sink.
    pub batch_rows: usize,
    /// Rows waiting across all cars before every car's batches are handed over.
    pub max_buffered_rows: usize,
    /// A line longer than this is passed over and counted.
    pub max_line: usize,
    /// Cars taken in one import (TeslaFi has one per file; more than this are passed over and counted).
    pub max_cars: usize,
    /// Stretches of imported time remembered per car (a month a file: a hundred years of months fit).
    pub max_pieces: usize,
    /// Rows of a file looked at first to tell its units.
    pub lookahead_rows: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            distance: None,
            temperature: None,
            time_zone: None,
            default_time_zone: "UTC".into(),
            batch_rows: 2048,
            max_buffered_rows: 16_384,
            max_line: 1 << 20,
            max_cars: 64,
            max_pieces: 1200,
            lookahead_rows: 500,
        }
    }
}

/// Where a segment runs over a piece's edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum End {
    /// Under way at the piece's first poll.
    Head,
    /// Under way at its last.
    Tail,
    /// Both: the whole piece.
    Through,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub at: End,
    pub seg: Agg,
}

/// A stretch of a car's time imported from one file: every poll in `[first_ms, last_ms]` is in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Piece {
    pub first_ms: u64,
    pub last_ms: u64,
    pub rows: u64,
    /// The software version at its start and end, with the time of the poll that showed it.
    #[serde(default)]
    pub first_version: Option<(u64, String)>,
    #[serde(default)]
    pub last_version: Option<(u64, String)>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// The time zone and units its rows were read in (`None`: not kept, taken as any later import's). Read again in
    /// others, its rows are replaced ([`Batch::Cleared`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance: Option<Distance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<Temperature>,
}

impl Piece {
    /// Whether its rows were read otherwise than in `tz` and `units`.
    fn read_otherwise(&self, tz: chrono_tz::Tz, units: &Units) -> bool {
        self.zone.as_deref().is_some_and(|z| z != tz.name())
            || self.distance.is_some_and(|d| d != units.distance)
            || self.temperature.is_some_and(|t| t != units.temperature)
    }
}

/// What earlier TeslaFi imports took from a car.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    /// In time order.
    pub pieces: Vec<Piece>,
    /// The ids of the updates found between pieces (recomputed as pieces come in).
    #[serde(default)]
    pub updates: Vec<i64>,
}

impl Default for State {
    fn default() -> State {
        State { version: STATE_VERSION, pieces: Vec::new(), updates: Vec::new() }
    }
}

/// The units a file was read in, and how they were chosen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Units {
    pub distance: Distance,
    /// `setting`, `detected` (from the file) or `default`.
    pub distance_from: String,
    pub temperature: Temperature,
    pub temperature_from: String,
}

/// What an import did. It holds no VINs, positions or values from the rows: counts, names and column names.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Report {
    /// `plain`, `gzip`, `xz` or `zip`.
    pub compression: String,
    pub packed_bytes: u64,
    pub unpacked_bytes: u64,
    /// TeslaFi files read.
    pub files: u64,
    /// Files passed over, with why (at most 100 listed).
    pub files_skipped: BTreeMap<String, String>,
    pub files_skipped_count: u64,
    pub rows_read: u64,
    pub rows_used: u64,
    /// Rows passed over, by why: `already_imported`, `duplicate`, `out_of_order`, `bad_time`, `dst_gap`,
    /// `car_left_out`, `too_many_cars`, `wrong_field_count`, `too_long`.
    pub rows_skipped: BTreeMap<String, u64>,
    /// Values there but unreadable, by column (read as empty).
    pub bad_values: BTreeMap<String, u64>,
    /// Lines read as Windows-1252 (not UTF-8).
    pub lines_windows_1252: u64,
    pub positions: u64,
    pub charge_points: u64,
    pub drives: u64,
    pub charges: u64,
    pub states: u64,
    pub updates: u64,
    /// Rows of earlier imports taken away because they were joined into another (a drive over a month's end).
    pub removed: u64,
    /// Stretches imported before in another time zone or other units and read again here: what they stored was
    /// replaced.
    #[serde(default)]
    pub read_again: u64,
    pub cars: u64,
    pub cars_left_out: u64,
    /// The time span of the rows used.
    pub first_ms: Option<u64>,
    pub last_ms: Option<u64>,
    /// The units, when every file was read in the same ones (else the first, with a warning).
    pub units: Option<Units>,
    /// The time zone the rows were read in (the first car's, when cars differ), and where it came from: `setting`,
    /// `car` or `default`.
    pub time_zone: Option<String>,
    pub time_zone_from: Option<String>,
    /// Rows in the hour the clocks go back (read in row order) and wall times the clocks skip (passed over).
    pub dst_repeated: u64,
    /// Columns read here that the files lack (read as empty).
    pub missing_columns: Vec<String>,
    /// Columns of the files not read here, how many.
    pub unknown_columns: u64,
    pub peak_buffered_rows: usize,
    pub warnings: Vec<String>,
    /// Each uploaded file's share, when several were imported as one ([`import_files`]); empty for one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uploads: Vec<Upload>,
}

impl Report {
    fn skip(&mut self, why: &str) {
        *self.rows_skipped.entry(why.to_string()).or_default() += 1;
    }

    fn warn(&mut self, w: String) {
        if !self.warnings.contains(&w) && self.warnings.len() < 100 {
            self.warnings.push(w);
        }
    }

    fn widen(&mut self, t: u64) {
        crate::report::widen(&mut self.first_ms, &mut self.last_ms, t);
    }
}

/// What a file's first line says it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sniff {
    /// TeslaFi's raw data (alone, packed, or a CSV among a zip's first files).
    Teslafi,
    /// TeslaFi's drives or charges list.
    TeslafiSummary,
    /// Something else.
    Other,
}

/// Entries of a zip looked at for TeslaFi's months.
const SNIFF_ENTRIES: usize = 16;

/// Looks at the start of a file to tell whether it is TeslaFi's export (reads only its first line; of a zip, the
/// first line of each of its first CSVs, other files passed over).
pub fn sniff<R: Read>(r: R) -> io::Result<Sniff> {
    let mut r = BufReader::with_capacity(64 << 10, r);
    let head = r.fill_buf()?.to_vec();
    let kind_of = |rd: &mut dyn BufRead| -> io::Result<Sniff> {
        let mut lines = Lines::new(rd, 1 << 20);
        let mut buf = Vec::new();
        loop {
            match lines.next(&mut buf)? {
                None | Some(Line::TooLong) => return Ok(Sniff::Other),
                Some(Line::Text) => {
                    let (t, _) = csv::text(&buf);
                    if t.trim().is_empty() {
                        continue;
                    }
                    return Ok(match header_of(&t).1 {
                        HeaderKind::Raw => Sniff::Teslafi,
                        HeaderKind::Summary => Sniff::TeslafiSummary,
                        HeaderKind::Other => Sniff::Other,
                    });
                }
            }
        }
    };
    if head.starts_with(&[0x1f, 0x8b]) {
        kind_of(&mut BufReader::new(flate2::read::MultiGzDecoder::new(r)))
    } else if head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        kind_of(&mut BufReader::new(lzma_rust2::XzReader::new_mem_limit(r, true, XZ_MAX_WINDOW_KIB)))
    } else if is_zip(&head) {
        // TeslaFi's months may come after other files (a readme, another CSV): those are passed over, as the import
        // passes them over, for the first entries; a CAN export's logs say at once it is not TeslaFi's
        let mut z = ZipStream::new(r);
        let mut summary = false;
        for _ in 0..SNIFF_ENTRIES {
            let Some(e) = z.next_entry().map_err(io::Error::from)? else { break };
            if skip_entry(&e.name) {
                continue;
            }
            let name = e.name.to_ascii_lowercase();
            if name.ends_with(".log.xz") || name.ends_with(".log") || name.ends_with("manifest.json") {
                break;
            }
            if !name.ends_with(".csv") {
                continue;
            }
            match kind_of(&mut BufReader::new(&mut z))? {
                Sniff::Teslafi => return Ok(Sniff::Teslafi),
                Sniff::TeslafiSummary => summary = true,
                Sniff::Other => {}
            }
        }
        Ok(if summary { Sniff::TeslafiSummary } else { Sniff::Other })
    } else {
        kind_of(&mut r)
    }
}

/// Folders and the files macOS adds to a zip.
fn skip_entry(name: &str) -> bool {
    name.ends_with('/') || name.starts_with("__MACOSX/") || name.rsplit('/').next().is_some_and(|n| n.starts_with('.'))
}

/// A row's VIN, when it is one.
fn row_vin(row: &Row<'_>) -> Option<String> {
    row.text(Col::Vin).filter(|v| vin_info(v).is_some()).map(|v| v.trim().to_ascii_uppercase())
}

fn id_key(row: &Row<'_>) -> Option<String> {
    row.text(Col::VehicleId).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).map(|v| format!("id:{v}"))
}

fn name_key(row: &Row<'_>) -> Option<String> {
    row.text(Col::DisplayName).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).map(|v| format!("name:{v}"))
}

fn header_of(line: &str) -> (Vec<String>, HeaderKind) {
    let mut f = Fields::default();
    f.split(line);
    let names: Vec<String> = (0..f.len()).map(|i| f.get(line, i).map(|s| s.into_owned()).unwrap_or_default()).collect();
    let kind = poll::header_kind(&names);
    (names, kind)
}

/// Rows of one car waiting to be handed over.
#[derive(Default)]
struct Pending {
    positions: Vec<PositionRow>,
    points: Vec<ChargePointRow>,
    drives: Vec<DriveRow>,
    charges: Vec<ChargeRow>,
    states: Vec<StateRow>,
    updates: Vec<UpdateRow>,
    removed: Vec<(Table, i64)>,
    cleared: Vec<(u64, u64)>,
}

impl Pending {
    fn len(&self) -> usize {
        self.positions.len()
            + self.points.len()
            + self.drives.len()
            + self.charges.len()
            + self.states.len()
            + self.updates.len()
            + self.removed.len()
            + self.cleared.len()
    }
}

/// A car of the files.
struct Slot {
    tm: TmCar,
    car: Option<CarId>,
    tz: chrono_tz::Tz,
    /// The state it came with (the TeslaMate part is kept as it is).
    base: ImportState,
    pieces: Vec<Piece>,
    old_updates: Vec<i64>,
    run: Option<Run>,
    /// The units of the file being read (kept with each piece).
    units: Option<(Distance, Temperature)>,
    /// The time zones of earlier pieces read in another zone (their rows are looked for in it too).
    other_zones: Vec<chrono_tz::Tz>,
    /// The last row's time in the file being read (for the repeated autumn hour).
    prev_ms: Option<u64>,
    pending: Pending,
    tally: [crate::report::Tally; 6],
    first_ms: Option<u64>,
    last_ms: Option<u64>,
    warnings: Vec<String>,
}

/// Tally positions by table.
fn ix(t: Table) -> usize {
    t.car_ix().unwrap_or(0)
}

struct Importer<'a> {
    sink: &'a mut dyn Sink,
    o: &'a Options,
    keys: HashMap<String, usize>,
    /// The VIN rows of a file's first lines show for a vehicle id or a name (`id:…`, `name:…`), for rows without one.
    hints: HashMap<String, String>,
    slots: Vec<Slot>,
    report: Report,
    bad: BTreeMap<&'static str, u64>,
    fields: Fields,
    missing: BTreeSet<&'static str>,
    units_seen: BTreeSet<(Distance, Temperature)>,
    buffered: usize,
}

fn sink_err(e: crate::rows::SinkError) -> Error {
    Error::Sink(e)
}

fn parse_tz(s: &str) -> Option<chrono_tz::Tz> {
    s.trim().parse().ok()
}

/// Whether `s` is a time zone the import can read times in (an IANA name such as `Europe/Berlin`).
pub fn valid_time_zone(s: &str) -> bool {
    parse_tz(s).is_some()
}

/// Imports TeslaFi's export from `r` (read once, as a stream). Rows go to the sink as they are worked out; each car's
/// report and new state at the end ([`Sink::car_done`]). If it stops part way rows already handed over stay and no
/// state is given: running it again replaces them.
pub fn import<R: Read>(r: R, sink: &mut dyn Sink, o: &Options) -> Result<Report, Error> {
    let mut imp = Importer::new(sink, o)?;
    let read = imp.source(r, "")?;
    if imp.report.files == 0 {
        return Err(Error::NotTeslafi);
    }
    imp.report.compression = read.compression.into();
    imp.report.unpacked_bytes = read.unpacked;
    let mut report = imp.finish()?;
    report.packed_bytes = read.packed;
    Ok(report)
}

/// What one uploaded file of [`import_files`] brought (its [`Report::uploads`] line).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Upload {
    pub name: String,
    /// `plain`, `gzip`, `xz` or `zip`.
    pub compression: String,
    pub packed_bytes: u64,
    pub unpacked_bytes: u64,
    /// TeslaFi files in it (a zip holds several) and those passed over.
    pub files: u64,
    pub files_skipped: u64,
    pub rows_read: u64,
    pub rows_used: u64,
    /// Rows passed over, whatever the reason ([`Report::rows_skipped`] has the reasons for all files together).
    pub rows_skipped: u64,
}

/// Imports several uploaded files of TeslaFi's export as one: what [`import`] does with one file (or a zip of them),
/// for files uploaded one by one (TeslaFi's months, each a CSV, gzipped or a zip). They are read in turn, as a zip's
/// files are, so a car's months join (a drive over a month's end is one, whichever file comes first) and the result
/// is the same rows as importing them one after the other. Only one file is open at a time; `files` opens each when
/// it is its turn. Each file is a TeslaFi file (or a zip with some): one that isn't is refused, named.
pub fn import_files<R: Read>(
    files: impl IntoIterator<Item = io::Result<(String, R)>>,
    sink: &mut dyn Sink,
    o: &Options,
) -> Result<Report, Error> {
    let mut imp = Importer::new(sink, o)?;
    let (mut packed, mut unpacked) = (0u64, 0u64);
    let mut kinds: BTreeSet<&'static str> = BTreeSet::new();
    let mut uploads = Vec::new();
    for f in files {
        let (name, r) = f?;
        let before = (
            imp.report.files,
            imp.report.files_skipped_count,
            imp.report.rows_read,
            imp.report.rows_used,
            imp.report.rows_skipped.values().sum::<u64>(),
        );
        let named = |e: Error| match e {
            Error::Unpack(m) => Error::Unpack(format!("{name}: {m}")),
            e => e,
        };
        let read = imp.source(r, &name).map_err(named)?;
        packed += read.packed;
        unpacked += read.unpacked;
        kinds.insert(read.compression);
        let rp = &imp.report;
        uploads.push(Upload {
            name: name.chars().take(200).collect(),
            compression: read.compression.into(),
            packed_bytes: read.packed,
            unpacked_bytes: read.unpacked,
            files: rp.files - before.0,
            files_skipped: rp.files_skipped_count - before.1,
            rows_read: rp.rows_read - before.2,
            rows_used: rp.rows_used - before.3,
            rows_skipped: rp.rows_skipped.values().sum::<u64>() - before.4,
        });
    }
    if imp.report.files == 0 {
        return Err(Error::NotTeslafi);
    }
    imp.report.compression = match kinds.len() {
        1 => kinds.first().copied().unwrap_or("plain").into(),
        _ => "several".into(),
    };
    imp.report.unpacked_bytes = unpacked;
    imp.report.uploads = uploads;
    let mut report = imp.finish()?;
    report.packed_bytes = packed;
    Ok(report)
}

/// What reading one file (an upload) saw.
struct SourceRead {
    compression: &'static str,
    packed: u64,
    unpacked: u64,
}

impl<'a> Importer<'a> {
    fn new(sink: &'a mut dyn Sink, o: &'a Options) -> Result<Importer<'a>, Error> {
        if let Some(z) = &o.time_zone
            && parse_tz(z).is_none()
        {
            return Err(Error::Unpack(format!("{z} is not a time zone (an IANA name such as Europe/Berlin)")));
        }
        Ok(Importer {
            sink,
            o,
            keys: HashMap::new(),
            hints: HashMap::new(),
            slots: Vec::new(),
            report: Report::default(),
            bad: BTreeMap::new(),
            fields: Fields::default(),
            missing: BTreeSet::new(),
            units_seen: BTreeSet::new(),
            buffered: 0,
        })
    }

    /// Reads one uploaded file: a CSV, plain, gzipped or xz'd, or a zip of them. `upload` names it among several
    /// (its zip's files are then named under it); empty for the one file of [`import`].
    fn source<R: Read>(&mut self, r: R, upload: &str) -> Result<SourceRead, Error> {
        let mut packed = 0u64;
        let counted = CountRead { inner: r, n: &mut packed };
        let mut src = BufReader::with_capacity(BUF, counted);
        let head = src.fill_buf()?.to_vec();
        let unpack = |e: io::Error, what: &str| {
            if e.kind() == io::ErrorKind::InvalidData || e.kind() == io::ErrorKind::UnexpectedEof {
                Error::Unpack(format!("{what}{e}"))
            } else {
                Error::Io(e)
            }
        };
        let alone_name = if upload.is_empty() { "the file" } else { upload };
        let (compression, unpacked) = if head.starts_with(&[0x1f, 0x8b]) {
            let r = BufReader::with_capacity(BUF, flate2::read::MultiGzDecoder::new(&mut src));
            ("gzip", self.file(alone_name, r, true).map_err(|e| e.unpack("gzip: "))?)
        } else if head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
            let r =
                BufReader::with_capacity(BUF, lzma_rust2::XzReader::new_mem_limit(&mut src, true, XZ_MAX_WINDOW_KIB));
            ("xz", self.file(alone_name, r, true).map_err(|e| e.unpack("xz: "))?)
        } else if is_zip(&head) {
            let mut z = ZipStream::new(&mut src);
            let mut unpacked = 0u64;
            loop {
                let e = match z.next_entry() {
                    Ok(Some(e)) => e,
                    Ok(None) => break,
                    Err(ZipError::Io(e)) => return Err(unpack(e, "zip: ")),
                    Err(e) => return Err(Error::Unpack(e.to_string())),
                };
                if skip_entry(&e.name) {
                    continue;
                }
                let name = if upload.is_empty() { e.name.clone() } else { format!("{upload}/{}", e.name) };
                let r = BufReader::with_capacity(BUF, &mut z);
                unpacked += self.file(&name, r, false).map_err(|e| e.unpack(""))?;
            }
            ("zip", unpacked)
        } else if head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
            return Err(Error::Unsupported("zstd"));
        } else if head.starts_with(b"BZh") {
            return Err(Error::Unsupported("bzip2"));
        } else {
            ("plain", self.file(alone_name, &mut src, true).map_err(|e| e.unpack(""))?)
        };
        drop(src);
        Ok(SourceRead { compression, packed, unpacked })
    }
}

/// An error from reading one file: the file's own fault, or the sink's.
enum FileError {
    Io(io::Error),
    Other(Error),
}

impl From<io::Error> for FileError {
    fn from(e: io::Error) -> FileError {
        FileError::Io(e)
    }
}

impl From<Error> for FileError {
    fn from(e: Error) -> FileError {
        FileError::Other(e)
    }
}

impl FileError {
    fn unpack(self, what: &str) -> Error {
        match self {
            FileError::Io(e) if e.kind() == io::ErrorKind::InvalidData || e.kind() == io::ErrorKind::UnexpectedEof => {
                Error::Unpack(format!("{what}{e}"))
            }
            FileError::Io(e) if e.kind() == io::ErrorKind::OutOfMemory => Error::Unpack(format!(
                "{what}it was packed with a window over {} MiB: pack it again at a normal level",
                XZ_MAX_WINDOW_KIB >> 10
            )),
            FileError::Io(e) => Error::Io(e),
            FileError::Other(e) => e,
        }
    }
}

struct CountRead<'n, R> {
    inner: R,
    n: &'n mut u64,
}

impl<R: Read> Read for CountRead<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let k = self.inner.read(buf)?;
        *self.n += k as u64;
        Ok(k)
    }
}

/// What a file's first rows show of its units.
fn detect(h: &Header, lines: &[Vec<u8>], o: &Options) -> Units {
    let mut d = Detect::default();
    let mut f = Fields::default();
    for b in lines {
        let (t, _) = csv::text(b);
        f.split(&t);
        d.see(&Row::Csv { h, f: &f, line: &t });
    }
    d.units(o)
}

/// What rows show of their units (the file's first rows, or one answer of TeslaFi's API).
#[derive(Default)]
pub(crate) struct Detect {
    ratios: Vec<f64>,
    t_diff: u32,
    t_same: u32,
    hot: bool,
    gui_d: Option<String>,
    gui_t: Option<String>,
    measure: Option<String>,
}

impl Detect {
    pub(crate) fn see(&mut self, row: &Row<'_>) {
        let mut bad = BTreeMap::new();
        if let (Some(a), Some(b)) = (row.num(Col::Odometer, &mut bad), row.num(Col::OdometerF, &mut bad))
            && a > 1.0
        {
            self.ratios.push(b / a);
        }
        if let (Some(a), Some(b)) = (row.num(Col::InsideTemp, &mut bad), row.num(Col::InsideTempF, &mut bad)) {
            if (a - b).abs() > 0.5 { self.t_diff += 1 } else { self.t_same += 1 }
        }
        for c in [Col::InsideTemp, Col::OutsideTemp] {
            if row.num(c, &mut bad).is_some_and(|x| x > 60.0) && !row.has(Col::InsideTempF) {
                self.hot = true;
            }
        }
        self.gui_d = self.gui_d.take().or_else(|| row.text(Col::GuiDistanceUnits));
        self.gui_t = self.gui_t.take().or_else(|| row.text(Col::GuiTemperatureUnits));
        self.measure = self.measure.take().or_else(|| row.text(Col::Measure));
    }

    pub(crate) fn units(mut self, o: &Options) -> Units {
        let (t_diff, t_same, hot) = (self.t_diff, self.t_same, self.hot);
        let gui_km = self.gui_d.as_deref().map(|g| g.to_ascii_lowercase().starts_with("km")).or_else(|| {
            self.measure.as_deref().map(|m| {
                let m = m.to_ascii_lowercase();
                m.contains("metric") || m.starts_with("km")
            })
        });
        let (distance, distance_from) = match o.distance {
            Some(d) => (d, "setting"),
            None => {
                self.ratios.sort_by(f64::total_cmp);
                match self.ratios.get(self.ratios.len() / 2) {
                    Some(r) if (r - 1.609_344).abs() < 0.05 => (Distance::Mi, "detected"),
                    Some(r) if (r - 0.621_371).abs() < 0.03 => (Distance::Km, "detected"),
                    Some(r) if (r - 1.0).abs() < 0.01 => match gui_km {
                        Some(true) => (Distance::Km, "detected"),
                        Some(false) => (Distance::Mi, "detected"),
                        None => (Distance::Mi, "default"),
                    },
                    _ => (Distance::Mi, "default"),
                }
            }
        };
        let (temperature, temperature_from) = match o.temperature {
            Some(t) => (t, "setting"),
            None if t_diff > 0 && t_diff >= t_same => (Temperature::C, "detected"),
            None if t_same > 0 => match self.gui_t.as_deref().map(|g| g.trim().to_ascii_uppercase()) {
                Some(g) if g.starts_with('F') => (Temperature::F, "detected"),
                Some(g) if g.starts_with('C') => (Temperature::C, "detected"),
                _ => (Temperature::C, "default"),
            },
            None if hot => (Temperature::F, "detected"),
            None => (Temperature::C, "default"),
        };
        Units { distance, distance_from: distance_from.into(), temperature, temperature_from: temperature_from.into() }
    }
}

impl Importer<'_> {
    /// Reads one file; `alone`: the upload is this one file (one that isn't TeslaFi's is then an error, not a file
    /// passed over). Answers the bytes read.
    fn file(&mut self, name: &str, r: impl BufRead, alone: bool) -> Result<u64, FileError> {
        let mut lines = Lines::new(r, self.o.max_line);
        let mut buf = Vec::new();
        // the header: the first line that isn't blank
        let header = loop {
            match lines.next(&mut buf)? {
                None => {
                    self.skip_file(name, "it is empty", alone)?;
                    return Ok(lines.bytes);
                }
                Some(Line::TooLong) => {
                    self.skip_file(name, "its first line is too long for a header", alone)?;
                    return Ok(drain(&mut lines, &mut buf)?);
                }
                Some(Line::Text) => {
                    let (t, _) = csv::text(&buf);
                    if t.trim().is_empty() {
                        continue;
                    }
                    let (names, kind) = header_of(&t);
                    match kind {
                        HeaderKind::Raw => break Header::new(&names),
                        HeaderKind::Summary if alone => return Err(Error::TeslafiSummary.into()),
                        HeaderKind::Summary => {
                            self.skip_file(
                                name,
                                "TeslaFi's list of drives or charges (no positions in it): the monthly data \
                                 downloads are read",
                                false,
                            )?;
                            return Ok(drain(&mut lines, &mut buf)?);
                        }
                        HeaderKind::Other => {
                            self.skip_file(name, "not TeslaFi's data", alone)?;
                            return Ok(drain(&mut lines, &mut buf)?);
                        }
                    }
                }
            }
        };
        self.report.files += 1;
        for m in header.missing() {
            self.missing.insert(m);
        }
        self.report.unknown_columns = self.report.unknown_columns.max(header.unknown as u64);
        // the first rows tell the units
        let mut ahead: Vec<Vec<u8>> = Vec::new();
        let mut too_long_ahead = 0u64;
        // (always: they also tell the VIN of rows that lack one)
        while ahead.len() < self.o.lookahead_rows.max(1) {
            match lines.next(&mut buf)? {
                None => break,
                Some(Line::TooLong) => too_long_ahead += 1,
                Some(Line::Text) => ahead.push(buf.clone()),
            }
        }
        self.hint_vins(&header, &ahead);
        let units = detect(&header, &ahead, self.o);
        self.units_seen.insert((units.distance, units.temperature));
        if self.report.units.is_none() {
            self.report.units = Some(units.clone());
        }
        for _ in 0..too_long_ahead {
            self.report.rows_read += 1;
            self.report.skip("too_long");
        }
        for l in &ahead {
            self.row(&header, l, &units)?;
        }
        drop(ahead);
        loop {
            match lines.next(&mut buf)? {
                None => break,
                Some(Line::TooLong) => {
                    self.report.rows_read += 1;
                    self.report.skip("too_long");
                }
                Some(Line::Text) => {
                    let line = std::mem::take(&mut buf);
                    self.row(&header, &line, &units)?;
                    buf = line;
                }
            }
        }
        // the file is over: each car's run in it ends here
        for i in 0..self.slots.len() {
            self.end_run(i)?;
            self.slots[i].prev_ms = None;
            self.flush(i)?;
        }
        Ok(lines.bytes)
    }

    fn skip_file(&mut self, name: &str, why: &str, alone: bool) -> Result<(), Error> {
        if alone {
            return Err(Error::NotTeslafi);
        }
        self.report.files_skipped_count += 1;
        if self.report.files_skipped.len() < 100 {
            self.report.files_skipped.insert(name.chars().take(200).collect(), why.into());
        }
        Ok(())
    }

    /// Notes the VIN that rows with a vehicle id or a name show, so a row of the same car without its VIN (TeslaFi
    /// leaves it blank now and then) goes to that car even when it comes first.
    fn hint_vins(&mut self, h: &Header, lines: &[Vec<u8>]) {
        let mut f = Fields::default();
        for b in lines {
            let (t, _) = csv::text(b);
            f.split(&t);
            let row = Row::Csv { h, f: &f, line: &t };
            let Some(vin) = row_vin(&row) else { continue };
            for k in [id_key(&row), name_key(&row)].into_iter().flatten() {
                self.hints.entry(k).or_insert_with(|| vin.clone());
            }
        }
    }

    /// The car a row belongs to (asked of the sink the first time), `None` when it can't be taken. A car is known by
    /// its VIN, else its vehicle id, else its name: a row without a VIN goes with the car whose rows had the same
    /// vehicle id (or name), never to a car of its own.
    fn slot_for(&mut self, row: &Row<'_>) -> Result<Option<usize>, Error> {
        let (id, name) = (id_key(row), name_key(row));
        let vin = row_vin(row).or_else(|| [&id, &name].into_iter().flatten().find_map(|k| self.hints.get(k)).cloned());
        let vin_key = vin.as_ref().map(|v| format!("vin:{v}"));
        let found = match &vin_key {
            Some(k) => self.keys.get(k).copied().or_else(|| {
                // a car first seen without a VIN (none shown for it yet) with the same id
                id.as_ref().and_then(|k| self.keys.get(k)).copied().filter(|&i| self.slots[i].tm.vin.is_none())
            }),
            None => [&id, &name].into_iter().flatten().find_map(|k| self.keys.get(k)).copied(),
        };
        let found = found.or_else(|| {
            (vin_key.is_none() && id.is_none() && name.is_none()).then(|| self.keys.get("").copied()).flatten()
        });
        if let Some(i) = found {
            for k in [vin_key, id, name].into_iter().flatten() {
                self.keys.entry(k).or_insert(i);
            }
            return Ok(Some(i));
        }
        if self.slots.len() >= self.o.max_cars {
            return Ok(None);
        }
        let tm = TmCar {
            tm_id: self.slots.len() as i64 + 1,
            vin: vin.as_deref().and_then(vin_info),
            name: row.text(Col::DisplayName),
            ..TmCar::default()
        };
        let target = self.sink.car(&tm).map_err(sink_err)?;
        let (car, base) = match target {
            Some(t) => (Some(t.car), t.state),
            None => (None, ImportState::default()),
        };
        let tf = base.teslafi.clone().unwrap_or_default();
        if tf.version != STATE_VERSION {
            return Err(Error::StateVersion(tf.version));
        }
        let (tz, tz_from) = match (&self.o.time_zone, car.and_then(|c| self.sink.time_zone(c))) {
            (Some(z), _) => (parse_tz(z).unwrap_or(chrono_tz::UTC), "setting"),
            (None, Some(z)) if parse_tz(&z).is_some() => (parse_tz(&z).unwrap_or(chrono_tz::UTC), "car"),
            _ => (parse_tz(&self.o.default_time_zone).unwrap_or(chrono_tz::UTC), "default"),
        };
        if self.report.time_zone.is_none() {
            self.report.time_zone = Some(tz.name().to_string());
            self.report.time_zone_from = Some(tz_from.into());
        } else if self.report.time_zone.as_deref() != Some(tz.name()) {
            self.report.warn("the cars' times were read in different time zones (each car's own)".into());
        }
        self.report.cars += 1;
        if car.is_none() {
            self.report.cars_left_out += 1;
        }
        let i = self.slots.len();
        let mut other_zones: Vec<chrono_tz::Tz> = Vec::new();
        for z in tf.pieces.iter().filter_map(|p| p.zone.as_deref().and_then(parse_tz)) {
            if z != tz && !other_zones.contains(&z) {
                other_zones.push(z);
            }
        }
        self.slots.push(Slot {
            tm,
            car,
            tz,
            base,
            pieces: tf.pieces,
            old_updates: tf.updates,
            run: None,
            units: None,
            other_zones,
            prev_ms: None,
            pending: Pending::default(),
            tally: Default::default(),
            first_ms: None,
            last_ms: None,
            warnings: Vec::new(),
        });
        let keys = [vin_key, id, name].into_iter().flatten().collect::<Vec<_>>();
        if keys.is_empty() {
            self.keys.insert(String::new(), i);
        }
        for k in keys {
            self.keys.entry(k).or_insert(i);
        }
        Ok(Some(i))
    }

    fn row(&mut self, h: &Header, line: &[u8], units: &Units) -> Result<(), Error> {
        let (text, latin) = csv::text(line);
        if text.trim().is_empty() {
            return Ok(());
        }
        self.report.rows_read += 1;
        if latin {
            self.report.lines_windows_1252 += 1;
        }
        let mut fields = std::mem::take(&mut self.fields);
        fields.split(&text);
        let r = self.row_in(h, &fields, &text, units);
        self.fields = fields;
        r
    }

    fn row_in(&mut self, h: &Header, fields: &Fields, text: &str, units: &Units) -> Result<(), Error> {
        if fields.len() < 2 || fields.len() > h.fields + 2 {
            self.report.skip("wrong_field_count");
            return Ok(());
        }
        let row = Row::Csv { h, f: fields, line: text };
        let Some(local) = row.text(Col::Date).as_deref().and_then(poll::parse_local) else {
            self.report.skip("bad_time");
            return Ok(());
        };
        let Some(i) = self.slot_for(&row)? else {
            self.report.skip("too_many_cars");
            return Ok(());
        };
        if self.slots[i].car.is_none() {
            self.report.skip("car_left_out");
            self.slots[i].tally[ix(Table::Positions)].read += 1;
            self.slots[i].tally[ix(Table::Positions)].skip(crate::Skip::CarLeftOut);
            return Ok(());
        }
        let s = &mut self.slots[i];
        s.tally[ix(Table::Positions)].read += 1;
        let prev = s.run.as_ref().map(|r| r.last_ms).or(s.prev_ms);
        let t = match poll::to_utc(s.tz, local, prev) {
            Utc::One(t) => t,
            Utc::Repeated(t) => {
                self.report.dst_repeated += 1;
                t
            }
            Utc::Skipped => {
                self.report.skip("dst_gap");
                s.tally[ix(Table::Positions)].skip(crate::Skip::BadValue);
                return Ok(());
            }
        };
        s.prev_ms = Some(t);
        s.units = Some((units.distance, units.temperature));
        // in a stretch an earlier import read in another time zone or other units: what it stored goes, the rows
        // are read again (else they would be passed over, or land beside the earlier ones at other times)
        self.read_again(i, local, t, units);
        let s = &mut self.slots[i];
        // already in from an earlier file or import
        let k = s.pieces.partition_point(|p| p.last_ms < t);
        if s.pieces.get(k).is_some_and(|p| p.first_ms <= t) {
            self.report.skip("already_imported");
            self.slots[i].tally[ix(Table::Positions)].skip(crate::Skip::AlreadyImported);
            return self.end_run(i);
        }
        if let Some(run) = &s.run
            && t <= run.last_ms
        {
            let why = if t == run.last_ms { "duplicate" } else { "out_of_order" };
            self.report.skip(why);
            s.tally[ix(Table::Positions)].skip(crate::Skip::BadValue);
            return Ok(());
        }
        // a stretch imported before lies between the run's last row and this one: the run ends before it
        if let Some(run) = &s.run
            && k > 0
            && s.pieces.get(k - 1).is_some_and(|before| before.first_ms > run.last_ms)
        {
            self.end_run(i)?;
        }
        let s = &mut self.slots[i];
        let mut p = poll::read(&row, units.distance, units.temperature, &mut self.bad);
        p.t_ms = t;
        let run = s.run.get_or_insert_with(Run::new);
        let mut out = Vec::new();
        run.push(&p, &mut out);
        let secs = |ms: u64| i64::try_from(ms / 1000).unwrap_or(0);
        let tm_id = p.data_id.unwrap_or(secs(t));
        if p.worth_a_point() {
            s.pending.positions.push(PositionRow {
                tm_id,
                tm_drive_id: run.drive_start().map(secs),
                sample: p.sample(),
                usable_soc: p.usable_soc,
            });
            s.tally[ix(Table::Positions)].imported += 1;
            self.report.positions += 1;
        }
        if p.charging
            && let Some(start) = run.charge_start()
        {
            s.pending.points.push(ChargePointRow {
                tm_id,
                tm_charge_id: secs(start),
                point: ChargePoint {
                    t_ms: t,
                    power_kw: p.charger_power_kw,
                    voltage_v: p.charger_voltage_v,
                    current_a: p.charger_current_a,
                    soc: p.soc,
                    battery_temp_c: None,
                    range_km: p.range_km,
                },
                fast_charger: p.kind.map(|k| k == ChargeKind::Dc),
                phases: p.phases,
                energy_added_kwh: p.energy_added_kwh,
                outside_temp_c: p.outside_temp_c,
                ideal_range_km: p.ideal_range_km,
                usable_soc: p.usable_soc,
            });
            self.report.charge_points += 1;
        }
        self.report.rows_used += 1;
        self.report.widen(t);
        crate::report::widen(&mut s.first_ms, &mut s.last_ms, t);
        self.take(i, out);
        self.buffered = self.slots.iter().map(|s| s.pending.len()).sum();
        self.report.peak_buffered_rows = self.report.peak_buffered_rows.max(self.buffered);
        if self.buffered >= self.o.max_buffered_rows {
            for j in 0..self.slots.len() {
                self.flush(j)?;
            }
        } else if self.slots[i].pending.len() >= self.o.batch_rows {
            self.flush_full(i)?;
        }
        Ok(())
    }

    /// Takes away the earlier piece that the row at wall time `local` (`t` in this import's zone) falls in, when that
    /// piece was read in another time zone (where the row is looked for in that zone) or in other units.
    fn read_again(&mut self, i: usize, local: chrono::NaiveDateTime, t: u64, units: &Units) {
        let s = &mut self.slots[i];
        let tz = s.tz;
        // the row's time in this zone, and in each zone an earlier piece was read in
        let mut at = vec![(tz, t)];
        for z in &s.other_zones {
            match poll::to_utc(*z, local, None) {
                Utc::One(x) | Utc::Repeated(x) => at.push((*z, x)),
                Utc::Skipped => {}
            }
        }
        for (z, x) in at {
            let k = s.pieces.partition_point(|p| p.last_ms < x);
            let hit = s.pieces.get(k).is_some_and(|p| {
                p.first_ms <= x
                    && p.zone.as_deref().and_then(parse_tz).unwrap_or(tz) == z
                    && p.read_otherwise(tz, units)
            });
            if !hit {
                continue;
            }
            let p = s.pieces.remove(k);
            s.pending.cleared.push((p.first_ms, p.last_ms));
            self.report.read_again += 1;
            let how = match &p.zone {
                Some(z) if z != tz.name() => format!("in {z}"),
                _ => "in other units".to_string(),
            };
            self.report.warn(format!(
                "months imported before {how} were read again: what they stored was replaced (read the months next \
                 to them again too, so a drive or charge over their edges is whole)"
            ));
            return;
        }
    }

    /// Segments and updates a car's run gave.
    fn take(&mut self, i: usize, out: Vec<Out>) {
        for o in out {
            match o {
                Out::Closed(a) => self.put(i, &a),
                Out::Update(from, to, version) => {
                    let s = &mut self.slots[i];
                    s.pending.updates.push(UpdateRow {
                        tm_id: i64::try_from(to / 1000).unwrap_or(0),
                        update: Update {
                            car: 0,
                            start_ms: from,
                            end_ms: Some(to),
                            version,
                            source: model::Source::Live,
                            inputs: Default::default(),
                        },
                    });
                    s.tally[ix(Table::Updates)].imported += 1;
                    self.report.updates += 1;
                }
            }
        }
    }

    /// A segment handed over (when it is new or changed), its parts stored earlier under other ids taken away.
    fn put(&mut self, i: usize, a: &Agg) {
        if !a.fresh {
            return;
        }
        let s = &mut self.slots[i];
        let id = a.id();
        let table = match a.kind {
            Kind::Drive => Table::Drives,
            Kind::Charge => Table::ChargingProcesses,
            Kind::State => Table::States,
        };
        for old in a.stored.iter().filter(|x| **x != id) {
            s.pending.removed.push((table, *old));
            self.report.removed += 1;
        }
        s.tally[ix(table)].imported += 1;
        match a.kind {
            Kind::Drive => {
                s.pending.drives.push(DriveRow { tm_id: id, drive: a.drive(), tm: Default::default() });
                self.report.drives += 1;
            }
            Kind::Charge => {
                s.pending.charges.push(ChargeRow { tm_id: id, charge: a.charge(), tm: Default::default() });
                self.report.charges += 1;
            }
            Kind::State => {
                let (span, offline) = a.span();
                s.pending.states.push(StateRow { tm_id: id, span, offline });
                self.report.states += 1;
            }
        }
    }

    /// Ends a car's run: what closed is handed over, the piece (with what was under way at its edges) kept.
    fn end_run(&mut self, i: usize) -> Result<(), Error> {
        let Some(run) = self.slots[i].run.take() else { return Ok(()) };
        let mut out = Vec::new();
        let (run, tails) = run.finish(&mut out);
        self.take(i, out);
        let Some(first) = run.first_ms else { return Ok(()) };
        let mut edges: Vec<Edge> = run.heads.into_iter().map(|seg| Edge { at: End::Head, seg }).collect();
        for seg in tails {
            let at = if seg.start_ms == first { End::Through } else { End::Tail };
            edges.push(Edge { at, seg });
        }
        let piece = Piece {
            first_ms: first,
            last_ms: run.last_ms,
            rows: run.rows,
            first_version: run.first_version,
            last_version: run.last_version,
            edges,
            zone: Some(self.slots[i].tz.name().to_string()),
            distance: self.slots[i].units.map(|u| u.0),
            temperature: self.slots[i].units.map(|u| u.1),
        };
        let s = &mut self.slots[i];
        let k = s.pieces.partition_point(|p| p.first_ms < piece.first_ms);
        s.pieces.insert(k, piece);
        Ok(())
    }

    fn flush_full(&mut self, i: usize) -> Result<(), Error> {
        let n = self.o.batch_rows;
        let p = &self.slots[i].pending;
        if p.positions.len() >= n
            || p.points.len() >= n
            || p.drives.len() >= n
            || p.charges.len() >= n
            || p.states.len() >= n
            || p.updates.len() >= n
            || p.removed.len() >= n
        {
            self.flush(i)?;
        }
        Ok(())
    }

    /// Hands over every row of a car that is waiting.
    fn flush(&mut self, i: usize) -> Result<(), Error> {
        let Some(car) = self.slots[i].car else {
            self.slots[i].pending = Pending::default();
            return Ok(());
        };
        let p = std::mem::take(&mut self.slots[i].pending);
        let n = self.o.batch_rows.max(1);
        let mut send = |b: Batch| -> Result<(), Error> {
            if b.is_empty() { Ok(()) } else { self.sink.batch(car, b).map_err(sink_err) }
        };
        // removals first: a joined row may reuse nothing of what it replaces, but its halves go before it comes;
        // stretches read again before that
        if !p.cleared.is_empty() {
            send(Batch::Cleared(p.cleared))?;
        }
        for c in p.removed.chunks(n) {
            send(Batch::Removed(c.to_vec()))?;
        }
        for c in chunks(p.positions, n) {
            send(Batch::Positions(c))?;
        }
        for c in chunks(p.points, n) {
            send(Batch::ChargePoints(c))?;
        }
        for c in chunks(p.drives, n) {
            send(Batch::Drives(c))?;
        }
        for c in chunks(p.charges, n) {
            send(Batch::Charges(c))?;
        }
        for c in chunks(p.states, n) {
            send(Batch::States(c))?;
        }
        for c in chunks(p.updates, n) {
            send(Batch::Updates(c))?;
        }
        self.buffered = self.slots.iter().map(|s| s.pending.len()).sum();
        Ok(())
    }

    /// Joins each car's pieces, hands over what is left, and gives each car's report and state.
    fn finish(mut self) -> Result<Report, Error> {
        for i in 0..self.slots.len() {
            self.end_run(i)?;
            if self.slots[i].car.is_some() {
                self.join(i);
            }
            self.flush(i)?;
        }
        self.report.missing_columns = self.missing.iter().map(|s| s.to_string()).collect();
        self.report.bad_values = self.bad.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        if self.units_seen.len() > 1 {
            self.report.warn(
                "the files were not all in the same units: each was read in its own (say which to use if that is \
                 wrong)"
                    .into(),
            );
        }
        let slots = std::mem::take(&mut self.slots);
        for s in slots {
            let report = CarReport {
                tm_car_id: s.tm.tm_id,
                car: s.car,
                vin_prefix: s.tm.vin.as_ref().map(|v| v.vin_prefix.clone()),
                name: s.tm.name.clone(),
                tables: Table::PER_CAR
                    .iter()
                    .filter_map(|t| {
                        let r: TableReport = s.tally[ix(*t)].report();
                        (r.read > 0 || r.imported > 0 || !r.skipped.is_empty()).then_some((*t, r))
                    })
                    .collect(),
                first_ms: s.first_ms,
                last_ms: s.last_ms,
                open_waiting: s.pieces.iter().map(|p| p.edges.len() as u64).sum(),
                warnings: s.warnings.clone(),
            };
            let state = s.car.map(|_| {
                let mut st = s.base.clone();
                st.teslafi = Some(State { version: STATE_VERSION, pieces: s.pieces.clone(), updates: s.old_updates });
                st
            });
            self.sink.car_done(report, state).map_err(sink_err)?;
        }
        Ok(self.report)
    }

    /// Joins what runs over the pieces' edges, finds the updates between pieces, and hands over what is new.
    fn join(&mut self, i: usize) {
        let max_pieces = self.o.max_pieces.max(2);
        let pieces = std::mem::take(&mut self.slots[i].pieces);
        let mut edges: Vec<Vec<Edge>> = vec![Vec::new(); pieces.len()];
        let mut emit: Vec<Agg> = Vec::new();
        for kind in Kind::ALL {
            let mut carry: Option<(Agg, usize, End)> = None;
            for (k, p) in pieces.iter().enumerate() {
                let head = p.edges.iter().find(|e| e.seg.kind == kind && matches!(e.at, End::Head | End::Through));
                let tail = p.edges.iter().find(|e| e.seg.kind == kind && e.at == End::Tail);
                let joinable = matches!((&carry, head), (Some((c, _, _)), Some(h)) if c.continues(&h.seg));
                if joinable && let (Some((mut c, _, _)), Some(h)) = (carry.take(), head) {
                    c.join(h.seg.clone());
                    c.fresh = true;
                    if h.at == End::Through {
                        carry = Some((c, k, End::Tail));
                        continue;
                    }
                    emit.push(c);
                } else {
                    if let Some((c, owner, end)) = carry.take() {
                        edges[owner].push(Edge { at: end, seg: c.clone() });
                        emit.push(c);
                    }
                    if let Some(h) = head {
                        if h.at == End::Through {
                            carry = Some((h.seg.clone(), k, End::Through));
                            continue;
                        }
                        edges[k].push(Edge { at: End::Head, seg: h.seg.clone() });
                        emit.push(h.seg.clone());
                    }
                }
                if let Some(t) = tail {
                    carry = Some((t.seg.clone(), k, End::Tail));
                }
            }
            if let Some((c, owner, end)) = carry {
                edges[owner].push(Edge { at: end, seg: c.clone() });
                emit.push(c);
            }
        }
        for a in &emit {
            self.put(i, a);
        }
        // updates between pieces: the version at one piece's end against the next one's start
        let mut want: BTreeMap<i64, UpdateRow> = BTreeMap::new();
        let mut prev: Option<&(u64, String)> = None;
        for p in &pieces {
            if let (Some(a), Some(b)) = (prev, &p.first_version)
                && a.1 != b.1
            {
                let id = i64::try_from(b.0 / 1000).unwrap_or(0);
                want.insert(
                    id,
                    UpdateRow {
                        tm_id: id,
                        update: Update {
                            car: 0,
                            start_ms: a.0,
                            end_ms: Some(b.0),
                            version: b.1.clone(),
                            source: model::Source::Live,
                            inputs: Default::default(),
                        },
                    },
                );
            }
            if p.last_version.is_some() {
                prev = p.last_version.as_ref();
            }
        }
        let s = &mut self.slots[i];
        let old: BTreeSet<i64> = s.old_updates.iter().copied().collect();
        for id in old.iter().filter(|id| !want.contains_key(id)) {
            s.pending.removed.push((Table::Updates, *id));
            self.report.removed += 1;
        }
        for (id, u) in &want {
            if !old.contains(id) {
                s.pending.updates.push(u.clone());
                s.tally[ix(Table::Updates)].imported += 1;
                self.report.updates += 1;
            }
        }
        s.old_updates = want.keys().copied().collect();
        // the new state: the pieces with what is still under way at their edges
        let mut pieces = pieces;
        for (p, e) in pieces.iter_mut().zip(edges) {
            p.edges = e
                .into_iter()
                .map(|mut e| {
                    e.seg.stored = vec![e.seg.id()];
                    e.seg.fresh = false;
                    e
                })
                .collect();
        }
        if pieces.len() > max_pieces {
            let n = pieces.len() - max_pieces;
            pieces.drain(..n);
            s.warnings.push(format!(
                "{n} of the oldest imported months are no longer remembered: importing them again replaces them"
            ));
        }
        s.pieces = pieces;
    }
}

fn chunks<T>(v: Vec<T>, n: usize) -> Vec<Vec<T>> {
    let mut out = Vec::new();
    let mut it = v.into_iter().peekable();
    while it.peek().is_some() {
        out.push(it.by_ref().take(n).collect());
    }
    out
}

fn drain<R: BufRead>(lines: &mut Lines<R>, buf: &mut Vec<u8>) -> io::Result<u64> {
    while lines.next(buf)?.is_some() {}
    Ok(lines.bytes)
}
