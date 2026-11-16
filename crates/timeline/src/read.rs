//! One stored log, read whole: its samples, its facts and the VIN, in one pass over its lines.

use crate::facts::{FactsCollector, LogFacts};
use crate::sample::{SampleBuilder, SampleParams};
use model::{Sample, VinInfo};

/// What one log gives the timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct LogRead {
    pub samples: Vec<Sample>,
    pub facts: LogFacts,
    pub vin: Option<VinInfo>,
}

/// Reads a `.log.xz` (either format) with its sidecar: decodes it with the car's DBC set (the sidecar's bus map
/// applies), and builds its samples, its facts and the VIN. Bounded as `logfmt` reads (lines, unpacked size), and a
/// log's samples are at most one per second of it.
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
            builder.feed(&mut decoder, f);
        }
        facts.line(&line);
    })?;
    let (samples, vin) = builder.finish();
    Ok(LogRead { samples, facts: facts.finish(), vin })
}
