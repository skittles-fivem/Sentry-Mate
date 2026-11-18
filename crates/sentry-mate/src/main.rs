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
//!   export > FILE                 the whole server as one export file (opens elsewhere with the recovery key)
//!   import FILE [--cancel]        a new server: bring an export file in (asks for the recovery key)
//!   health                        exit 0 when the upload port answers (for Docker)

mod migrate;

use clap::{Parser, Subcommand};
use ingest::server::{Identity, serve_tls, tls_dir};
use ingest::{Judge, Store, Tokens, Uploads, router};
use sha2::{Digest, Sha256};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};
use vault::{Kdf, Vault};

/// This version (an export or import says it, an import refuses an export from a newer one).
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// How long a command waits for a pause of the writes (an export's snapshot) to end, or a snapshot for the writes
/// under way.
const GATE_WAIT: Duration = Duration::from_secs(60);

#[derive(Parser)]
#[command(name = "sentry-mate", version, about = "A car logger fed by the Screenmate CAN Bridge's uploads")]
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
    /// The whole server as one export file, to standard output: `docker exec <container> /sentry-mate export >
    /// sentry-mate.smexport`. It opens on a new server with this server's recovery key.
    Export,
    /// On a new server: bring an export file in (put it in the data folder first). Asks for the recovery key, or
    /// reads it from standard input. Run it again after a restart to carry on.
    Import {
        /// The export file, e.g. /data/sentry-mate-2026-11-17.smexport.
        file: Option<PathBuf>,
        /// Remove a half-done import instead (nothing else is touched).
        #[arg(long)]
        cancel: bool,
    },
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

#[tokio::main]
async fn main() -> ExitCode {
    vault::harden_process();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sentry-mate: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_recovery(key: &vault::RecoveryKey) {
    println!("Recovery key (shown once; Sentry Mate does not keep it):\n\n  {}\n", key.as_str());
    println!("Write it down or put it in a password manager, away from this server. With the key file it is the only");
    println!("way to open your data on a new machine: lose both and the data can't be opened by anyone.");
}

async fn run(cli: Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let (data, keys) = (cli.data, cli.keys);
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
            // a token's files are part of the data folder: written through its gate, like the server's writes
            let _pass = gate::Gate::open(&data)?.enter(GATE_WAIT)?;
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
            let _pass = gate::Gate::open(&data)?.enter(GATE_WAIT)?;
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
        Cmd::Export => return export_cmd(&data, &keys),
        Cmd::Import { file, cancel } => return import_cmd(&data, &keys, file.as_deref(), cancel).await,
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
    std::fs::create_dir_all(data)?;
    let _lock = take_server_lock(data).await?;
    let gate = gate::Gate::open(data)?;
    // before the vault is opened or made: what a crash left, and an import ready to switch to
    moving::import::tidy(data, keys);
    let mut refused: Option<String> = None;
    loop {
        if !carry_on_import(data, keys, &mut refused).await {
            tracing::info!("stopping");
            return Ok(());
        }
        let Instance { mut server, housekeeping } = start(data, keys, &gate, bind, port, names).await?;
        let ready = import_ready(data.to_path_buf(), keys.to_path_buf(), refused.clone());
        let switch = tokio::select! {
            r = &mut server => {
                housekeeping.abort();
                r?;
                return Ok(());
            }
            _ = shutdown() => false,
            _ = ready => true,
        };
        if !switch {
            housekeeping.abort();
            tracing::info!("stopping");
            return Ok(());
        }
        // an import was read and checked by `import` while this server ran: stop the writes, switch, start again
        tracing::info!("import: the imported data is checked; this server stops its uploads and switches to it");
        let g = gate.clone();
        let pause = match tokio::task::spawn_blocking(move || g.pause(GATE_WAIT)).await? {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("import: the writes didn't stop in time ({e}); trying again");
                housekeeping.abort();
                continue;
            }
        };
        gate.close(gate::Closed {
            code: "moving",
            message: "This server is switching to imported data; try again in a minute.".into(),
            retry_after_s: ingest::api::MOVING_RETRY_S,
        });
        // the upload port and its connections end here (no write is under way: the pause waited for them)
        drop(server);
        housekeeping.abort();
        let (d, k) = (data.to_path_buf(), keys.to_path_buf());
        let done = tokio::task::spawn_blocking(move || moving::import::switch(&d, &k)).await?;
        report_switch(data, keys, done, &mut refused);
        drop(pause);
        gate.reopen();
    }
}

