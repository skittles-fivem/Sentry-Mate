//! A sealed key/value store for the other parts of Sentry Mate (the address cache, sessions, a UI pack's notes...):
//! `namespace` + `key` → bytes. One sealed file per key, so it holds any number of keys without loading them, and a
//! write replaces one small file. File names are a keyed hash (HMAC-SHA256 with a random key kept sealed next to
//! them), so neither namespaces nor keys show on disk:
//!
//! ```text
//! db/kv/names.key                   the hashing key (sealed)
//! db/kv/<hash of namespace>/<hash of namespace and key>.kv
//!       = "SMDB" | kind 4 | version | varint namespace length | namespace | varint key length | key | value
//! ```

use crate::error::{Error, Result};
use crate::file::{Ctx, Kind, check_header, header, remove_if_exists};
use crate::segment::columns::{Reader, put_varint};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use zeroize::Zeroizing;

/// The largest value kept (a store of small things; big files belong elsewhere).
pub const MAX_VALUE: usize = 16 << 20;
const STRIPES: usize = 64;

pub struct Kv {
    ctx: Arc<Ctx>,
    dir: PathBuf,
    names: OnceLock<Zeroizing<[u8; 32]>>,
    init: Mutex<()>,
    /// Writes of the same key one at a time (their temporary file is the same).
    stripes: Vec<Mutex<()>>,
}

impl Kv {
    pub(crate) fn new(ctx: Arc<Ctx>, dir: PathBuf) -> Kv {
        Kv {
            ctx,
            dir,
            names: OnceLock::new(),
            init: Mutex::new(()),
            stripes: (0..STRIPES).map(|_| Mutex::new(())).collect(),
        }
    }

    fn name_key(&self) -> Result<&[u8; 32]> {
        if let Some(k) = self.names.get() {
            return Ok(k);
        }
        let _one = self.init.lock().map_err(|_| Error::invalid("kv lock"))?;
        if let Some(k) = self.names.get() {
            return Ok(k);
        }
        let file = self.dir.join("names.key");
        let label = self.ctx.label(&file);
        let key = match self.ctx.read(&file)? {
            Some(b) => {
                let body = check_header(&b, Kind::NameKey, &label)?;
                let mut k = Zeroizing::new([0u8; 32]);
                if body.len() != 32 {
                    return Err(Error::Damaged { file: label, why: "not a key".into() });
                }
                k.copy_from_slice(body);
                k
            }
            None => {
                let mut k = Zeroizing::new([0u8; 32]);
                getrandom::fill(&mut *k).map_err(|e| Error::invalid(format!("no random numbers: {e}")))?;
                let mut b = Zeroizing::new(header(Kind::NameKey));
                b.extend_from_slice(&*k);
                self.ctx.write_atomic(&file, &b)?;
                k
            }
        };
        Ok(self.names.get_or_init(|| key))
    }

    fn hash(&self, parts: &[&str]) -> Result<String> {
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(self.name_key()?).map_err(|_| Error::invalid("hmac key"))?;
        for p in parts {
            m.update(&(p.len() as u64).to_be_bytes());
            m.update(p.as_bytes());
        }
        Ok(hex::encode(&m.finalize().into_bytes()[..16]))
    }

    fn file(&self, ns: &str, key: &str) -> Result<(PathBuf, usize)> {
        let d = self.hash(&["namespace", ns])?;
        let f = self.hash(&["key", ns, key])?;
        let stripe = usize::from_str_radix(&f[..2], 16).unwrap_or(0) % STRIPES;
        Ok((self.dir.join(d).join(format!("{f}.kv")), stripe))
    }

    fn parse(&self, file: &std::path::Path, b: &[u8]) -> Result<(String, String, Vec<u8>)> {
        let label = self.ctx.label(file);
        let body = check_header(b, Kind::Kv, &label)?;
        let mut r = Reader::new(body);
        let text = |r: &mut Reader<'_>| -> Result<String> {
            let n = r.count(1)?;
            String::from_utf8(r.bytes(n)?.to_vec()).map_err(|_| Error::Corrupt("not text".into()))
        };
        let ns = text(&mut r).map_err(|e| e.in_file(&label))?;
        let key = text(&mut r).map_err(|e| e.in_file(&label))?;
        Ok((ns, key, r.rest().to_vec()))
    }

    pub fn get(&self, ns: &str, key: &str) -> Result<Option<Vec<u8>>> {
        let (file, _) = self.file(ns, key)?;
        let Some(b) = self.ctx.read(&file)? else { return Ok(None) };
        let (n, k, v) = self.parse(&file, &b)?;
        if n != ns || k != key {
            return Err(Error::Damaged { file: self.ctx.label(&file), why: "holds another key".into() });
        }
        Ok(Some(v))
    }

    pub fn put(&self, ns: &str, key: &str, value: &[u8]) -> Result<()> {
        if value.len() > MAX_VALUE {
            return Err(Error::invalid(format!("a value can be at most {MAX_VALUE} bytes")));
        }
        let (file, stripe) = self.file(ns, key)?;
        let mut b = Zeroizing::new(header(Kind::Kv));
        put_varint(&mut b, ns.len() as u64);
        b.extend_from_slice(ns.as_bytes());
        put_varint(&mut b, key.len() as u64);
        b.extend_from_slice(key.as_bytes());
        b.extend_from_slice(value);
        let _one = self.stripes[stripe].lock().map_err(|_| Error::invalid("kv lock"))?;
        self.ctx.write_atomic(&file, &b)
    }

    /// Whether there was such a key.
    pub fn delete(&self, ns: &str, key: &str) -> Result<bool> {
        let (file, stripe) = self.file(ns, key)?;
        let _one = self.stripes[stripe].lock().map_err(|_| Error::invalid("kv lock"))?;
        remove_if_exists(&file)
    }

    /// Every key of a namespace (this reads each of its files: for listing now and then, not every request).
    pub fn keys(&self, ns: &str) -> Result<Vec<String>> {
        let dir = self.dir.join(self.hash(&["namespace", ns])?);
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut keys = Vec::new();
        for e in rd {
            let path = e?.path();
            if path.extension().is_none_or(|x| x != "kv") {
                continue;
            }
            if let Some(b) = self.ctx.read(&path)? {
                let (n, k, _) = self.parse(&path, &b)?;
                if n == ns {
                    keys.push(k);
                }
            }
        }
        keys.sort();
        Ok(keys)
    }
}
