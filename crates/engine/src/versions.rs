//! Why the server works its history out again, said so an owner can read it: the **changes of the version numbers**
//! that make stored derived data out of date ([`BUMPS`]), and the **causes** a rebuild carries ([`Cause`]): a server
//! update and what it changed ("positions are worked out differently"), a setting saved, an admin's ask.
//!
//! A version is bumped only when what is stored would really come out otherwise. Each bump names the test that shows
//! the output changing ([`Bump::proof`]); a test here fails when a version is bumped without a [`Bump`] that names
//! such a test, and when the test named is not there (`versions_bumped_have_a_proof`). The changes are explained in
//! `docs/rebuilds.md`, one heading per bump ([`Bump::anchor`]).

use serde::{Deserialize, Serialize};

/// Which version a bump is of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// How stored CAN logs are decoded ([`crate::DECODE_VERSION`]): a full rebuild.
    Decode,
    /// How drives, charges, states and visits are worked out ([`timeline::RULES_VERSION`]): a full rebuild.
    Rules,
    /// How a CAN log's check is judged ([`trust::RULES_VERSION`]): the logs that failed are checked again once.
    Trust,
}

impl Part {
    pub fn word(self) -> &'static str {
        match self {
            Part::Decode => "decode",
            Part::Rules => "rules",
            Part::Trust => "trust",
        }
    }

    /// The version this server has.
    pub fn now(self) -> u32 {
        match self {
            Part::Decode => crate::records::DECODE_VERSION,
            Part::Rules => timeline::RULES_VERSION,
            Part::Trust => trust::RULES_VERSION,
        }
    }
}

/// One change of a version number.
#[derive(Debug, Clone, Copy)]
pub struct Bump {
    pub part: Part,
    /// The version it went to.
    pub version: u32,
    /// What it changes, as the owner reads it after "Server updated: " (lower case first).
    pub what: &'static str,
    /// The test that shows the stored output changing, and its file (from the workspace's root).
    pub proof: &'static str,
    pub proof_file: &'static str,
}

impl Bump {
    /// Its heading in `docs/rebuilds.md` (`decode-3`, `rules-2`).
    pub fn anchor(&self) -> String {
        format!("{}-{}", self.part.word(), self.version)
    }
}

/// Every bump since the first version of each part (1), oldest first per part.
pub const BUMPS: &[Bump] = &[
    Bump {
        part: Part::Decode,
        version: 2,
        what: "a log that failed its check leaves a hole of a few minutes only, whatever its sidecar claims",
        proof: "failed_logs_are_unknown_for_their_own_minutes_whatever_their_sidecar_claims",
        proof_file: "crates/engine/tests/pipeline.rs",
    },
    Bump {
        part: Part::Decode,
        version: 3,
        what: "software updates are found from each log's firmware fingerprints",
        proof: "a_flapping_install_is_one_update_and_stored_ones_regroup_with_their_name",
        proof_file: "crates/engine/tests/firmware_installs.rs",
    },
    Bump {
        part: Part::Rules,
        version: 2,
        what: "positions are worked out differently: fixes that cannot be real are left out",
        proof: "bad_fixes_at_the_start_or_end_and_placeholders_are_left_out",
        proof_file: "crates/timeline/src/calc.rs",
    },
    Bump {
        part: Part::Rules,
        version: 3,
        what: "charging efficiency is worked out differently: the energies are compared over one span",
        proof: "a_charge_whose_energies_cover_different_spans_shows_no_efficiency_and_says_why",
        proof_file: "crates/engine/tests/tesla_input.rs",
    },
    Bump {
        part: Part::Rules,
        version: 4,
        what: "energy and distance are summed differently: every phase counts, and readings far apart or late leave nothing out",
        proof: "grid_energy_counts_every_phase",
        proof_file: "crates/timeline/tests/energy.rs",
    },
    Bump {
        part: Part::Rules,
        version: 5,
        what: "drives and charges are worked out differently: a drive an API read runs up to where it parked, a bad last odometer reading or a stop between sparse fixes sets no distance, and a few seconds of charging are no charge",
        proof: "api_only_drive_keeps_its_last_leg",
        proof_file: "crates/timeline/tests/drive_and_charge_edges.rs",
    },
    Bump {
        part: Part::Trust,
        version: 2,
        what: "a break in a log chain fails only the log that breaks it",
        proof: "a_chain_with_one_bad_log_fails_that_log_alone",
        proof_file: "crates/ingest/tests/checks.rs",
    },
    Bump {
        part: Part::Trust,
        version: 3,
        what: "an unsigned CAN log says why it is unsigned, so the owner can choose to use it",
        proof: "unsigned_logs_judged_by_older_rules_learn_why_they_are_unsigned",
        proof_file: "crates/ingest/tests/unsigned_use.rs",
    },
];

