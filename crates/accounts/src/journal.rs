//! A sealed, append-only journal of small JSON lines, on top of [`vault::Records`].
//!
//! Things that change often (sessions seen, accounts added) are written as one line each at the end of a journal
//! instead of rewriting a whole file every time; the state is the journal replayed. When the journal holds many more
//! lines than the state needs, it is written again from the state (compaction: a new file, synced, renamed into
//! place), so it stays small and each change costs one short append (amortized), whatever the number of accounts or
//! sessions.
//!
//! Lines are separated by `\n` (JSON keeps newlines inside strings escaped). An append bigger than one sealed record
//! (64 KiB) is split over several; if a crash keeps only the first of them, the cut line is skipped when read, and
//! the next append starts on a new line so it can't be glued onto it.

use crate::error::{Result, corrupt};
use std::path::{Path, PathBuf};
use vault::{Records, Vault};

pub(crate) struct Journal {
    records: Records,
    path: PathBuf,
    label: String,
    lines: u64,
    /// The last line wasn't finished (a cut append): start the next append with a newline.
    torn: bool,
    /// An append failed and the file couldn't be read again since: nothing more is added until it can.
    stale: bool,
    /// Tests: make the next append fail the way a full disk does.
    #[cfg(test)]
    pub(crate) fail_next_append: bool,
}

fn sync_dir(path: &Path) {
    if let Some(d) = path.parent() {
        if let Ok(d) = std::fs::File::open(d) {
            let _ = d.sync_all();
        }
    }
}

/// Reads every whole line; returns how many there were and whether the last one was cut short.
fn scan(records: &mut Records, line: &mut impl FnMut(&[u8])) -> Result<(u64, bool)> {
    let (mut buf, mut lines) = (Vec::new(), 0u64);
    records
        .read_all(|piece| {
            buf.extend_from_slice(piece);
            let mut start = 0;
            while let Some(i) = buf[start..].iter().position(|&b| b == b'\n') {
                let l = &buf[start..start + i];
                if !l.is_empty() {
                    line(l);
                }
                lines += 1;
                start += i + 1;
            }
            buf.drain(..start);
            Ok(())
        })
        .map_err(vault::unwrap_io)?;
    Ok((lines, !buf.is_empty()))
}

