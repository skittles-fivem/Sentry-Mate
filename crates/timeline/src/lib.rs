//! A car's history worked out from its decoded CAN signals: when it was online, asleep or out of sight, its drives,
//! its charges, where it went and its charge curves.
//!
//! Everything here is per car and pure: no global state, nothing shared between cars, no input but what is passed
//! in. Any number of cars are worked on at once (one thread each, or a pool), and a car's work is bounded by the
//! window asked for, never by its whole history or by how many other cars there are.
//!
//! 1. **Samples** ([`SampleBuilder`], or [`read_log`] for a stored log): one log's decoded values → a
//!    [`model::Sample`] once a second, each value from the best signal that has one (Sentry Six Manager's choices),
//!    converted by the DBC's own units. The VIN comes from 0x405 / 0x508 on the way ([`vin`]).
//! 2. **Log facts** ([`LogFacts`]): when a log's frames came and on which buses, why the bridge closed it, recording
//!    pauses and lost frames: what the states come from.
//! 3. **The timeline** ([`build`]): a window's samples and log facts → states, drives, charges, positions and charge
//!    points, and the idle cut points where nothing goes on across. [`rebuild_window`] says which window a newly
//!    arrived log changes: back and forward to the nearest sleep, long gap or cut point, so a car that never sleeps
//!    still gets small windows.
//!
//! 4. **Two live inputs** ([`fuse`]): SMO's samples and the Tesla API's made one stream before the timeline is
//!    built, SMO's winning where it has data, the API's filling the rest; the API's view of the car's state fills
//!    what SMO does not know ([`Input::api`]). Rows name the inputs that gave them.
//!
//! Units are those of `model`: ms since 1970 UTC, km, km/h, kW, kWh, °C, bar; battery power positive out of the pack.

mod calc;
mod charges;
mod drives;
pub mod facts;
pub mod fuse;
mod read;
pub mod sample;
mod signals;
mod states;
mod timeline;
mod units;
pub mod vin;
mod window;

pub use facts::{Closed, FactsCollector, LogFacts, NotRecorded};
pub use fuse::{ApiSpan, ApiState, FuseParams, Fused, fuse};
pub use read::{LogRead, MAX_LOG_SAMPLES, read_log};
pub use sample::{SampleBuilder, SampleParams};
pub use states::is_settled;
pub use timeline::{Input, Loss, Params, RULES_VERSION, Timeline, build, charge_id, drive_id};
pub use vin::{VinCollector, decode_vin};
pub use window::{Window, rebuild_window};
