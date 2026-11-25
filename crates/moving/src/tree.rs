//! What is carried from a data folder, in what order, and what kind each file is: one rule, used by the export, the
//! import's checks and the "is this server new" check (and later by the move's list and digests), so an export
//! Sentry Mate made always passes its own import's checks.
//!
//! - A **regular file** is carried when every part of its path is made of letters, digits and `._-` only (so UTF-8),
//!   is not `.` or `..`, and the whole path is at most 1,024 bytes. Every name Sentry Mate makes fits.
//! - **Never carried**: `vault.json` (the new server writes its own); anything at the top whose name starts with a
//!   dot (the locks, `.export/`, `.importing/`, `.move/`, later `.backups/`); temporary files anywhere (`*.tmp`,
//!   `*.sealing`, `*.to-v2`, `*.new`) and `db.restoring/`; an export file (`*.smexport`) at the top; and in an export
//!   only, the public downloads (`geonames/`, `maps/`), which the new server can download again.
//! - **Skipped and named**: links and anything that isn't a regular file or a folder, folders that can't be read
//!   (`lost+found` at the root of an ext4 disk), files this server may not open (the callers that open files skip
//!   those, see [`open_file`]), and names off the rule (Synology's `@eaDir`). They stay where they
//!   are.
//!
//! **Walk order**: paths compared part by part, each part by its bytes, a part that is the start of another first.
//! It is the order of a depth-first walk with each folder sorted by name, and **not** the byte order of whole paths
//! (`tokens/x.key` comes before `tokens.json`, though `/` is 0x2F and `.` is 0x2E).

use std::cmp::Ordering;
use std::ops::ControlFlow;
use std::path::Path;

/// A path is at most this long.
pub const MAX_PATH: usize = 1024;
/// At most this many skipped entries are named (the rest are counted).
pub const NAMED_SKIPS: usize = 100;

/// The three kinds of file, told apart by their first bytes. Each has one copy rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A sealed whole file (`SMENC` 0x01): written under a temporary name and renamed, never changed in place.
    Sealed,
    /// A growing file (vault records, `SMPRT` 0x01 or 0x02): only added to.
    Records,
    /// Anything else (the certificate, empty markers, public downloads).
    Plain,
}

impl Kind {
    /// The kind of a file that starts with `head` (its first 6 bytes are enough; a shorter file is plain).
    pub fn of(head: &[u8]) -> Kind {
        if vault::is_sealed(head) {
            Kind::Sealed
        } else if vault::records::is_records(head) {
            Kind::Records
        } else {
            Kind::Plain
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Kind::Sealed => 1,
            Kind::Records => 2,
            Kind::Plain => 3,
        }
    }

    pub fn from_code(c: u8) -> Option<Kind> {
        match c {
            1 => Some(Kind::Sealed),
            2 => Some(Kind::Records),
            3 => Some(Kind::Plain),
            _ => None,
        }
    }
}

/// The bytes a move reads of a file's head: the magic and the file id.
pub const HEAD: usize = 22;

/// A file's kind and file id from its first [`HEAD`] bytes, as a move names its versions: a sealed or records file
/// shorter than its header has no file id, and counts as plain.
pub fn head_kind(head: &[u8]) -> (Kind, Option<[u8; 16]>) {
    let k = Kind::of(head);
    if k == Kind::Plain || head.len() < HEAD {
        return (Kind::Plain, None);
    }
    let mut id = [0u8; 16];
    id.copy_from_slice(&head[6..HEAD]);
    (k, Some(id))
}

/// What the walk is for: an export leaves out the public downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Move,
    Export,
}

/// Walk order (see the module docs).
pub fn walk_cmp(a: &str, b: &str) -> Ordering {
    a.split('/').cmp(b.split('/'))
}

