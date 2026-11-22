//! The write gate of a data folder: what lets a copy of the folder (an export's snapshot, a move's last round) see
//! one moment while the server runs.
//!
//! Files in the data folder depend on each other (a trust head names the generation of its records, a log's count is
//! in `counts.json`). Every write that changes the data folder holds a [`Pass`] while it writes; a [`Pause`] waits
//! for the writes already under way, holds new ones back, and while it lasts nothing in the folder changes.
//!
//! Across processes (the server, a command run with `docker exec`) it is two plain `flock` locks on empty files at
//! the top of the data folder:
//!
//! - `.gate`: writers hold it shared, a pause holds it exclusively.
//! - `.gate-door`: a writer takes it exclusively, takes `.gate` shared and lets go of the door at once; a pause takes
//!   the door and keeps it, then takes `.gate`. `flock` gives a waiting exclusive lock no priority over new shared
//!   ones, so without the door a busy server (writes that overlap without end) would never let a pause in. With it,
//!   a pause waits only for the writers already inside, and new writers wait at the door.
//!
//! Inside one process the writers are counted in memory: the process holds `.gate` shared while any of them is in,
//! and a pause of this process makes new writers wait before they touch a lock at all. Each new writer still looks at
//! the door, so a pause from another process holds it back too.
//!
//! A gate can be **closed** ([`Gate::close`]): from then on every writer is refused with the reason given, also one
//! that was waiting when it closed. The check is made together with the count, so a writer never gets in after the
//! gate closed. (A server whose data moved away closes it for good; one that brings an import in closes it while it
//! switches.)
//!
//! `<data>/.lock` ([`ServerLock`]) is held by the running server its whole life, so a second server can't open the
//! same data folder and a command can tell whether a server runs.
//!
//! The locks need a local file system with `flock` (ZFS, ext4, xfs, btrfs).

use rustix::fs::{FlockOperation, flock};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Held shared by writers, exclusively by a pause.
pub const GATE: &str = ".gate";
/// Taken by every writer on its way in, kept by a pause.
pub const DOOR: &str = ".gate-door";
/// Held by the running server.
pub const LOCK: &str = ".lock";
/// How often a waiting writer or pause looks again.
pub const STEP: Duration = Duration::from_millis(5);

/// Why the gate is closed: what a refused upload is answered (`503 <code>`, `Retry-After`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closed {
    pub code: &'static str,
    pub message: String,
    pub retry_after_s: u64,
}

/// Why a writer or a pause didn't get in.
#[derive(Debug)]
pub enum Refused {
    /// A pause (or, for a pause, a writer) held on past the wait.
    Busy,
    /// The gate is closed.
    Closed(Closed),
    Io(io::Error),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Busy => write!(f, "the data folder is paused for a moment (a copy is being taken); try again"),
            Refused::Closed(c) => write!(f, "{}", c.message),
            Refused::Io(e) => write!(f, "the data folder's write gate: {e}"),
        }
    }
}

impl std::error::Error for Refused {}

struct Files {
    dir: PathBuf,
    door: File,
    gate: File,
}

#[derive(Default)]
struct State {
    /// Writers of this process inside now.
    inside: usize,
    /// Pauses of this process waiting or held.
    pausing: usize,
    closed: Option<Closed>,
}

struct Inner {
    files: Option<Files>,
    state: Mutex<State>,
}

/// The gate of one data folder, for this process (cheap to clone; every clone is the same gate).
#[derive(Clone)]
pub struct Gate(Arc<Inner>);

/// Held by a writer while it writes. Clones are the same pass: the writer is out when the last one is dropped.
#[derive(Clone)]
pub struct Pass(#[allow(dead_code)] Arc<PassInner>);

struct PassInner(Arc<Inner>);

impl Drop for PassInner {
    fn drop(&mut self) {
        self.0.leave();
    }
}

/// While it is held, nothing writes to the data folder (through the gate). Dropped: writes go on.
pub struct Pause {
    inner: Arc<Inner>,
    held: Option<(File, File)>,
}

impl Drop for Pause {
    fn drop(&mut self) {
        if let Some((door, gate)) = self.held.take() {
            let _ = flock(&gate, FlockOperation::Unlock);
            let _ = flock(&door, FlockOperation::Unlock);
        }
        let mut st = self.inner.lock();
        st.pausing = st.pausing.saturating_sub(1);
    }
}

fn would_block(e: rustix::io::Errno) -> bool {
    e == rustix::io::Errno::WOULDBLOCK || e == rustix::io::Errno::AGAIN
}

fn lock_file(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)
}

