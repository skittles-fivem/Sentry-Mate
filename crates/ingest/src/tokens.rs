//! Upload tokens: one per car, write-only. Only each token's SHA-256 is kept. A token's first upload ties it to
//! that car's CAN Bridge key; from then on it can upload, and ask about, only that key's files.

use base64::Engine;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use vault::Vault;

/// At most this many tokens in use at once.
pub const MAX_TOKENS: usize = 20;
const PREFIX: &str = "smt_";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Token {
    pub id: String,
    /// The car's name, as the owner typed it.
    pub car: String,
    /// SHA-256 (hex) of the token.
    pub hash: String,
    /// The CAN Bridge key it is tied to, after its first upload.
    pub vehicle_key: Option<String>,
    pub created_ms: u64,
    pub revoked: bool,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Book {
    tokens: Vec<Token>,
}

/// The token file, read afresh on every use (it is tiny), so tokens made from the command line while the server runs
/// work at once. Changes are made one at a time and written whole.
pub struct Tokens {
    file: PathBuf,
    vault: Arc<Vault>,
    lock: Mutex<()>,
}

const LABEL: &str = "tokens.json";

fn hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.trim().as_bytes()))
}

impl Tokens {
    pub async fn open(file: impl Into<PathBuf>, vault: Arc<Vault>) -> std::io::Result<Tokens> {
        let t = Tokens { file: file.into(), vault, lock: Mutex::new(()) };
        t.load().await?;
        Ok(t)
    }

    async fn load(&self) -> std::io::Result<Book> {
        match self.vault.read_file(&self.file, LABEL).map_err(std::io::Error::other)? {
            Some(b) => serde_json::from_slice(&b).map_err(std::io::Error::other),
            None => Ok(Book::default()),
        }
    }

    async fn save(&self, book: &Book) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(book).map_err(std::io::Error::other)?;
        self.vault.write_file(&self.file, LABEL, &bytes).map_err(std::io::Error::other)
    }

    /// A new token for a car: returns it (shown once) and its record.
    pub async fn create(&self, car: &str) -> std::io::Result<(String, Token)> {
        let _one = self.lock.lock().await;
        let mut book = self.load().await?;
        if book.tokens.iter().filter(|t| !t.revoked).count() >= MAX_TOKENS {
            return Err(std::io::Error::other(format!("at most {MAX_TOKENS} tokens; revoke one first")));
        }
        let mut raw = [0u8; 32];
        getrandom::fill(&mut raw).map_err(std::io::Error::other)?;
        let token = format!("{PREFIX}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw));
        let mut id = [0u8; 6];
        getrandom::fill(&mut id).map_err(std::io::Error::other)?;
        let t = Token {
            id: hex::encode(id),
            car: trust::one_line(car, 80),
            hash: hash(&token),
            vehicle_key: None,
            created_ms: crate::store::now_ms(),
            revoked: false,
        };
        book.tokens.push(t.clone());
        self.save(&book).await?;
        Ok((token, t))
    }

    /// The token's record, if it is one in use.
    pub async fn find(&self, token: &str) -> Option<Token> {
        let h = hash(token);
        self.load().await.ok()?.tokens.into_iter().find(|t| !t.revoked && t.hash == h)
    }

    /// Ties a token to a car key (its first upload). Returns the key it is tied to (an earlier one wins).
    pub async fn bind(&self, id: &str, vehicle_key: &str) -> std::io::Result<String> {
        let _one = self.lock.lock().await;
        let mut book = self.load().await?;
        let Some(t) = book.tokens.iter_mut().find(|t| t.id == id) else {
            return Err(std::io::Error::other("no such token"));
        };
        if let Some(k) = &t.vehicle_key {
            return Ok(k.clone());
        }
        t.vehicle_key = Some(vehicle_key.to_string());
        self.save(&book).await?;
        Ok(vehicle_key.to_string())
    }

    pub async fn revoke(&self, id: &str) -> std::io::Result<bool> {
        let _one = self.lock.lock().await;
        let mut book = self.load().await?;
        let Some(t) = book.tokens.iter_mut().find(|t| t.id == id && !t.revoked) else {
            return Ok(false);
        };
        t.revoked = true;
        self.save(&book).await?;
        Ok(true)
    }

    pub async fn list(&self) -> Vec<Token> {
        self.load().await.map(|b| b.tokens).unwrap_or_default()
    }
}
