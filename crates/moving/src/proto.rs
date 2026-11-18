//! The move protocol on the upload port (`/api/move/v1/`): what both sides send, the proofs, the fetch frames and
//! the digest. See `docs/plans/move-server.md`, 2.2 and 2.3.
//!
//! The claim proves the code without giving it away. Both ends take a TLS exporter value from their connection
//! (RFC 8446 section 7.5, label `EXPORTER-sentry-mate-move-v1`, 32 bytes): the same at both ends of one connection,
//! different on any other. The new server sends `proof = HMAC-SHA256(auth, "sentry-mate move v1 client" |
//! exporter)`; the old server answers with the session (`HMAC-SHA256(auth, "sentry-mate move v1 session" |
//! claim)`) and `server_proof = HMAC-SHA256(auth, "sentry-mate move v1 server" | exporter | session | app |
//! cert_sha256 | files | bytes | recovery)`, each field with its length in front. Someone in between runs two
//! connections, one to each side, so a proof made on one is worthless on the other, and a fake server can't make the
//! server proof.

use crate::tree::{Kind, Skipped};
use hmac::{Hmac, Mac};
use sha2::{Digest as _, Sha256};

pub use ingest::api::EXPORTER_LABEL;

/// The protocol's version, in its paths.
pub const BASE: &str = "/api/move/v1";
/// Lines a list answer holds at most.
pub const MAX_LIST: usize = 20_000;
/// Wants a fetch asks for at most.
pub const MAX_WANTS: usize = 4096;
/// The most bytes of files one fetch answer carries.
pub const MAX_FETCH: u64 = 64 << 20;
/// A file is fetched at most this much at a time (so a big file comes in pieces that can be carried on).
pub const PIECE: u64 = 16 << 20;
/// A file whose time is less than this before it was read is fetched again in the next round (a second change
/// with the same length could keep the same time on a file system with coarse times).
pub const RACY_US: u64 = 2_000_000;
/// The bytes before a growing file's copied end that must still be the same (the last copied record's tag).
pub const TAIL: u64 = 16;

type HmacSha256 = Hmac<Sha256>;

