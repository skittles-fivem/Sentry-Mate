//! Import gap filling at read time ([`timeline::gapfill`]): where the live inputs (SMO, the Tesla API, TeslaFi's API)
//! all have nothing for a value at a time, the import shown for that time fills it, labelled as imported. Priority
//! SMO > the Tesla API > TeslaFi's API > the chosen import (TeslaMate's or TeslaFi's, one per span:
//! [`super::system::Sites`]).
//!
//! Only with `source=all` (asking for one source shows that source alone), and only while the car's setting (else
//! the server's, on by default) says so. Drives, charges and states still come from one source per span: a live row
//! gets only its null values from the import, never a second row, and never its distance, energy or cost, so totals
//! do not change. Filled values are named in the answers' `value_sources` (and the import in `sources`).

use super::system::Sites;
use super::{QResult, SourceFilter, SourceRef, Sources};
use crate::Engine;
use model::{CarId, Charge, Drive, ImportSite, Origin, Sample, Source, Stream};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

const DAY: u64 = 86_400_000;

/// Days of import samples kept read at once (a list walks its rows in time order).
const KEPT_DAYS: usize = 6;

/// A site's day of samples, kept read.
type Kept = ((ImportSite, u64), Rc<Vec<Sample>>);

/// Which values of an answer came from an import: field → the imports that gave it (`value_sources`).
pub type ValueSources = BTreeMap<String, Vec<SourceRef>>;

/// A car's import gap filling for one answer.
pub(crate) struct Gaps<'e> {
    e: &'e Engine,
    car: CarId,
    /// Values are filled (else the imports are only read, for a series' seconds where live history has none).
    on: bool,
    sites: Sites,
    /// Each import site's sample days (UTC day starts).
    days: Vec<(ImportSite, BTreeSet<u64>)>,
    kept: RefCell<Vec<Kept>>,
}