/// Where the changes are explained (from the repository's root).
pub const CHANGES_DOC: &str = "docs/rebuilds.md";

/// Why a rebuild (or a check again) runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cause {
    /// `server_update`, `owner`, `gps_layout`, `source_order`, `preferred_key`, `import_moved`, `key_moved`, ...
    pub code: String,
    /// In words ("Server updated: positions are worked out differently ...").
    pub text: String,
    /// The change explained (`docs/rebuilds.md#rules-2`), when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl Cause {
    pub fn new(code: &str, text: impl Into<String>) -> Cause {
        Cause { code: code.to_string(), text: text.into(), link: None }
    }

    /// An admin asked for it.
    pub fn owner(by: Option<&str>) -> Cause {
        match by {
            Some(b) => Cause::new("owner", format!("Asked for by {b}")),
            None => Cause::new("owner", "Asked for by an admin"),
        }
    }

    /// The car's own layout of the Screenmate's GPS frame changed.
    pub fn gps_layout() -> Cause {
        Cause::new("gps_layout", "The car's GPS layout changed: its positions are read again")
    }

    /// The order the car's live inputs win in changed.
    pub fn source_order() -> Cause {
        Cause::new("source_order", "The car's source order changed: its inputs are joined again")
    }

    /// The car's preferred CAN key changed.
    pub fn preferred_key() -> Cause {
        Cause::new("preferred_key", "The car's preferred CAN key changed: each second's source follows it")
    }

    /// Imported rows moved to or from the car.
    pub fn import_moved() -> Cause {
        Cause::new("import_moved", "Imported rows moved to or from the car")
    }

    fn bump(b: &Bump) -> Cause {
        Cause {
            code: "server_update".into(),
            text: format!("Server updated: {}", b.what),
            link: Some(format!("{CHANGES_DOC}#{}", b.anchor())),
        }
    }

    /// Adds `c` unless one with the same code and words is there.
    pub fn add(list: &mut Vec<Cause>, c: Cause) {
        if !list.contains(&c) {
            list.push(c);
        }
    }
}

/// The parts of a derived version (`decode-3.rules-3.dbc-614c9494`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Made {
    pub decode: Option<u32>,
    pub rules: Option<u32>,
    pub dbc: Option<String>,
}

pub fn parse(v: &str) -> Made {
    let mut m = Made::default();
    for p in v.split('.') {
        if let Some(n) = p.strip_prefix("decode-") {
            m.decode = n.parse().ok();
        } else if let Some(n) = p.strip_prefix("rules-") {
            m.rules = n.parse().ok();
        } else if let Some(h) = p.strip_prefix("dbc-") {
            m.dbc = Some(h.to_string());
        }
    }
    m
}

