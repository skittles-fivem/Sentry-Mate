//! Embeds the built-in UI packs (`ui/packs/<pack>/ui.json` and `web/`) in the binary: a generated table of
//! `include_bytes!`, so the program serves them with no files beside it. Pure Rust, nothing compiled.
//!
//! Left out on purpose: files only the pack's developers use (`README.md` notes, a `mock-api.js` or other `mock`
//! files, tests), so the real server never serves them.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn dev_only(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let lower = name.to_ascii_lowercase();
    lower == "readme.md"
        || lower.starts_with("mock-")
        || lower.starts_with("mock.")
        || lower.contains(".test.")
        || lower.contains(".spec.")
        || rel.split('/').any(|seg| seg == "tests" || seg == "test" || seg == "mock" || seg.starts_with('.'))
}

fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let kind = e.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            walk(&path, base, out)?;
        } else if kind.is_file() {
            let rel = path
                .strip_prefix(base)
                .map_err(std::io::Error::other)?
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if !dev_only(&rel) {
                out.push((rel, path));
            }
        }
    }
    Ok(())
}

fn main() -> std::io::Result<()> {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").map_err(std::io::Error::other)?);
    let packs = manifest.join("../../ui/packs");
    println!("cargo:rerun-if-changed={}", packs.display());
    println!("cargo:rerun-if-changed=build.rs");
    let mut code = String::from("pub static PACKS: &[BuiltinPack] = &[\n");
    let mut dirs: Vec<_> = std::fs::read_dir(&packs)
        .map_err(|e| std::io::Error::other(format!("the built-in UI packs ({}): {e}", packs.display())))?
        .collect::<Result<_, _>>()?;
    dirs.sort_by_key(|e| e.file_name());
    for d in dirs {
        let dir = d.path();
        let (ui, web) = (dir.join("ui.json"), dir.join("web"));
        if !ui.is_file() || !web.join("index.html").is_file() {
            continue;
        }
        let id = d.file_name().to_string_lossy().into_owned();
        let mut files = Vec::new();
        walk(&web, &web, &mut files)?;
        let ui = ui.canonicalize()?;
        writeln!(
            code,
            "    BuiltinPack {{ id: {id:?}, ui_json: include_bytes!({:?}), files: &[",
            ui.display().to_string()
        )
        .map_err(std::io::Error::other)?;
        for (rel, path) in files {
            let abs = path.canonicalize()?;
            writeln!(code, "        ({rel:?}, include_bytes!({:?})),", abs.display().to_string())
                .map_err(std::io::Error::other)?;
        }
        code.push_str("    ] },\n");
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").map_err(std::io::Error::other)?).join("builtin_packs.rs");
    std::fs::write(out, code)
}
