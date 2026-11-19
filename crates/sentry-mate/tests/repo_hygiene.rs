#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! Checks on the repository itself, not the program:
//! - every GitHub Action the image workflow runs is pinned to a full commit id, and the Rust base image in the
//!   Dockerfile to a digest: the build and publish jobs hold a token that can push `latest`, so a tag or branch
//!   moved upstream must not change what runs there;
//! - Dependabot proposes the bumps, so the pins don't go stale;
//! - this public repo carries none of the markers that once came with the owner's car data or a local session path.

use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// What is wrong with one `uses:` value (the text after `uses:`, comment included), if anything.
fn bad_uses(value: &str) -> Option<String> {
    let (target, comment) = match value.split_once('#') {
        Some((t, c)) => (t.trim(), Some(c.trim())),
        None => (value.trim(), None),
    };
    let target = target.trim_matches(|c| c == '"' || c == '\'');
    if target.starts_with("./") {
        return None; // an action in this repo: pinned by the commit itself
    }
    if target.starts_with("docker://") {
        return match target.split_once("@sha256:") {
            Some((_, d)) if is_hex(d, 64) => None,
            _ => Some(format!("{target}: a docker:// action needs @sha256:<digest>")),
        };
    }
    let Some((_, git_ref)) = target.rsplit_once('@') else {
        return Some(format!("{target}: no ref at all"));
    };
    if !is_hex(git_ref, 40) {
        return Some(format!("{target}: `{git_ref}` is a tag or branch that can be moved; pin a full commit id"));
    }
    if comment.is_none_or(str::is_empty) {
        return Some(format!("{target}: say which version the commit is in a comment (`# v1.2.3`)"));
    }
    None
}

/// What is wrong with one Dockerfile's FROM lines.
fn bad_froms(dockerfile: &str) -> Vec<String> {
    let mut stages: Vec<String> = Vec::new();
    let mut bad = Vec::new();
    for line in dockerfile.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.first().is_none_or(|w| !w.eq_ignore_ascii_case("FROM")) {
            continue;
        }
        let rest: Vec<&str> = words[1..].iter().copied().filter(|w| !w.starts_with("--")).collect();
        let Some(image) = rest.first() else {
            bad.push(format!("{line}: no image"));
            continue;
        };
        let ok = *image == "scratch"
            || stages.iter().any(|s| s.eq_ignore_ascii_case(image))
            || image.split_once("@sha256:").is_some_and(|(_, d)| is_hex(d, 64));
        if !ok {
            bad.push(format!("{line}: pin the image by digest (`{image}@sha256:<digest>`)"));
        }
        if let Some(i) = rest.iter().position(|w| w.eq_ignore_ascii_case("AS"))
            && let Some(name) = rest.get(i + 1)
        {
            stages.push(name.to_string());
        }
    }
    bad
}

fn workflow_files() -> Vec<PathBuf> {
    let dir = repo().join(".github/workflows");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "yml" || x == "yaml"))
        .collect();
    files.sort();
    files
}

/// The `uses:` values of a workflow file.
fn uses_values(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            let t = l.trim_start().trim_start_matches("- ").trim_start();
            t.strip_prefix("uses:").map(|v| v.trim().to_string())
        })
        .collect()
}

