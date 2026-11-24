//! The last few rebuilds the server ran, with what started them and how long they took (Data health shows them):
//! full rebuilds, a car's history made again, its positions read again, its inputs joined again. Kept in the engine's
//! record (`engine` / `rebuild-history`), newest first, at most [`KEPT`].

use crate::versions::Cause;
use crate::{Engine, Result, lock, records};
use model::CarId;
use serde::{Deserialize, Serialize};

const KEY: &str = "rebuild-history";
/// Runs kept.
pub const KEPT: usize = 10;

/// One rebuild that ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    /// `full` (everything made again beside the history), `car` (a car's history made again from its logs, or a
    /// period of it), `positions` (only the logs whose positions change read again), `inputs` (the car's inputs
    /// joined again, nothing decoded again).
    pub kind: String,
    pub cars: Vec<CarId>,
    /// Every car.
    pub all: bool,
    pub causes: Vec<Cause>,
    /// `done`, `failed`, `cancelled`.
    pub state: String,
    pub started_ms: u64,
    pub finished_ms: u64,
    /// Logs decoded again.
    pub logs: u64,
}

impl Run {
    pub fn took_ms(&self) -> u64 {
        self.finished_ms.saturating_sub(self.started_ms)
    }
}

impl Engine {
    /// The last rebuilds, newest first.
    pub fn rebuild_history(&self) -> Result<Vec<Run>> {
        Ok(records::get::<Vec<Run>>(&self.inner.db, records::NS, KEY)?.unwrap_or_default())
    }

    /// Notes a rebuild that ended. Called before the rebuild is told as ended (its record, its job's status), so
    /// that an end seen is an end in the history.
    pub(crate) fn note_rebuild(&self, run: Run) {
        let _one = lock(&self.inner.history);
        let mut v = self.rebuild_history().unwrap_or_default();
        // (a full rebuild's end is noted before its record says it ended: one cut off between the two ends again
        // when the server starts, and is still one run)
        v.retain(|x| {
            !(run.kind == "full" && x.kind == run.kind && x.started_ms == run.started_ms && x.cars == run.cars)
        });
        v.insert(0, run);
        v.truncate(KEPT);
        if let Err(e) = records::put(&self.inner.db, records::NS, KEY, &v) {
            tracing::warn!("the rebuilds' history: {e}");
        }
    }
}
