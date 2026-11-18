//! Moving a Sentry Mate data folder to another server.
//!
//! Everything in the data folder is carried exactly as stored: each file's sealing is bound to its path under the
//! data folder, so at the same path it opens on the new server with the same data key. Nothing is opened, decoded or
//! sealed again. The key file never leaves its folder: the new server writes its own.
//!
//! - [`tree`]: what is carried, in what order, and the three kinds of file (one rule for every part).
//! - [`export`]: the export file, made from a snapshot taken in a pause of the writes ([`gate`]), read again from any
//!   byte (a resumed download).
//! - [`import`]: an export file into a new server with the recovery key: checked before anything is used, carried on
//!   after a restart, and switched into place in steps that can each run again.
//!
//! The direct move between two running servers (`docs/plans/move-server.md`) builds on the same walk and the same
//! switch.

mod error;
pub mod export;
mod format;
pub mod import;
pub mod tree;

pub use error::Error;
pub use export::{Header, Snapshot};
pub use format::{Entry, size};
pub use import::{ExportFile, Import, Options, Outcome, Pending, Progress, Resume, Switched};