/// Why a history made with `old` (`None`: before versions were kept) is made again by a server with `new`: one
/// cause per bump between them, and one when only the CAN message definitions differ.
pub fn causes_between(old: Option<&str>, new: &str) -> Vec<Cause> {
    let n = parse(new);
    let Some(old) = old else {
        return vec![Cause::new(
            "server_update",
            "Server updated: this history was made before the server noted how it decodes; it is made again once",
        )];
    };
    let o = parse(old);
    let mut out = Vec::new();
    for (part, from, to) in [(Part::Decode, o.decode, n.decode), (Part::Rules, o.rules, n.rules)] {
        let (from, to) = (from.unwrap_or(0), to.unwrap_or(0));
        for b in BUMPS.iter().filter(|b| b.part == part && b.version > from && b.version <= to) {
            Cause::add(&mut out, Cause::bump(b));
        }
        if to < from {
            Cause::add(
                &mut out,
                Cause::new("server_update", "Server changed: an older version of the server runs over this history"),
            );
        }
    }
    if o.dbc != n.dbc {
        Cause::add(&mut out, Cause::new("server_update", "Server updated: the CAN message definitions changed"));
    }
    if out.is_empty() && old != new {
        out.push(Cause::new("server_update", "Server updated: the logs are decoded differently"));
    }
    out
}

/// The cause of a check again of failed CAN logs by newer trust rules.
pub fn trust_cause(from: u32) -> Vec<Cause> {
    BUMPS
        .iter()
        .filter(|b| b.part == Part::Trust && b.version > from && b.version <= Part::Trust.now())
        .map(Cause::bump)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// The check that a version is bumped only with a test showing the stored output change: each part's versions
    /// from 2 up to this server's have exactly one [`Bump`], none beyond, and the test each names is there.
    #[test]
    fn versions_bumped_have_a_proof() {
        for part in [Part::Decode, Part::Rules, Part::Trust] {
            for v in 2..=part.now() {
                let n = BUMPS.iter().filter(|b| b.part == part && b.version == v).count();
                assert_eq!(
                    n,
                    1,
                    "{} version {v}: add one Bump to engine::versions::BUMPS naming the test that shows the stored \
                     output change (and a heading `{}-{v}` in docs/rebuilds.md)",
                    part.word(),
                    part.word()
                );
            }
            assert!(
                BUMPS.iter().filter(|b| b.part == part).all(|b| b.version >= 2 && b.version <= part.now()),
                "{}: a Bump for a version this server does not have",
                part.word()
            );
        }
        let doc = std::fs::read_to_string(root().join(CHANGES_DOC)).expect("docs/rebuilds.md");
        for b in BUMPS {
            let src = std::fs::read_to_string(root().join(b.proof_file)).unwrap_or_default();
            assert!(
                src.contains(&format!("fn {}(", b.proof)),
                "{}: the proof `{}` is not in {}",
                b.anchor(),
                b.proof,
                b.proof_file
            );
            assert!(
                doc.contains(&format!("<a id=\"{}\"></a>", b.anchor())),
                "docs/rebuilds.md: no heading {}",
                b.anchor()
            );
            assert!(b.what.chars().next().is_some_and(char::is_lowercase), "{}: lower case first", b.anchor());
        }
    }

    #[test]
    fn the_causes_between_two_versions_name_each_change() {
        let c = causes_between(Some("decode-3.rules-1.dbc-aa"), "decode-3.rules-3.dbc-aa");
        assert_eq!(c.len(), 2, "{c:?}");
        assert!(c[0].text.starts_with("Server updated: positions are worked out differently"), "{c:?}");
        assert_eq!(c[0].link.as_deref(), Some("docs/rebuilds.md#rules-2"));
        assert_eq!(c[1].link.as_deref(), Some("docs/rebuilds.md#rules-3"));
        let c = causes_between(Some("decode-2.rules-3.dbc-aa"), "decode-3.rules-3.dbc-bb");
        assert_eq!(c.len(), 2, "{c:?}");
        assert!(c[1].text.contains("CAN message definitions"), "{c:?}");
        assert!(causes_between(Some("decode-3.rules-3.dbc-aa"), "decode-3.rules-3.dbc-aa").is_empty());
        assert_eq!(causes_between(None, "decode-3.rules-3.dbc-aa").len(), 1);
        assert_eq!(trust_cause(1).len(), 2);
        assert_eq!(trust_cause(2).len(), 1);
        assert!(trust_cause(trust::RULES_VERSION).is_empty());
    }
}
