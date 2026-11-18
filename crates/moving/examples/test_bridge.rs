//! A made-up CAN Bridge for the Docker move test (`tools/move-e2e.sh`): it uploads signed candump logs to a Sentry
//! Mate over TLS 1.3, pinned to the certificate SHA-256, in chunks, the way the bridge does (ping, status, chunks
//! with `Upload-Offset`, `Upload-Length` and `Upload-SHA256`, then the sidecar).
//!
//! All made up: the car key is given, the signing key and every frame come from it and the log's number, so a log
//! is the same bytes each time and its chain link can be made without remembering anything. No real car data.
//!
//! ```text
//! test_bridge name KEY I                               the log's name and its SHA-256
//! test_bridge send ADDR FP TOKEN KEY FROM TO           logs FROM..TO (TO not included), each whole, with its sidecar
//! test_bridge part ADDR FP TOKEN KEY I                 only the first chunk of log I (an upload left part way)
//! test_bridge run ADDR_FILE FP TOKEN KEY FROM STOP     one log every second from FROM until the file STOP exists;
//!                                                      the address is read from ADDR_FILE before each request
//!                                                      (the owner changing it in the app); refused or cut
//!                                                      requests are tried again, as the bridge does
//! test_bridge ping ADDR FP TOKEN                       one ping: its status and code
//! test_bridge random-code                            a move code no server made (a well-formed wrong one)
//! test_bridge expire-code DATA KEYS                    the old server's waiting move code expires now (the test
//!                                                      can't wait 24 hours; run it as the server's user)
//! ```
//!
//! Each request prints one line: `<status> <code or -> <name> <what>`; a log whose sidecar was taken prints `done
//! <i> <name> <sha256>`. Status 0 is a request that got no answer (no connection, wrong certificate).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout, clippy::print_stderr)]

use base64::Engine;
use moving::client::{Address, Client};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use p256::pkcs8::EncodePublicKey;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;
use std::time::Duration;

/// A log's name: one a minute from 2026-11-08 10:00 UTC.
fn name(key: &str, i: u32) -> String {
    let (d, m) = (8 + i / (24 * 60), i % (24 * 60));
    format!("{key}_202611{d:02}T{:02}{:02}00Z_{}.log.xz", m / 60, m % 60, i % 999_999 + 1)
}

/// A made-up log as the bridge writes it: candump text in xz (`canbridge-candump-xz/1`), 1,500 frames (about 70 KB);
/// every 50th from the 8th (7, 57, ...) is a long drive of 400,000 frames (about 18 MB: more than one 16 MiB piece of
/// the move's copy).
fn log(key: &str, i: u32) -> Vec<u8> {
    let start_s = 1_794_096_000 + u64::from(i) * 60;
    let mut seed = u64::from_be_bytes(Sha256::digest(format!("{key}/{i}"))[..8].try_into().unwrap()) | 1;
    let mut text = format!("# clock wall={start_s}.000000 mono=0\n");
    let ids = [0x118u32, 0x129, 0x132, 0x257, 0x261, 0x292, 0x3d8, 0x3f2];
    let frames = if i % 50 == 7 { 400_000u64 } else { 1500 };
    for j in 0..frames {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let id = ids[(seed % ids.len() as u64) as usize];
        let t = start_s * 1_000_000 + j * 40_000 / (frames / 1500) + (seed >> 40) % 1000;
        text.push_str(&format!("({}.{:06}) can{} {id:03X}#{:016X}\n", t / 1_000_000, t % 1_000_000, j % 2, seed));
    }
    let mut out = Vec::new();
    lzma_rs::xz_compress(&mut text.as_bytes(), &mut out).unwrap();
    out
}

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn signing_key(key: &str) -> SigningKey {
    SigningKey::from_slice(&Sha256::digest(format!("made-up test signing key for {key}"))).unwrap()
}

