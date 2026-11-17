//! The data and keys folders, checked before they are used. A folder Sentry Mate can't write to (most often one
//! made by root: by Docker for a bind mount whose folder was missing, or by `sudo mkdir`) stops it with what to do,
//! not with "Permission denied (os error 13)".

use std::io::{ErrorKind, Write};
use std::path::Path;

/// Who this process runs as: (uid, gid).
pub fn runs_as() -> (u32, u32) {
    (rustix::process::geteuid().as_raw(), rustix::process::getegid().as_raw())
}

/// Checks a folder before use: there (made when missing and `write`), a folder, and writable (`write`) or readable.
/// `what` names it in the message ("data", "keys").
pub fn check(dir: &Path, what: &str, write: bool) -> Result<(), String> {
    match std::fs::metadata(dir) {
        Ok(m) if !m.is_dir() => return Err(format!("the {what} folder {} is a file, not a folder", dir.display())),
        Ok(_) => {}
        // (not a directory: a file where one of the folders above it should be)
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) && write => {
            if let Err(e) = std::fs::create_dir_all(dir) {
                return Err(missing(dir, what, Some(&e)));
            }
        }
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Err(missing(dir, what, None));
        }
        Err(e) => return Err(explain(dir, what, write, &e)),
    }
    let tried = if write { probe(dir) } else { std::fs::read_dir(dir).map(|_| ()) };
    tried.map_err(|e| explain(dir, what, write, &e))
}

/// Writes and removes a small file: what Sentry Mate does in the folder, tried first.
fn probe(dir: &Path) -> std::io::Result<()> {
    let file = dir.join(format!(".write-test-{}", std::process::id()));
    let r = std::fs::File::create(&file).and_then(|mut f| f.write_all(b"ok"));
    let _ = std::fs::remove_file(&file);
    r
}

fn missing(dir: &Path, what: &str, e: Option<&std::io::Error>) -> String {
    let made = e.map(|e| format!(" and can't be made ({e})")).unwrap_or_default();
    format!(
        "the {what} folder {d} is missing{made}. Mount a folder of the server there (in the compose file: \
         `- /path/on/server:{d}`)",
        d = dir.display()
    )
}

fn explain(dir: &Path, what: &str, write: bool, e: &std::io::Error) -> String {
    let doing = if write { "write to" } else { "read" };
    match e.kind() {
        ErrorKind::PermissionDenied => denied(dir, what, doing, runs_as(), owner_of(dir)),
        ErrorKind::ReadOnlyFilesystem => format!(
            "the {what} folder {d} is read-only. Mount a folder of the server there, read-write (in the compose \
             file: `- /path/on/server:{d}`, without `:ro`)",
            d = dir.display()
        ),
        _ => format!("can't {doing} the {what} folder {}: {e}", dir.display()),
    }
}

/// The folder's owner (uid, gid) and permission bits, when they can be seen.
fn owner_of(dir: &Path) -> Option<(u32, u32, u32)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(dir).ok().map(|m| (m.uid(), m.gid(), m.mode() & 0o7777))
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}

/// The message for a folder this user may not use: who Sentry Mate runs as, who owns the folder, the command.
fn denied(dir: &Path, what: &str, doing: &str, me: (u32, u32), owner: Option<(u32, u32, u32)>) -> String {
    let (uid, gid) = me;
    let d = dir.display();
    let head = format!("Sentry Mate runs as uid {uid} (group {gid}) and can't {doing} the {what} folder {d}");
    match owner {
        Some((ou, _, mode)) if ou == uid => format!(
            "{head}: it is its own folder, but its permissions ({mode:o}) don't let it.\nOn the server: sudo chmod \
             u+rwx <the folder mounted at {d}>"
        ),
        Some((ou, og, _)) => {
            let root = if ou == 0 { ", root" } else { "" };
            let mut s = format!(
                "{head}: it belongs to uid {ou} (group {og}){root}.\nOn the server, give the folder mounted at {d} \
                 to uid {uid}: sudo chown -R {uid}:{gid} <that folder>"
            );
            if ou != 0 {
                s += &format!("\nOr run Sentry Mate as the folder's owner: user: \"{ou}:{og}\" in the compose file");
            }
            s
        }
        None => format!(
            "{head}.\nOn the server, give the folder mounted at {d} to uid {uid}: sudo chown -R {uid}:{gid} <that \
             folder>"
        ),
    }
}

