//! `sentry-mate`: receives the Screenmate CAN Bridge's logs and (later) turns them into the car's history.
//!
//! Everything it stores is sealed with the vault: the data key opens with the key file in the keys folder (a volume
//! of its own), or with the recovery key shown once when the vault is made.
//!
//! Commands:
//!   serve                         run the upload server (TLS 1.3) until stopped (makes the vault on first start)
//!   init                          make the vault now and print the recovery key
//!   restore                       new machine or lost key file: type the recovery key, a new key file is written
//!   recovery-key new              replace the recovery key (the old one stops working)
//!   token new --car NAME          make an upload token for a car (shown once)
//!   token list | token revoke ID
//!   fingerprint                   the certificate SHA-256 the app pins
//!   log export NAME               a stored log or sidecar, unsealed, to standard output
//!   log candump FILE              a log file of either format as candump text
//!   log check FILE [SIDECAR]      what its signature says
//!   chain KEY                     the chain report of a car key's logs
//!   health                        exit 0 when the upload port answers (for Docker)

mod folders;
mod migrate;

use clap::{Parser, Subcommand};
use ingest::server::{Identity, serve_tls, tls_dir};
use ingest::{Judge, Store, Tokens, Uploads, router};
use sha2::{Digest, Sha256};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use vault::{Kdf, Vault};

#[derive(Parser)]
#[command(name = "sentry-mate", version = VERSION, about = "A car logger fed by the Screenmate CAN Bridge's uploads")]
struct Cli {
    /// Where everything is kept (all of it sealed).
    #[arg(long, env = "SENTRY_MATE_DATA", default_value = "/data", global = true)]
    data: PathBuf,
    /// The key file's folder: its own volume, never inside the data folder or its backups.
    #[arg(long, env = "SENTRY_MATE_KEYS", default_value = "/keys", global = true)]
    keys: PathBuf,
    /// The CAN Bridge upload port (HTTPS).
    #[arg(long, env = "SENTRY_MATE_UPLOAD_PORT", default_value_t = 10100, global = true)]
    upload_port: u16,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the upload server until stopped.
    Serve {
        /// Address to listen on.
        #[arg(long, env = "SENTRY_MATE_BIND", default_value = "0.0.0.0")]
        bind: String,
        /// Host names or addresses for the certificate (comma separated), e.g. the server's LAN name.
        #[arg(long, env = "SENTRY_MATE_NAMES", value_delimiter = ',')]
        names: Vec<String>,
    },
    /// Make the vault now and print its recovery key (once).
    Init,
    /// Open the vault with the recovery key (read from standard input) and write a new key file.
    Restore,
    /// The recovery key.
    RecoveryKey {
        #[command(subcommand)]
        cmd: RecoveryCmd,
    },
    /// Upload tokens (one per car).
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
    /// The SHA-256 of the upload port's certificate, for the app's "certificate SHA-256" field.
    Fingerprint,
    /// Logs.
    Log {
        #[command(subcommand)]
        cmd: LogCmd,
    },
    /// The chain report of a car key's signed logs (JSON).
    Chain { key: String },
    /// Exit 0 when the upload port accepts connections.
    Health,
}

#[derive(Subcommand)]
enum RecoveryCmd {
    /// Make a new recovery key; the old one stops working.
    New,
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Make a token for a car; it is shown once.
    New {
        #[arg(long)]
        car: String,
    },
    List,
    Revoke {
        id: String,
    },
}

#[derive(Subcommand)]
enum LogCmd {
    /// A stored log or sidecar (by its name), unsealed, to standard output.
    Export { name: String },
    /// A stored log's decoded signals as CSV (each value when it changes): time, signal, value, unit, value name.
    Decode { name: String },
    /// A log file of either format as candump text.
    Candump { file: PathBuf },
    /// What a log file's signature says.
    Check { file: PathBuf, sidecar: Option<PathBuf> },
}

fn can_dir(data: &Path) -> PathBuf {
    data.join("can")
}

fn tokens_file(data: &Path) -> PathBuf {
    data.join("tokens.json")
}

const RECOVERY_FILE: &str = "RECOVERY-KEY.txt";

/// The version, and the image build's number, commit and date when it was given them (build.rs).
const VERSION: &str = env!("SENTRY_MATE_VERSION");

#[tokio::main]
async fn main() -> ExitCode {
    vault::harden_process();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();
    let cli = Cli::parse();
    let (data, keys) = (cli.data.clone(), cli.keys.clone());
    match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sentry-mate: {e}");
            if let Some(h) = folders::hint(&*e, &data, &keys) {
                eprintln!("{h}");
            }
            if let Some(h) = restore_hint(&*e) {
                eprintln!("{h}");
            }
            ExitCode::FAILURE
        }
    }
}

