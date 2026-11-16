//! What each uploaded log is worth: judged once its log and sidecar are both here, against the car key's pairing.
//!
//! ```text
//! <can>/<vehicleKey>/trust.json   { v, pin, new_key, logs: { name: Record } }
//! ```
//!
//! The first signed log of a key pins its public key; from then on only logs signed by it are verified. A log
//! whose sidecar hasn't come after [`WAIT_MS`] is judged without it (unsigned, or failed once the car is paired).
//! Records judged by older rules ([`trust::RULES_VERSION`]) are judged again by [`Judge::rejudge_all`].

use crate::store::now_ms;
use crate::tokens::write_atomic;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;
use trust::{Check, Entry, Link, LogName, Verdict};

/// A log without its sidecar this long is judged without it.
pub const WAIT_MS: u64 = 10 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Verified,
    Failed,
    CouldNotSign,
    Unsigned,
    Waiting,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Record {
    pub v: u32,
    pub status: Status,
    pub reason: Option<String>,
    /// The chain fields, for a log signed by some key (its key may not be the paired one).
    pub link: Option<Link>,
    /// The owner said to use it anyway.
    #[serde(default)]
    pub override_use: bool,
    pub judged_ms: u64,
    /// When the log itself arrived (for the sidecar wait).
    pub arrived_ms: u64,
}