impl Journal {
    /// Opens (or starts) the journal at `path` and hands each whole line to `line`, oldest first.
    pub(crate) fn open(vault: &Vault, path: &Path, label: &str, mut line: impl FnMut(&[u8])) -> Result<Journal> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        // an interrupted compaction leaves its new file behind; the old journal is still whole
        match std::fs::remove_file(path.with_extension("new")) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        let mut records = vault.records(path, label)?;
        let (lines, torn) = scan(&mut records, &mut line)?;
        Ok(Journal {
            records,
            path: path.to_path_buf(),
            label: label.to_string(),
            lines,
            torn,
            stale: false,
            #[cfg(test)]
            fail_next_append: false,
        })
    }

    /// Hands each whole line to `line` again, oldest first (through this handle, so it never races its appends).
    pub(crate) fn read(&mut self, mut line: impl FnMut(&[u8])) -> Result<()> {
        scan(&mut self.records, &mut line).map(|_| ())
    }

    /// Lines written so far (since the last compaction).
    pub(crate) fn lines(&self) -> u64 {
        self.lines
    }

    fn body<T: serde::Serialize>(items: &[T], torn: bool) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(items.len() * 96 + 1);
        if torn {
            out.push(b'\n');
        }
        for it in items {
            serde_json::to_writer(&mut out, it).map_err(corrupt)?;
            out.push(b'\n');
        }
        Ok(out)
    }

    /// Adds lines at the end, synced to disk before it returns.
    ///
    /// When writing fails (a full disk, say), the file is opened again before anything more is added:
    /// `vault::Records` has already counted the records it was asked to write by then, so the next append would
    /// number its records after ones that aren't there, and the whole journal would no longer open. Opened again, it
    /// counts the records really on disk (a cut one is dropped) and carries on after them.
    pub(crate) fn append<T: serde::Serialize>(&mut self, vault: &Vault, items: &[T]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        if self.stale {
            self.reopen(vault)?;
        }
        let body = Self::body(items, self.torn)?;
        if let Err(e) = self.write(&body) {
            self.stale = true;
            // if this fails too, the next append tries again first
            let _ = self.reopen(vault);
            return Err(e.into());
        }
        self.torn = false;
        self.lines += items.len() as u64;
        Ok(())
    }

    fn write(&mut self, body: &[u8]) -> std::io::Result<()> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_append) {
            // what a failed write leaves behind: the records counted by `vault::Records`, the disk without them
            let len = std::fs::metadata(&self.path)?.len();
            self.records.append(body)?;
            std::fs::OpenOptions::new().write(true).open(&self.path)?.set_len(len)?;
            return Err(std::io::Error::other("no space left on device (test)"));
        }
        self.records.append(body)
    }

    /// Opens the file again and counts what is really in it (after a failed append).
    fn reopen(&mut self, vault: &Vault) -> Result<()> {
        let mut records = vault.records(&self.path, &self.label)?;
        let (lines, torn) = scan(&mut records, &mut |_| {})?;
        (self.records, self.lines, self.torn, self.stale) = (records, lines, torn, false);
        Ok(())
    }

    /// Replaces the whole journal with these lines (the state as it is now): written to a new file, synced, then
    /// renamed over the old one, so a crash leaves one or the other whole.
    pub(crate) fn rewrite<T: serde::Serialize>(&mut self, vault: &Vault, items: &[T]) -> Result<()> {
        let tmp = self.path.with_extension("new");
        match std::fs::remove_file(&tmp) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        let mut records = vault.records(&tmp, &self.label)?;
        records.append(&Self::body(items, false)?)?;
        std::fs::rename(&tmp, &self.path)?;
        sync_dir(&self.path);
        self.records = records;
        self.lines = items.len() as u64;
        self.torn = false;
        self.stale = false;
        Ok(())
    }

    /// Whether the journal is worth compacting: many more lines than the `live` ones the state needs.
    pub(crate) fn wants_compaction(&self, live: usize) -> bool {
        self.lines > 1024 + 4 * live as u64
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use std::io::Write;

    pub(crate) fn vault() -> (tempfile::TempDir, std::path::PathBuf, Vault) {
        let t = tempfile::tempdir().unwrap();
        let (data, keys) = (t.path().join("data"), t.path().join("keys"));
        std::fs::create_dir_all(&data).unwrap();
        let (v, _) = Vault::create(&data, &keys, vault::Kdf::FAST_FOR_TESTS).unwrap();
        (t, data, v)
    }

    fn read(v: &Vault, path: &Path, label: &str) -> (Vec<String>, Journal) {
        let mut lines = Vec::new();
        let j = Journal::open(v, path, label, |l| lines.push(String::from_utf8(l.to_vec()).unwrap())).unwrap();
        (lines, j)
    }

    #[test]
    fn lines_come_back_in_order_across_records_and_reopening() {
        let (_t, data, v) = vault();
        let path = data.join("x.journal");
        let (lines, mut j) = read(&v, &path, "x");
        assert!(lines.is_empty());
        j.append(&v, &[1, 2, 3]).unwrap();
        // one append bigger than a sealed record (64 KiB)
        let big: Vec<String> = (0..2000).map(|i| format!("{i:0>60}")).collect();
        j.append(&v, &big).unwrap();
        j.append(&v, &["last"]).unwrap();
        drop(j);
        let (lines, j) = read(&v, &path, "x");
        assert_eq!(lines.len(), 3 + 2000 + 1);
        assert_eq!(&lines[..3], ["1", "2", "3"]);
        assert_eq!(lines[3 + 1999], format!("\"{:0>60}\"", 1999));
        assert_eq!(lines[2003], "\"last\"");
        assert_eq!(j.lines(), 2004);
        // under another label it doesn't open
        assert!(Journal::open(&v, &path, "y", |_| {}).is_err());
    }

    #[test]
    fn a_line_cut_by_a_crash_is_skipped_and_not_glued_to_the_next() {
        let (_t, data, v) = vault();
        let path = data.join("x.journal");
        let mut j = Journal::open(&v, &path, "x", |_| {}).unwrap();
        j.append(&v, &["a"]).unwrap();
        // what a crash in the middle of a multi-record append leaves: a line with no end
        j.records.append(b"{\"half").unwrap();
        drop(j);
        let (lines, mut j) = read(&v, &path, "x");
        assert_eq!(lines, ["\"a\""]);
        assert!(j.torn);
        j.append(&v, &["b"]).unwrap();
        drop(j);
        let (lines, _) = read(&v, &path, "x");
        assert_eq!(lines, ["\"a\"", "{\"half", "\"b\""], "the cut line stands alone (and won't parse)");
    }

    #[test]
    fn a_failed_append_does_not_break_the_journal() {
        let (_t, data, v) = vault();
        let path = data.join("x.journal");
        let mut j = Journal::open(&v, &path, "x", |_| {}).unwrap();
        j.append(&v, &["a"]).unwrap();
        // a full disk: the append fails, then space is freed and work carries on
        j.fail_next_append = true;
        assert!(j.append(&v, &["lost"]).is_err());
        j.append(&v, &["b", "c"]).unwrap();
        assert_eq!(j.lines(), 3);
        let mut now = Vec::new();
        j.read(|l| now.push(String::from_utf8(l.to_vec()).unwrap())).unwrap();
        assert_eq!(now, ["\"a\"", "\"b\"", "\"c\""]);
        drop(j);
        // and after a restart everything written since still opens
        let (lines, _) = read(&v, &path, "x");
        assert_eq!(lines, ["\"a\"", "\"b\"", "\"c\""]);
    }

    #[test]
    fn compaction_replaces_the_journal_whole() {
        let (_t, data, v) = vault();
        let path = data.join("x.journal");
        let mut j = Journal::open(&v, &path, "x", |_| {}).unwrap();
        for i in 0..3000 {
            j.append(&v, &[i]).unwrap();
        }
        assert!(j.wants_compaction(10));
        assert!(!j.wants_compaction(1000));
        j.rewrite(&v, &["state"]).unwrap();
        j.append(&v, &["after"]).unwrap();
        assert_eq!(j.lines(), 2);
        drop(j);
        let (lines, _) = read(&v, &path, "x");
        assert_eq!(lines, ["\"state\"", "\"after\""]);
        // an interrupted compaction's file is dropped, the journal kept
        std::fs::File::create(path.with_extension("new")).unwrap().write_all(b"junk").unwrap();
        let (lines, _) = read(&v, &path, "x");
        assert_eq!(lines.len(), 2);
        assert!(!path.with_extension("new").exists());
    }
}