fn print_recovery(key: &vault::RecoveryKey) {
    println!("Recovery key (shown once; Sentry Mate does not keep it):\n\n  {}\n", key.as_str());
    println!("Write it down or put it in a password manager, away from this server. With the key file it is the only");
    println!("way to open your data on a new machine: lose both and the data can't be opened by anyone.");
}

/// What to do when the key file is missing or wrong: the restore runs with Sentry Mate stopped, so not with
/// `docker exec`.
fn restore_hint(e: &(dyn std::error::Error + 'static)) -> Option<&'static str> {
    match e.downcast_ref::<vault::Error>()? {
        vault::Error::NoKeyFile | vault::Error::WrongKeyFile => Some(
            "Check the keys folder's path first. To restore with the recovery key, stop Sentry Mate, then: docker \
             compose run --rm -it sentry-mate restore (TrueNAS: Restore in docs/install.md)",
        ),
        _ => None,
    }
}

/// What a command needs of the data and keys folders: whether it writes to each, or `None` when it uses neither.
fn needs(cmd: &Cmd) -> Option<(bool, bool)> {
    match cmd {
        Cmd::Serve { .. } | Cmd::Init | Cmd::Restore => Some((true, true)),
        Cmd::RecoveryKey { .. } | Cmd::Token { .. } | Cmd::Fingerprint => Some((true, false)),
        Cmd::Log { cmd: LogCmd::Export { .. } | LogCmd::Decode { .. } } | Cmd::Chain { .. } => Some((false, false)),
        Cmd::Log { .. } | Cmd::Health => None,
    }
}

async fn run(cli: Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let (data, keys) = (cli.data, cli.keys);
    if let Some((data_write, keys_write)) = needs(&cli.cmd) {
        // both checked, so that both are named when both are wrong (two folders Docker made as root)
        let mut wrong = Vec::new();
        wrong.extend(folders::check(&data, "data", data_write).err());
        // a keys folder inside the data folder is refused by the vault: not made here first
        if !keys.starts_with(&data) {
            wrong.extend(folders::check(&keys, "keys", keys_write).err());
        }
        if !wrong.is_empty() {
            return Err(wrong.join("\n").into());
        }
    }
    match cli.cmd {
        Cmd::Serve { bind, names } => serve(&data, &keys, &bind, cli.upload_port, &names).await?,
        Cmd::Init => {
            std::fs::create_dir_all(&data)?;
            if data.join(vault::VAULT_FILE).exists() {
                eprintln!("there is a vault in {} already", data.display());
                return Ok(ExitCode::FAILURE);
            }
            let (_, rk) = Vault::create(&data, &keys, Kdf::STRONG)?;
            print_recovery(&rk);
        }
        Cmd::Restore => {
            eprint!("Recovery key: ");
            let mut line = zeroize::Zeroizing::new(String::new());
            std::io::stdin().lock().read_line(&mut line)?;
            let v = Vault::restore(&data, &keys, &line)?;
            println!(
                "Restored: a new key file is in {}. The vault opens with it again (key {}).",
                keys.display(),
                v.key_id()
            );
        }
        Cmd::RecoveryKey { cmd: RecoveryCmd::New } => {
            let v = Vault::open(&data, &keys)?;
            let rk = v.new_recovery_key(&data, Kdf::STRONG)?;
            print_recovery(&rk);
            println!("The old recovery key no longer works.");
        }
        Cmd::Token { cmd } => {
            let v = Arc::new(Vault::open(&data, &keys)?);
            let tokens = Tokens::open(tokens_file(&data), v.clone()).await?;
            match cmd {
                TokenCmd::New { car } => {
                    let (token, t) = tokens.create(&car).await?;
                    let fp = Identity::load_or_create(&tls_dir(&data), &[], &v)?.fingerprint();
                    println!("Token for {} (id {}), shown once:\n\n  {token}\n", t.car, t.id);
                    println!("In the CAN Bridge app, Second Sentry Six Manager:");
                    println!("  address              https://<this server>:{}/api/can/logs", cli.upload_port);
                    println!("  token                the one above");
                    println!("  certificate SHA-256  {fp}");
                }
                TokenCmd::List => {
                    for t in tokens.list().await {
                        let state = if t.revoked { "revoked" } else { "in use" };
                        let key = t.vehicle_key.as_deref().unwrap_or("not tied to a car key yet");
                        println!("{}  {:<20}  {state:<8}  {key}", t.id, t.car);
                    }
                }
                TokenCmd::Revoke { id } => {
                    if tokens.revoke(&id).await? {
                        println!("revoked {id}");
                    } else {
                        eprintln!("no token {id} in use");
                        return Ok(ExitCode::FAILURE);
                    }
                }
            }
        }
        Cmd::Fingerprint => {
            let v = Vault::open(&data, &keys)?;
            println!("{}", Identity::load_or_create(&tls_dir(&data), &[], &v)?.fingerprint());
        }
        Cmd::Log { cmd: LogCmd::Decode { name } } => {
            let store = Store::new(can_dir(&data), Arc::new(Vault::open(&data, &keys)?));
            let Some(log) = store.read(&name).await? else {
                eprintln!("no stored log {name}");
                return Ok(ExitCode::FAILURE);
            };
            let side = store
                .read(&format!("{name}.json"))
                .await?
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            decode_csv(&log, &side)?;
        }
        Cmd::Log { cmd: LogCmd::Export { name } } => {
            let store = Store::new(can_dir(&data), Arc::new(Vault::open(&data, &keys)?));
            match store.read(&name).await? {
                Some(bytes) => std::io::stdout().lock().write_all(&bytes)?,
                None => {
                    eprintln!("no stored file {name}");
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
        Cmd::Log { cmd } => return log_cmd(cmd),
        Cmd::Chain { key } => {
            let store = Store::new(can_dir(&data), Arc::new(Vault::open(&data, &keys)?));
            let report = Judge::new(store).chain(&key).await;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Cmd::Health => {
            let addr = format!("127.0.0.1:{}", cli.upload_port);
            let ok = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(&addr))
                .await
                .is_ok_and(|r| r.is_ok());
            return Ok(if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE });
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn log_cmd(cmd: LogCmd) -> Result<ExitCode, Box<dyn std::error::Error>> {
    match cmd {
        LogCmd::Candump { file } => {
            let bytes = std::fs::read(&file)?;
            let out = std::io::stdout();
            let mut out = std::io::BufWriter::new(out.lock());
            let s = logfmt::read_log(&bytes, logfmt::Limits::default(), |l| {
                let _ = out.write_all(l);
                let _ = out.write_all(b"\n");
            })?;
            out.flush()?;
            let cut = if s.truncated { ", cut at the limit" } else { "" };
            eprintln!("{}: {} lines, {} long lines skipped{cut}", s.format.name(), s.lines, s.skipped_long_lines);
        }
        LogCmd::Check { file, sidecar } => {
            let bytes = std::fs::read(&file)?;
            let side_path = sidecar.unwrap_or_else(|| PathBuf::from(format!("{}.json", file.display())));
            let side: serde_json::Value = serde_json::from_slice(&std::fs::read(&side_path)?)?;
            let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let sha = hex::encode(Sha256::digest(&bytes));
            match trust::check(name, &sha, bytes.len() as u64, &side) {
                trust::Check::Signed(l) => {
                    println!("signed and intact: key {}, chain index {}", l.key_fingerprint, l.index)
                }
                trust::Check::NotSigned { bridge_error } => {
                    println!(
                        "not signed{}",
                        bridge_error.map(|e| format!(" (the bridge says: {e})")).unwrap_or_default()
                    );
                }
                trust::Check::Bad { reason, .. } => {
                    println!("failed: {reason}");
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
        LogCmd::Export { .. } | LogCmd::Decode { .. } => {}
    }
    Ok(ExitCode::SUCCESS)
}

/// Decoded signals as CSV, a row when a value changes.
fn decode_csv(log: &[u8], side: &serde_json::Value) -> Result<(), Box<dyn std::error::Error>> {
    let mut d = dbc::Decoder::new(dbc::Profile::Tesla3Y, side);
    let mut last: std::collections::HashMap<&'static str, f64> = std::collections::HashMap::new();
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::new(out.lock());
    writeln!(out, "timestamp_utc,signal,value,unit,value_name")?;
    let mut err = None;
    logfmt::read_log(log, logfmt::Limits::default(), |l| {
        let logfmt::Line::Frame(f) = logfmt::Line::parse(l) else { return };
        for (sig, v) in d.decode(&f) {
            if last.insert(sig.series.as_str(), v) == Some(v) {
                continue;
            }
            let name = sig.values.as_ref().and_then(|n| n.get(&(v as i64))).map(String::as_str).unwrap_or("");
            let (s, us) = (f.time_us / 1_000_000, f.time_us % 1_000_000);
            if let Err(e) = writeln!(out, "{s}.{us:06},{},{v},{},{name}", sig.series, sig.unit) {
                err = Some(e);
            }
        }
    })?;
    out.flush()?;
    err.map_or(Ok(()), |e| Err(e.into()))
}

async fn serve(
    data: &Path,
    keys: &Path,
    bind: &str,
    port: u16,
    names: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!("Sentry Mate {VERSION}");
    std::fs::create_dir_all(can_dir(data))?;
    let (v, made) = Vault::open_or_create(data, keys, Kdf::STRONG)?;
    if let Some(rk) = made {
        // not in the log (container logs are kept in the clear): in a file of the keys folder, for the owner to take
        let file = keys.join(RECOVERY_FILE);
        vault::write_private(&file, format!("Sentry Mate recovery key:\n\n  {}\n\nCopy it somewhere safe away from this server (a password manager, paper), then delete this file.\n", rk.as_str()).as_bytes())?;
        tracing::warn!(
            "new vault made: its recovery key is in {} — copy it somewhere safe off this server, then delete that file",
            file.display()
        );
    } else if keys.join(RECOVERY_FILE).exists() {
        tracing::warn!(
            "{} is still there: copy the recovery key somewhere safe off this server, then delete it",
            keys.join(RECOVERY_FILE).display()
        );
    }
    let v = Arc::new(v);
    let moved = migrate::seal_plain_files(data, &v)?;
    if moved > 0 {
        tracing::info!("sealed {moved} file(s) that were stored in the clear by an older version");
    }
    let id = Identity::load_or_create(&tls_dir(data), names, &v)?;
    let tokens = Tokens::open(tokens_file(data), v.clone()).await?;
    let any_token = tokens.has_any().await;
    let store = Store::new(can_dir(data), v.clone());
    let up = Uploads::new(store.clone(), tokens, Judge::new(store));
    let listener = tokio::net::TcpListener::bind((bind, port)).await?;
    tracing::info!(
        "CAN Bridge uploads on https://{bind}:{port}/api/can/logs (TLS 1.3), certificate SHA-256 {}, vault {}",
        id.fingerprint(),
        v.key_id()
    );
    if !any_token {
        tracing::info!(
            "no upload token yet: make one with `docker exec sentry-mate /sentry-mate token new --car \"My car\"`"
        );
    }

    // logs whose sidecar never came are judged without it (only cars with logs waiting are looked at); unfinished
    // uploads are dropped after two days (only the cars' `uploading/` folders are looked in)
    let housekeeping = up.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        let mut n: u64 = 0;
        loop {
            tick.tick().await;
            if let Err(e) = housekeeping.judge.sweep().await {
                tracing::warn!("judging waiting logs: {e}");
            }
            if n % 60 == 0 {
                let dropped = housekeeping.store.clean_stale(ingest::store::STALE_PARTIAL_MS).await;
                if dropped > 0 {
                    tracing::info!("dropped {dropped} unfinished upload(s) older than two days");
                }
            }
            n += 1;
        }
    });

    let server = serve_tls(listener, id.tls_config()?, router(up));
    tokio::select! {
        r = server => r?,
        _ = shutdown() => tracing::info!("stopping"),
    }
    Ok(())
}

/// Ctrl-C or SIGTERM (`docker stop`).
async fn shutdown() {
    #[cfg(unix)]
    {
        let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) else {
            return std::future::pending().await;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[tokio::test]
    async fn both_folders_are_named_when_both_are_wrong() {
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::write(&data, b"x").unwrap();
        std::fs::write(&keys, b"x").unwrap();
        let cli = Cli { data, keys, upload_port: 0, cmd: Cmd::Fingerprint };
        let Err(e) = run(cli).await else { panic!("it ran") };
        let said = e.to_string();
        assert!(said.contains("the data folder") && said.contains("the keys folder"), "{said}");
    }

    #[test]
    fn a_missing_or_wrong_key_file_says_how_to_restore() {
        for e in [vault::Error::NoKeyFile, vault::Error::WrongKeyFile] {
            let e: Box<dyn std::error::Error> = e.into();
            assert!(restore_hint(&*e).is_some_and(|h| h.contains("docker compose run --rm -it sentry-mate restore")));
        }
        let e: Box<dyn std::error::Error> = vault::Error::NoVault.into();
        assert_eq!(restore_hint(&*e), None);
    }

    #[test]
    fn the_version_starts_with_the_crates_own() {
        assert!(VERSION.starts_with(env!("CARGO_PKG_VERSION")), "{VERSION}");
    }

    #[test]
    fn commands_that_need_no_folder_dont_check_one() {
        assert_eq!(needs(&Cmd::Health), None);
        assert_eq!(needs(&Cmd::Log { cmd: LogCmd::Candump { file: PathBuf::from("x") } }), None);
        assert_eq!(needs(&Cmd::Serve { bind: String::new(), names: vec![] }), Some((true, true)));
        assert_eq!(needs(&Cmd::Chain { key: String::new() }), Some((false, false)));
    }
}
