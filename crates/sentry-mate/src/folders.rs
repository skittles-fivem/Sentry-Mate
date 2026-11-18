//! The data and keys folders, checked before they are used. A folder Sentry Mate can't write to (most often one
//! made by root: by Docker for a bind mount whose folder was missing, or by `sudo mkdir`) stops it with what to do,
//! not with "Permission denied (os error 13)".
//!
//! In the image (`SENTRY_MATE_REQUIRE_MOUNTS=1`) each folder must also be a mount of its own: a folder left out of
//! the compose file would otherwise be the container's own, and lost with it on the next update. And the keys folder
//! must not be the data folder, or a folder right inside it, on the server: the key file would be in the data's
//! backups.

use std::ffi::OsStr;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

/// What is mounted where, as this process sees it (Linux).
pub const MOUNTINFO: &str = "/proc/self/mountinfo";

/// Who this process runs as: (uid, gid).
pub fn runs_as() -> (u32, u32) {
    (rustix::process::geteuid().as_raw(), rustix::process::getegid().as_raw())
}

/// Whether `SENTRY_MATE_REQUIRE_MOUNTS` (its value here) asks for the mount check: set to anything but nothing, 0,
/// false, no or off.
pub fn mounts_required(value: Option<&OsStr>) -> bool {
    value.is_some_and(|v| {
        let v = v.to_string_lossy();
        let v = v.trim();
        !v.is_empty() && !["0", "false", "no", "off"].iter().any(|off| v.eq_ignore_ascii_case(off))
    })
}

/// Checks both folders before a command uses them: there, a folder, writable (`writes`: data, keys) or readable, and
/// with `mountinfo` (the text of [`MOUNTINFO`], when the mounts are required) each a mount of its own. Then that the
/// keys folder is apart from the data folder ([`apart`]). All that is wrong with both is said at once, so that two
/// folders Docker made as root, or two lines missing from the compose file, are named together.
pub fn check_all(data: &Path, keys: &Path, writes: (bool, bool), mountinfo: Option<&str>) -> Result<(), String> {
    // a keys folder inside the data folder (as given) is refused by the vault: not made or looked at here
    let keys_inside = keys.starts_with(data);
    let mut dirs = vec![(data, "data", writes.0)];
    if !keys_inside {
        dirs.push((keys, "keys", writes.1));
    }
    let unmounted: Vec<(&Path, &str)> = match mountinfo {
        Some(m) => dirs.iter().filter(|(dir, ..)| !mounted(m, dir)).map(|&(dir, what, _)| (dir, what)).collect(),
        None => Vec::new(),
    };
    let mut wrong = Vec::new();
    if !unmounted.is_empty() {
        wrong.push(not_mounted(&unmounted));
    }
    for (dir, what, write) in dirs {
        if !unmounted.iter().any(|&(d, _)| d == dir) {
            wrong.extend(check(dir, what, write).err());
        }
    }
    if !wrong.is_empty() {
        return Err(wrong.join("\n"));
    }
    if keys_inside {
        return Ok(());
    }
    apart(data, keys)
}

/// Whether the folder is a mount of its own: its real path is a mount point. A folder that isn't there isn't one.
fn mounted(mountinfo: &str, dir: &Path) -> bool {
    std::fs::canonicalize(dir).is_ok_and(|real| is_mount_point(mountinfo, &real))
}

/// Whether `real` (a real path: absolute, no `..`, no symbolic links) is a mount point in `mountinfo`, the text of
/// [`MOUNTINFO`]: the fifth field of one of its lines.
pub fn is_mount_point(mountinfo: &str, real: &Path) -> bool {
    mountinfo.lines().filter_map(|line| line.split(' ').nth(4)).any(|point| unescape(point) == real)
}

/// A mountinfo field as a path: the kernel writes a space, a tab, a newline and a backslash as `\040`, `\011`,
/// `\012` and `\134`.
fn unescape(field: &str) -> PathBuf {
    let b = field.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        let octal = b.get(i + 1..i + 4).filter(|o| c == b'\\' && o.iter().all(|d| (b'0'..=b'7').contains(d)));
        match octal.and_then(|o| u8::from_str_radix(std::str::from_utf8(o).ok()?, 8).ok()) {
            Some(byte) => {
                out.push(byte);
                i += 4;
            }
            None => {
                out.push(c);
                i += 1;
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(out))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(&out).into_owned())
    }
}

