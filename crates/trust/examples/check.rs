//! `cargo run -p trust --example check -- LOG.log.xz [LOG.log.xz.json]`: what a log's signature says.
use sha2::{Digest, Sha256};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(log) = args.first() else {
        eprintln!("usage: check LOG.log.xz [SIDECAR]");
        std::process::exit(2);
    };
    let side = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| format!("{log}.json"));
    let (Ok(bytes), Ok(side_bytes)) = (std::fs::read(log), std::fs::read(&side)) else {
        eprintln!("cannot read {log} or {side}");
        std::process::exit(1);
    };
    let sidecar: serde_json::Value = serde_json::from_slice(&side_bytes).unwrap_or_default();
    let name = std::path::Path::new(log)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let sha = hex::encode(Sha256::digest(&bytes));
    let c = trust::check(name, &sha, bytes.len() as u64, &sidecar);
    match &c {
        trust::Check::Signed(l) => println!(
            "signed and intact: key {}, index {}",
            l.key_fingerprint, l.index
        ),
        trust::Check::NotSigned { bridge_error } => println!(
            "not signed{}",
            bridge_error
                .as_ref()
                .map(|e| format!(": {e}"))
                .unwrap_or_default()
        ),
        trust::Check::Bad { reason, .. } => {
            println!("failed: {reason}");
            std::process::exit(1);
        }
    }
}
