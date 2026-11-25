//! `sentry-mate dev seed` (hidden, and only in a build with the `dev-seed` feature): fills a data folder with made-up
//! cars' logs from the `synth` crate, through ingest's own storage (sealed with the vault, judged like any upload:
//! their signatures verified, each car paired with its made-up key), so the whole server can be tried without anyone's
//! car.
//!
//! Each car gets a token of its own (tied to its CAN Bridge key, as its first upload would), and its logs and
//! sidecars go in the bridge's chunk size, in the order the bridge would send them. The server's engine picks them
//! up when it starts (it compares each car's logs with what it decoded).
//!
//! The same options always make the same logs, and a run of more days only adds logs (synth's first days do not
//! depend on how many there are), so seeding again is safe. Options that would make other bytes under the same car
//! key (`--light`, `--clean`, `--candump`, or an older synth) are refused: the token's name records them.

use ingest::{Judge, Store, Tokens};
use std::io::Write;

/// The bridge's chunk size (8 MiB).
const CHUNK: usize = 8 << 20;

/// The start of every made-up car's token name.
const MADE_UP: &str = "Made-up car ";

/// Which synth makes the logs: a change that changes their bytes changes this, so older seeds are refused.
const SYNTH_GENERATION: u32 = 2;

pub struct Options {
    pub days: u32,
    pub cars: u32,
    pub seed: u64,
    /// At most a frame a second per message: smaller and quicker, still a value every second.
    pub light: bool,
    /// No trouble: no lost log, no dropped frames, no pause in recording, no app update.
    pub clean: bool,
    /// Candump text logs (`/1`) instead of columns (`/2`).
    pub candump: bool,
    /// Seed into a data folder that has real cars' tokens too.
    pub with_real_cars: bool,
    /// The first day's UTC midnight (Unix ms); `None` is synth's own first day.
    pub start_ms: Option<u64>,
    /// Minutes the car stays awake (logging) after it was last used, as in Sentry Mode; `None` is synth's own 10.
    pub awake_min: Option<u32>,
}

/// `YYYY-MM-DD` as that day's UTC midnight in Unix ms (days from 1970 through the civil calendar), at the earliest
/// 2000-01-01 and at the latest 2099-12-31.
pub fn parse_day(s: &str) -> Result<u64, String> {
    let bad = || format!("--start {s:?}: a day as YYYY-MM-DD, from 2000-01-01 to 2099-12-31");
    let mut it = s.split('-');
    let (Some(y), Some(m), Some(d), None) = (it.next(), it.next(), it.next(), it.next()) else { return Err(bad()) };
    let num = |t: &str, len: usize| if t.len() == len { t.parse::<u64>().ok() } else { None };
    let (Some(y), Some(m), Some(d)) = (num(y, 4), num(m, 2), num(d, 2)) else { return Err(bad()) };
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if !(2000..=2099).contains(&y) || !(1..=12).contains(&m) || d == 0 || d > month_days[(m - 1) as usize] {
        return Err(bad());
    }
    let years: u64 = (1970..y).map(|yy| if yy % 4 == 0 && (yy % 100 != 0 || yy % 400 == 0) { 366 } else { 365 }).sum();
    let days = years + month_days[..(m - 1) as usize].iter().sum::<u64>() + (d - 1);
    Ok(days * 86_400_000)
}

impl Options {
    /// What decides a car's bytes besides its days, as its token's name records it.
    fn tag(&self) -> String {
        let mut tag = format!(
            "synth {SYNTH_GENERATION}, seed {}, {}, {}, {}",
            self.seed,
            if self.light { "light" } else { "realistic" },
            if self.clean { "clean" } else { "trouble" },
            if self.candump { "candump" } else { "columns" }
        );
        // only a start of its own is recorded, so cars seeded before this option keep their names; with a time awake
        // too, both are written short (`d<day>`, `a<minutes awake>`), so the name stays within a token's 80 characters
        let start = self.start_ms.filter(|ms| *ms != synth::DEFAULT_START_MS).map(|ms| ms / 86_400_000);
        match (start, self.awake_min) {
            (Some(day), None) => tag.push_str(&format!(", from day {day}")),
            (Some(day), Some(m)) => tag.push_str(&format!(", d{day}, a{m}")),
            (None, Some(m)) => tag.push_str(&format!(", a{m}")),
            (None, None) => {}
        }
        tag
    }
}