/// The message for folders with nothing mounted on them (data, keys or both): what would be lost, and the lines to
/// add to the compose file and the TrueNAS app.
fn not_mounted(dirs: &[(&Path, &str)]) -> String {
    let line = |dir: &Path, what: &str, truenas: bool| {
        let server = match (what, truenas) {
            ("keys", false) => "./keys",
            ("keys", true) => "/mnt/tank/apps/sentry-mate-keys",
            (_, false) => "./data",
            (_, true) => "/mnt/tank/apps/sentry-mate",
        };
        format!("\n  - {server}:{}", dir.display())
    };
    let lines = |truenas: bool| dirs.iter().map(|&(dir, what)| line(dir, what, truenas)).collect::<String>();
    let named = |&(dir, what): &(&Path, &str)| format!("the {what} folder {}", dir.display());
    let (head, lost, mount, datasets) = match dirs {
        [one @ (_, "keys")] => (
            format!("{} is not mounted", named(one)),
            "The key file written there would be lost when the container is replaced (an update does that), and \
             the data would then open only with the recovery key",
            "Mount a folder of the server there, not inside the data folder",
            "your keys dataset's path",
        ),
        [one] => (
            format!("{} is not mounted", named(one)),
            "Everything Sentry Mate keeps there would be lost when the container is replaced (an update does that)",
            "Mount a folder of the server there",
            "your data dataset's path",
        ),
        _ => (
            format!("{} are not mounted", dirs.iter().map(named).collect::<Vec<_>>().join(" and ")),
            "What Sentry Mate keeps there would be lost when the container is replaced (an update does that)",
            "Mount a folder of the server at each, the keys one not inside the data one",
            "your datasets' paths",
        ),
    };
    format!(
        "{head}. {lost}.\n{mount}. In the compose file, under volumes:{}\nOn TrueNAS, with {datasets}:{}",
        lines(false),
        lines(true)
    )
}

/// Refuses a keys folder that is the data folder, or one of the folders right inside it, through a mount of its own
/// (the same folder of the server mounted at both, or the keys folder inside the data folder there): the key file
/// would then be in the data's backups. Compared by device and inode, which a mount keeps. A folder that can't be
/// looked at is left to the vault.
pub fn apart(data: &Path, keys: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let id = |m: &std::fs::Metadata| (m.dev(), m.ino());
        let (Ok(d), Ok(k)) = (std::fs::metadata(data), std::fs::metadata(keys)) else { return Ok(()) };
        let k = id(&k);
        if id(&d) == k {
            return Err(not_apart(data, keys, None));
        }
        let Ok(entries) = std::fs::read_dir(data) else { return Ok(()) };
        // (an entry's metadata is looked up through a mount on it: a folder mounted there has the mount's own)
        if let Some(e) = entries.flatten().find(|e| e.metadata().is_ok_and(|m| id(&m) == k)) {
            return Err(not_apart(data, keys, Some(&e.file_name())));
        }
    }
    #[cfg(not(unix))]
    let _ = (data, keys);
    Ok(())
}

