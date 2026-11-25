//! Receiving CAN Bridge logs: the upload API the app speaks, kept files exactly as they came, per-car tokens, and each
//! log's trust record. See [`api`] for the routes and [`server`] for TLS and connection limits.
//!
//! Any number of cars: everything is kept per car on disk and only recently used cars are in memory (bounded caches,
//! see [`Memory`]); each car has its own lock, so cars upload in parallel; nothing an upload does reads a car's whole
//! folder or every car's files, and no history file is rewritten whole on an upload.

pub mod api;
mod cache;
pub mod contacts;
pub mod error;
pub mod judge;
mod ledger;
pub mod missing;
mod owner_use;
mod pack;
pub mod pool;
pub mod server;
pub mod store;
pub mod tokens;

pub use api::{Conn, Exporter, Memory, RefusedCall, Uploads, router};
pub use contacts::{Call, Contact, Contacts};
pub use error::ApiError;
pub use judge::{Judge, Judged, OnJudged};
pub use missing::{Missing, MissingLog};
pub use store::Store;
pub use tokens::Tokens;
