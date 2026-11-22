#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! A large folder goes out and back in in constant memory: the export holds one chunk and one entry at a time, the
//! import one chunk and one entry, whatever the number of files or their size. (One test in this file: the memory
//! read is the whole process's.)

mod common;

use std::io::Write;
use std::ops::ControlFlow;
use std::time::Duration;

/// The most memory the process has held at once (`VmHWM`), in bytes.
fn peak() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        .map_or(0, |kb| kb * 1024)
}

#[test]
fn a_large_folder_is_streamed_in_constant_memory() {
    if !std::path::Path::new("/proc/self/status").exists() {
        return;
    }
    let old = common::server();
    // 12,000 files in four folders, and one file of 32 MiB (sparse on disk: it starts like a sealed file, so it is
    // linked into the snapshot and read as stored)
    let mut head = b"SMENC\x01".to_vec();
    head.extend_from_slice(&[5u8; 16]);
    for d in 0..4 {
        let dir = old.data.join(format!("can/car{d}"));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..3000 {
            let mut b = head.clone();
            b.extend_from_slice(format!("file {d} {i}").as_bytes());
            std::fs::write(dir.join(format!("car{d}_20261108T1000{:05}Z_{i}.log.xz", i)), b).unwrap();
        }
    }
    const BIG: u64 = 32 << 20;
    let big = old.data.join("can/car0/big.log.xz");
    let mut f = std::fs::File::create(&big).unwrap();
    f.write_all(&head).unwrap();
    f.set_len(BIG).unwrap();
    drop(f);
    // the peak from here on (Linux 4.0 and later reset it on "5")
    let _ = std::fs::write("/proc/self/clear_refs", "5");
    let before = peak();

    let gate = gate::Gate::open(&old.data).unwrap();
    let snap = moving::Snapshot::make(&old.data, &old.vault, &gate, common::APP, Duration::from_secs(5)).unwrap();
    let path = old.dir.path().join("x.smexport");
    let mut out = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&path).unwrap());
    snap.write_to(&old.vault, &mut out).unwrap();
    drop(out);
    drop(snap);
    let (_d, data, keys) = common::empty();
    let opts = common::opts();
    let ex = moving::import::open_file(&path, common::APP).unwrap();
    assert!(ex.header.bytes > BIG);
    let mut imp = moving::import::begin(&data, &keys, &ex, &old.recovery, &opts).unwrap();
    assert_eq!(imp.run(|_| ControlFlow::Continue(())).unwrap(), moving::Outcome::Ready);
    drop(imp);
    moving::import::switch(&data, &keys).unwrap();

    let grew = peak().saturating_sub(before);
    eprintln!("the peak grew by {} KiB", grew >> 10);
    assert!(grew < 16 << 20, "the peak grew by {} MiB for a {} MiB folder", grew >> 20, ex.header.bytes >> 20);
    assert_eq!(std::fs::metadata(data.join("can/car0/big.log.xz")).unwrap().len(), BIG);
    let mut n = 0;
    for d in 0..4 {
        n += std::fs::read_dir(data.join(format!("can/car{d}"))).unwrap().count();
    }
    assert_eq!(n, 12_001);
}
