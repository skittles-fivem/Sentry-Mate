//! Made-up but realistic Screenmate CAN Bridge logs, so all of Sentry Mate can be tested end to end without anyone's
//! car. For tests and the hidden `sentry-mate dev seed` command only: nothing the server stores or decodes for a real
//! car goes through here.
//!
//! A [`Scenario`] is a made-up car (a clearly fake VIN that still reads as a Model Y or 3, its own CAN Bridge key and
//! signing key) living in a made-up town in the open sea (0.30° N, 0.20° E). Day by day it sleeps, wakes, commutes,
//! runs errands, takes a trip to a supercharger (DC, along a charge curve), charges at home overnight (AC, with a
//! pause), updates its software (the buses go quiet while modules restart), and has the bridge's trouble when asked
//! (a lost log, dropped frames, a pause in recording, the app updating itself).
//!
//! Its frames are encoded with the DBC set `dbc` decodes (`tesla-3y`), on the buses the car uses: the vehicle bus
//! (`can0`), the chassis bus (`can1`, asleep while charging) and the Screenmate's internal bus (`can3`, its own GPS
//! in 0x04F), at the car's rates (10 ms to 1 s, [`Rates::Realistic`]) or at most once a second ([`Rates::Light`]).
//! The VIN comes in 0x405's three pages.
//!
//! The made-up bridge writes what the real one does (SMO `docs/sentry-manager-format.md`): a log a minute, closed
//! `quiet` after 60 s without frames, `# clock` and `# helper-clock` lines, `# not recorded` and `# dropped` when
//! they happen, candump text (`/1`) or columns (`/2`) in xz, and a sidecar whose `integrity` is signed and chained by
//! its key exactly as the bridge signs, so `trust` verifies it.
//!
//! ```no_run
//! let scenario = synth::Scenario::new(7).days(2).with_trouble(true);
//! let mut logs = scenario.logs();
//! for log in logs.by_ref() {
//!     // log.name, log.bytes, log.sidecar_name(), log.sidecar_bytes()
//!     let _ = log;
//! }
//! let report = logs.report(); // the drives and charges that really happened, the lost logs ...
//! assert!(!report.truth.drives.is_empty());
//! ```
//!
//! Everything is deterministic: the same scenario and seed give the same logs, byte for byte.

mod bridge;
mod car;
pub mod columns;
mod geo;
mod plan;
mod rng;
mod scenario;
mod signals;
mod signer;
mod sim;
pub mod xz;

pub use bridge::{Formats, LogFile, utc_stamp};
pub use car::{CarModel, CarState, ChargeStatus};
pub use geo::LatLon;
pub use plan::{Place, Trouble};
pub use scenario::{DEFAULT_START_MS, Logs, Report, Scenario, TOWN, fleet};
pub use signals::{Encoded, Rates, frames_for, vin_pages};
pub use signer::Signer;
pub use sim::{TrueCharge, TrueDrive, Truth};
