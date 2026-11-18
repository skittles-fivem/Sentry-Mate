#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use gate::{Closed, Gate, Refused, ServerLock};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn folder() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// Two gates on one folder, as two processes have (each its own lock handles).
fn two(dir: &tempfile::TempDir) -> (Gate, Gate) {
    (Gate::open(dir.path()).unwrap(), Gate::open(dir.path()).unwrap())
}

#[test]
fn a_pause_waits_for_the_write_under_way_and_holds_new_ones() {
    for across_processes in [true, false] {
        let dir = folder();
        let (a, b) = two(&dir);
        let b = if across_processes { b } else { a.clone() };
        let pass = a.enter(Duration::from_secs(1)).unwrap();
        let started = Instant::now();
        let paused = std::thread::scope(|s| {
            let p = s.spawn(|| b.pause(Duration::from_secs(5)).map(|p| (p, started.elapsed())));
            std::thread::sleep(Duration::from_millis(150));
            assert!(a.try_enter().unwrap().is_none(), "a new writer waits once a pause is waiting");
            drop(pass);
            p.join().unwrap()
        });
        let (pause, waited) = paused.unwrap();
        assert!(waited >= Duration::from_millis(150), "it waited for the write under way");
        assert!(a.try_enter().unwrap().is_none(), "held while paused");
        assert!(matches!(a.enter(Duration::from_millis(50)), Err(Refused::Busy)));
        drop(pause);
        assert!(a.try_enter().unwrap().is_some(), "writes go on after the pause");
    }
}

#[test]
fn a_pause_gives_up_after_its_wait_and_the_writers_go_on() {
    let dir = folder();
    let (a, b) = two(&dir);
    let pass = a.enter(Duration::from_secs(1)).unwrap();
    assert!(matches!(b.pause(Duration::from_millis(100)), Err(Refused::Busy)));
    let second = a.try_enter().unwrap().expect("not held back by a pause that gave up");
    drop((pass, second));
    assert!(b.pause(Duration::from_millis(100)).is_ok());
}

/// Writers that overlap without end (some writer is inside at every moment) never keep a pause out: new writers wait
/// at the door, and the pause waits only for those already in.
#[test]
fn a_pause_is_not_starved_by_writers_that_overlap_without_end() {
    for across_processes in [true, false] {
        let dir = folder();
        let (a, b) = two(&dir);
        let pauser = if across_processes { b } else { a.clone() };
        let stop = Arc::new(AtomicBool::new(false));
        let writes = Arc::new(AtomicUsize::new(0));
        let during = Arc::new(AtomicUsize::new(0));
        let paused = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        for i in 0..8u64 {
            let (a, stop, writes, during, paused) =
                (a.clone(), stop.clone(), writes.clone(), during.clone(), paused.clone());
            threads.push(std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(i));
                while !stop.load(Ordering::Relaxed) {
                    if let Ok(p) = a.enter(Duration::from_millis(20)) {
                        if paused.load(Ordering::SeqCst) {
                            during.fetch_add(1, Ordering::SeqCst);
                        }
                        std::thread::sleep(Duration::from_millis(3));
                        writes.fetch_add(1, Ordering::Relaxed);
                        drop(p);
                    }
                }
            }));
        }
        std::thread::sleep(Duration::from_millis(100));
        let t = Instant::now();
        let pause = pauser.pause(Duration::from_secs(10)).expect("the pause gets in");
        paused.store(true, Ordering::SeqCst);
        assert!(t.elapsed() < Duration::from_secs(2), "it waited {:?}", t.elapsed());
        std::thread::sleep(Duration::from_millis(100));
        paused.store(false, Ordering::SeqCst);
        drop(pause);
        let before = writes.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(100));
        stop.store(true, Ordering::Relaxed);
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(during.load(Ordering::SeqCst), 0, "no writer got in while paused");
        assert!(writes.load(Ordering::Relaxed) > before, "the writers went on afterwards");
    }
}

/// A writer that waited during a pause, which then closes the gate (a move's commit), is refused: it never gets in
/// once the pause ends.
#[test]
fn a_writer_waiting_when_the_gate_closes_is_refused() {
    let dir = folder();
    let g = Gate::open(dir.path()).unwrap();
    let pause = g.pause(Duration::from_secs(1)).unwrap();
    let got = std::thread::scope(|s| {
        let w = s.spawn(|| g.enter(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(100));
        g.close(Closed { code: "moved", message: "moved away".into(), retry_after_s: 3600 });
        drop(pause);
        w.join().unwrap()
    });
    assert!(matches!(got, Err(Refused::Closed(c)) if c.code == "moved"));
    assert!(matches!(g.try_enter(), Err(Refused::Closed(_))));
    g.reopen();
    assert!(g.try_enter().unwrap().is_some());
}

#[test]
fn a_pass_and_its_clones_leave_together() {
    let g = Gate::memory();
    let p = g.enter(Duration::from_secs(1)).unwrap();
    let q = p.clone();
    drop(p);
    assert_eq!(g.inside(), 1);
    assert!(g.pause(Duration::from_millis(50)).is_err(), "the clone still writes");
    drop(q);
    assert_eq!(g.inside(), 0);
    assert!(g.pause(Duration::from_millis(50)).is_ok());
}

#[test]
fn one_server_per_data_folder() {
    let dir = folder();
    let held = ServerLock::try_take(dir.path()).unwrap().expect("free");
    assert!(ServerLock::try_take(dir.path()).unwrap().is_none(), "a second server can't");
    assert!(gate::server_running(dir.path()).unwrap());
    drop(held);
    assert!(!gate::server_running(dir.path()).unwrap());
    assert!(ServerLock::try_take(dir.path()).unwrap().is_some());
}
