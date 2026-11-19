//! Queries that decode stored logs while they answer (`/signals`, `/signals/latest`, the pack readings of the battery
//! pages): how far one answer may go, and how the caller stops one whose request is gone.
//!
//! The web side runs such a query inside [`with_cancel`] with a flag it sets when the request is dropped (the client
//! went away, or the request timed out); the decoders look at it between logs and every few thousand lines, and end
//! the query with [`QueryError::Engine`] once it is set. One answer decodes at most [`MAX_DECODED_BYTES`] of log
//! lines, shared out evenly over the logs it reads, so a period of large logs is a thinner read, never an unbounded
//! one.

use super::QueryError;
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Log lines (bytes, as candump text) one answer decodes at most.
pub const MAX_DECODED_BYTES: u64 = 2 << 30;
/// Lines between two looks at the cancel flag.
const LOOK_EVERY: u32 = 4096;

thread_local! {
    static CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// Runs `f` (a query, on this thread) so that it stops early once `flag` is set.
pub fn with_cancel<R>(flag: Arc<AtomicBool>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Arc<AtomicBool>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let old = self.0.take();
            CANCEL.with(|c| *c.borrow_mut() = old);
        }
    }
    let _restore = Restore(CANCEL.with(|c| c.borrow_mut().replace(flag)));
    f()
}

/// Whether the query running on this thread was cancelled.
pub fn cancelled() -> bool {
    CANCEL.with(|c| c.borrow().as_ref().is_some_and(|f| f.load(Ordering::Relaxed)))
}

/// The error a cancelled query ends with (nobody reads it: its request is gone).
pub(crate) fn stopped() -> QueryError {
    QueryError::Engine(crate::Error::Store("the request was dropped".into()))
}

/// Reads `xz` line by line into `on_line`, within `budget` bytes of lines, and `Err` once the query is cancelled.
/// The bytes of lines read (a log that cannot be read to its end gives what came before the trouble).
pub(crate) fn read(xz: &[u8], budget: u64, mut on_line: impl FnMut(&[u8])) -> Result<u64, QueryError> {
    if cancelled() {
        return Err(stopped());
    }
    let (mut used, mut n, mut gone) = (0u64, 0u32, false);
    let _ = logfmt::read_log_while(xz, logfmt::Limits::default(), |l| {
        n = n.wrapping_add(1);
        if n % LOOK_EVERY == 0 && cancelled() {
            gone = true;
            return false;
        }
        used += l.len() as u64 + 1;
        if used > budget {
            return false;
        }
        on_line(l);
        true
    });
    if gone || cancelled() {
        return Err(stopped());
    }
    Ok(used.min(budget))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_flag_is_seen_only_inside_and_restored_after() {
        assert!(!cancelled());
        let flag = Arc::new(AtomicBool::new(false));
        with_cancel(flag.clone(), || {
            assert!(!cancelled());
            flag.store(true, Ordering::Relaxed);
            assert!(cancelled());
            assert!(read(b"", 10, |_| {}).is_err(), "a cancelled query reads nothing");
        });
        assert!(!cancelled());
    }

    #[test]
    fn a_log_is_read_within_its_budget() {
        let mut text = Vec::new();
        for i in 0..1000 {
            text.extend_from_slice(format!("(1.{i:06}) can0 123#00\n").as_bytes());
        }
        let mut xz = Vec::new();
        lzma_rs::xz_compress(&mut std::io::BufReader::new(&text[..]), &mut xz).unwrap();
        let mut seen = 0u64;
        let used = read(&xz, 2500, |l| seen += l.len() as u64 + 1).unwrap();
        assert!(used <= 2500 && seen <= 2500 && seen > 2000, "{used} {seen}");
        let mut lines = 0;
        assert_eq!(read(&xz, u64::MAX, |_| lines += 1).unwrap(), text.len() as u64);
        assert_eq!(lines, 1000);
    }
}