impl<'e> Gaps<'e> {
    /// Filling for an answer with `filter` (`source=`): on only for `source=all`, while the car's setting says so,
    /// for a car with imported samples.
    pub(crate) fn new(e: &'e Engine, car: CarId, filter: Option<SourceFilter>) -> QResult<Gaps<'e>> {
        let mut g = Gaps { e, car, on: false, sites: Sites::default(), days: Vec::new(), kept: RefCell::default() };
        if filter.is_some() {
            return Ok(g);
        }
        for site in ImportSite::ALL {
            let d: BTreeSet<u64> =
                e.db().days::<Sample>(car, Stream::of_site(site))?.into_iter().map(|d| d.start_ms).collect();
            if !d.is_empty() {
                g.days.push((site, d));
            }
        }
        if g.days.is_empty() {
            return Ok(g);
        }
        g.sites = Sites::new(e, car)?;
        g.on = super::fleet::gap_fill_on(e, car)?;
        Ok(g)
    }

    /// One site's samples of a UTC day (read once, a few days kept).
    fn day(&self, site: ImportSite, day: u64) -> QResult<Rc<Vec<Sample>>> {
        if let Some((_, v)) = self.kept.borrow().iter().find(|(k, _)| *k == (site, day)) {
            return Ok(v.clone());
        }
        let pts = self.e.db().read_points::<Sample>(self.car, Stream::of_site(site), day, day + DAY)?;
        let v = Rc::new(pts);
        let mut kept = self.kept.borrow_mut();
        if kept.len() >= KEPT_DAYS {
            kept.remove(0);
        }
        kept.push(((site, day), v.clone()));
        Ok(v)
    }

    /// Each import site's samples in `[a, b)` that are shown (where both imports cover a span, the chosen one's
    /// only), TeslaMate's first; sites with none are left out.
    pub(crate) fn import(&self, a: u64, b: u64) -> QResult<Vec<(ImportSite, Vec<Sample>)>> {
        let mut out = Vec::new();
        if a >= b {
            return Ok(out);
        }
        for (site, days) in &self.days {
            let mut pts = Vec::new();
            for &d in days.range(a / DAY * DAY..b) {
                pts.extend(
                    self.day(*site, d)?
                        .iter()
                        .filter(|p| p.t_ms >= a && p.t_ms < b && self.sites.shows(*site, p.t_ms))
                        .cloned(),
                );
            }
            if !pts.is_empty() {
                out.push((*site, pts));
            }
        }
        Ok(out)
    }

    /// Fills a live drive's null values (its row is otherwise as stored): `(field, site)` of each value filled,
    /// the API's field names.
    pub(crate) fn drive(&self, d: &mut Drive) -> QResult<Filled> {
        if !self.on || d.source != Source::Live || !drive_lacks(d) {
            return Ok(Filled::default());
        }
        let p = &self.e.inner.opts.fuse;
        let m = p.margin_ms();
        let mut out = Filled::default();
        for (site, pts) in self.import(d.start_ms.saturating_sub(m), d.end_ms.saturating_add(m + 1))? {
            for f in timeline::gapfill::fill_drive(d, &pts, p) {
                out.0.push((drive_word(f), site));
            }
        }
        Ok(out)
    }

    /// Fills a live charge's null values, as [`Self::drive`].
    pub(crate) fn charge(&self, c: &mut Charge) -> QResult<Filled> {
        if !self.on || c.source != Source::Live || !charge_lacks(c) {
            return Ok(Filled::default());
        }
        let p = &self.e.inner.opts.fuse;
        let m = p.margin_ms();
        let mut out = Filled::default();
        for (site, pts) in self.import(c.start_ms.saturating_sub(m), c.end_ms.saturating_add(m + 1))? {
            for f in timeline::gapfill::fill_charge(c, &pts, p) {
                out.0.push((charge_word(f), site));
                if f == "position" && c.position.as_ref().is_some_and(|x| x.odometer_km.is_some()) {
                    out.0.push(("odometer_km", site));
                }
            }
        }
        Ok(out)
    }

    /// Live samples of `[a, b)` (fused, in order) with the values the import fills while filling is on, and the
    /// import's own seconds outside their coverage when `own` asks for them (`own` is taken only while filling is
    /// on unless [`Own::Always`]: a series shows the import where there is no live history whatever the switch says).
    /// Where a live value still holds (its hold time, as the Tesla API's fills SMO's), an import second never shows
    /// another one for that field; with filling off it is left out there altogether.
    pub(crate) fn samples(&self, live: Vec<Sample>, a: u64, b: u64, cover_ms: u64, own: Own) -> QResult<Given> {
        let (own, values) = match own {
            Own::Always => (true, self.on),
            Own::WhenOn => (self.on, self.on),
        };
        if !own && !values {
            return Ok(Given { samples: live, masks: Vec::new() });
        }
        let p = &self.e.inner.opts.fuse;
        let m = p.margin_ms();
        let mut cur = live;
        let mut masks: Vec<(u64, ImportSite, u64)> = Vec::new();
        for (site, pts) in self.import(a.saturating_sub(m), b.saturating_add(m))? {
            let how = timeline::gapfill::How { cover_ms, own, values };
            let f = timeline::gapfill::fill(&cur, &pts, p, how);
            masks.extend(f.filled.iter().map(|(t, mk)| (*t, site, *mk)));
            masks.extend(f.own.iter().map(|t| (*t, site, u64::MAX)));
            cur = f.samples;
        }
        cur.retain(|s| s.t_ms >= a && s.t_ms < b);
        masks.sort_by_key(|x| x.0);
        Ok(Given { samples: cur, masks })
    }
}

/// When the import's own seconds join live samples ([`Gaps::samples`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Own {
    /// Whatever the switch says (a series: the import is shown where there is no live history).
    Always,
    /// Only while filling is on (a drive's track gap).
    WhenOn,
}

/// The values a row got from an import: `(field, site)`.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Filled(pub Vec<(&'static str, ImportSite)>);

impl Filled {
    /// `value_sources` of the filled fields.
    pub(crate) fn value_sources(&self) -> ValueSources {
        let mut out = ValueSources::new();
        for (f, site) in &self.0 {
            let r = SourceRef::from(Origin::Import(*site));
            let v = out.entry((*f).to_string()).or_default();
            if !v.contains(&r) {
                v.push(r);
            }
        }
        out
    }

