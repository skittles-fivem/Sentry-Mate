//! `sentry-mate log-checks --car c1`: every log of a car's CAN Bridge keys as the app is told it now (the `check` and
//! `check_reason` of the upload API's status call), counted, and when the failed ones were (runs of failed logs by
//! their start times, UTC). Prints no log name, car key, signing key or position, so an owner can pass it on. Only
//! reads.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use ingest::judge::{FailedRun, LogChecks};

/// `yyyy-mm-dd hh:mm` UTC of ms since 1970.
fn utc(ms: i64) -> String {
    let (d, rest) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000) / 60_000);
    let (y, m, day) = trust::name::civil_from_days(d);
    format!("{y:04}-{m:02}-{day:02} {:02}:{:02}", rest / 60, rest % 60)
}

fn reasons(r: &FailedRun) -> String {
    [
        ("mismatch", r.mismatch),
        ("bad_signature", r.bad_signature),
        ("unsigned", r.unsigned),
        ("chain_break", r.chain_break),
    ]
    .iter()
    .filter(|(_, n)| *n > 0)
    .map(|(w, n)| format!("{w} {n}"))
    .collect::<Vec<_>>()
    .join(", ")
}

/// The report of `c` (of a car with `keys` car keys).
pub fn print(c: &LogChecks, car: &str, keys: usize, out: &mut dyn Write) -> std::io::Result<()> {
    let k = if keys == 1 { "1 CAN Bridge key".to_string() } else { format!("{keys} CAN Bridge keys") };
    writeln!(out, "log checks of car {car} ({k}), as the CAN Bridge app is told them now")?;
    writeln!(out, "  verified       {:>8}", c.verified)?;
    writeln!(out, "  pending        {:>8}", c.pending)?;
    writeln!(out, "  failed         {:>8}", c.failed)?;
    for (w, n) in [
        ("mismatch", c.mismatch),
        ("bad_signature", c.bad_signature),
        ("unsigned", c.unsigned),
        ("chain_break", c.chain_break),
    ] {
        writeln!(out, "    {w:<13}{n:>8}")?;
    }
    if c.failed_runs.is_empty() {
        writeln!(out, "no failed logs")?;
        return Ok(());
    }
    let shown = if c.runs_total > c.failed_runs.len() as u64 {
        format!(" (the newest {} of {})", c.failed_runs.len(), c.runs_total)
    } else {
        String::new()
    };
    writeln!(out, "runs of failed logs, by the logs' start times (UTC){shown}:")?;
    for r in &c.failed_runs {
        let when = if r.first_start_ms == r.last_start_ms {
            utc(r.first_start_ms)
        } else {
            format!("{} to {}", utc(r.first_start_ms), utc(r.last_start_ms))
        };
        let logs = if r.logs == 1 { "1 log".to_string() } else { format!("{} logs", r.logs) };
        writeln!(out, "  {when}  {logs}: {}", reasons(r))?;
    }
    Ok(())
}

/// Runs the command over the data folder (only read).
pub async fn run(
    data: &Path,
    keys: &Path,
    car: model::CarId,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let vault = Arc::new(vault::Vault::open(data, keys)?);
    let gate = gate::Gate::open(data)?;
    let reader = db::Reader::new(data, vault.clone());
    let record = {
        // the read of the db while the folder's writes pause (a few milliseconds)
        let _pause = gate.pause(Duration::from_secs(30)).map_err(|e| format!("the data folder stayed busy: {e:?}"))?;
        reader.car(car)?
    };
    let Some(c) = record else {
        writeln!(err, "no car c{car} (the car's id is in its page's address, /cars/c1/...)")?;
        return Ok(ExitCode::FAILURE);
    };
    let store = ingest::Store::new(data.join("can"), vault);
    let judge = ingest::Judge::new(store.clone());
    let mut all = LogChecks::default();
    let mut n = 0;
    for key in &c.vehicle_keys {
        if !judge.known(key) {
            continue;
        }
        n += 1;
        all.merge(judge.log_checks(key, store.uploading(key).await).await?);
    }
    print(&all, &format!("c{car}"), n, out)?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prints_counts_and_runs_without_names() {
        let mut c =
            LogChecks { verified: 20_380, pending: 3, failed: 7, unsigned: 5, chain_break: 2, ..Default::default() };
        let day = trust::name::days_from_civil(2026, 11, 7) * 86_400_000;
        c.failed_runs = vec![
            FailedRun {
                first_start_ms: day + 14 * 3_600_000,
                last_start_ms: day + 15 * 3_600_000 + 8 * 60_000,
                logs: 5,
                unsigned: 5,
                ..Default::default()
            },
            FailedRun {
                first_start_ms: day + 86_400_000,
                last_start_ms: day + 86_400_000,
                logs: 2,
                chain_break: 2,
                ..Default::default()
            },
        ];
        c.runs_total = 2;
        let mut out = Vec::new();
        print(&c, "c1", 1, &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("verified          20380"), "{s}");
        assert!(s.contains("    unsigned            5"), "{s}");
        assert!(s.contains("  2026-11-07 14:00 to 2026-11-07 15:08  5 logs: unsigned 5"), "{s}");
        assert!(s.contains("  2026-11-08 00:00  2 logs: chain_break 2"), "{s}");
        assert!(!s.contains(".log.xz"), "{s}");
    }
}