/// What one start of the server runs: the upload port and the housekeeping.
struct Instance {
    server: std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>> + Send>>,
    housekeeping: tokio::task::JoinHandle<()>,
}

/// `.lock`: one server per data folder. Waits (and says so) while another holds it, rather than exiting into a
/// restart loop.
async fn take_server_lock(data: &Path) -> std::io::Result<gate::ServerLock> {
    let mut said = false;
    loop {
        if let Some(lock) = gate::ServerLock::try_take(data)? {
            return Ok(lock);
        }
        if !said {
            tracing::warn!(
                "another Sentry Mate uses this data folder (a second container on the same volume, or a command that needs it alone): waiting until it stops"
            );
            said = true;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn report_switch(
    data: &Path,
    keys: &Path,
    done: Result<moving::Switched, moving::Error>,
    refused: &mut Option<String>,
) {
    match done {
        Ok(s) => {
            tracing::info!(
                "import: done: this server holds the data of the export made by Sentry Mate {} on {} ({} files, {})",
                s.app,
                moving::export::day_of(s.made_ms),
                s.files,
                moving::size(s.bytes)
            );
            tracing::info!(
                "import: the CAN Bridge's token and certificate SHA-256 stay as they were; if this server has another address, change it in the CAN Bridge app. The old server's recovery key opens the data here too."
            );
            if s.replaced_first_start {
                tracing::info!(
                    "import: the vault this server made at its first start was replaced; its {} opened nothing and was removed",
                    moving::import::RECOVERY_FILE
                );
            }
        }
        Err(e) => {
            tracing::error!("import: the switch didn't happen: {e}");
            if let moving::Pending::Ready { export_id } = moving::import::pending(data, keys) {
                *refused = Some(export_id);
            }
        }
    }
}

async fn pending_import(data: &Path, keys: &Path) -> moving::Pending {
    let (d, k) = (data.to_path_buf(), keys.to_path_buf());
    tokio::task::spawn_blocking(move || moving::import::pending(&d, &k)).await.unwrap_or(moving::Pending::None)
}

/// How long a server whose switch to an import didn't finish waits before it tries again.
const SWITCH_AGAIN: Duration = Duration::from_secs(30);

/// On start: an import that was ready, or whose switch a crash stopped, is switched to now. A switch that has begun
/// is finished before anything else: the server never starts on a data folder switched half way (files of the
/// import beside the vault of the first start). If it can't be finished (a full disk, say), the server waits and
/// tries again. `false`: the server was told to stop meanwhile.
async fn carry_on_import(data: &Path, keys: &Path, refused: &mut Option<String>) -> bool {
    loop {
        match pending_import(data, keys).await {
            moving::Pending::None => return true,
            moving::Pending::Ready { export_id } if refused.as_deref() == Some(export_id.as_str()) => return true,
            moving::Pending::Ready { .. } | moving::Pending::Switching => {
                tracing::info!("import: switching to the imported data");
                let (d, k) = (data.to_path_buf(), keys.to_path_buf());
                match tokio::task::spawn_blocking(move || moving::import::switch(&d, &k)).await {
                    Ok(done) => report_switch(data, keys, done, refused),
                    Err(e) => tracing::error!("import: the switch didn't run: {e}"),
                }
                if pending_import(data, keys).await != moving::Pending::Switching {
                    return true;
                }
                tracing::warn!(
                    "import: the switch to the imported data began and didn't finish; this server doesn't start on a half-switched data folder. Trying again in {} s",
                    SWITCH_AGAIN.as_secs()
                );
                tokio::select! {
                    _ = tokio::time::sleep(SWITCH_AGAIN) => {}
                    _ = shutdown() => return false,
                }
            }
            moving::Pending::Reading { percent } => {
                tracing::warn!(
                    "import: an import is half done ({percent} %): run `sentry-mate import FILE` again to carry it on, or `sentry-mate import --cancel` to remove it"
                );
                return true;
            }
            moving::Pending::Unreadable => {
                tracing::warn!(
                    "import: a half-done import is here and can't be read: remove it with `sentry-mate import --cancel`"
                );
                return true;
            }
        }
    }
}

/// Resolves once an import has been read and checked by the `import` command (it looks every second).
async fn import_ready(data: PathBuf, keys: PathBuf, refused: Option<String>) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if !data.join(moving::import::STAGING).exists() {
            continue;
        }
        let (d, k) = (data.clone(), keys.clone());
        if let Ok(moving::Pending::Ready { export_id }) =
            tokio::task::spawn_blocking(move || moving::import::pending(&d, &k)).await
        {
            if refused.as_deref() != Some(export_id.as_str()) {
                return;
            }
        }
    }
}

async fn start(
    data: &Path,
    keys: &Path,
    gate: &gate::Gate,
    bind: &str,
    port: u16,
    names: &[String],
) -> Result<Instance, Box<dyn std::error::Error>> {
    // the start's own writes go through the gate too (an export's snapshot from the command line may be under way)
    let pass = ingest::api::enter_gate(gate, GATE_WAIT).await?;
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
    drop(pass);
    let any_token = tokens.has_any().await;
    let store = Store::new(can_dir(data), v.clone());
    let up = Uploads::with_gate(store.clone(), tokens, Judge::new(store), gate.clone(), ingest::api::GATE_WAIT);
    let listener = tokio::net::TcpListener::bind((bind, port)).await?;
    tracing::info!(
        "CAN Bridge uploads on https://{bind}:{port}/api/can/logs (TLS 1.3), certificate SHA-256 {}, vault {}",
        id.fingerprint(),
        v.key_id()
    );
    if !any_token {
        tracing::info!("no upload token yet: make one with `sentry-mate token new --car \"My car\"`");
    }

    // logs whose sidecar never came are judged without it (only cars with logs waiting are looked at); unfinished
    // uploads are dropped after two days (only the cars' `uploading/` folders are looked in). Each through the gate.
    let housekeeping = up.clone();
    let housekeeping = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        let mut n: u64 = 0;
        loop {
            tick.tick().await;
            let Ok(_pass) = ingest::api::enter_gate(&housekeeping.gate, ingest::api::GATE_WAIT).await else {
                continue;
            };
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
    let server = Box::pin(serve_tls(listener, id.tls_config()?, router(up)));
    Ok(Instance { server, housekeeping })
}

fn export_cmd(data: &Path, keys: &Path) -> Result<ExitCode, Box<dyn std::error::Error>> {
    if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        eprintln!("export writes the file to standard output: send it to a file, for example");
        eprintln!("  docker exec <container> /sentry-mate export > sentry-mate.smexport");
        eprintln!(
            "(with docker compose: docker compose exec -T sentry-mate /sentry-mate export > sentry-mate.smexport)"
        );
        return Ok(ExitCode::FAILURE);
    }
    let v = Vault::open(data, keys)?;
    let g = gate::Gate::open(data)?;
    eprintln!("export: taking a snapshot (uploads wait a few seconds) ...");
    let snap = moving::Snapshot::make(data, &v, &g, VERSION, GATE_WAIT)?;
    let h = snap.header();
    eprintln!(
        "export: snapshot taken: {} files, {}; writing {} ({})",
        h.files,
        moving::size(h.bytes),
        snap.file_name(),
        moving::size(snap.size())
    );
    if snap.skips().count > 0 {
        eprintln!("export: {} entries were skipped (not carried): {}", snap.skips().count, snap.skips().summary());
    }
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::with_capacity(1 << 20, out.lock());
    snap.write_to(&v, &mut out)?;
    out.flush()?;
    eprintln!(
        "export: done. It opens on a new server with this server's recovery key (vault {}): put it in the new server's data folder and run `sentry-mate import /data/{}` there.",
        v.key_id(),
        snap.file_name()
    );
    eprintln!("export: it holds all the data, sealed; without the recovery key it opens for no one.");
    Ok(ExitCode::SUCCESS)
}

fn read_recovery_key() -> Result<zeroize::Zeroizing<String>, Box<dyn std::error::Error>> {
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprint!("Recovery key of the server the export came from: ");
    }
    let mut line = zeroize::Zeroizing::new(String::new());
    std::io::stdin().lock().read_line(&mut line)?;
    if line.trim().is_empty() {
        return Err(
            "no recovery key given: run it with `docker exec -it` to type it, or send it on standard input".into()
        );
    }
    Ok(line)
}

