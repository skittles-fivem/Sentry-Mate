//! One stored log, read whole: its samples, its facts and the VIN, in one pass over its lines.

use crate::facts::{FactsCollector, LogFacts};
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
    let mut decoder = dbc::Decoder::new(profile, sidecar);
    let mut builder = SampleBuilder::new(profile, params);
    let mut facts = FactsCollector::new(LogFacts::from_sidecar(sidecar));
    logfmt::read_log(xz, logfmt::Limits::default(), |l| {
        let line = logfmt::Line::parse(l);
        if let logfmt::Line::Frame(f) = &line {
            if builder.pending() < MAX_LOG_SAMPLES {
                builder.feed(&mut decoder, f);
            }
        }
        facts.line(&line);
    })?;
    let (samples, vin) = builder.finish();
    Ok(LogRead { samples, facts: facts.finish(), vin })
}