impl Record {
    /// Whether its data may be decoded.
    pub fn usable(&self) -> bool {
        matches!(self.status, Status::Verified | Status::Unsigned) || self.override_use
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Pin {
    pub fingerprint: String,
    pub pinned_ms: u64,
    /// "first-signed-log" or "owner".
    pub how: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Book {
    pub v: u32,
    pub pin: Option<Pin>,
    /// Another key that signed logs here, offered to the owner to trust.
    pub new_key: Option<String>,
    pub logs: BTreeMap<String, Record>,
}

pub struct Judge {
    dir: PathBuf,
    books: Mutex<HashMap<String, Book>>,
}

async fn hash_file(file: &Path) -> std::io::Result<(String, u64)> {
    let bytes = tokio::fs::read(file).await?;
    Ok((hex::encode(Sha256::digest(&bytes)), bytes.len() as u64))
}

impl Judge {
    pub fn new(can_dir: impl Into<PathBuf>) -> Judge {
        Judge {
            dir: can_dir.into(),
            books: Mutex::new(HashMap::new()),
        }
    }

    fn file_of(&self, key: &str) -> PathBuf {
        self.dir.join(key).join("trust.json")
    }

    async fn load<'a>(&self, books: &'a mut HashMap<String, Book>, key: &str) -> &'a mut Book {
        if !books.contains_key(key) {
            let book = match tokio::fs::read(self.file_of(key)).await {
                Ok(b) => serde_json::from_slice(&b).unwrap_or_default(),
                Err(_) => Book {
                    v: trust::RULES_VERSION,
                    ..Book::default()
                },
            };
            books.insert(key.to_string(), book);
        }
        books.entry(key.to_string()).or_default()
    }

    async fn save(&self, key: &str, book: &Book) -> std::io::Result<()> {
        write_atomic(
            &self.file_of(key),
            &serde_json::to_vec(book).map_err(std::io::Error::other)?,
        )
        .await
    }

    /// A log or sidecar finished arriving: judge its log if both are here (or mark it waiting).
    pub async fn arrived(&self, name: &str) -> std::io::Result<Option<Record>> {
        let Some(n) = LogName::parse(name) else {
            return Ok(None);
        };
        let log_name = name.strip_suffix(".json").unwrap_or(name).to_string();
        let key = n.vehicle_key.to_string();
        let mut books = self.books.lock().await;
        let book = self.load(&mut books, &key).await;
        let r = self.judge_one(book, &key, &log_name, false).await?;
        let book = book.clone();
        drop(books);
        self.save(&key, &book).await?;
        Ok(r)
    }

    /// Judges one log into `book`. `force`: judge without the sidecar if it is still missing.
    async fn judge_one(
        &self,
        book: &mut Book,
        key: &str,
        log_name: &str,
        force: bool,
    ) -> std::io::Result<Option<Record>> {
        let log = self.dir.join(key).join(log_name);
        if tokio::fs::metadata(&log).await.is_err() {
            return Ok(None);
        }
        let arrived_ms = book
            .logs
            .get(log_name)
            .map_or_else(now_ms, |r| r.arrived_ms);
        let side_bytes = tokio::fs::read(self.dir.join(key).join(format!("{log_name}.json")))
            .await
            .ok();
        let waited = now_ms().saturating_sub(arrived_ms) >= WAIT_MS;
        let Some(side) = side_bytes
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .or(if force || waited {
                Some(serde_json::Value::Null)
            } else {
                None
            })
        else {
            let r = Record {
                v: trust::RULES_VERSION,
                status: Status::Waiting,
                reason: None,
                link: None,
                override_use: false,
                judged_ms: now_ms(),
                arrived_ms,
            };
            book.logs.insert(log_name.to_string(), r.clone());
            return Ok(Some(r));
        };
        let (sha, len) = hash_file(&log).await?;
        let check = trust::check(log_name, &sha, len, &side);
        let verdict = trust::judge(&check, book.pin.as_ref().map(|p| p.fingerprint.as_str()));
        let link = match &check {
            Check::Signed(l) => Some(l.clone()),
            Check::Bad { link, .. } => link.clone(),
            Check::NotSigned { .. } => None,
        };
        if let (Verdict::Verified, None, Some(l)) = (&verdict, &book.pin, &link) {
            book.pin = Some(Pin {
                fingerprint: l.key_fingerprint.clone(),
                pinned_ms: now_ms(),
                how: "first-signed-log".into(),
            });
        }
        if let (Verdict::Failed(reason), Some(l)) = (&verdict, &link) {
            if reason == "signed by another key" && book.new_key.is_none() {
                book.new_key = Some(l.key_fingerprint.clone());
            }
        }
        let (status, reason) = match verdict {
            Verdict::Verified => (Status::Verified, None),
            Verdict::Failed(r) => (Status::Failed, Some(r)),
            Verdict::CouldNotSign(r) => (Status::CouldNotSign, Some(r)),
            Verdict::Unsigned => (Status::Unsigned, None),
        };
        let override_use = book.logs.get(log_name).is_some_and(|r| r.override_use);
        let r = Record {
            v: trust::RULES_VERSION,
            status,
            reason,
            link,
            override_use,
            judged_ms: now_ms(),
            arrived_ms,
        };
        book.logs.insert(log_name.to_string(), r.clone());
        Ok(Some(r))
    }

    /// Logs still waiting for their sidecar past [`WAIT_MS`] are judged without it. Returns how many.
    pub async fn sweep(&self) -> std::io::Result<usize> {
        let keys: Vec<String> = match tokio::fs::read_dir(&self.dir).await {
            Ok(mut rd) => {
                let mut v = Vec::new();
                while let Ok(Some(e)) = rd.next_entry().await {
                    if let Some(k) = e.file_name().to_str().filter(|k| !k.starts_with('.')) {
                        v.push(k.to_string());
                    }
                }
                v
            }
            Err(_) => return Ok(0),
        };
        let mut n = 0;
        for key in keys {
            let mut books = self.books.lock().await;
            let book = self.load(&mut books, &key).await;
            let waiting: Vec<String> = book
                .logs
                .iter()
                .filter(|(_, r)| {
                    r.status == Status::Waiting && now_ms().saturating_sub(r.arrived_ms) >= WAIT_MS
                })
                .map(|(k, _)| k.clone())
                .collect();
            for name in &waiting {
                self.judge_one(book, &key, name, true).await?;
            }
            n += waiting.len();
            if !waiting.is_empty() {
                let b = book.clone();
                drop(books);
                self.save(&key, &b).await?;
            }
        }
        Ok(n)
    }

    /// Judges every log of a key again (after a pairing change, or records from older rules).
    pub async fn rejudge_all(&self, key: &str) -> std::io::Result<usize> {
        let mut books = self.books.lock().await;
        let book = self.load(&mut books, key).await;
        let names: Vec<String> = book.logs.keys().cloned().collect();
        for name in &names {
            let force = book
                .logs
                .get(name)
                .is_some_and(|r| r.status != Status::Waiting);
            self.judge_one(book, key, name, force).await?;
        }
        book.v = trust::RULES_VERSION;
        let b = book.clone();
        drop(books);
        self.save(key, &b).await?;
        Ok(names.len())
    }

    /// The owner trusts another key (checked by its fingerprint in the app): it becomes the pin, all logs judged again.
    pub async fn trust_key(&self, key: &str, fingerprint: &str) -> std::io::Result<()> {
        {
            let mut books = self.books.lock().await;
            let book = self.load(&mut books, key).await;
            book.pin = Some(Pin {
                fingerprint: fingerprint.to_string(),
                pinned_ms: now_ms(),
                how: "owner".into(),
            });
            book.new_key = None;
        }
        self.rejudge_all(key).await.map(|_| ())
    }

    pub async fn book(&self, key: &str) -> Book {
        let mut books = self.books.lock().await;
        self.load(&mut books, key).await.clone()
    }

    pub async fn record(&self, key: &str, log_name: &str) -> Option<Record> {
        self.book(key).await.logs.get(log_name).cloned()
    }

    /// The chain report of a key's signed logs by its paired key.
    pub async fn chain(&self, key: &str) -> trust::ChainReport {
        let book = self.book(key).await;
        let Some(pin) = &book.pin else {
            return trust::ChainReport::default();
        };
        let entries: Vec<Entry> = book
            .logs
            .values()
            .filter_map(|r| {
                let l = r.link.as_ref()?;
                if !trust::same_fingerprint(&l.key_fingerprint, &pin.fingerprint) {
                    return None;
                }
                match r.status {
                    Status::Verified => Some(Entry {
                        link: l.clone(),
                        failed: None,
                    }),
                    Status::Failed => Some(Entry {
                        link: l.clone(),
                        failed: r.reason.clone(),
                    }),
                    _ => None,
                }
            })
            .collect();
        trust::chain(&entries)
    }
}
