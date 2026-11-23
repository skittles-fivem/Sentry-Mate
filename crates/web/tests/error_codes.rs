#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! docs/ui-api.md lists the error codes in use ("Error codes in use", with their statuses), so a page can give
//! each its words. Every code the web side answers with is in that list, under its status.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The first `"code"` of the text, when it starts (after spaces and an optional status) with one.
fn leading_code(text: &str) -> Option<(Option<u16>, String)> {
    let t = text.trim_start();
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    let status = digits.parse::<u16>().ok();
    let t = t[digits.len()..].trim_start();
    let t = if status.is_some() { t.strip_prefix(',')?.trim_start() } else { t };
    let t = t.strip_prefix('"')?;
    let code = &t[..t.find('"')?];
    let plain = !code.is_empty() && code.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    plain.then(|| (status, code.to_string()))
}

/// Every code a source file answers with: `ApiError::new(<status>, "code"`, `ApiError::conflict("code"` (409)
/// and `ApiError::not_found("code"` (404), with the status where the call names it.
fn codes_in_source() -> BTreeMap<String, Option<u16>> {
    let mut files = Vec::new();
    rust_files(&crate_dir().join("src"), &mut files);
    files.push(crate_dir().join("../sentry-mate/src/api.rs"));
    let mut out = BTreeMap::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        for (call, fixed) in
            [("ApiError::new(", None), ("ApiError::conflict(", Some(409)), ("ApiError::not_found(", Some(404))]
        {
            for (i, _) in text.match_indices(call) {
                let rest = &text[i + call.len()..];
                let Some((status, code)) = leading_code(rest) else { continue };
                out.insert(code, fixed.or(status));
            }
        }
    }
    out
}

#[test]
fn every_error_code_the_server_answers_is_in_the_contract() {
    let doc = std::fs::read_to_string(crate_dir().join("../../docs/ui-api.md")).unwrap();
    let line = doc.lines().find(|l| l.starts_with("- **Error codes** in use:")).expect("the list of error codes");
    let codes = codes_in_source();
    assert!(codes.len() > 90, "the source scan found the codes: {codes:?}");
    for known in ["no_such_pack", "two_step_first", "bad_range", "teslamate_not_added", "stale_version"] {
        assert!(codes.contains_key(known), "the scan misses {known}");
    }
    let mut missing = Vec::new();
    for (code, status) in &codes {
        // the entry of the list (between two `;`) that names the code, and the status that entry ends with
        let entry = line.split(';').find(|e| e.contains(&format!("`{code}`")));
        let Some(entry) = entry else {
            missing.push(format!("{code} ({})", status.map_or("?".to_string(), |s| s.to_string())));
            continue;
        };
        if let Some(s) = status {
            assert!(
                entry.contains(&format!(" {s}")),
                "docs/ui-api.md lists `{code}` under another status than {s}: {entry}"
            );
        }
    }
    assert!(missing.is_empty(), "error codes missing from docs/ui-api.md, \"Error codes in use\": {missing:?}");
}
