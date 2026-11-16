//! The chain across a car key's signed logs: `index` counts up by 1 per log and each log names the one before it
//! (`prev_name`, `prev_sha256`). Same rules as Sentry Six Manager's `canTrust.js` `chain()`.

use crate::{GENESIS_SHA, Link, LogName};
use std::collections::BTreeMap;

/// One log of the paired key, as the chain sees it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub link: Link,
    /// `None`: verified. `Some(reason)`: signed by the paired key but failed its check (still a link).
    pub failed: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ChainIssue {
    Gap { from: u64, to: u64, text: String },
    Duplicate { index: u64, names: [String; 2], text: String },
    Failed { index: u64, name: String, text: String },
    Mismatch { index: u64, name: String, text: String },
    Reordered { index: u64, name: String, text: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Restart {
    pub index: u64,
    pub name: String,
    pub note: Option<String>,
    /// The last log of the segment before (index, name).
    pub after: Option<(u64, String)>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Segment {
    pub first: u64,
    pub last: u64,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct ChainReport {
    pub count: u64,
    pub first: Option<u64>,
    pub last: Option<u64>,
    pub issues: Vec<ChainIssue>,
    pub restarts: Vec<Restart>,
    pub segments: Vec<Segment>,
}

fn at(name: &str) -> Option<i64> {
    LogName::parse(name).map(|n| n.started_ms)
}

fn note_of(l: &Link) -> String {
    l.chain_note.as_ref().map(|n| format!(" (the bridge says: {n})")).unwrap_or_default()
}

/// The chain of one car key's signed logs by its paired key (verified ones and ones that failed their check).
pub fn chain(entries: &[Entry]) -> ChainReport {
    // where the bridge began again: a verified log at 0 naming no previous one, after the key's first such log;
    // the first one only when two or more logs are older (a chain received from its middle)
    let mut geneses: Vec<i64> = entries
        .iter()
        .filter(|e| {
            e.failed.is_none() && e.link.index == 0 && e.link.prev_name == "-" && e.link.prev_sha256 == GENESIS_SHA
        })
        .filter_map(|e| at(&e.link.name))
        .collect();
    geneses.sort_unstable();
    let older = |a: i64| entries.iter().filter(|e| at(&e.link.name).is_some_and(|t| t < a)).count();
    let mut bounds: Vec<i64> = Vec::new();
    for (k, &a) in geneses.iter().enumerate() {
        if ((k > 0 && a > geneses[0]) || older(a) >= 2) && !bounds.contains(&a) {
            bounds.push(a);
        }
    }
    let seg_of = |name: &str| at(name).map_or(0, |a| bounds.iter().take_while(|&&b| b <= a).count());
    let mut parts: Vec<Vec<&Entry>> = vec![Vec::new(); bounds.len() + 1];
    for e in entries {
        parts[seg_of(&e.link.name)].push(e);
    }

    let mut report = ChainReport::default();
    let mut last_names: Vec<String> = Vec::new();
    for (p, part) in parts.iter().enumerate() {
        let mut by_index: BTreeMap<u64, &Entry> = BTreeMap::new();
        for e in part {
            match by_index.get(&e.link.index) {
                Some(other) if other.link.name != e.link.name => {
                    let note = if note_of(&e.link).is_empty() { note_of(&other.link) } else { note_of(&e.link) };
                    report.issues.push(ChainIssue::Duplicate {
                        index: e.link.index,
                        names: [other.link.name.clone(), e.link.name.clone()],
                        text: format!("two logs with index {}{note}", e.link.index),
                    });
                }
                Some(_) => {}
                None => {
                    by_index.insert(e.link.index, e);
                }
            }
        }
        let idx: Vec<u64> = by_index.keys().copied().collect();
        if p > 0 {
            if let Some(s0) = idx.first().and_then(|i| by_index.get(i)) {
                let before = report.segments.last().map(|s| (s.last, last_names.last().cloned().unwrap_or_default()));
                let said = s0
                    .link
                    .chain_note
                    .as_ref()
                    .map_or(" (the bridge gave no note)".to_string(), |n| format!(" (the bridge says: {n})"));
                let tail = before.as_ref().map_or(String::new(), |(i, n)| {
                    format!(": logs signed between {n} (#{i}) and it cannot be checked for gaps")
                });
                report.restarts.push(Restart {
                    index: s0.link.index,
                    name: s0.link.name.clone(),
                    note: s0.link.chain_note.clone(),
                    after: before,
                    text: format!("the chain started again at {} with {}{said}{tail}", s0.link.index, s0.link.name),
                });
            }
        }
        if idx.is_empty() {
            continue;
        }
        for (k, &i) in idx.iter().enumerate() {
            let cur = by_index[&i];
            if k > 0 && i != idx[k - 1] + 1 {
                let (from, to) = (idx[k - 1] + 1, i - 1);
                let text = if from == to {
                    format!("log {from} is missing")
                } else {
                    format!("logs {from} to {to} are missing")
                };
                report.issues.push(ChainIssue::Gap { from, to, text });
            }
            if let Some(reason) = &cur.failed {
                report.issues.push(ChainIssue::Failed {
                    index: i,
                    name: cur.link.name.clone(),
                    text: format!("log {i} ({}) failed its check: {reason}", cur.link.name),
                });
            }
            let prev = i.checked_sub(1).and_then(|j| by_index.get(&j));
            if let Some(prev) = prev {
                // a failed log is already reported: the one after it is compared with what was signed, not with it
                if prev.failed.is_none()
                    && (cur.link.prev_name != prev.link.name || cur.link.prev_sha256 != prev.link.sha256)
                {
                    report.issues.push(ChainIssue::Mismatch {
                        index: i,
                        name: cur.link.name.clone(),
                        text: format!(
                            "log {i} ({}) does not follow the stored log {} ({}): swapped or changed{}",
                            cur.link.name,
                            i - 1,
                            prev.link.name,
                            note_of(&cur.link)
                        ),
                    });
                }
                if let (Some(a), Some(b)) = (at(&prev.link.name), at(&cur.link.name)) {
                    if b < a {
                        report.issues.push(ChainIssue::Reordered {
                            index: i,
                            name: cur.link.name.clone(),
                            text: format!("log {i} starts before log {}", i - 1),
                        });
                    }
                }
            }
        }
        let (first, last) = (idx[0], idx[idx.len() - 1]);
        report.segments.push(Segment { first, last, count: idx.len() as u64 });
        last_names.push(by_index[&last].link.name.clone());
    }
    report.count = report.segments.iter().map(|s| s.count).sum();
    report.first = report.segments.first().map(|s| s.first);
    report.last = report.segments.last().map(|s| s.last);
    report
}