/// A path part the rule allows.
pub fn name_ok(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && part.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// A file being written, or what a crash left of one.
pub fn temporary(name: &str) -> bool {
    name.ends_with(".tmp")
        || name.ends_with(".sealing")
        || name.ends_with(vault::records::CONVERTING)
        || name.ends_with(".new")
}

/// Never carried, never named (see the module docs).
fn never(top: bool, name: &str, dir: bool, purpose: Purpose) -> bool {
    if top && name.starts_with('.') {
        return true;
    }
    if dir {
        top && (name == "db.restoring" || (purpose == Purpose::Export && (name == "geonames" || name == "maps")))
    } else {
        temporary(name) || (top && (name == vault::VAULT_FILE || name.ends_with(".smexport")))
    }
}

/// Whether `path` (from elsewhere: an import's manifest) is a file path the walk would carry.
pub fn carried(path: &str, purpose: Purpose) -> bool {
    if path.len() > MAX_PATH {
        return false;
    }
    let parts: Vec<&str> = path.split('/').collect();
    let last = parts.len() - 1;
    parts.iter().enumerate().all(|(i, p)| name_ok(p) && !never(i == 0, p, i < last, purpose))
}

/// A file the walk carries.
pub struct Found<'a> {
    /// Its path under the data folder, parts joined with `/`.
    pub path: &'a str,
    pub full: &'a Path,
    pub len: u64,
    /// Its modification time, in microseconds since 1970.
    pub mtime_us: u64,
    /// The file system's device and inode numbers (0 where there are none): what a cache of the files' heads is
    /// keyed by.
    pub dev: u64,
    pub ino: u64,
}

/// An entry the walk skipped, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub path: String,
    pub why: &'static str,
}

/// What the walk skipped: the first [`NAMED_SKIPS`] by name, and how many in all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Skips {
    pub named: Vec<Skipped>,
    pub count: u64,
}

impl Skips {
    /// Names a skipped entry (the first [`NAMED_SKIPS`]) and counts it.
    pub fn add(&mut self, path: String, why: &'static str) {
        self.count += 1;
        if self.named.len() < NAMED_SKIPS {
            self.named.push(Skipped { path, why });
        }
    }

    /// Adds what another walk skipped.
    pub fn merge(&mut self, other: Skips) {
        let named = other.named.len() as u64;
        for k in other.named {
            self.add(k.path, k.why);
        }
        self.count += other.count.saturating_sub(named);
    }

    /// One line for a log: "lost+found (can't be read), @eaDir (name off the rule), ...".
    pub fn summary(&self) -> String {
        let mut s: Vec<String> = self.named.iter().map(|k| format!("{} ({})", k.path, k.why)).collect();
        if self.count > self.named.len() as u64 {
            s.push(format!("and {} more", self.count - self.named.len() as u64));
        }
        s.join(", ")
    }
}

/// The reasons a walk gives for a skipped entry, by their words (a saved plan names them again).
pub fn why_of(s: &str) -> &'static str {
    ["name off the rule", "a link", "not a file or folder", UNREADABLE]
        .into_iter()
        .find(|w| *w == s)
        .unwrap_or("skipped")
}

/// Why a file that can't be opened is skipped (as for a folder that can't be read).
pub const UNREADABLE: &str = "can't be read";

/// Opens a carried file to read it, not through a link. A file this server's user may not open (one made by root,
/// mode 0600) answers `PermissionDenied`: the move's list, its digest and the export skip it and name it.
pub fn open_file(p: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(test)]
    if test_denied::is(p) {
        return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    }
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut o, rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    o.open(p)
}

/// Files the tests make unopenable (the tests run as root, which opens everything).
#[cfg(test)]
pub(crate) mod test_denied {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static DENIED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    pub(crate) fn deny(p: &Path) {
        DENIED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(p.to_path_buf());
    }

    pub(crate) fn is(p: &Path) -> bool {
        DENIED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().any(|d| d == p)
    }
}