async fn import_cmd(
    data: &Path,
    keys: &Path,
    file: Option<&Path>,
    cancel: bool,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    use moving::import::Phase;
    if cancel {
        if moving::import::cancel(data, keys)? {
            println!("import: the half-done import was removed (nothing else was touched)");
        } else {
            println!("import: there is no import here");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let Some(file) = file else {
        eprintln!("give the export file, for example: sentry-mate import /data/sentry-mate-2026-11-17.smexport");
        return Ok(ExitCode::FAILURE);
    };
    let ex = moving::import::open_file(file, VERSION)?;
    std::fs::create_dir_all(data)?;
    let h = &ex.header;
    eprintln!(
        "import: an export made by Sentry Mate {} on {}: {} files, {}",
        h.app,
        moving::export::day_of(h.made_ms),
        h.files,
        moving::size(h.bytes)
    );
    let opts = moving::Options::new(VERSION);
    let mut imp = match moving::import::resume(data, keys, &ex, &opts)? {
        moving::Resume::Same(i) => {
            let i = *i;
            eprintln!(
                "import: carrying on the import of this file ({} % read)",
                i.progress().read * 100 / i.progress().size.max(1)
            );
            i
        }
        moving::Resume::Other(Phase::Ready) => return Err(moving::Error::OtherImport.into()),
        moving::Resume::Other(Phase::Switching) => return Err(moving::Error::Switching.into()),
        moving::Resume::None | moving::Resume::Other(Phase::Reading) => {
            // a server in use is refused before the recovery key is asked for (it would be typed for nothing)
            moving::import::unused(data, keys)?;
            let key = read_recovery_key()?;
            eprintln!("import: opening it with the recovery key (a few seconds) ...");
            moving::import::begin(data, keys, &ex, &key, &opts)?
        }
    };
    let mut last = Instant::now();
    imp.run(|p| {
        if last.elapsed() >= Duration::from_secs(5) {
            eprintln!("import: {} % ({} of {} files)", p.read * 100 / p.size.max(1), p.files_done, p.files);
            last = Instant::now();
        }
        std::ops::ControlFlow::Continue(())
    })?;
    drop(imp);
    eprintln!("import: every file read and checked");
    let done = |s: &moving::Switched| {
        eprintln!("import: done: this server holds the data now ({} files, {}).", s.files, moving::size(s.bytes));
        eprintln!(
            "import: the CAN Bridge's token and certificate SHA-256 stay as they were; if this server has another address, change it in the CAN Bridge app."
        );
        eprintln!(
            "import: the old server's recovery key opens the data here too. You can delete {} now.",
            file.display()
        );
    };
    match gate::ServerLock::try_take(data)? {
        Some(lock) => {
            let s = moving::import::switch(data, keys)?;
            drop(lock);
            done(&s);
            eprintln!("import: start the server now.");
        }
        None => {
            eprintln!("import: the server running here switches to it now (its uploads stop for a moment) ...");
            let until = Instant::now() + Duration::from_secs(180);
            while data.join(moving::import::STAGING).exists() {
                if Instant::now() >= until {
                    eprintln!(
                        "import: the server hasn't switched yet (see its log): restart the app, and it switches as it starts"
                    );
                    return Ok(ExitCode::FAILURE);
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            let v = Vault::open(data, keys)?;
            match moving::import::imported(data, &v) {
                Some(id) if id == ex.header.id => {
                    eprintln!("import: done: the server switched and serves the imported data.")
                }
                _ => {
                    eprintln!("import: the import is gone but this server doesn't hold it: see the server's log");
                    return Ok(ExitCode::FAILURE);
                }
            }
            eprintln!(
                "import: the CAN Bridge's token and certificate SHA-256 stay as they were; if this server has another address, change it in the CAN Bridge app. You can delete {} now.",
                file.display()
            );
        }
    }
    Ok(ExitCode::SUCCESS)
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
