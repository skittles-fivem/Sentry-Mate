//! One stored log, read whole: its samples, its facts, the VIN and the firmware fingerprints, in one pass over its
//! lines.

use crate::facts::{FactsCollector, LogFacts};
use crate::firmware::{FirmwareCollector, LogFirmware};
use crate::sample::{SampleBuilder, SampleParams};
use model::{Sample, VinInfo};

/// The most samples one log gives (an hour of seconds). A real log has about 60 (the bridge closes one after a minute
/// of frames), but a sample is made at every change of second, so a crafted or broken log whose frame times go back and
/// forth would otherwise make one per frame: millions, at about 450 bytes each. Past this, the rest of the log's frames
/// still count for its facts, not for samples.
pub const MAX_LOG_SAMPLES: usize = 3_600;

/// What one log gives the timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct LogRead {
    pub samples: Vec<Sample>,
    pub facts: LogFacts,
    pub vin: Option<VinInfo>,
    /// The firmware fingerprints its vehicle-bus frames carried, and the bridge's hint lines.
    pub firmware: LogFirmware,
    /// Whether the Screenmate's own GPS frames gave positions: only such a log reads otherwise when the car's layout
    /// of that frame changes.
    pub internal_gps: bool,
}

/// Reads a `.log.xz` (either format) with its sidecar: decodes it with the car's DBC set (the sidecar's bus map
/// applies), and builds its samples, its facts and the VIN. Bounded as `logfmt` reads (lines, unpacked size), and a
/// log gives at most [`MAX_LOG_SAMPLES`] samples (one per second of it).
pub fn read_log(
    xz: &[u8],
    sidecar: &serde_json::Value,
    profile: dbc::Profile,
    params: SampleParams,
) -> Result<LogRead, logfmt::Error> {
    read_log_with(xz, sidecar, profile, params, None)
}

/// [`read_log`], the Screenmate's own GPS frames read with the car's layout when it has one
/// ([`SampleBuilder::set_internal_gps`]).
pub fn read_log_with(
    xz: &[u8],
    sidecar: &serde_json::Value,
    profile: dbc::Profile,
    params: SampleParams,
    internal_gps: Option<model::gps::GpsLayout>,
) -> Result<LogRead, logfmt::Error> {
    let mut decoder = dbc::Decoder::new(profile, sidecar);
    let mut builder = SampleBuilder::new(profile, params);
    builder.set_internal_gps(internal_gps);
    let mut facts = FactsCollector::new(LogFacts::from_sidecar(sidecar));
    // the vehicle bus as the sidecar's bus map names it, as the decoder reads it
    let mut firmware = FirmwareCollector::on_buses(crate::firmware::vehicle_buses(decoder.buses()));
    logfmt::read_lines(xz, logfmt::Limits::default(), |line| {
        if let logfmt::Line::Frame(f) = line
            && builder.pending() < MAX_LOG_SAMPLES
        {
            builder.feed(&mut decoder, f);
        }
        facts.line(line);
        firmware.line(line);
    })?;
    let internal_gps = builder.used_internal_gps();
    let (samples, vin) = builder.finish();
    Ok(LogRead { samples, facts: facts.finish(), vin, firmware: firmware.finish(), internal_gps })
}