/// What to add to an error that says "Permission denied" (a file inside the folders that belongs to another user,
/// say after a change of the user Sentry Mate runs as): who it runs as and the command.
pub fn hint(e: &(dyn std::error::Error + 'static), data: &Path, keys: &Path) -> Option<String> {
    let mut cur = Some(e);
    while let Some(err) = cur {
        if err.downcast_ref::<std::io::Error>().is_some_and(|io| io.kind() == ErrorKind::PermissionDenied) {
            let (uid, gid) = runs_as();
            return Some(format!(
                "Sentry Mate runs as uid {uid} (group {gid}): the data and keys folders and everything in them must \
                 belong to it.\nOn the server: sudo chown -R {uid}:{gid} <the folder mounted at {}> <the folder \
                 mounted at {}>",
                data.display(),
                keys.display()
            ));
        }
        cur = err.source();
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn a_missing_folder_is_made_and_a_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("a/data");
        check(&data, "data", true).unwrap();
        assert!(data.is_dir());
        // the test file is gone again
        assert_eq!(std::fs::read_dir(&data).unwrap().count(), 0);
        check(&data, "data", false).unwrap();

        let file = dir.path().join("keys");
        std::fs::write(&file, b"x").unwrap();
        let e = check(&file, "keys", true).unwrap_err();
        assert!(e.contains("is a file, not a folder"), "{e}");

        // a command that only reads doesn't make it
        let e = check(&dir.path().join("none"), "data", false).unwrap_err();
        assert!(e.starts_with("the data folder") && e.contains("is missing. Mount a folder"), "{e}");
        assert!(!dir.path().join("none").exists());
    }

    #[test]
    fn a_folder_that_cant_be_made_says_to_mount_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file"), b"x").unwrap();
        let e = check(&dir.path().join("file/data"), "data", true).unwrap_err();
        assert!(e.contains("is missing and can't be made") && e.contains("Mount a folder of the server there"), "{e}");
    }

    #[test]
    fn a_folder_of_root_names_the_user_and_the_command() {
        let e = denied(Path::new("/data"), "data", "write to", (568, 568), Some((0, 0, 0o755)));
        assert_eq!(
            e,
            "Sentry Mate runs as uid 568 (group 568) and can't write to the data folder /data: it belongs to uid 0 \
             (group 0), root.\nOn the server, give the folder mounted at /data to uid 568: sudo chown -R 568:568 \
             <that folder>"
        );
    }

    #[test]
    fn a_folder_of_another_user_also_offers_that_user() {
        let e = denied(Path::new("/keys"), "keys", "write to", (1000, 1000), Some((568, 568, 0o700)));
        assert!(e.contains("it belongs to uid 568 (group 568).\n"), "{e}");
        assert!(e.contains("sudo chown -R 1000:1000 <that folder>"), "{e}");
        assert!(e.ends_with("user: \"568:568\" in the compose file"), "{e}");
    }

    #[test]
    fn its_own_folder_without_write_permission_says_chmod() {
        let e = denied(Path::new("/data"), "data", "write to", (1000, 1000), Some((1000, 1000, 0o555)));
        assert!(e.contains("its permissions (555) don't let it"), "{e}");
        assert!(e.ends_with("sudo chmod u+rwx <the folder mounted at /data>"), "{e}");
        let e = denied(Path::new("/data"), "data", "read", (1000, 1000), None);
        assert!(e.contains("can't read the data folder /data.\n"), "{e}");
        assert!(e.contains("sudo chown -R 1000:1000"), "{e}");
    }

    #[test]
    fn permission_denied_anywhere_in_an_error_gets_the_hint() {
        let inner = std::io::Error::new(ErrorKind::PermissionDenied, "Permission denied");
        let e: Box<dyn std::error::Error> = Box::new(vault::Error::Io(inner));
        let (data, keys) = (Path::new("/data"), Path::new("/keys"));
        let h = hint(&*e, data, keys).unwrap();
        let (uid, gid) = runs_as();
        assert!(h.starts_with(&format!("Sentry Mate runs as uid {uid} (group {gid})")), "{h}");
        assert!(
            h.ends_with(&format!(
                "sudo chown -R {uid}:{gid} <the folder mounted at /data> <the folder mounted at /keys>"
            )),
            "{h}"
        );
        let other: Box<dyn std::error::Error> = Box::new(std::io::Error::other("something else"));
        assert!(hint(&*other, data, keys).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_this_user_cant_write_to_is_explained() {
        use std::os::unix::fs::PermissionsExt;
        if runs_as().0 == 0 {
            return; // root writes anywhere: nothing to see
        }
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir(&data).unwrap();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o555)).unwrap();
        let e = check(&data, "data", true).unwrap_err();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(e.contains("its permissions (555) don't let it"), "{e}");
    }
}