fn hmac(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
    // an HMAC takes a key of any length
    let Ok(mut m) = HmacSha256::new_from_slice(key) else { return [0; 32] };
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

/// The new server's proof that it holds the code, bound to its TLS connection.
pub fn client_proof(auth: &[u8; 32], exporter: &[u8; 32]) -> [u8; 32] {
    hmac(auth, &[b"sentry-mate move v1 client", exporter])
}

/// The session a claim gets: made from the claim, so a lost answer gets the same one again.
pub fn session(auth: &[u8; 32], claim: &[u8; 16]) -> [u8; 32] {
    hmac(auth, &[b"sentry-mate move v1 session", claim])
}

/// The old server's proof over everything its claim answer says.
pub fn server_proof(auth: &[u8; 32], exporter: &[u8; 32], a: &ClaimAnswer) -> [u8; 32] {
    let mut m = Vec::with_capacity(512);
    for f in [a.session.as_bytes(), a.app.as_bytes(), a.cert_sha256.as_bytes(), a.recovery.as_bytes()] {
        m.extend_from_slice(&(f.len() as u64).to_be_bytes());
        m.extend_from_slice(f);
    }
    m.extend_from_slice(&a.files.to_be_bytes());
    m.extend_from_slice(&a.bytes.to_be_bytes());
    hmac(auth, &[b"sentry-mate move v1 server", exporter, &m])
}

/// Equal in constant time.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `N` bytes from hex.
pub fn hex_n<const N: usize>(s: &str) -> Option<[u8; N]> {
    hex::decode(s).ok()?.try_into().ok()
}

/// SHA-256 of a session, hex: what the old server keeps of it.
pub fn session_hash(session: &[u8]) -> String {
    hex::encode(Sha256::digest(session))
}

/// What the data key is wrapped for in the finish answer.
pub fn data_key_purpose(key_id: &str, session: &[u8; 32]) -> Vec<u8> {
    let mut p = b"sentry-mate move v1: data key".to_vec();
    p.extend_from_slice(key_id.as_bytes());
    p.extend_from_slice(session);
    p
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClaimRequest {
    pub v: u32,
    /// 16 random bytes (hex) the new server saved first.
    pub claim: String,
    /// The new server's Sentry Mate version.
    pub app: String,
    /// [`client_proof`] (hex).
    pub proof: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClaimAnswer {
    /// The session (hex): `Authorization: Bearer <session>` from now on.
    pub session: String,
    /// [`server_proof`] (hex).
    pub server_proof: String,
    /// The old server's Sentry Mate version.
    pub app: String,
    /// SHA-256 of the old server's certificate (hex): pinned on every connection from now on.
    pub cert_sha256: String,
    pub files: u64,
    pub bytes: u64,
    /// The recovery part of the old server's vault (JSON of [`vault::RecoveryPart`]), as text, so the proof covers
    /// its exact bytes.
    pub recovery: String,
}

/// One file in a list answer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Line {
    pub p: String,
    /// `s` sealed whole, `r` records, `p` plain.
    pub k: String,
    /// For records while writes go on: the end of the last whole record; else the file's length.
    pub len: u64,
    pub mt: u64,
    /// The file id (hex), for sealed and records files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// The last line of a list answer.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct End {
    pub end: bool,
    /// Where the next page starts (`after=`), or none: this was the last.
    pub next: Option<String>,
    pub now_us: u64,
    /// The pause this list was made in, if any: then records files are given whole as stored.
    pub paused: Option<String>,
    #[serde(default)]
    pub skipped: Vec<SkippedLine>,
    #[serde(default)]
    pub skipped_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkippedLine {
    pub path: String,
    pub why: String,
}

impl From<&Skipped> for SkippedLine {
    fn from(s: &Skipped) -> SkippedLine {
        SkippedLine { path: s.path.clone(), why: s.why.to_string() }
    }
}

pub fn kind_letter(k: Kind) -> &'static str {
    match k {
        Kind::Sealed => "s",
        Kind::Records => "r",
        Kind::Plain => "p",
    }
}

pub fn kind_of_letter(s: &str) -> Option<Kind> {
    match s {
        "s" => Some(Kind::Sealed),
        "r" => Some(Kind::Records),
        "p" => Some(Kind::Plain),
        _ => None,
    }
}

/// A plain file's version as a want names it.
pub fn plain_version(len: u64, mt: u64) -> String {
    format!("{len}:{mt}")
}

/// One piece of a file a fetch asks for.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Want {
    pub p: String,
    pub from: u64,
    pub to: u64,
    /// The version it continues: the file id (hex) for sealed and records files, `len:mt` for plain ones.
    pub v: String,
    /// For a growing file's catch-up: the 16 bytes before `from` (hex) must still be the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FetchRequest {
    pub want: Vec<Want>,
}

/// A frame of a fetch answer: no bytes when changed or gone.
pub const CHANGED: u8 = 1;
pub const GONE: u8 = 2;

/// A frame's head (the bytes follow it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub p: String,
    pub from: u64,
    pub count: u64,
    pub len_now: u64,
    pub mt: u64,
    pub read_at: u64,
    pub flags: u8,
}

impl Frame {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(1);
        crate::format::put_varint(out, self.p.len() as u64);
        out.extend_from_slice(self.p.as_bytes());
        for n in [self.from, self.count, self.len_now, self.mt, self.read_at] {
            out.extend_from_slice(&n.to_be_bytes());
        }
        out.push(self.flags);
    }
}

/// The end of a fetch answer.
pub fn encode_end(out: &mut Vec<u8>, frames: u64) {
    out.push(0);
    crate::format::put_varint(out, frames);
}

