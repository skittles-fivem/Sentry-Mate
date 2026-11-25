//! Every GPS layout fit run, kept per car beside the engine's other records (sealed like them), so a run is there
//! after a restart or an update, and the layouts a car was set to over time with who set them.
//!
//! ```text
//! engine  gps-fit-seq               the last run's id (ids are the server's, never used twice)
//! engine  car/<id>/gps-fits         the ids of the car's finished runs, oldest first
//! engine  car/<id>/gps-fit/<run>    one finished run ([`FitStatus`]: its counts and its result, never a position)
//! engine  car/<id>/gps-layout-log   the layouts the car was set to, newest last ([`LayoutChange`])
//! ```
//!
//! A run under way lives in memory (it goes on in the server whatever page is open); it is kept the moment it ends.

use crate::gps_fit::{FitState, FitStatus};
use crate::{Engine, Result, lock, now_ms, records};
use model::CarId;
use serde::{Deserialize, Serialize};

const SEQ_KEY: &str = "gps-fit-seq";
/// The most layout changes kept per car.
const CHANGES_KEPT: usize = 200;

fn ids_key(car: CarId) -> String {
    format!("car/{car}/gps-fits")
}

fn run_key(car: CarId, id: u64) -> String {
    format!("car/{car}/gps-fit/{id}")
}

fn changes_key(car: CarId) -> String {
    format!("car/{car}/gps-layout-log")
}

/// One change of the layout a car's Screenmate GPS frames are read with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutChange {
    pub at_ms: u64,
    /// The account that made it (`None`: a command, or before names were kept).
    pub by: Option<String>,
    /// The layout it was set to as `sentry-mate gps-layout` prints it (`None`: the DBC's).
    pub layout: Option<String>,
    /// The layout before it (`None`: the DBC's).
    pub before: Option<String>,
    /// The run it was applied from, when one (an id as text in the API, like every id).
    #[serde(with = "id_text")]
    pub fit: Option<u64>,
}

mod id_text {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<u64>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(n) => s.serialize_str(&n.to_string()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Id {
            Text(String),
            Number(u64),
        }
        Ok(match Option::<Id>::deserialize(d)? {
            None => None,
            Some(Id::Number(n)) => Some(n),
            Some(Id::Text(t)) => t.parse().ok(),
        })
    }
}

/// What deleting a run did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deleted {
    Gone,
    /// It is still under way.
    Running,
    NotFound,
}

impl Engine {
    /// The next run's id.
    pub(crate) fn gps_fit_next_id(&self) -> Result<u64> {
        let id = records::get::<u64>(&self.inner.db, records::NS, SEQ_KEY)?.unwrap_or(0) + 1;
        records::put(&self.inner.db, records::NS, SEQ_KEY, &id)?;
        Ok(id)
    }

    fn gps_fit_ids(&self, car: CarId) -> Result<Vec<u64>> {
        Ok(records::get::<Vec<u64>>(&self.inner.db, records::NS, &ids_key(car))?.unwrap_or_default())
    }

    /// Keeps a run that ended.
    pub(crate) fn gps_fit_store(&self, s: &FitStatus) -> Result<()> {
        let _one = lock(&self.inner.gps_fits);
        records::put(&self.inner.db, records::NS, &run_key(s.car, s.id), s)?;
        let mut ids = self.gps_fit_ids(s.car)?;
        if !ids.contains(&s.id) {
            ids.push(s.id);
            ids.sort_unstable();
            records::put(&self.inner.db, records::NS, &ids_key(s.car), &ids)?;
        }
        Ok(())
    }

    fn gps_fit_stored(&self, car: CarId, id: u64) -> Option<FitStatus> {
        records::kept::<FitStatus>(&self.inner.db, records::NS, &run_key(car, id)).ok().flatten()
    }

    /// The car's newest run: the one in memory, or after a restart the newest kept.
    pub(crate) fn gps_fit_newest(&self, car: CarId) -> Option<FitStatus> {
        if let Some(s) = lock(&self.inner.gps_fits).jobs.get(&car).cloned() {
            return Some(s);
        }
        let id = self.gps_fit_ids(car).ok()?.into_iter().max()?;
        self.gps_fit_stored(car, id)
    }

