//! The owner's word on unsigned logs of a paired car: used anyway ([`Judge::use_unsigned`]), or no longer.
//!
//! Only a log that failed its check for having no signature can be used this way ([`Record::unsigned_only`]: not
//! signed although the car is paired, or the bridge could not sign it). A log with a bad signature, another key's
//! signature, a file that is not the one signed, bad chain fields or a wrong name never is: those are what pairing is
//! there to refuse. The choice is kept in the log's record (`override_use`, with the key's head), so judging the log
//! again keeps it while it still fails only for being unsigned; each changed record is told to the engine like any
//! other judging ([`crate::OnJudged`]), which decodes the log (or leaves it out again) and rebuilds the window it is in.
//!
//! A log's time (from its name) is chosen by whoever holds the car's upload token, so being older than the key's
//! first signed log is never a reason to use a log by itself: [`Pick::All`] can be narrowed to those logs, for an
//! owner who knows their CAN Bridge app signed nothing before then (docs/security.md).

use crate::judge::{Fold, Judge, Record, blocking};
use crate::ledger::{self, Change};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;
use trust::LogName;

/// Which logs of a car key [`Judge::use_unsigned`] changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// These logs (names of the car key's logs); any that can't be changed is named in [`OwnerUse::refused`] and
    /// nothing changes.
    Names(Vec<String>),
    /// Every log that can be (others are passed over): `before_first_signed`, only those whose time (from their
    /// name) is before the car key's first signed log.
    All { before_first_signed: bool },
}

/// What [`Judge::use_unsigned`] did, or would do.
#[derive(Debug, Clone, Default)]
pub struct OwnerUse {
    /// The logs changed, in name order, with their records as they are now (or would be).
    pub changed: Vec<(String, Record)>,
    /// The names asked for that can't be changed, in name order, and why: nothing changed.
    pub refused: Vec<(String, String)>,
}

/// A car key's unsigned logs as the owner's choice sees them ([`Judge::unsigned_summary`]).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct UnsignedSummary {
    /// The time (from its name, UTC ms) of the key's first signed log, verified by the key it is paired with.
    pub first_signed_ms: Option<i64>,
    /// Logs that failed only for being unsigned and are not used: what the owner can choose to use.
    pub can_use: u64,
    /// Of those, the ones from before the first signed log.
    pub can_use_before_first_signed: u64,
    /// Of those, how many of each kind ([`Record::unsigned_kind`]).
    pub kinds: BTreeMap<&'static str, u64>,
    /// Logs used on the owner's word.
    pub in_use: u64,
    /// Logs that failed for anything else: never used.
    pub never: u64,
    /// The newest of the logs the owner can use or uses (at most the number asked for), newest first.
    #[serde(skip)]
    pub logs: Vec<(String, Record)>,
    /// How many there are of those.
    pub logs_total: u64,
}

/// Why a log can't be used on the owner's word.
fn why_not(r: &Record) -> String {
    use crate::judge::Status;
    match r.status {
        Status::Verified => "verified: its data is used already".into(),
        Status::Unsigned => "unsigned from a car that was not paired then: its data is used already".into(),
        Status::Waiting => "not checked yet: its sidecar has not come".into(),
        Status::Failed | Status::CouldNotSign => r.reason.clone().unwrap_or_else(|| "failed its check".into()),
    }
}

fn start_of(name: &str) -> Option<i64> {
    LogName::parse(name).map(|n| n.started_ms)
}

