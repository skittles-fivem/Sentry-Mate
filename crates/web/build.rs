//! Embeds the built-in UI packs (`ui/packs/<pack>/ui.json` and `web/`) in the binary: a generated table of
//! `include_bytes!`, so the program serves them with no files beside it. Pure Rust, nothing compiled.
//!
//! Left out on purpose: files only the pack's developers use (`README.md` notes, a `mock-api.js` or other `mock`
//! files, tests), so the real server never serves them.
//!
//! With `SENTRY_MATE_UI_BUNDLE=DIR` (the release build: `ui/tools/bundle/bundle.mjs DIR`), each pack's bundled and
//! minified files in `DIR/<pack>/web/` are embedded too, as its `bundle`: served in place of the sources' files of the
//! same path. Without it (tests, a plain `cargo build`) the packs are served as they are in `ui/packs`.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

#[path = "src/packs/compress.rs"]
#[allow(dead_code)]
mod compress;

/// The brotli and gzip copies of a file (by the SHA-256 of its bytes), made once and kept in `OUT_DIR` so a rebuild
/// after one file changed compresses only that file. `None` for a file that stays as it is.
fn precompress(cache: &Path, path: &Path, rel: &str) -> std::io::Result<Option<(String, PathBuf, PathBuf)>> {
    if !compress::compressible(rel) {
        return Ok(None);
    }
    let data = std::fs::read(path)?;
    if data.len() < compress::MIN_BYTES {
        return Ok(None);
    }
    let sum = hex::encode(Sha256::digest(&data));
    let (br, gz) = (cache.join(format!("{sum}.br")), cache.join(format!("{sum}.gz")));
    if !br.is_file() || !gz.is_file() {
        let (b, g) = (compress::brotli(&data, 11), compress::gzip(&data, 9));
        if !compress::worth(data.len(), b.len()) {
            return Ok(None);
        }
        let tmp = cache.join(format!("{sum}.tmp"));
        std::fs::write(&tmp, &g)?;
        std::fs::rename(&tmp, &gz)?;
        std::fs::write(&tmp, &b)?;
        std::fs::rename(&tmp, &br)?;
    }
    Ok(Some((sum, br, gz)))
}

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
    println!("cargo:rerun-if-env-changed=SENTRY_MATE_UI_BUNDLE");
    let bundle = std::env::var_os("SENTRY_MATE_UI_BUNDLE").filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(b) = &bundle {
        if !b.is_dir() {
            return Err(std::io::Error::other(format!("SENTRY_MATE_UI_BUNDLE: no folder {}", b.display())));
        }
        println!("cargo:rerun-if-changed={}", b.display());
    }
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").map_err(std::io::Error::other)?);
    let cache = out_dir.join("precompressed");
    std::fs::create_dir_all(&cache)?;
    let mut packed: BTreeMap<String, (PathBuf, PathBuf)> = BTreeMap::new();
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
            if let Some((sum, br, gz)) = precompress(&cache, &path, &rel)? {
                packed.insert(sum, (br, gz));
            }
            let abs = path.canonicalize()?;
            writeln!(code, "        ({rel:?}, include_bytes!({:?})),", abs.display().to_string())
                .map_err(std::io::Error::other)?;
        }
        code.push_str("    ], bundle: &[\n");
        if let Some(bweb) = bundle.as_ref().map(|b| b.join(&id).join("web")) {
            if !bweb.join("core").is_dir() {
                return Err(std::io::Error::other(format!(
                    "SENTRY_MATE_UI_BUNDLE: no bundle of pack {id} in {}",
                    bweb.display()
                )));
            }
            let mut bfiles = Vec::new();
            walk(&bweb, &bweb, &mut bfiles)?;
            for (rel, path) in bfiles {
                if let Some((sum, br, gz)) = precompress(&cache, &path, &rel)? {
                    packed.insert(sum, (br, gz));
                }
                let abs = path.canonicalize()?;
                writeln!(code, "        ({rel:?}, include_bytes!({:?})),", abs.display().to_string())
                    .map_err(std::io::Error::other)?;
            }
        }
        code.push_str("    ] },\n");
    }
    code.push_str("];\n");
    // one copy of each, though the same file is in several packs (the vendored libraries, ui/shared)
    code.push_str("pub static PRECOMPRESSED: &[(&str, &[u8], &[u8])] = &[\n");
    for (sum, (br, gz)) in &packed {
        writeln!(
            code,
            "    ({sum:?}, include_bytes!({:?}), include_bytes!({:?})),",
            br.display().to_string(),
            gz.display().to_string()
        )
        .map_err(std::io::Error::other)?;
    }
    code.push_str("];\n");
    let out = out_dir.join("builtin_packs.rs");
    std::fs::write(out, code)
}