pub fn mtime_us(md: &std::fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

/// Calls `each` for every carried file under `data`, in walk order; `Break` stops the walk. Skipped entries are added
/// to `skips`. Entries removed while the walk runs are passed over.
pub fn walk<E: From<std::io::Error>>(
    data: &Path,
    purpose: Purpose,
    skips: &mut Skips,
    each: impl FnMut(&Found<'_>) -> Result<ControlFlow<()>, E>,
) -> Result<(), E> {
    walk_from(data, purpose, skips, None, each)
}

/// [`walk`], only the files after `after` in walk order (a list served a page at a time). Folders wholly before it
/// are not read at all; only those on its path are.
pub fn walk_from<E: From<std::io::Error>>(
    data: &Path,
    purpose: Purpose,
    skips: &mut Skips,
    after: Option<&str>,
    mut each: impl FnMut(&Found<'_>) -> Result<ControlFlow<()>, E>,
) -> Result<(), E> {
    let parts: Option<Vec<&str>> = after.map(|a| a.split('/').collect());
    walk_dir(data, "", purpose, skips, parts.as_deref(), &mut each).map(|_| ())
}

#[cfg(unix)]
fn ids(md: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (md.dev(), md.ino())
}

#[cfg(not(unix))]
fn ids(_: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

fn walk_dir<E: From<std::io::Error>>(
    dir: &Path,
    rel: &str,
    purpose: Purpose,
    skips: &mut Skips,
    after: Option<&[&str]>,
    each: &mut impl FnMut(&Found<'_>) -> Result<ControlFlow<()>, E>,
) -> Result<ControlFlow<()>, E> {
    use std::io::ErrorKind;
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == ErrorKind::PermissionDenied && !rel.is_empty() => {
            skips.add(rel.to_string(), UNREADABLE);
            return Ok(ControlFlow::Continue(()));
        }
        Err(e) if e.kind() == ErrorKind::NotFound && !rel.is_empty() => return Ok(ControlFlow::Continue(())),
        Err(e) => return Err(e.into()),
    };
    let mut entries: Vec<(String, std::fs::FileType)> = Vec::new();
    for e in rd {
        let e = match e {
            Ok(e) => e,
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => return Err(err.into()),
        };
        let name = e.file_name();
        let Some(name) = name.to_str() else {
            let lossy = name.to_string_lossy();
            skips.add(if rel.is_empty() { lossy.into_owned() } else { format!("{rel}/{lossy}") }, "name off the rule");
            continue;
        };
        let ft = match e.file_type() {
            Ok(t) => t,
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => return Err(err.into()),
        };
        entries.push((name.to_string(), ft));
    }
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let top = rel.is_empty();
    for (name, ft) in entries {
        let dir_like = ft.is_dir();
        // only what comes after `after`: a folder on its path is walked from there on, one after it wholly
        let mut inner = None;
        if let Some(&[first, ref rest @ ..]) = after {
            match name.as_str().cmp(first) {
                Ordering::Less => continue,
                Ordering::Equal if !dir_like => continue,
                Ordering::Equal if !rest.is_empty() => inner = Some(rest),
                _ => {}
            }
        }
        let path = if top { name.clone() } else { format!("{rel}/{name}") };
        if never(top, &name, dir_like, purpose) {
            continue;
        }
        if (dir_like || ft.is_file()) && (!name_ok(&name) || path.len() > MAX_PATH) {
            skips.add(path, "name off the rule");
            continue;
        }
        let full = dir.join(&name);
        if dir_like {
            if walk_dir(&full, &path, purpose, skips, inner, each)?.is_break() {
                return Ok(ControlFlow::Break(()));
            }
        } else if ft.is_file() {
            let md = match std::fs::symlink_metadata(&full) {
                Ok(m) => m,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            let (dev, ino) = ids(&md);
            let found = Found { path: &path, full: &full, len: md.len(), mtime_us: mtime_us(&md), dev, ino };
            if each(&found)?.is_break() {
                return Ok(ControlFlow::Break(()));
            }
        } else if ft.is_symlink() {
            skips.add(path, "a link");
        } else {
            skips.add(path, "not a file or folder");
        }
    }
    Ok(ControlFlow::Continue(()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn walk_order_is_part_by_part() {
        let mut v = vec!["tokens.json", "tokens/ff.key", "a-b", "a.b", "a/x", "a", "b", "a/x/y", "tokens/00.json"];
        v.sort_by(|a, b| walk_cmp(a, b));
        assert_eq!(v, ["a", "a/x", "a/x/y", "a-b", "a.b", "b", "tokens/00.json", "tokens/ff.key", "tokens.json"]);
        // not the byte order of whole paths
        assert!("tokens.json" < "tokens/ff.key");
        assert_eq!(walk_cmp("tokens/ff.key", "tokens.json"), Ordering::Less);
    }

    #[test]
    fn a_walk_from_any_path_gives_the_rest_in_walk_order() {
        let d = tempfile::tempdir().unwrap();
        let all = ["a", "a/x", "a/x/y", "a/z", "a-b", "a.b", "b", "tokens/00.json", "tokens/ff.key", "tokens.json"];
        for p in all {
            let full = d.path().join(p);
            if all.iter().any(|q| q.starts_with(&format!("{p}/"))) {
                std::fs::create_dir_all(&full).unwrap();
            } else {
                std::fs::create_dir_all(full.parent().unwrap()).unwrap();
                std::fs::write(&full, p).unwrap();
            }
        }
        let files: Vec<&str> =
            all.iter().copied().filter(|p| !all.iter().any(|q| q.starts_with(&format!("{p}/")))).collect();
        let list = |after: Option<&str>| {
            let mut out = Vec::new();
            walk_from(d.path(), Purpose::Move, &mut Skips::default(), after, |f| -> std::io::Result<ControlFlow<()>> {
                out.push(f.path.to_string());
                Ok(ControlFlow::Continue(()))
            })
            .unwrap();
            out
        };
        assert_eq!(list(None), files);
        for (i, f) in files.iter().enumerate() {
            assert_eq!(list(Some(f)), files[i + 1..], "after {f}");
        }
        // after a path that isn't there (removed since), or a folder's name
        assert_eq!(list(Some("a/y")), ["a/z", "a-b", "a.b", "b", "tokens/00.json", "tokens/ff.key", "tokens.json"]);
        assert_eq!(list(Some("tokens")), ["tokens/00.json", "tokens/ff.key", "tokens.json"]);
        assert_eq!(list(Some("zzz")), Vec::<String>::new());
    }

    #[test]
    fn the_rule() {
        for ok in
            ["can/abc123/abc123_20261108T143000Z_1.log.xz", "can/.waiting/.ready", "tokens/ab.json", "tls/cert.pem"]
        {
            assert!(carried(ok, Purpose::Export), "{ok}");
        }
        for no in [
            "vault.json",
            ".lock",
            ".export/x/files/a",
            "can/a/../b",
            "can/a/./b",
            "can//b",
            "notes (1).txt",
            "@eaDir/x",
            "can/a/x.log.xz.sealing",
            "db/fleet.snap.tmp",
            "db/x.new",
            "can/a/uploading/x.part.to-v2",
            "db.restoring/x",
            "sentry-mate.smexport",
            "maps/world.pmtiles",
            "geonames/cities.txt",
            "caf\u{e9}",
        ] {
            assert!(!carried(no, Purpose::Export), "{no}");
        }
        assert!(carried("maps/world.pmtiles", Purpose::Move), "the direct move carries the downloads");
        assert!(carried("x/vault.json", Purpose::Move), "only the top one is the vault");
        assert!(!carried(&format!("{}b", "a/".repeat(600)), Purpose::Move), "too long");
    }
}