/// The time of the first log in name order verified with a signature (a key's names are in time order).
fn first_signed(v: &crate::judge::View) -> std::io::Result<Option<i64>> {
    let mut first = None;
    v.for_each(|name, r| {
        if r.status == crate::judge::Status::Verified && r.link.is_some() {
            first = start_of(name);
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    })?;
    Ok(first)
}

impl Judge {
    /// Sets (`on`) or clears the owner's "use it anyway" on logs of a car key that failed only for being unsigned
    /// (see the module). `apply: false` only says what would change. With [`Pick::Names`], a name that can't be used
    /// (signed badly, by another key, changed, not failed at all, or no log of the key) is refused and nothing
    /// changes; a log already as asked is passed over. Each record changed is told to the engine once written.
    pub async fn use_unsigned(&self, key: &str, pick: &Pick, on: bool, apply: bool) -> std::io::Result<OwnerUse> {
        if !ledger::valid_key(key) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a CAN Bridge key"));
        }
        let _held = self.hold(key).await?;
        let head = self.take_head(key).await?;
        let (ctx, key_s, pick) = (self.ctx(), key.to_string(), pick.clone());
        let (head, out) = blocking(move || {
            let mut head = head;
            let out = ctx.view(&key_s, &head, |v| {
                let first = match &pick {
                    Pick::All { before_first_signed: true } => first_signed(v)?,
                    _ => None,
                };
                let mut wanted: BTreeSet<String> = match &pick {
                    Pick::Names(n) => n.iter().cloned().collect(),
                    Pick::All { .. } => BTreeSet::new(),
                };
                let named = matches!(pick, Pick::Names(_));
                let mut out = OwnerUse::default();
                v.for_each(|name, r| {
                    if named && !wanted.remove(name) {
                        return ControlFlow::Continue(());
                    }
                    if r.override_use == on {
                        // as asked already (an unused log asked to stop: nothing to stop)
                        return ControlFlow::Continue(());
                    }
                    if on && !r.unsigned_only() {
                        if named {
                            out.refused.push((name.to_string(), why_not(r)));
                        }
                        return ControlFlow::Continue(());
                    }
                    if let Pick::All { before_first_signed: true } = pick
                        && !first.is_some_and(|f| start_of(name).is_some_and(|s| s < f))
                    {
                        return ControlFlow::Continue(());
                    }
                    let mut now = r.clone();
                    now.override_use = on;
                    out.changed.push((name.to_string(), now));
                    ControlFlow::Continue(())
                })?;
                for name in wanted {
                    out.refused.push((name, "no such log of this car key".into()));
                }
                out.refused.sort();
                Ok(out)
            })?;
            if !out.refused.is_empty() {
                return Ok((Some(head), OwnerUse { changed: Vec::new(), refused: out.refused }));
            }
            if !apply || out.changed.is_empty() {
                return Ok((Some(head), out));
            }
            // one journal record per log, as an upload's: each change stands on its own
            let groups: Vec<Vec<Change>> = out
                .changed
                .iter()
                .map(|(name, record)| vec![Change::Log { name: name.clone(), record: record.clone() }])
                .collect();
            for c in groups.iter().flatten() {
                head.apply(c);
            }
            let head = ctx.commit(&key_s, head, false, groups, Fold::WhenLong)?;
            Ok((Some(head), out))
        })
        .await?;
        if let Some(h) = head {
            self.put_head(key, h);
        }
        Ok(out)
    }

    /// The time (from its name, UTC ms) of a car key's first log verified with a signature; `None` before one.
    pub async fn first_signed_ms(&self, key: &str) -> std::io::Result<Option<i64>> {
        self.read(key, |v, _| first_signed(v)).await
    }

    /// A car key's logs as the owner's choice sees them: those that failed only for being unsigned, by kind, those
    /// used on the owner's word, and the newest `max` of either (one pass over the key's records, after the one that
    /// finds its first signed log).
    pub async fn unsigned_summary(&self, key: &str, max: usize) -> std::io::Result<UnsignedSummary> {
        self.read(key, move |v, _| {
            let first = first_signed(v)?;
            let mut s = UnsignedSummary { first_signed_ms: first, ..UnsignedSummary::default() };
            let mut newest = std::collections::VecDeque::new();
            v.for_each(|name, r| {
                if r.override_use {
                    s.in_use += 1;
                } else if r.unsigned_only() {
                    s.can_use += 1;
                    if first.is_some_and(|f| start_of(name).is_some_and(|t| t < f)) {
                        s.can_use_before_first_signed += 1;
                    }
                    *s.kinds.entry(r.unsigned_kind().unwrap_or("unknown")).or_default() += 1;
                } else {
                    if r.status == crate::judge::Status::Failed {
                        s.never += 1;
                    }
                    return ControlFlow::Continue(());
                }
                s.logs_total += 1;
                if max > 0 {
                    if newest.len() == max {
                        newest.pop_front();
                    }
                    newest.push_back((name.to_string(), r.clone()));
                }
                ControlFlow::Continue(())
            })?;
            s.logs = newest.into_iter().rev().collect();
            Ok(s)
        })
        .await
    }
}