    /// The row's sources with the imports that filled it added (its old `source` word kept: the row is still its
    /// own source's).
    pub(crate) fn sources(&self, from: &Sources) -> Sources {
        let mut origins: Vec<Origin> = from.sources.iter().filter_map(origin_of).collect();
        for (_, site) in &self.0 {
            let o = Origin::Import(*site);
            if !origins.contains(&o) {
                origins.push(o);
            }
        }
        Sources { source: from.source, sources: super::source_refs(&origins), source_label: Origin::label_of(&origins) }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The origin a source ref names.
fn origin_of(r: &SourceRef) -> Option<Origin> {
    match r.kind {
        "import" => r.site.and_then(ImportSite::from_word).map(Origin::Import),
        // a live input's own word (`smo`, `tesla_api`, `teslafi_api`)
        k => model::Input::ALL.into_iter().map(Origin::from).find(|o| o.kind() == k),
    }
}

/// Filled samples and, by time, the values each import gave: `(t_ms, site, mask)` (mask bits of
/// [`timeline::gapfill::slot_names`]; all bits for one of the import's own seconds).
#[derive(Debug, Clone, Default)]
pub(crate) struct Given {
    pub samples: Vec<Sample>,
    pub masks: Vec<(u64, ImportSite, u64)>,
}

impl Given {
    /// The sites (and their masks) that gave values to the second at `t`.
    pub(crate) fn at(&self, t: u64) -> &[(u64, ImportSite, u64)] {
        let i = self.masks.partition_point(|m| m.0 < t);
        let j = self.masks.partition_point(|m| m.0 <= t);
        &self.masks[i..j]
    }
}

fn drive_lacks(d: &Drive) -> bool {
    // (never its end: a live drive without one is one whose data stopped, [`timeline::gapfill::fill_drive`])
    d.start.is_none()
        || d.start_odometer_km.is_none()
        || d.end_odometer_km.is_none()
        || d.start_soc.is_none()
        || d.end_soc.is_none()
        || d.start_range_km.is_none()
        || d.end_range_km.is_none()
        || d.outside_temp_avg_c.is_none()
        || d.inside_temp_avg_c.is_none()
        || d.ascent_m.is_none() && d.descent_m.is_none()
}

fn charge_lacks(c: &Charge) -> bool {
    c.position.is_none()
        || c.start_soc.is_none()
        || c.end_soc.is_none()
        || c.start_range_km.is_none()
        || c.end_range_km.is_none()
        || c.outside_temp_avg_c.is_none()
        || c.battery_temp_avg_c.is_none()
}

/// A drive field's name in the API.
fn drive_word(f: &'static str) -> &'static str {
    match f {
        "start_soc" => "start_soc_pct",
        "end_soc" => "end_soc_pct",
        "outside_temp_avg_c" => "outside_avg_c",
        "inside_temp_avg_c" => "cabin_avg_c",
        "ascent_m" => "climb_m",
        other => other,
    }
}

/// A charge field's name in the API.
fn charge_word(f: &'static str) -> &'static str {
    match f {
        "start_soc" => "start_soc_pct",
        "end_soc" => "end_soc_pct",
        "outside_temp_avg_c" => "outside_avg_c",
        "battery_temp_avg_c" => "battery_avg_c",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn filled_rows_keep_their_word_and_name_the_import() {
        let row = Sources::of(Source::Live, model::Inputs::SMO);
        let f = Filled(vec![("outside_avg_c", ImportSite::Teslamate), ("climb_m", ImportSite::Teslamate)]);
        let s = f.sources(&row);
        assert_eq!(s.source, "can");
        assert_eq!(s.source_label, "Data from SMO; imported from TeslaMate");
        let v = f.value_sources();
        assert_eq!(v.len(), 2);
        assert_eq!(v["outside_avg_c"][0].label, "Imported from TeslaMate");
        assert!(Filled::default().sources(&row) == row && Filled::default().is_empty());
    }
}