/// The sidecar, signed, linked to the log before it.
fn sidecar(key: &str, i: u32, log: &[u8]) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let sk = signing_key(key);
    let der = sk.verifying_key().to_public_key_der().unwrap().as_bytes().to_vec();
    let (prev_name, prev_sha) =
        if i == 0 { ("-".to_string(), trust::GENESIS_SHA.to_string()) } else { (name(key, i - 1), sha(&self::log(key, i - 1))) };
    let n = name(key, i);
    let s = sha(log);
    let msg = trust::signed_message(key, &n, &s, log.len() as u64, u64::from(i), &prev_name, &prev_sha);
    let sig: Signature = sk.sign(msg.as_bytes());
    serde_json::to_vec(&serde_json::json!({ "format": "canbridge-candump-xz/1", "integrity": {
        "signed": true, "version": trust::VERSION, "algorithm": trust::ALGORITHM, "public_key": b64.encode(&der),
        "key_fingerprint": trust::fingerprint(&der), "vehicle_key": key, "sha256": s, "length": log.len(),
        "index": i, "prev_name": prev_name, "prev_sha256": prev_sha,
        "signature": b64.encode(sig.to_der().as_bytes()) } }))
    .unwrap()
}

struct Bridge {
    addr: String,
    fp: String,
    token: String,
    client: Option<Client>,
}

impl Bridge {
    fn client(&mut self) -> Option<&mut Client> {
        if self.client.is_none() {
            let a = Address::parse(&self.addr).ok()?;
            let mut c = Client::pinned(a, &self.fp).ok()?;
            c.set_timeout(Duration::from_secs(60));
            self.client = Some(c);
        }
        self.client.as_mut()
    }

    /// One request: its status (0: no answer) and the error code or the body.
    fn call(&mut self, method: &str, path: &str, extra: &[(&str, &str)], body: &[u8]) -> (u16, serde_json::Value) {
        let auth = format!("Bearer {}", self.token);
        let mut h = vec![("Authorization", auth.as_str())];
        h.extend_from_slice(extra);
        let Some(c) = self.client() else { return (0, serde_json::Value::Null) };
        let got = c.request(method, path, &h, body).and_then(|r| {
            let status = r.status;
            r.bytes(1 << 20).map(|b| (status, b))
        });
        match got {
            Ok((status, b)) => (status, serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null)),
            Err(e) => {
                self.client = None;
                (0, serde_json::json!({ "code": "no_answer", "error": e.to_string() }))
            }
        }
    }

    fn chunk(&mut self, name: &str, whole: &[u8], from: usize, to: usize) -> (u16, serde_json::Value) {
        let (off, len, s) = (from.to_string(), whole.len().to_string(), sha(whole));
        let h = [("Upload-Offset", off.as_str()), ("Upload-Length", len.as_str()), ("Upload-SHA256", s.as_str())];
        self.call("POST", &format!("/api/can/logs/{name}"), &h, &whole[from..to])
    }

    /// Where the server has got to with a file: `None` when it didn't answer.
    fn received(&mut self, name: &str) -> Option<(String, usize)> {
        let body = serde_json::to_vec(&serde_json::json!({ "names": [name] })).unwrap();
        let (s, b) = self.call("POST", "/api/can/logs/status", &[("Content-Type", "application/json")], &body);
        say(s, &b, name, "status");
        if s != 200 {
            return None;
        }
        let f = &b["files"][name];
        Some((f["state"].as_str().unwrap_or("").to_string(), f["received"].as_u64().unwrap_or(0) as usize))
    }

    /// Sends a file from where the server has got to; `true` once it holds all of it.
    fn put(&mut self, name: &str, whole: &[u8], split: bool) -> bool {
        let Some((state, got)) = self.received(name) else { return false };
        if state == "complete" {
            return true;
        }
        let mut at = if state == "partial" { got } else { 0 };
        while at < whole.len() {
            let to = if split && at < whole.len() / 2 { whole.len() / 2 } else { whole.len() };
            let (s, b) = self.chunk(name, whole, at, to);
            say(s, &b, name, &format!("chunk {at}..{to}"));
            match s {
                200 | 201 => at = b["received"].as_u64().map_or(to, |r| r as usize),
                409 if b["code"] == "offset_mismatch" => at = b["received"].as_u64().unwrap_or(0) as usize,
                _ => return false,
            }
        }
        true
    }

    /// A whole log and its sidecar; `true` once both are there.
    fn send_log(&mut self, key: &str, i: u32) -> bool {
        let (n, l) = (name(key, i), log(key, i));
        if !self.put(&n, &l, true) {
            return false;
        }
        let side = sidecar(key, i, &l);
        if !self.put(&format!("{n}.json"), &side, false) {
            return false;
        }
        println!("done {i} {n} {}", sha(&l));
        true
    }
}

