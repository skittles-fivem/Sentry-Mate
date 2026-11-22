//! Years of TeslaFi polls in one upload stream through: memory stays flat however big the file. (Its own test binary,
//! so the memory measured is this test's.)
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

mod tf;

use chrono::NaiveDate;
use import::teslafi::{self, Options};
use import::{Batch, CarReport, ImportState, Sink, SinkError, Target, TmCar};
use model::CarId;
use std::io::Read;
use tf::{Gen, VIN1};

/// TeslaFi rows made a day at a time as they are read: the file never exists whole.
struct Days {
    g: Gen,
    day: NaiveDate,
    left: u32,
    buf: Vec<u8>,
    pos: usize,
    pub bytes: u64,
}

impl Read for Days {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.buf.len() {
            if self.left == 0 {
                return Ok(0);
            }
            self.left -= 1;
            let header = self.bytes == 0;
            self.g.day(self.day);
            self.day = self.day.succ_opt().unwrap();
            let mut s = if header { self.g.header() + "\n" } else { String::new() };
            for l in self.g.lines.drain(..) {
                s.push_str(&l);
                s.push('\n');
            }
            self.buf = s.into_bytes();
            self.pos = 0;
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        self.bytes += n as u64;
        Ok(n)
    }
}

#[derive(Default)]
struct Count {
    rows: [u64; 7],
    largest: usize,
}

impl Sink for Count {
    fn car(&mut self, _car: &TmCar) -> Result<Option<Target>, SinkError> {
        Ok(Some(Target { car: 1, state: ImportState::default() }))
    }
    fn batch(&mut self, _car: CarId, b: Batch) -> Result<(), SinkError> {
        self.largest = self.largest.max(b.len());
        let i = match &b {
            Batch::Positions(_) => 0,
            Batch::Drives(_) => 1,
            Batch::Charges(_) => 2,
            Batch::ChargePoints(_) => 3,
            Batch::States(_) => 4,
            Batch::Updates(_) => 5,
            Batch::Removed(_) | Batch::Cleared(_) => 6,
        };
        self.rows[i] += b.len() as u64;
        Ok(())
    }
    fn places(&mut self, _rows: Vec<import::PlaceRow>) -> Result<(), SinkError> {
        Ok(())
    }
    fn addresses(&mut self, _rows: Vec<import::AddressRow>) -> Result<(), SinkError> {
        Ok(())
    }
    fn car_done(&mut self, _report: CarReport, _state: Option<ImportState>) -> Result<(), SinkError> {
        Ok(())
    }
}

fn status(field: &str) -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let l = s.lines().find(|l| l.starts_with(field))?;
    Some(l.split_whitespace().nth(1)?.parse::<u64>().ok()? * 1024)
}

#[test]
fn years_of_polls_stream_through_in_flat_memory() {
    let days = if cfg!(debug_assertions) { 1500 } else { 3000 };
    let mut g = Gen::new(VIN1);
    g.pad = 150;
    let mut r =
        Days { g, day: NaiveDate::from_ymd_opt(2019, 1, 1).unwrap(), left: days, buf: Vec::new(), pos: 0, bytes: 0 };
    let _ = std::fs::write("/proc/self/clear_refs", "5");
    let before = status("VmRSS:");
    let o = Options { batch_rows: 1024, max_buffered_rows: 4096, ..Options::default() };
    let mut sink = Count::default();
    let t0 = std::time::Instant::now();
    let rep = teslafi::import(&mut r, &mut sink, &o).unwrap();
    let peak = status("VmHWM:");
    eprintln!(
        "{} days, {} MB, {} rows in {:?}; at most {} rows waiting; memory {} MB before, peak {} MB",
        days,
        r.bytes >> 20,
        rep.rows_read,
        t0.elapsed(),
        rep.peak_buffered_rows,
        before.unwrap_or(0) >> 20,
        peak.unwrap_or(0) >> 20
    );
    assert_eq!(rep.unpacked_bytes, r.bytes);
    assert_eq!(rep.rows_used, rep.rows_read);
    assert_eq!(sink.rows[1], 2 * u64::from(days));
    assert_eq!(sink.rows[2], 2 * u64::from(days));
    assert!(rep.peak_buffered_rows <= o.max_buffered_rows + 8, "{}", rep.peak_buffered_rows);
    assert!(sink.largest <= o.batch_rows);
    assert!(r.bytes > 150 << 20, "a big file: {} MB", r.bytes >> 20);
    if let (Some(b), Some(p)) = (before, peak) {
        assert!(p.saturating_sub(b) < 48 << 20, "memory grew by {} MB", (p - b) >> 20);
    }
}
