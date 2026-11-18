//! `sentry-mate dev seed` (hidden, for development): fills a data folder with made-up cars' logs from the `synth`
//! crate, through ingest's own storage (sealed with the vault, judged like any upload: their signatures verified,
//! each car paired with its made-up key), so the whole server can be tried without anyone's car.
//!
//! Each car gets a token of its own (tied to its CAN Bridge key, as its first upload would), and its logs and
//! sidecars go in the bridge's chunk size, in the order the bridge would send them. The server's engine picks them
//! up when it starts (it compares each car's logs with what it decoded).

use ingest::{Judge, Store, Tokens};
use std::io::Write;

/// The bridge's chunk size (8 MiB).
const CHUNK: usize = 8 << 20;

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
}

/// One file (log or sidecar) through the store in chunks, then judged as the upload API does.
async fn put(store: &Store, judge: &Judge, name: &str, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    use sha2::Digest;
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
    out: &mut impl Write,
) -> Result<(), Box<dyn std::error::Error>> {
    for s in synth::fleet(o.seed, o.cars.max(1), o.days.max(1)) {
        let s = s
            .rates(if o.light { synth::Rates::Light } else { synth::Rates::Realistic })
            .with_trouble(!o.clean)
            .formats(if o.candump { synth::Formats::Candump } else { synth::Formats::Columns });
        let key = s.vehicle_key();
        // its token (one already tied to its key is used again)
        let token = match tokens.list().await.into_iter().find(|t| !t.revoked && t.vehicle_key.as_deref() == Some(&key))
        {
            Some(t) => t,
            None => {
                let (_, t) = tokens.create(&format!("Made-up car {}", s.car + 1)).await?;
                tokens.tie(&t, &key).await?;
                t
            }
        };
        let mut logs = s.logs();
        let (mut n, mut bytes) = (0u64, 0u64);
        for log in logs.by_ref() {
            put(store, judge, &log.name, &log.bytes).await?;
            put(store, judge, &log.sidecar_name(), &log.sidecar_bytes()).await?;
            n += 1;
            bytes += log.bytes.len() as u64;
        }
        let r = logs.report();
        let usable = judge.latest_usable(&key, usize::try_from(n).unwrap_or(0)).await?.len();
        writeln!(
            out,
            "car {} ({:?}, VIN {}, CAN key {key}, token {}): {n} logs ({usable} usable), {:.1} MB, {} drives, {} \
             charges, signing key {}",
            s.car + 1,
            s.model,
            s.vin(),
            token.id,
            bytes as f64 / 1e6,
            r.truth.drives.len(),
            r.truth.charges.len(),
            r.fingerprint
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn a_made_up_car_goes_in_verified_and_paired() {
        let kit = crate::testkit::kit();
        let tokens =
            Tokens::open(kit.data.join("tokens.json"), Arc::new(vault::Vault::open(&kit.data, &kit.keys).unwrap()))
                .await
                .unwrap();
        let o = Options { days: 1, cars: 1, seed: 9, light: true, clean: true, candump: false };
        let mut out = Vec::new();
        run(&kit.store, &kit.judge, &tokens, &o, &mut out).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.matches("car ").count(), 1, "{text}");
        let list = tokens.list().await;
        assert_eq!(list.len(), 1);
        for t in &list {
            let key = t.vehicle_key.clone().unwrap();
            let names = kit.judge.latest_usable(&key, 10_000).await.unwrap();
            assert!(names.len() > 20, "{key}: {}", names.len());
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
        let before = kit.judge.latest_usable(&list[0].vehicle_key.clone().unwrap(), 10_000).await.unwrap().len();
        let mut out = Vec::new();
        let tokens2 = tokens;
        run(&kit.store, &kit.judge, &tokens2, &o, &mut out).await.unwrap();
        let after = kit.judge.latest_usable(&list[0].vehicle_key.clone().unwrap(), 10_000).await.unwrap().len();
        assert_eq!(before, after);
        assert_eq!(tokens2.list().await.len(), 1, "the same token");
    }
}