/// The message for a keys folder that is the data folder (`inside`: `None`) or the folder `inside` in it.
fn not_apart(data: &Path, keys: &Path, inside: Option<&OsStr>) -> String {
    let (d, k) = (data.display(), keys.display());
    let (what, keep) = match inside {
        None => (
            format!("the keys folder {k} and the data folder {d} are the same folder of the server"),
            format!("move {} into it first", vault::KEY_FILE),
        ),
        Some(name) => (
            format!(
                "the keys folder {k} is a folder inside the data folder on the server (it is {} here)",
                data.join(name).display()
            ),
            "copy what is in it there first".to_owned(),
        ),
    };
    format!(
        "{what}. The key file would be in every backup of the data, and anyone with a backup could open the \
         data.\nMount a folder of its own at {k}, not inside the data folder (on TrueNAS: a dataset of its own, not \
         inside the data dataset). To keep the key file, {keep} (docs/install.md, When it does not start)"
    )
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

    /// A container's mountinfo as Docker makes it (shortened): its root, /proc, /dev, a file it puts in, and a bind
    /// mount at each of `points`.
    fn mountinfo(points: &[&Path]) -> String {
        let mut s = String::from(
            "1520 1393 0:330 / / ro,relatime master:1 - overlay overlay rw,lowerdir=/var/lib/docker/overlay2/l/A\n\
             1521 1520 0:333 / /proc rw,nosuid,nodev,noexec,relatime - proc proc rw\n\
             1522 1520 0:334 / /dev rw,nosuid - tmpfs tmpfs rw,size=65536k,mode=755\n\
             1530 1520 8:1 /var/lib/docker/containers/c/hosts /etc/hosts rw,relatime - ext4 /dev/sda1 rw\n",
        );
        for (n, p) in points.iter().enumerate() {
            let p = p.display().to_string().replace('\\', "\\134").replace(' ', "\\040");
            s += &format!("{} 1520 8:1 /srv/sentry-mate/{n} {p} rw,relatime - ext4 /dev/sda1 rw\n", 1540 + n);
        }
        s
    }

    #[test]
    fn the_image_s_setting_asks_for_the_mount_check() {
        for on in ["1", "yes", "true", " 1 "] {
            assert!(mounts_required(Some(OsStr::new(on))), "{on}");
        }
        for off in ["", "0", "false", "No", "OFF", " "] {
            assert!(!mounts_required(Some(OsStr::new(off))), "{off:?}");
        }
        assert!(!mounts_required(None));
    }

    #[test]
    fn mount_points_are_read_from_mountinfo() {
        let m = mountinfo(&[Path::new("/data"), Path::new("/keys")]);
        for yes in ["/data", "/keys", "/", "/proc", "/etc/hosts"] {
            assert!(is_mount_point(&m, Path::new(yes)), "{yes}");
        }
        // a folder inside a mount, or with a name that only starts the same, is not one; nor is a mount's source
        for no in ["/data/can", "/dat", "/data2", "/srv/sentry-mate/0", "/mnt"] {
            assert!(!is_mount_point(&m, Path::new(no)), "{no}");
        }
        let m = mountinfo(&[Path::new("/data")]);
        assert!(!is_mount_point(&m, Path::new("/keys")));
        assert!(!is_mount_point("", Path::new("/data")));

        // TrueNAS: a ZFS dataset, with optional fields after the options
        let zfs = "1600 1520 0:61 / /data rw,noatime shared:5 master:1 - zfs tank/apps/sentry-mate rw,xattr,posixacl";
        assert!(is_mount_point(zfs, Path::new("/data")));
        // spaces and backslashes are written in octal
        let odd = mountinfo(&[Path::new("/my data"), Path::new("/a\\b")]);
        assert!(odd.contains(" /my\\040data rw") && odd.contains(" /a\\134b rw"), "{odd}");
        assert!(is_mount_point(&odd, Path::new("/my data")) && is_mount_point(&odd, Path::new("/a\\b")));
        assert!(!is_mount_point(&odd, Path::new("/my\\040data")));
        // a backslash that starts no octal number is kept
        assert!(is_mount_point("1 2 0:1 / /x\\9 rw - ext4 /dev/sda1 rw", Path::new("/x\\9")));
    }

    #[test]
    fn with_the_setting_a_folder_that_is_not_mounted_stops_it() {
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir(&data).unwrap();
        std::fs::create_dir(&keys).unwrap();
        let (d, k) = (std::fs::canonicalize(&data).unwrap(), std::fs::canonicalize(&keys).unwrap());
        let writes = (true, true);

        check_all(&data, &keys, writes, Some(&mountinfo(&[&d, &k]))).unwrap();

        let e = check_all(&data, &keys, writes, Some(&mountinfo(&[&d]))).unwrap_err();
        let (dd, kd) = (data.display(), keys.display());
        assert!(e.starts_with(&format!("the keys folder {kd} is not mounted. The key file written there")), "{e}");
        assert!(e.contains("only with the recovery key.\nMount a folder of the server there, not inside the data"));
        assert!(e.contains(&format!("under volumes:\n  - ./keys:{kd}\nOn TrueNAS, with your keys dataset's path:")));
        assert!(e.ends_with(&format!("\n  - /mnt/tank/apps/sentry-mate-keys:{kd}")), "{e}");
        assert!(!e.contains(&format!("the data folder {dd}")), "{e}");

        let e = check_all(&data, &keys, writes, Some(&mountinfo(&[&k]))).unwrap_err();
        assert_eq!(
            e,
            format!(
                "the data folder {dd} is not mounted. Everything Sentry Mate keeps there would be lost when the \
                 container is replaced (an update does that).\nMount a folder of the server there. In the compose \
                 file, under volumes:\n  - ./data:{dd}\nOn TrueNAS, with your data dataset's path:\n  - \
                 /mnt/tank/apps/sentry-mate:{dd}"
            )
        );

        // none: both named in one message, with both lines
        let e = check_all(&data, &keys, writes, Some(&mountinfo(&[]))).unwrap_err();
        assert!(e.starts_with(&format!("the data folder {dd} and the keys folder {kd} are not mounted. ")), "{e}");
        assert!(e.contains(&format!("under volumes:\n  - ./data:{dd}\n  - ./keys:{kd}\nOn TrueNAS")), "{e}");
        assert!(e.ends_with(&format!(":{dd}\n  - /mnt/tank/apps/sentry-mate-keys:{kd}")), "{e}");

        // nothing is made or written in a folder that is not mounted
        let none = dir.path().join("none");
        let e = check_all(&none, &keys, writes, Some(&mountinfo(&[&k]))).unwrap_err();
        assert!(e.starts_with(&format!("the data folder {} is not mounted.", none.display())), "{e}");
        assert!(!none.exists());
        assert_eq!(std::fs::read_dir(&data).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(&keys).unwrap().count(), 0);

        // a mounted folder is still checked like any other
        std::fs::remove_dir(&keys).unwrap();
        std::fs::write(&keys, b"x").unwrap();
        let e = check_all(&data, &keys, writes, Some(&mountinfo(&[&d, &k]))).unwrap_err();
        assert!(e.contains("is a file, not a folder") && !e.contains("not mounted"), "{e}");
    }

    #[test]
    fn without_the_setting_no_mount_is_needed() {
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        // (made by the check, as before: commands that write make a missing folder)
        check_all(&data, &keys, (true, true), None).unwrap();
        assert!(data.is_dir() && keys.is_dir());
        // the same folders, not mounted, with the setting
        assert!(check_all(&data, &keys, (true, true), Some(&mountinfo(&[]))).is_err());
        // a keys folder inside the data folder (as given) is left to the vault, which refuses it
        check_all(&data, &data.join("keys"), (true, true), None).unwrap();
        assert!(!data.join("keys").exists());
    }

    /// A symbolic link stands in for a second mount of a folder here: it reaches the same folder (the same device
    /// and inode) by another path, as a bind mount of the same folder of the server does.
    #[cfg(unix)]
    #[test]
    fn a_keys_folder_that_is_the_data_folder_or_right_inside_it_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(data.join("keys")).unwrap();
        std::fs::create_dir_all(data.join("can")).unwrap();

        // the keys folder of the server inside the data folder, mounted at both
        let keys = dir.path().join("keys");
        std::os::unix::fs::symlink(data.join("keys"), &keys).unwrap();
        let e = apart(&data, &keys).unwrap_err();
        let (dd, kd) = (data.display(), keys.display());
        assert!(
            e.starts_with(&format!(
                "the keys folder {kd} is a folder inside the data folder on the server (it is {dd}/keys here). The \
                 key file would be in every backup of the data, and anyone with a backup could open the data.\n"
            )),
            "{e}"
        );
        assert!(e.contains("not inside the data dataset). To keep the key file, copy what is in it there first"));
        // the same with the mounts required, both mounted
        let (d, k) = (std::fs::canonicalize(&data).unwrap(), std::fs::canonicalize(&keys).unwrap());
        let e = check_all(&data, &keys, (true, true), Some(&mountinfo(&[&d, &k]))).unwrap_err();
        assert!(e.contains("is a folder inside the data folder on the server"), "{e}");
        assert!(check_all(&data, &keys, (true, true), None).is_err());

        // the same folder of the server mounted at both
        let same = dir.path().join("same");
        std::os::unix::fs::symlink(&data, &same).unwrap();
        let e = apart(&data, &same).unwrap_err();
        assert!(
            e.starts_with(&format!(
                "the keys folder {} and the data folder {dd} are the same folder of the server. The key file would",
                same.display()
            )),
            "{e}"
        );
        assert!(e.contains("To keep the key file, move sentry-mate.key into it first"), "{e}");

        // a keys folder of its own is fine, even next to a folder named keys in the data folder
        let own = dir.path().join("own-keys");
        std::fs::create_dir(&own).unwrap();
        apart(&data, &own).unwrap();
        check_all(&data, &own, (true, true), None).unwrap();
        // one that can't be looked at is left to the vault
        apart(&data, &dir.path().join("none")).unwrap();
    }
}