    /// Every run of the car, newest first: the one under way, then the kept ones.
    pub fn gps_fit_runs(&self, car: CarId) -> Result<Vec<FitStatus>> {
        let mut out: Vec<FitStatus> = Vec::new();
        if let Some(s) = lock(&self.inner.gps_fits).jobs.get(&car).cloned()
            && s.state == FitState::Running
        {
            out.push(s);
        }
        let mut ids = self.gps_fit_ids(car)?;
        ids.sort_unstable_by(|a, b| b.cmp(a));
        out.extend(ids.into_iter().filter_map(|id| self.gps_fit_stored(car, id)));
        Ok(out)
    }

    /// A page of the car's runs, newest first.
    pub fn gps_fit_page(
        &self,
        car: CarId,
        paging: &crate::query::Paging,
    ) -> crate::query::QResult<crate::query::List<FitStatus>> {
        let rows = self.gps_fit_runs(car)?;
        crate::query::paginate(rows, |s| (s.id as f64, s.id.to_string()), true, "gps-fits", paging, true)
    }

    /// A page of the layouts the car was set to, newest first.
    pub fn gps_layout_page(
        &self,
        car: CarId,
        paging: &crate::query::Paging,
    ) -> crate::query::QResult<crate::query::List<LayoutChange>> {
        let rows: Vec<(u64, LayoutChange)> =
            self.gps_layout_changes(car)?.into_iter().rev().enumerate().map(|(i, c)| (i as u64, c)).collect();
        let page = crate::query::paginate(rows, |r| (r.0 as f64, r.0.to_string()), true, "gps-layouts", paging, true)?;
        Ok(crate::query::List {
            items: page.items.into_iter().map(|r| r.1).collect(),
            next: page.next,
            total: page.total,
        })
    }

    /// One run of the car (under way, or kept).
    pub fn gps_fit_run(&self, car: CarId, id: u64) -> Result<Option<FitStatus>> {
        if let Some(s) = lock(&self.inner.gps_fits).jobs.get(&car).filter(|s| s.id == id).cloned() {
            return Ok(Some(s));
        }
        Ok(self.gps_fit_stored(car, id))
    }

    /// Deletes a kept run (one under way cannot be).
    pub fn gps_fit_delete(&self, car: CarId, id: u64) -> Result<Deleted> {
        let mut fits = lock(&self.inner.gps_fits);
        if let Some(s) = fits.jobs.get(&car).filter(|s| s.id == id) {
            if s.state == FitState::Running {
                return Ok(Deleted::Running);
            }
            fits.jobs.remove(&car);
        }
        let mut ids = self.gps_fit_ids(car)?;
        let Some(at) = ids.iter().position(|i| *i == id) else { return Ok(Deleted::NotFound) };
        ids.remove(at);
        records::put(&self.inner.db, records::NS, &ids_key(car), &ids)?;
        self.inner.db.kv().delete(records::NS, &run_key(car, id))?;
        Ok(Deleted::Gone)
    }

    /// Notes that the layout of a run was applied (by `by`), in the run.
    pub fn gps_fit_applied(&self, car: CarId, id: u64, by: Option<String>) -> Result<()> {
        let at = now_ms();
        {
            let mut fits = lock(&self.inner.gps_fits);
            if let Some(s) = fits.jobs.get_mut(&car).filter(|s| s.id == id) {
                (s.applied_ms, s.applied_by) = (Some(at), by.clone());
            }
        }
        if let Some(mut s) = self.gps_fit_stored(car, id) {
            (s.applied_ms, s.applied_by) = (Some(at), by);
            self.gps_fit_store(&s)?;
        }
        Ok(())
    }

    /// The layouts the car was set to, newest first.
    pub fn gps_layout_changes(&self, car: CarId) -> Result<Vec<LayoutChange>> {
        let mut v =
            records::kept::<Vec<LayoutChange>>(&self.inner.db, records::NS, &changes_key(car))?.unwrap_or_default();
        v.reverse();
        Ok(v)
    }

    pub(crate) fn gps_layout_note(&self, car: CarId, change: LayoutChange) -> Result<()> {
        let mut v =
            records::kept::<Vec<LayoutChange>>(&self.inner.db, records::NS, &changes_key(car))?.unwrap_or_default();
        v.push(change);
        let extra = v.len().saturating_sub(CHANGES_KEPT);
        v.drain(..extra);
        records::put(&self.inner.db, records::NS, &changes_key(car), &v)
    }
}
