//! A made-up CAN Bridge's signing key and chain: signs each finished log exactly as the bridge does (SMO
//! `docs/sentry-manager-format.md`, "Integrity"), so `trust::check` passes it. The key comes from a seed: it is
//! nobody's device key, and anyone can make it again.

use base64::Engine;
use p256::ecdsa::{Signature, SigningKey, signature::Signer as _};
use p256::pkcs8::EncodePublicKey;
use sha2::{Digest, Sha256};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// A signing key and where its chain is.
#[derive(Clone)]
pub struct Signer {
    key: SigningKey,
    der: Vec<u8>,
    /// The next log's index.
    pub index: u64,
    /// The last signed log: (name, sha256).
    pub prev: (String, String),
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signer").field("fingerprint", &self.fingerprint()).field("index", &self.index).finish()
    }
}

impl Signer {
    /// The key made from `seed` (the same seed, the same key), at the start of its chain.
    pub fn from_seed(seed: &[u8]) -> Signer {
        let mut n = 0u32;
        let key = loop {
            let mut h = Sha256::new();
            h.update(b"synth: a made-up CAN Bridge signing key\n");
            h.update(seed);
            h.update(n.to_le_bytes());
            let bytes = h.finalize();
            if let Ok(k) = SigningKey::from_bytes(&bytes) {
                break k;
            }
            n += 1;
        };
        let der = key.verifying_key().to_public_key_der().map(|d| d.as_bytes().to_vec()).unwrap_or_default();
        Signer { key, der, index: 0, prev: ("-".into(), trust::GENESIS_SHA.into()) }
    }

    pub fn public_key_der(&self) -> &[u8] {
        &self.der
    }

    /// As the app shows it ("Log signing key").
    pub fn fingerprint(&self) -> String {
        trust::fingerprint(&self.der)
    }

    /// The sidecar's `integrity` object for a log (its name and bytes), and moves the chain on.
    pub fn sign(&mut self, vehicle_key: &str, name: &str, log: &[u8]) -> serde_json::Value {
        let sha = hex::encode(Sha256::digest(log));
        let msg =
            trust::signed_message(vehicle_key, name, &sha, log.len() as u64, self.index, &self.prev.0, &self.prev.1);
        let sig: Signature = self.key.sign(msg.as_bytes());
        let v = serde_json::json!({
            "signed": true,
            "version": trust::VERSION,
            "algorithm": trust::ALGORITHM,
            "public_key": B64.encode(&self.der),
            "key_fingerprint": self.fingerprint(),
            "vehicle_key": vehicle_key,
            "sha256": sha,
            "length": log.len(),
            "index": self.index,
            "prev_name": self.prev.0,
            "prev_sha256": self.prev.1,
            "signature": B64.encode(sig.to_der().as_bytes()),
        });
        self.index += 1;
        self.prev = (name.to_string(), sha);
        v
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
    use super::*;

    #[test]
    fn its_logs_pass_the_trust_check_and_chain() {
        let mut s = Signer::from_seed(b"test");
        assert_eq!(s.fingerprint(), Signer::from_seed(b"test").fingerprint());
        assert_ne!(s.fingerprint(), Signer::from_seed(b"other").fingerprint());
        let mut entries = Vec::new();
        for m in 0..3 {
            let name = format!("abcdef123456_20260302T08{m:02}00Z_{}.log.xz", m + 1);
            let log = format!("log {m}").into_bytes();
            let side = serde_json::json!({ "integrity": s.sign("abcdef123456", &name, &log) });
            let sha = hex::encode(Sha256::digest(&log));
            let trust::Check::Signed(link) = trust::check(&name, &sha, log.len() as u64, &side) else {
                panic!("{:?}", trust::check(&name, &sha, log.len() as u64, &side))
            };
            entries.push(trust::Entry { link, failed: None });
        }
        let r = trust::chain(&entries);
        assert!(r.issues.is_empty() && r.restarts.is_empty(), "{r:?}");
        assert_eq!(r.count, 3);
    }
}