fn say(status: u16, b: &serde_json::Value, name: &str, what: &str) {
    let code = b["code"].as_str().unwrap_or("-");
    println!("{status} {code} {name} {what}");
    let _ = std::io::stdout().flush();
}

fn usage() -> ! {
    eprintln!("usage: see the top of crates/moving/examples/test_bridge.rs");
    std::process::exit(2)
}

fn num(s: Option<&String>) -> u32 {
    s.and_then(|s| s.parse().ok()).unwrap_or_else(|| usage())
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| a.get(i).cloned().unwrap_or_else(|| usage());
    match a.first().map(String::as_str) {
        Some("name") => {
            let (key, i) = (arg(1), num(a.get(2)));
            println!("{} {}", name(&key, i), sha(&log(&key, i)));
        }
        Some("ping") => {
            let mut b = Bridge { addr: arg(1), fp: arg(2), token: arg(3), client: None };
            let (s, v) = b.call("GET", "/api/can/logs/ping", &[], &[]);
            say(s, &v, "-", "ping");
        }
        Some("send") => {
            let mut b = Bridge { addr: arg(1), fp: arg(2), token: arg(3), client: None };
            let key = arg(4);
            for i in num(a.get(5))..num(a.get(6)) {
                if !b.send_log(&key, i) {
                    std::process::exit(1);
                }
            }
        }
        Some("part") => {
            let mut b = Bridge { addr: arg(1), fp: arg(2), token: arg(3), client: None };
            let (key, i) = (arg(4), num(a.get(5)));
            let (n, l) = (name(&key, i), log(&key, i));
            let (s, v) = b.chunk(&n, &l, 0, l.len() / 2);
            say(s, &v, &n, &format!("chunk 0..{}", l.len() / 2));
            if s != 200 {
                std::process::exit(1);
            }
        }
        Some("run") => {
            let (addr_file, fp, token, key) = (arg(1), arg(2), arg(3), arg(4));
            let (mut i, stop) = (num(a.get(5)), arg(6));
            let mut b = Bridge { addr: String::new(), fp, token, client: None };
            while !Path::new(&stop).exists() {
                let addr = std::fs::read_to_string(&addr_file).unwrap_or_default().trim().to_string();
                if addr != b.addr {
                    println!("address {addr}");
                    b.addr = addr;
                    b.client = None;
                }
                if b.send_log(&key, i) {
                    i += 1;
                    std::thread::sleep(Duration::from_secs(1));
                } else {
                    // refused for now (503 moving or moved) or no answer: the bridge tries again at its next pass
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
            println!("stopped at {i}");
        }
        Some("random-code") => println!("{}", moving::code::MoveCode::random().unwrap().text().as_str()),
        Some("expire-code") => {
            let (data, keys) = (arg(1), arg(2));
            let v = vault::Vault::open(Path::new(&data), Path::new(&keys)).unwrap();
            moving::old::expire_code_now(Path::new(&data), &v).unwrap();
            println!("the waiting move code expired");
        }
        _ => usage(),
    }
}