/// Takes `op` (a non-blocking lock) on `f`, trying again until `until`.
fn flock_until(f: &File, op: FlockOperation, until: Instant) -> Result<(), Refused> {
    loop {
        match flock(f, op) {
            Ok(()) => return Ok(()),
            Err(e) if would_block(e) => {}
            Err(e) => return Err(Refused::Io(e.into())),
        }
        if Instant::now() >= until {
            return Err(Refused::Busy);
        }
        std::thread::sleep(STEP);
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn leave(&self) {
        let mut st = self.lock();
        st.inside = st.inside.saturating_sub(1);
        if st.inside == 0
            && let Some(f) = &self.files
        {
            let _ = flock(&f.gate, FlockOperation::Unlock);
        }
    }
}

impl Gate {
    /// The gate of the data folder `data` (its two lock files are made if missing).
    pub fn open(data: &Path) -> io::Result<Gate> {
        let (door, gate) = (lock_file(&data.join(DOOR))?, lock_file(&data.join(GATE))?);
        let files = Some(Files { dir: data.to_path_buf(), door, gate });
        Ok(Gate(Arc::new(Inner { files, state: Mutex::new(State::default()) })))
    }

    /// A gate for this process only, with no lock files (tests, and code that keeps nothing in a data folder).
    pub fn memory() -> Gate {
        Gate(Arc::new(Inner { files: None, state: Mutex::new(State::default()) }))
    }

    /// Goes in if it can now. `Ok(None)`: a pause holds the gate or waits for it (try again later).
    pub fn try_enter(&self) -> Result<Option<Pass>, Refused> {
        let inner = &self.0;
        let mut st = inner.lock();
        if let Some(c) = &st.closed {
            return Err(Refused::Closed(c.clone()));
        }
        if st.pausing > 0 {
            return Ok(None);
        }
        if let Some(f) = &inner.files {
            match flock(&f.door, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => {}
                Err(e) if would_block(e) => return Ok(None),
                Err(e) => return Err(Refused::Io(e.into())),
            }
            // with the door held no pause holds the gate (a pause keeps the door until it has let go of the gate)
            let shared = if st.inside == 0 { flock(&f.gate, FlockOperation::NonBlockingLockShared) } else { Ok(()) };
            let _ = flock(&f.door, FlockOperation::Unlock);
            match shared {
                Ok(()) => {}
                Err(e) if would_block(e) => return Ok(None),
                Err(e) => return Err(Refused::Io(e.into())),
            }
        }
        st.inside += 1;
        Ok(Some(Pass(Arc::new(PassInner(inner.clone())))))
    }

    /// Goes in, waiting at most `wait` for a pause to end ([`Refused::Busy`] after that).
    pub fn enter(&self, wait: Duration) -> Result<Pass, Refused> {
        let until = Instant::now() + wait;
        loop {
            if let Some(p) = self.try_enter()? {
                return Ok(p);
            }
            if Instant::now() >= until {
                return Err(Refused::Busy);
            }
            std::thread::sleep(STEP);
        }
    }

    /// Pauses the writes: new writers wait from now on, and this returns once the writes under way (in every
    /// process) are done, or [`Refused::Busy`] if they take longer than `wait` (the writers then go on as before).
    pub fn pause(&self, wait: Duration) -> Result<Pause, Refused> {
        let until = Instant::now() + wait;
        self.0.lock().pausing += 1;
        let mut pause = Pause { inner: self.0.clone(), held: None };
        match &self.0.files {
            None => loop {
                if self.0.lock().inside == 0 {
                    break;
                }
                if Instant::now() >= until {
                    return Err(Refused::Busy);
                }
                std::thread::sleep(STEP);
            },
            Some(f) => {
                // handles of its own: a lock on this process's handles would not keep this process's writers out
                let door = lock_file(&f.dir.join(DOOR)).map_err(Refused::Io)?;
                flock_until(&door, FlockOperation::NonBlockingLockExclusive, until)?;
                let gate = lock_file(&f.dir.join(GATE)).map_err(Refused::Io)?;
                flock_until(&gate, FlockOperation::NonBlockingLockExclusive, until)?;
                pause.held = Some((door, gate));
            }
        }
        Ok(pause)
    }

    /// Closes the gate: every writer from now on is refused with `why`, one waiting at this moment too.
    pub fn close(&self, why: Closed) {
        self.0.lock().closed = Some(why);
    }

    /// Opens a closed gate again.
    pub fn reopen(&self) {
        self.0.lock().closed = None;
    }

    pub fn closed(&self) -> Option<Closed> {
        self.0.lock().closed.clone()
    }

    /// Writers of this process inside now.
    pub fn inside(&self) -> usize {
        self.0.lock().inside
    }
}

/// `<data>/.lock`, held by the running server while it runs.
pub struct ServerLock {
    _file: File,
}

impl ServerLock {
    /// Takes it if no one holds it (`None`: a server runs on this data folder, or a command that needs it alone).
    pub fn try_take(data: &Path) -> io::Result<Option<ServerLock>> {
        let f = lock_file(&data.join(LOCK))?;
        match flock(&f, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Some(ServerLock { _file: f })),
            Err(e) if would_block(e) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// Whether a server (or a command holding the lock) uses the data folder now.
pub fn server_running(data: &Path) -> io::Result<bool> {
    if !data.is_dir() {
        return Ok(false);
    }
    Ok(ServerLock::try_take(data)?.is_none())
}