#[test]
fn a_moved_tag_or_branch_is_refused() {
    // the refs the image workflow used to follow: each can be moved by whoever controls the action's repo
    for v in [
        "actions/checkout@v7",
        "dtolnay/rust-toolchain@stable",
        "docker/build-push-action@v7.4.0 # v7.4.0",
        "docker/login-action@8aa5831 # a short id is not unique",
        "docker/login-action",
        "docker://alpine:3",
        "github/codeql-action/init@main",
    ] {
        assert!(bad_uses(v).is_some(), "{v} passed");
    }
    // a full commit id with its version, an action of this repo, an image by digest
    for v in [
        "docker/login-action@8aa5831fb8d48d8f76594c7ddde61ae58019f550 # v4.8.0",
        "github/codeql-action/init@8aa5831fb8d48d8f76594c7ddde61ae58019f550 # v3",
        "./.github/actions/local",
        "docker://alpine@sha256:8d0e82abfdf9a38071e07cabc5afba4ddbe077631a80e4e742a760c38beadc33",
    ] {
        assert_eq!(bad_uses(v), None, "{v}");
    }
    assert!(bad_uses("docker/login-action@8aa5831fb8d48d8f76594c7ddde61ae58019f550").is_some(), "no version comment");

    let old = "FROM rust:1-alpine AS manifests\nFROM rust:1-alpine AS build\nFROM scratch\nCOPY --from=build /x /x\n";
    assert_eq!(bad_froms(old).len(), 2);
    let pinned = "FROM rust:1.100.0-alpine@sha256:8d0e82abfdf9a38071e07cabc5afba4ddbe077631a80e4e742a760c38beadc33 AS b\n\
                  FROM b AS c\nFROM --platform=linux/amd64 scratch\n";
    assert!(bad_froms(pinned).is_empty(), "{:?}", bad_froms(pinned));
    assert_eq!(bad_froms("FROM alpine@sha256:abc\n").len(), 1, "a short digest");
}

#[test]
fn every_action_is_pinned_to_a_commit() {
    let files = workflow_files();
    assert!(!files.is_empty());
    let mut bad = Vec::new();
    let mut seen = 0;
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        for v in uses_values(&text) {
            seen += 1;
            if let Some(why) = bad_uses(&v) {
                bad.push(format!("{}: {why}", f.file_name().unwrap().to_string_lossy()));
            }
        }
    }
    assert!(seen > 0, "no uses: lines found");
    assert!(bad.is_empty(), "unpinned actions:\n{}", bad.join("\n"));
}

#[test]
fn the_base_image_is_pinned_by_digest() {
    let text = std::fs::read_to_string(repo().join("Dockerfile")).unwrap();
    let bad = bad_froms(&text);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn dependabot_bumps_the_pins() {
    let text = std::fs::read_to_string(repo().join(".github/dependabot.yml")).unwrap();
    for eco in ["package-ecosystem: github-actions", "package-ecosystem: docker"] {
        assert!(text.contains(eco), "dependabot.yml has no `{eco}`");
    }
}

/// The repository's files: `git ls-files` when git is there (only what is committed or staged), else a walk.
fn repo_files(root: &Path) -> Vec<PathBuf> {
    if let Ok(out) = std::process::Command::new("git").arg("-C").arg(root).args(["ls-files", "-z"]).output()
        && out.status.success()
        && !out.stdout.is_empty()
    {
        return out
            .stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| root.join(String::from_utf8_lossy(p).as_ref()))
            .collect();
    }
    let mut files = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            let name = e.file_name();
            if matches!(name.to_str(), Some(".git" | "target" | "node_modules")) {
                continue;
            }
            match e.file_type() {
                Ok(t) if t.is_dir() => dirs.push(p),
                Ok(t) if t.is_file() => files.push(p),
                _ => {}
            }
        }
    }
    files
}

#[test]
fn no_owner_data_or_session_paths_in_the_repo() {
    let root = repo();
    let this = Path::new(file!()).file_name().unwrap().to_owned();
    // Phrases that came with real data from the owner's car or a local session before. (A real VIN, position or
    // log is never committed; these catch the wording that used to come with one, and fixed local paths.)
    let markers = [
        "/tmp/claude-",
        "agent-data/",
        "taken from a real minute",
        "VIN year code",
        "the owner's car",
        "owner's Model Y",
        "plugged in at home",
    ];
    let mut hits = Vec::new();
    for f in repo_files(&root) {
        if f.file_name() == Some(this.as_os_str()) {
            continue;
        }
        let Ok(text) = std::fs::read(&f) else { continue };
        let text = String::from_utf8_lossy(&text);
        for m in markers {
            if text.contains(m) {
                hits.push(format!("{}: {m:?}", f.strip_prefix(&root).unwrap_or(&f).display()));
            }
        }
    }
    assert!(hits.is_empty(), "owner data or local paths in the repo:\n{}", hits.join("\n"));
}