/// What reading the front of a fetch answer found.
pub enum Read {
    Need,
    Bad(&'static str),
    Frame(Frame, usize),
    End(u64, usize),
}

/// Reads a frame's head or the end from the front of `b`.
pub fn decode(b: &[u8]) -> Read {
    use crate::format::{Decoded, get_varint};
    let Some(&tag) = b.first() else { return Read::Need };
    match tag {
        0 => match get_varint(&b[1..]) {
            Decoded::Need => Read::Need,
            Decoded::Bad(w) => Read::Bad(w),
            Decoded::Got(n, k) => Read::End(n, 1 + k),
        },
        1 => {
            let (n, k) = match get_varint(&b[1..]) {
                Decoded::Need => return Read::Need,
                Decoded::Bad(w) => return Read::Bad(w),
                Decoded::Got(n, k) => (n, k),
            };
            if n == 0 || n > crate::tree::MAX_PATH as u64 {
                return Read::Bad("a path of no length, or too long");
            }
            let at = 1 + k + n as usize;
            let end = at + 5 * 8 + 1;
            if b.len() < end {
                return Read::Need;
            }
            let Ok(p) = std::str::from_utf8(&b[1 + k..at]) else { return Read::Bad("a path that isn't text") };
            let num = |i: usize| {
                let mut x = [0u8; 8];
                x.copy_from_slice(&b[at + 8 * i..at + 8 * i + 8]);
                u64::from_be_bytes(x)
            };
            let f = Frame {
                p: p.to_string(),
                from: num(0),
                count: num(1),
                len_now: num(2),
                mt: num(3),
                read_at: num(4),
                flags: b[end - 1],
            };
            Read::Frame(f, end)
        }
        _ => Read::Bad("an unknown frame"),
    }
}

/// Whether a file read at `read_at` with time `mt` must be fetched again next round.
pub fn racy(mt: u64, read_at: u64) -> bool {
    read_at >= mt && read_at - mt < RACY_US
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PauseAnswer {
    pub pause: String,
    pub until_ms: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FinishRequest {
    pub pause: String,
    pub files: u64,
    pub bytes: u64,
    /// [`Digest`] (hex).
    pub digest: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct FinishAnswer {
    pub moved_ms: u64,
    pub key_id: String,
    /// The data key wrapped with the code's wrap key, for [`data_key_purpose`].
    pub data_key: vault::WrappedKey,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SwitchedRequest {
    pub at_ms: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StatusAnswer {
    /// `copying`, `paused`, `moved` or `done`.
    pub state: String,
    pub pause: Option<String>,
    pub moved_ms: Option<u64>,
    pub switched_ms: Option<u64>,
}

/// The digest of a whole tree at one moment: SHA-256 over `path \t kind \t length \t time \t file id \n` for every
/// file in walk order (the file id empty for plain files), with the count and the bytes.
#[derive(Clone)]
pub struct Digest {
    h: Sha256,
    pub files: u64,
    pub bytes: u64,
}

impl Default for Digest {
    fn default() -> Digest {
        Digest { h: Sha256::new(), files: 0, bytes: 0 }
    }
}

impl Digest {
    pub fn add(&mut self, path: &str, kind: Kind, len: u64, mt: u64, id: Option<&str>) {
        let line = format!("{path}\t{}\t{len}\t{mt}\t{}\n", kind_letter(kind), id.unwrap_or(""));
        self.h.update(line.as_bytes());
        self.files += 1;
        self.bytes += len;
    }

    pub fn hex(&self) -> String {
        hex::encode(self.h.clone().finalize())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn frames_read_back() {
        let f = Frame { p: "can/a/b.log.xz".into(), from: 5, count: 7, len_now: 100, mt: 9, read_at: 10, flags: 0 };
        let mut b = Vec::new();
        f.encode(&mut b);
        b.extend_from_slice(b"1234567");
        encode_end(&mut b, 1);
        let Read::Frame(g, n) = decode(&b) else { panic!() };
        assert_eq!(g, f);
        assert_eq!(&b[n..n + 7], b"1234567");
        assert!(matches!(decode(&b[n + 7..]), Read::End(1, 2)));
        for cut in 0..n {
            assert!(matches!(decode(&b[..cut]), Read::Need), "{cut}");
        }
        assert!(matches!(decode(&[9]), Read::Bad(_)));
    }

    #[test]
    fn racy_is_only_a_time_just_before_the_read() {
        assert!(racy(1_000_000, 1_000_000));
        assert!(racy(1_000_000, 2_999_999));
        assert!(!racy(1_000_000, 3_000_000));
        assert!(!racy(5_000_000, 1_000_000), "a time in the future is never racy");
    }

    #[test]
    fn proofs_are_bound_to_the_connection() {
        let auth = [7u8; 32];
        assert_ne!(client_proof(&auth, &[1; 32]), client_proof(&auth, &[2; 32]));
        assert_ne!(client_proof(&auth, &[1; 32]), client_proof(&[8; 32], &[1; 32]));
        assert_eq!(session(&auth, &[3; 16]), session(&auth, &[3; 16]));
        assert_ne!(session(&auth, &[3; 16]), session(&auth, &[4; 16]));
        assert!(same(b"abc", b"abc") && !same(b"abc", b"abd") && !same(b"abc", b"ab"));
    }
}
