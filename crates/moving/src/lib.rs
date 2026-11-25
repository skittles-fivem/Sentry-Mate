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
//! The direct move between two running servers (`docs/plans/move-server.md`, section 2) builds on the same walk
//! and the same switch:
//!
//! - [`code`]: the move code the owner copies from the old server to the new one.
//! - [`proto`]: what the two servers say to each other on the upload port (`/api/move/v1/`).
//! - [`old`]: the old server's side: the code, the claim, the lists and fetches, the pause, the finish, the moved
//!   state.
//! - [`pull`]: the new server's side: the claim, the copy rounds, the finish and the switch.
//! - [`client`]: the new server's TLS 1.3 connection to the old one, pinned to its certificate.
//! - [`log`]: the move's log for the owner on each server, step by step, nothing secret in it.

pub mod client;
pub mod code;
mod error;
pub mod export;
mod format;
pub mod import;
pub mod log;
pub mod old;
pub mod proto;
pub mod pull;
pub mod tree;

pub use error::Error;
pub use export::{Header, Phase, Seen, Snapshot, Watch};
pub use format::{Entry, size};
pub use import::{ExportFile, Import, Options, Opts, Outcome, Pending, Progress, Resume, Staged, Switched};