/// Enters the data folder's write gate for one file (and checks the folder still takes writes).
pub type Enter<'a> = &'a dyn Fn() -> Result<gate::Pass, Box<dyn std::error::Error>>;

/// One file (log or sidecar) through the store in chunks, then judged as the upload API does, inside the gate.
pub(crate) async fn put(
    store: &Store,
    judge: &Judge,
    enter: Enter<'_>,
    name: &str,
    bytes: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    use sha2::Digest;
    let _pass = enter()?;
    let sha = hex::encode(sha2::Sha256::digest(bytes));
    let len = bytes.len() as u64;
    let mut offset = 0usize;
    loop {
        let end = (offset + CHUNK).min(bytes.len());
        let body = bytes::Bytes::copy_from_slice(&bytes[offset..end]);
        let r = store.receive(name, body, offset as u64, len, &sha).await?;
        if r.complete {
            if !r.already {
                judge.arrived(name).await?;
            }
            return Ok(());
        }
        if end == offset {
            return Err(format!("{name}: the store took no more bytes").into());
        }
        offset = end;
    }
}

pub async fn run(
    store: &Store,
    judge: &Judge,
    tokens: &Tokens,
    o: &Options,
    enter: Enter<'_>,
    out: &mut impl Write,
) -> Result<(), Box<dyn std::error::Error>> {
    if o.cars > synth::MAX_FLEET {
        return Err(format!("at most {} made-up cars (--cars {})", synth::MAX_FLEET, o.cars).into());
    }
    let list = tokens.list().await;
    if !o.with_real_cars
        && let Some(t) = list.iter().find(|t| !t.revoked && !t.car.starts_with(MADE_UP))
    {
        return Err(format!(
            "this data folder has a real car's token ({:?}): made-up cars go in a data folder of their own (or add \
             --with-real-cars, or SENTRY_MATE_DEV=1, to put them here anyway: they show in its fleet)",
            t.car
        )
        .into());
    }
    let fleet: Vec<synth::Scenario> = synth::fleet(o.seed, o.cars.max(1), o.days.max(1))
        .into_iter()
        .map(|s| {
            let s = match o.start_ms {
                Some(ms) => s.start(ms),
                None => s,
            };
            let s = match o.awake_min {
                Some(m) => s.stay_awake(u64::from(m) * 60_000),
                None => s,
            };
            s.rates(if o.light { synth::Rates::Light } else { synth::Rates::Realistic })
                .with_trouble(!o.clean)
                .formats(if o.candump { synth::Formats::Candump } else { synth::Formats::Columns })
        })
        .collect();
    // every car first: a car key seeded before must have been seeded with these options, or its logs would clash
    let mut tied = Vec::new();
    for s in &fleet {
        let key = s.vehicle_key();
        let want = format!("{MADE_UP}{} ({})", s.car + 1, o.tag());
        let had = list.iter().find(|t| !t.revoked && t.vehicle_key.as_deref() == Some(&key));
        match had {
            Some(t) if t.car == want => tied.push(Some(t.clone())),
            Some(t) => {
                return Err(format!(
                    "car key {key} was seeded before with other options or an older synth ({:?}; now {:?}): seed \
                     again with the same options, or into a data folder of its own",
                    t.car, want
                )
                .into());
            }
            None if judge.known(&key) => {
                return Err(format!(
                    "car key {key} already has logs but no token: they would clash; seed into a data folder of its own"
                )
                .into());
            }
            None => tied.push(None),
        }
    }
    for (s, had) in fleet.iter().zip(tied) {
        let key = s.vehicle_key();
        let token = match had {
            Some(t) => t,
            None => {
                let (_, t) = tokens.create(&format!("{MADE_UP}{} ({})", s.car + 1, o.tag())).await?;
                tokens.tie(&t, &key).await?;
                t
            }
        };
        let mut logs = s.logs();
        let (mut n, mut bytes) = (0u64, 0u64);
        for log in logs.by_ref() {
            put(store, judge, enter, &log.name, &log.bytes).await?;
            put(store, judge, enter, &log.sidecar_name(), &log.sidecar_bytes()).await?;
            n += 1;
            bytes += log.bytes.len() as u64;
        }
        let r = logs.report();
        let book = judge.book(&key).await;
        let usable = book.logs.values().filter(|r| r.usable()).count();
        writeln!(
            out,
            "car {} ({:?}, VIN {}, CAN key {key}, token {}): {n} logs this run ({:.1} MB), {} stored ({usable} usable), \
             {} drives, {} charges, {} lost on the way, signing key {}",
            s.car + 1,
            s.model,
            s.vin(),
            token.id,
            bytes as f64 / 1e6,
            book.logs.len(),
            r.truth.drives.len(),
            r.truth.charges.len(),
            r.lost.len(),
            r.fingerprint
        )?;
    }
    writeln!(
        out,
        "These cars, their places and their logs are made up ({}). Their signing keys come from a public seed, \
         so anyone can forge their logs: never seed a server others rely on. To remove them, revoke their tokens \
         (`sentry-mate token list`, `token revoke ID`) or start again with an empty data folder. A running server's \
         engine finds them when it starts again.",
        o.tag()
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use std::sync::Arc;

    async fn tokens(kit: &crate::testkit::Kit) -> Tokens {
        let v = Arc::new(vault::Vault::open(&kit.data, &kit.keys).unwrap());
        Tokens::open(kit.data.join("tokens.json"), v).await.unwrap()
    }

    #[test]
    fn start_days_are_utc_midnights() {
        assert_eq!(parse_day("2026-03-02").unwrap(), synth::DEFAULT_START_MS);
        assert_eq!(parse_day("2000-01-01").unwrap(), 946_684_800_000);
        assert_eq!(parse_day("2024-02-29").unwrap(), 1_709_164_800_000);
        assert_eq!(parse_day("2026-11-19").unwrap(), 1_795_046_400_000);
        for bad in ["2023-02-29", "2026-13-01", "2026-1-01", "1999-12-31", "2100-01-01", "2026-03-02-1", "today", ""] {
            assert!(parse_day(bad).is_err(), "{bad}");
        }
        let o = |start_ms| Options {
            days: 1,
            cars: 1,
            seed: 1,
            light: false,
            clean: false,
            candump: false,
            with_real_cars: false,
            start_ms,
            awake_min: None,
        };
        assert_eq!(o(None).tag(), o(Some(synth::DEFAULT_START_MS)).tag());
        assert!(o(Some(parse_day("2026-11-10").unwrap())).tag().ends_with(", from day 20767"));
        // a car's name (what a later run is compared with) fits in a token's 80 characters with every option
        let long = Options { awake_min: Some(1440), ..o(Some(parse_day("2026-11-10").unwrap())) };
        let name = format!("{MADE_UP}{} ({})", synth::MAX_FLEET, long.tag());
        assert!(name.chars().count() <= 80, "{name}");
        assert!(name.ends_with(", d20767, a1440)"), "{name}");
    }

    fn opts(days: u32, clean: bool) -> Options {
        Options {
            days,
            cars: 1,
            seed: 9,
            light: true,
            clean,
            candump: false,
            with_real_cars: false,
            start_ms: None,
            awake_min: None,
        }
    }

    async fn seed(kit: &crate::testkit::Kit, tokens: &Tokens, o: &Options) -> Result<String, String> {
        let gate = gate::Gate::memory();
        let enter = || -> Result<gate::Pass, Box<dyn std::error::Error>> { Ok(gate.enter(GATE)?) };
        let mut out = Vec::new();
        run(&kit.store, &kit.judge, tokens, o, &enter, &mut out).await.map_err(|e| e.to_string())?;
        Ok(String::from_utf8(out).unwrap())
    }

    const GATE: std::time::Duration = std::time::Duration::from_secs(5);

    #[tokio::test]
    async fn a_made_up_car_goes_in_verified_and_paired() {
        let kit = crate::testkit::kit();
        let tokens = tokens(&kit).await;
        let o = opts(1, true);
        let text = seed(&kit, &tokens, &o).await.unwrap();
        assert_eq!(text.matches("car ").count(), 1, "{text}");
        assert!(text.contains("made up"), "{text}");
        let list = tokens.list().await;
        assert_eq!(list.len(), 1);
        for t in &list {
            let key = t.vehicle_key.clone().unwrap();
            let names = kit.judge.latest_usable(&key, 10_000).await.unwrap();
            assert!(names.len() > 20, "{key}: {}", names.len());
            assert!(text.contains(&format!("{} stored ({} usable)", names.len(), names.len())), "{text}");
            for n in names.iter().take(5) {
                let rec = kit.judge.record(&key, n).await.unwrap();
                assert_eq!(rec.status, ingest::judge::Status::Verified, "{n}: {rec:?}");
            }
            let chain = kit.judge.chain(&key).await;
            let json = serde_json::to_value(&chain).unwrap();
            assert_eq!(json["issues"], serde_json::json!([]), "{json}");
            assert_eq!(json["count"].as_u64(), Some(names.len() as u64));
        }
        // again: every file is already there, nothing changes
        let key = list[0].vehicle_key.clone().unwrap();
        let before = kit.judge.latest_usable(&key, 10_000).await.unwrap().len();
        seed(&kit, &tokens, &o).await.unwrap();
        let after = kit.judge.latest_usable(&key, 10_000).await.unwrap().len();
        assert_eq!(before, after);
        assert_eq!(tokens.list().await.len(), 1, "the same token");
    }

    #[tokio::test]
    async fn trouble_shows_as_one_gap_and_more_days_only_add_logs() {
        let kit = crate::testkit::kit();
        let tokens = tokens(&kit).await;
        seed(&kit, &tokens, &opts(1, false)).await.unwrap();
        let key = tokens.list().await[0].vehicle_key.clone().unwrap();
        let one = kit.judge.latest_usable(&key, 100_000).await.unwrap().len();
        let check = |json: serde_json::Value| {
            let issues = json["issues"].as_array().unwrap().clone();
            assert_eq!(issues.len(), 1, "only the lost log: {json}");
            assert_eq!(issues[0]["kind"], "gap", "{json}");
            assert_eq!(issues[0]["from"], issues[0]["to"], "one log: {json}");
        };
        check(serde_json::to_value(kit.judge.chain(&key).await).unwrap());
        // a second day over the first: the first day's logs are the same, the chain goes on
        let text = seed(&kit, &tokens, &opts(2, false)).await.unwrap();
        let two = kit.judge.latest_usable(&key, 100_000).await.unwrap().len();
        assert!(two > one + 20, "{one} then {two}");
        assert!(text.contains(&format!("{two} stored ({two} usable)")), "{text}");
        check(serde_json::to_value(kit.judge.chain(&key).await).unwrap());
        // and fewer days again: nothing changes
        seed(&kit, &tokens, &opts(1, false)).await.unwrap();
        assert_eq!(kit.judge.latest_usable(&key, 100_000).await.unwrap().len(), two);
        // other options would make other bytes under the same car key: refused, nothing written
        let e = seed(&kit, &tokens, &opts(2, true)).await.unwrap_err();
        assert!(e.contains("other options"), "{e}");
        assert_eq!(kit.judge.latest_usable(&key, 100_000).await.unwrap().len(), two);
        assert_eq!(tokens.list().await.len(), 1);
    }

    #[tokio::test]
    async fn a_folder_with_real_cars_is_refused_unless_asked() {
        let kit = crate::testkit::kit();
        let tokens = tokens(&kit).await;
        tokens.create("My car").await.unwrap();
        let o = Options {
            days: 1,
            cars: 1,
            seed: 9,
            light: true,
            clean: true,
            candump: false,
            with_real_cars: false,
            start_ms: None,
            awake_min: None,
        };
        let e = seed(&kit, &tokens, &o).await.unwrap_err();
        assert!(e.contains("real car"), "{e}");
        assert_eq!(tokens.list().await.len(), 1);
        let e = seed(&kit, &tokens, &Options { cars: synth::MAX_FLEET + 1, with_real_cars: true, ..o }).await;
        assert!(e.unwrap_err().contains("at most"));
    }
}
