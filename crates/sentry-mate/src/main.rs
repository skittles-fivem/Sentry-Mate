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
//!   move code [--anywhere]        the old server: a one-time move code for the new one
//!   move from ADDRESS CODE        a new server: bring the data here from the old one
//!   move status | cancel | undo   what a move does; stop it; after one, take uploads here again
//!   move finish --recovery-key    a new server, last resort: switch to what was copied
//!   health                        exit 0 when the upload port answers, or a move comes in (for Docker)
//!
//! A new server can also move in with no shell at all: `SENTRY_MATE_MOVE_FROM` and `SENTRY_MATE_MOVE_CODE` in its
//! environment, and it brings the data in as it starts (`docs/plans/move-server.md`).

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
        /// A new server: the old server's address, to bring its data here as this one starts (with the move code).
        #[arg(long, env = "SENTRY_MATE_MOVE_FROM")]
        move_from: Option<String>,
        /// A new server: the move code the old server showed.
        #[arg(long, env = "SENTRY_MATE_MOVE_CODE", hide_env_values = true)]
        move_code: Option<String>,
        /// The old server: `yes` keeps a move code ready and prints it in the log (home network only); `anywhere`
        /// the same for a new server anywhere.
        #[arg(long, env = "SENTRY_MATE_OFFER_MOVE")]
        offer_move: Option<String>,
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
    /// Moving to another server in one step (see `move code` on the old server, `move from` on the new one).
    Move {
        #[command(subcommand)]
        cmd: MoveCmd,
    },
    /// Exit 0 when the upload port accepts connections (or a move comes in, while it copies).
    Health,
}

#[derive(Subcommand)]
enum MoveCmd {
    /// The old server: make a one-time move code (shown once; valid 24 hours).
    Code {
        /// For a new server outside this home network (a rented server, say).
        #[arg(long)]
        anywhere: bool,
        /// End the move under way and make a new code (asks to type yes).
        #[arg(long)]
        replace: bool,
    },
    /// What the move is doing, on either server.
    Status,
    /// The old server: the code stops working and a move not finished ends. The new server: the move stops and
    /// what was copied is removed.
    Cancel,
    /// The old server, after a move: take uploads here again (asks to type yes).
    Undo,
    /// A new server (empty data and keys folders): bring the data here from the old server.
    From {
        /// The old server's address, as the CAN Bridge uses it (192.168.1.20, nas.local:10100, ...).
        address: String,
        /// The move code the old server showed.
        code: String,
    },
    /// A new server, last resort when the old server is gone: switch to what was copied, opened with the old
    /// server's recovery key. The last changes may be missing.
    Finish {
        #[arg(long, required = true)]
        recovery_key: bool,
    },
}

/// The move's two environment lines on a new server, and the offer on an old one.
#[derive(Clone, Default)]
struct MoveEnv {
    from: Option<String>,
    code: Option<String>,
    offer: Option<String>,
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
        Cmd::Serve { bind, names, move_from, move_code, offer_move } => {
            let env = MoveEnv {
                from: move_from.filter(|s| !s.trim().is_empty()),
                code: move_code.filter(|s| !s.trim().is_empty()),
                offer: offer_move.filter(|s| !s.trim().is_empty()),
            };
            serve(&data, &keys, &bind, cli.upload_port, &names, &env).await?
        }
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
            refuse_writes(&data, &v)?;
            let rk = v.new_recovery_key(&data, Kdf::STRONG)?;
            print_recovery(&rk);
            println!("The old recovery key no longer works.");
        }
        Cmd::Token { cmd } => {
            let v = Arc::new(Vault::open(&data, &keys)?);
            if !matches!(cmd, TokenCmd::List) {
                refuse_writes(&data, &v)?;
            }
            // a token's files are part of the data folder: written through its gate, like the server's writes (a
            // server whose data moved away holds its gate and writes nothing: a list reads without it)
            let pass = if matches!(cmd, TokenCmd::List) && moving::old::is_moved(&data) {
                Ok(None)
            } else {
                gate::Gate::open(&data)?.enter(GATE_WAIT).map(Some)
            };
            if !matches!(cmd, TokenCmd::List) {
                // checked again holding the gate: a move may have finished meanwhile
                refuse_writes(&data, &v)?;
            }
            let _pass = pass?;
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
            // the certificate is made if missing, so through the gate; a moved server has one and holds its gate
            let _pass =
                if moving::old::is_moved(&data) { None } else { Some(gate::Gate::open(&data)?.enter(GATE_WAIT)?) };
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
        Cmd::Move { cmd } => return move_cmd(&data, &keys, cli.upload_port, cmd).await,
        Cmd::Health => {
            // a move coming in keeps the upload port closed until it switches: healthy meanwhile
            if data.join(moving::pull::STAGING).exists() {
                return Ok(ExitCode::SUCCESS);
            }
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
    env: &MoveEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(data)?;
    let _lock = take_server_lock(data).await?;
    let gate = gate::Gate::open(data)?;
    // before the vault is opened or made: what a crash left, an import ready to switch to, a move coming in
    moving::import::tidy(data, keys);
    moving::pull::tidy(data, keys);
    let mut refused: Option<String> = None;
    let mut tried: Option<String> = None;
    loop {
        if !carry_on_import(data, keys, &mut refused).await {
            tracing::info!("stopping");
            return Ok(());
        }
        if !carry_on_move(data, keys, env, port, &mut tried).await {
            tracing::info!("stopping");
            return Ok(());
        }
        let Instance { mut server, housekeeping, offer, ticker, confirm } =
            start(data, keys, &gate, bind, port, names, env).await?;
        let mut housekeeping = Some(housekeeping);
        let mut moved = offer.watch();
        let was_moved = offer.is_moved();
        let ready = import_ready(data.to_path_buf(), keys.to_path_buf(), refused.clone());
        tokio::pin!(ready);
        let incoming = move_incoming(data.to_path_buf(), keys.to_path_buf());
        tokio::pin!(incoming);
        let mut hourly = tokio::time::interval(Duration::from_secs(3600));
        hourly.tick().await;
        let mut said_refused: Option<u64> = None;
        let stop = |h: &mut Option<tokio::task::JoinHandle<()>>| {
            if let Some(h) = h.take() {
                h.abort();
            }
            ticker.abort();
            if let Some(c) = &confirm {
                c.abort();
            }
        };
        let next = loop {
            tokio::select! {
                r = &mut server => {
                    stop(&mut housekeeping);
                    r?;
                    return Ok(());
                }
                _ = shutdown() => break Next::Stop,
                _ = &mut ready => break Next::Import,
                _ = &mut incoming => break Next::Restart,
                c = moved.changed() => {
                    if c.is_err() {
                        continue;
                    }
                    let now = *moved.borrow_and_update();
                    if now {
                        // the data moved away: nothing writes here from now on
                        if let Some(h) = housekeeping.take() {
                            h.abort();
                        }
                    } else if was_moved || housekeeping.is_none() {
                        // `move undo`: this server takes uploads again, with its housekeeping
                        break Next::Restart;
                    }
                }
                _ = hourly.tick() => {
                    if let Some(r) = offer.last_refused() {
                        if said_refused != Some(r.at_ms) && ingest::store::now_ms().saturating_sub(r.at_ms) < 3_600_000 {
                            said_refused = Some(r.at_ms);
                            let car = r.car_key.as_deref().map(|k| format!(" (car key {k})")).unwrap_or_default();
                            tracing::warn!(
                                "move: a CAN Bridge{car} still sends here, last on {}: change its server address to the new server",
                                moving::old::when(r.at_ms)
                            );
                        }
                    }
                }
            }
        };
        match next {
            Next::Stop => {
                stop(&mut housekeeping);
                tracing::info!("stopping");
                return Ok(());
            }
            Next::Restart => {
                stop(&mut housekeeping);
                drop(server);
                continue;
            }
            Next::Import => {}
        }
        // an import was read and checked by `import` while this server ran: stop the writes, switch, start again
        tracing::info!("import: the imported data is checked; this server stops its uploads and switches to it");
        let g = gate.clone();
        let pause = match tokio::task::spawn_blocking(move || g.pause(GATE_WAIT)).await? {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("import: the writes didn't stop in time ({e}); trying again");
                stop(&mut housekeeping);
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
        stop(&mut housekeeping);
        let (d, k) = (data.to_path_buf(), keys.to_path_buf());
        let done = tokio::task::spawn_blocking(move || moving::import::switch(&d, &k)).await?;
        report_switch(data, keys, done, &mut refused);
        drop(pause);
        gate.reopen();
    }
}

/// What ended a run of the server.
enum Next {
    Stop,
    Import,
    Restart,
}

/// What one start of the server runs: the upload port (with the move's routes), the housekeeping, the move's timers.
struct Instance {
    server: std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>> + Send>>,
    housekeeping: tokio::task::JoinHandle<()>,
    offer: Arc<OfferAndUploads>,
    ticker: tokio::task::JoinHandle<()>,
    /// Telling the old server this one serves (after a move in).
    confirm: Option<tokio::task::JoinHandle<()>>,
}

/// The move side of the running server, with its uploads (for the line about bridges still sending here).
struct OfferAndUploads {
    offer: Arc<moving::old::Offer>,
    uploads: Arc<Uploads>,
}

impl OfferAndUploads {
    fn watch(&self) -> tokio::sync::watch::Receiver<bool> {
        self.offer.watch()
    }

    fn is_moved(&self) -> bool {
        self.offer.is_moved()
    }

    fn last_refused(&self) -> Option<ingest::RefusedCall> {
        self.uploads.last_refused()
    }
}

/// Commands that change the data refuse on a server whose data moved away, and on one a move is coming into.
fn refuse_writes(data: &Path, v: &Vault) -> Result<(), Box<dyn std::error::Error>> {
    moving::old::refuse_if_moved(data, v)?;
    if data.join(moving::pull::STAGING).exists() {
        return Err("a move is coming in here (see `sentry-mate move status`): wait until it is done, or `sentry-mate move cancel`".into());
    }
    Ok(())
}

/// Resolves once a move was started here by `move from` while this server runs (it looks every second).
async fn move_incoming(data: PathBuf, keys: PathBuf) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if !data.join(moving::pull::STAGING).exists() {
            continue;
        }
        let (d, k) = (data.clone(), keys.clone());
        if let Ok(p) = tokio::task::spawn_blocking(move || moving::pull::pending(&d, &k)).await {
            if pull_wanted(&p) {
                return;
            }
        }
    }
}

/// Whether a staged move is one to carry on now (not one waiting for the owner).
fn pull_wanted(p: &moving::pull::Pending) -> bool {
    match p {
        moving::pull::Pending::Moving { phase, problem, cancelled, .. } => {
            *phase == moving::pull::Phase::Switching || (!cancelled && problem.as_ref().is_none_or(|p| p.retry))
        }
        _ => false,
    }
}

fn code_id(code: &str) -> Option<String> {
    moving::code::MoveCode::parse(code).ok().map(|c| c.keys().id)
}

/// On start, a move into this server: carried on if one is staged, started if the two lines are set and this
/// server is new. Runs until the move is done and switched; a problem only the owner can solve is said in the log,
/// and the server then starts as usual (it has no tokens, so nothing reaches it). `false`: told to stop meanwhile.
async fn carry_on_move(data: &Path, keys: &Path, env: &MoveEnv, port: u16, tried: &mut Option<String>) -> bool {
    let opts = Arc::new(move_options());
    let mut just_begun = false;
    loop {
        let (d, k) = (data.to_path_buf(), keys.to_path_buf());
        let pending = tokio::task::spawn_blocking(move || moving::pull::pending(&d, &k))
            .await
            .unwrap_or(moving::pull::Pending::None);
        let new_code = match (&env.from, &env.code) {
            (Some(f), Some(c)) => Some((f.clone(), c.clone(), code_id(c))),
            _ => None,
        };
        match &pending {
            moving::pull::Pending::Unreadable => {
                tracing::warn!("move: a half-done move here can't be read: remove it with `sentry-mate move cancel`");
                return true;
            }
            moving::pull::Pending::None => {
                let Some((from, code, id)) = new_code else { return true };
                if tried.is_some() && *tried == id {
                    return true;
                }
                if let Some(note) = moved_in_note(data, keys) {
                    if id.as_deref() == Some(note.code_id.as_str()) {
                        tracing::info!(
                            "move: the move from {} is done; you can remove SENTRY_MATE_MOVE_FROM and SENTRY_MATE_MOVE_CODE",
                            note.from
                        );
                        return true;
                    }
                }
                let (d, k) = (data.to_path_buf(), keys.to_path_buf());
                if let Ok(Err(e)) = tokio::task::spawn_blocking(move || moving::import::unused(&d, &k)).await {
                    tracing::warn!(
                        "move: nothing was moved: {e}. Remove SENTRY_MATE_MOVE_FROM and SENTRY_MATE_MOVE_CODE"
                    );
                    return true;
                }
                *tried = id;
                if !begin_move(data, keys, &from, &code, &opts).await {
                    return true;
                }
                just_begun = true;
            }
            moving::pull::Pending::Moving { code_id, from, .. } if !pull_wanted(&pending) => {
                // waiting for the owner: a new code in the lines takes the move on
                match new_code {
                    Some((addr, code, Some(id))) if id != *code_id && *tried != Some(id.clone()) => {
                        *tried = Some(id);
                        if !begin_move(data, keys, &addr, &code, &opts).await {
                            return true;
                        }
                        just_begun = true;
                    }
                    _ => {
                        if let moving::pull::Pending::Moving { problem: Some(p), .. } = &pending {
                            tracing::warn!("move: the move from {from} waits: {}", p.message);
                        } else {
                            tracing::warn!(
                                "move: the move from {from} waits for a new move code (`sentry-mate move status`)"
                            );
                        }
                        return true;
                    }
                }
            }
            moving::pull::Pending::Moving { from, code_id, percent, .. } => {
                if just_begun {
                } else if *percent > 0 {
                    tracing::info!(
                        "move: carrying on the move from {from} (move code {code_id}; {percent} % was copied)"
                    );
                } else {
                    tracing::info!("move: bringing the data from {from} (move code {code_id})");
                }
                just_begun = false;
                let (d, k, o) = (data.to_path_buf(), keys.to_path_buf(), opts.clone());
                let run = tokio::task::spawn_blocking(move || moving::pull::run(&d, &k, &o));
                tokio::pin!(run);
                let r = tokio::select! {
                    r = &mut run => r,
                    _ = shutdown() => {
                        opts.stop.store(true, std::sync::atomic::Ordering::Relaxed);
                        let _ = run.await;
                        return false;
                    }
                };
                match r {
                    Ok(Ok(done)) => {
                        report_moved_in(&done, port);
                        return true;
                    }
                    Ok(Err(p)) if p.code == "cancelled_here" => {
                        // `move cancel` removes the copy; what a stop part way left is removed here
                        let (d, k) = (data.to_path_buf(), keys.to_path_buf());
                        let _ = tokio::task::spawn_blocking(move || moving::pull::tidy(&d, &k)).await;
                        tracing::info!("move: the move was cancelled here; this server starts as a new one");
                        return true;
                    }
                    r => {
                        let (msg, retry) = match r {
                            Ok(Err(p)) => (p.message, p.retry),
                            Err(e) => (format!("the move stopped: {e}"), true),
                            Ok(Ok(_)) => (String::new(), false),
                        };
                        tracing::warn!("move: {msg}");
                        // a switch that began is finished before anything else: never a start on a half-switched
                        // data folder
                        let (d, k) = (data.to_path_buf(), keys.to_path_buf());
                        let switching = matches!(
                            tokio::task::spawn_blocking(move || moving::pull::pending(&d, &k)).await,
                            Ok(moving::pull::Pending::Moving { phase: moving::pull::Phase::Switching, .. })
                        );
                        if !(retry || switching) {
                            return true;
                        }
                        tracing::warn!("move: trying again in {} s", SWITCH_AGAIN.as_secs());
                        tokio::select! {
                            _ = tokio::time::sleep(SWITCH_AGAIN) => continue,
                            _ = shutdown() => return false,
                        }
                    }
                }
            }
        }
    }
}

/// The claim of a move the two lines (or `move from`) asked for. `false`: it didn't happen (said in the log).
async fn begin_move(data: &Path, keys: &Path, from: &str, code: &str, opts: &Arc<moving::pull::Options>) -> bool {
    let id = code_id(code).map(|i| format!(" (move code {i})")).unwrap_or_default();
    tracing::info!("move: bringing the data from {from}{id}");
    let (d, k, f, c, o) = (data.to_path_buf(), keys.to_path_buf(), from.to_string(), code.to_string(), opts.clone());
    match tokio::task::spawn_blocking(move || moving::pull::begin(&d, &k, &f, &c, &o)).await {
        Ok(Ok(())) => true,
        Ok(Err(p)) if p.retry => {
            // staged: carried on (and tried again) by the move itself
            tracing::warn!("move: {}", p.message);
            true
        }
        Ok(Err(p)) => {
            tracing::warn!("move: {}", p.message);
            tracing::warn!(
                "move: nothing was moved; this server starts as a new one meanwhile (it has no data and no tokens). Fix what the line above says (for a code: a new one in SENTRY_MATE_MOVE_CODE) and restart it."
            );
            false
        }
        Err(e) => {
            tracing::error!("move: {e}");
            false
        }
    }
}

fn move_options() -> moving::pull::Options {
    let mut o = moving::pull::Options::new(VERSION);
    o.min_free = ingest::store::MIN_FREE;
    o
}

fn moved_in_note(data: &Path, keys: &Path) -> Option<moving::pull::MovedIn> {
    let v = Vault::open(data, keys).ok()?;
    moving::pull::moved_in(data, &v)
}

fn report_moved_in(done: &moving::pull::Done, port: u16) {
    let fp: String = done.cert_sha256.chars().take(8).collect();
    tracing::info!(
        "move: finished: this server holds the data now ({} files, {}) and serves it. The old server stopped taking uploads on {}.",
        done.files,
        moving::size(done.bytes),
        moving::old::when(done.moved_ms)
    );
    tracing::info!(
        "move: in the CAN Bridge app, set the server address to https://<this server>:{port}/api/can/logs; the token and the certificate SHA-256 stay as they are ({fp}…)"
    );
    tracing::info!(
        "move: the old server's recovery key opens the data here too. You can remove SENTRY_MATE_MOVE_FROM and SENTRY_MATE_MOVE_CODE now."
    );
    if done.same_machine {
        tracing::warn!(
            "move: the old server ran on this machine. Stop the old app now, then give this app port 10100 again (10100:10100) and redeploy: the CAN Bridge then needs no change."
        );
    }
    if done.forced {
        tracing::warn!(
            "move: switched with the recovery key: the last changes may be missing, and the old server doesn't know it moved. Don't start it again with the CAN Bridge pointed at it."
        );
    }
    if done.replaced_first_start {
        tracing::info!(
            "move: the vault this server made at its first start was replaced; its {} opened nothing and was removed",
            moving::import::RECOVERY_FILE
        );
    }
}

/// After a move in: tells the old server this one serves, trying again every 30 s until it is told.
fn spawn_confirm(data: &Path, v: Arc<Vault>) -> Option<tokio::task::JoinHandle<()>> {
    let note = moving::pull::moved_in(data, &v)?;
    if note.confirmed || note.forced {
        return None;
    }
    let data = data.to_path_buf();
    Some(tokio::spawn(async move {
        let mut said = false;
        loop {
            let (d, v2) = (data.clone(), v.clone());
            match tokio::task::spawn_blocking(move || moving::pull::confirm(&d, &v2, Duration::from_secs(30))).await {
                Ok(Ok(_)) => {
                    tracing::info!(
                        "move: the old server knows this one serves now: its log says done, and it can be stopped"
                    );
                    return;
                }
                Ok(Err(p)) if !said => {
                    tracing::warn!(
                        "move: couldn't tell the old server this one serves yet ({}); trying again every 30 s",
                        p.message
                    );
                    said = true;
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    }))
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
    env: &MoveEnv,
) -> Result<Instance, Box<dyn std::error::Error>> {
    let moved = moving::old::is_moved(data);
    // the start's own writes go through the gate too (an export's snapshot from the command line may be under way);
    // a server whose data moved away writes nothing
    let pass = if moved { None } else { Some(ingest::api::enter_gate(gate, GATE_WAIT).await?) };
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
    if !moved {
        let sealed = migrate::seal_plain_files(data, &v)?;
        if sealed > 0 {
            tracing::info!("sealed {sealed} file(s) that were stored in the clear by an older version");
        }
    }
    let id = Identity::load_or_create(&tls_dir(data), names, &v)?;
    let tokens = Tokens::open(tokens_file(data), v.clone()).await?;
    drop(pass);
    let any_token = tokens.has_any().await;
    let store = Store::new(can_dir(data), v.clone());
    let up = Uploads::with_gate(store.clone(), tokens, Judge::new(store), gate.clone(), ingest::api::GATE_WAIT);
    // the move's side of this server: a moved one closes its gate and holds it from here on
    let (d, v2, g, fp) = (data.to_path_buf(), v.clone(), gate.clone(), id.fingerprint());
    let offer = tokio::task::spawn_blocking(move || moving::old::Offer::open(&d, v2, g, VERSION, &fp)).await??;
    if let Some(o) = env.offer.as_deref() {
        offer_from_env(data, &v, o);
    }
    let listener = tokio::net::TcpListener::bind((bind, port)).await?;
    tracing::info!(
        "CAN Bridge uploads on https://{bind}:{port}/api/can/logs (TLS 1.3), certificate SHA-256 {}, vault {}",
        id.fingerprint(),
        v.key_id()
    );
    if !any_token && !offer.is_moved() {
        tracing::info!("no upload token yet: make one with `sentry-mate token new --car \"My car\"`");
    }
    let confirm = spawn_confirm(data, v.clone());

    // logs whose sidecar never came are judged without it (only cars with logs waiting are looked at); unfinished
    // uploads are dropped after two days (only the cars' `uploading/` folders are looked in). Each through the gate.
    let housekeeping = up.clone();
    let moved_now = offer.is_moved();
    let housekeeping = tokio::spawn(async move {
        if moved_now {
            return;
        }
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
    let ticker = offer.spawn_ticker();
    let app = router(up.clone()).merge(moving::old::router(offer.clone()));
    let server = Box::pin(serve_tls(listener, id.tls_config()?, app));
    let offer = Arc::new(OfferAndUploads { offer, uploads: up });
    Ok(Instance { server, housekeeping, offer, ticker, confirm })
}

/// `SENTRY_MATE_OFFER_MOVE`: keeps a move code ready and prints it in the log (with no shell, the log is the only
/// screen the owner has).
fn offer_from_env(data: &Path, v: &Vault, value: &str) {
    let anywhere = match value.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" | "1" | "home" => false,
        "anywhere" => true,
        "no" | "false" | "0" => return,
        other => {
            tracing::warn!(
                "move: SENTRY_MATE_OFFER_MOVE={other} isn't understood: use yes (home network only) or anywhere"
            );
            return;
        }
    };
    use moving::old::EnvOffer;
    match moving::old::offer_from_env(data, v, anywhere) {
        Ok(EnvOffer::Made(m) | EnvOffer::Waiting(m)) => {
            let reach = if m.anywhere { "from anywhere" } else { "from this home network only" };
            tracing::warn!(
                "move: a move code is ready (id {}), valid until {}; it works once, {reach}. Don't share it:",
                m.id,
                moving::old::when(m.expires_ms)
            );
            tracing::warn!("move:   {}", m.code.as_str());
            tracing::warn!(
                "move: on the new server (empty data and keys folders) set SENTRY_MATE_MOVE_FROM=<this server's address> and SENTRY_MATE_MOVE_CODE=<the code above>, then start it"
            );
        }
        Ok(EnvOffer::WaitingUnshown { id, expires_ms }) => tracing::info!(
            "move: a move code made with `move code` (id {id}) waits until {}; SENTRY_MATE_OFFER_MOVE makes no other",
            moving::old::when(expires_ms)
        ),
        Ok(EnvOffer::UnderWay { from, at_ms }) => tracing::info!(
            "move: a move to {from} is under way (since {}); SENTRY_MATE_OFFER_MOVE makes no new code",
            moving::old::when(at_ms)
        ),
        Ok(EnvOffer::Moved) => {}
        Err(e) => tracing::warn!("move: no move code made: {e}"),
    }
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
    // a server whose data moved away writes nothing (and holds its gate): its snapshot needs no pause
    let g = if moving::old::is_moved(data) { gate::Gate::memory() } else { gate::Gate::open(data)? };
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
    if data.join(moving::pull::STAGING).exists() {
        eprintln!(
            "import: a move is coming in here (see `sentry-mate move status`): finish it, or `sentry-mate move cancel`, first"
        );
        return Ok(ExitCode::FAILURE);
    }
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

fn percent_of(done: u64, total: u64) -> u64 {
    done.saturating_mul(100).checked_div(total).unwrap_or(0).min(100)
}

/// Asks the owner to type `yes` (a terminal, or a line on standard input).
fn confirm_yes(what: &str) -> Result<bool, Box<dyn std::error::Error>> {
    eprintln!("{what}");
    eprint!("Type yes to go on: ");
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().eq_ignore_ascii_case("yes"))
}

fn show_status(data: &Path, keys: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use moving::pull::Phase;
    if let Some(s) = moving::pull::status(data, keys)? {
        let p = &s.progress;
        let pct = percent_of(p.bytes_done, p.bytes_total);
        let phase = match s.phase {
            Phase::Claiming => "proving the move code to the old server",
            Phase::Copying if p.round <= 1 => "copying",
            Phase::Copying => "catching up",
            Phase::Switching => "switching to the copied data",
        };
        println!(
            "A move comes in here from {} (move code {}), started {}: {phase}.",
            s.from,
            s.code_id,
            moving::old::when(s.started_ms)
        );
        if let Some(app) = &s.old_app {
            println!(
                "  The old server runs Sentry Mate {app}; its certificate SHA-256 is {}.",
                s.cert_sha256.as_deref().unwrap_or("?")
            );
        }
        if p.bytes_total > 0 {
            println!(
                "  Round {}: {pct} % ({} of {}, {} of {} files).",
                p.round.max(1),
                moving::size(p.bytes_done),
                moving::size(p.bytes_total),
                p.files_done,
                p.files_total
            );
        }
        if s.cancelled {
            println!(
                "  It was cancelled on the old server: with a new move code from that server it carries on from here."
            );
        }
        if let Some(pr) = &s.problem {
            let when = if pr.retry { "it tries again by itself" } else { "it waits for you" };
            println!("  Last problem ({when}): {}", pr.message);
        }
        if s.skipped_count > 0 {
            let names: Vec<String> = s.skipped.iter().map(|k| format!("{} ({})", k.path, k.why)).collect();
            println!("  Skipped on the old server (not carried): {}", names.join(", "));
        }
        return Ok(());
    }
    let Ok(v) = Vault::open(data, keys) else {
        println!("No move here (and no vault yet).");
        return Ok(());
    };
    let moved_in = moving::pull::moved_in(data, &v);
    if let Some(n) = &moved_in {
        let told = if n.confirmed { "the old server was told" } else { "the old server wasn't told yet" };
        println!(
            "This server's data came from {} by a move (move code {}) on {}; {told}.",
            n.from,
            n.code_id,
            moving::old::when(n.moved_ms)
        );
    }
    let st = moving::old::status(data, &v)?;
    if let Some(m) = &st.moved {
        match m.switched_ms {
            Some(t) => println!(
                "This server's data moved to {} on {}; it serves the data since {}. This server takes no uploads (`move undo` to take them again).",
                m.to,
                moving::old::when(m.at_ms),
                moving::old::when(t)
            ),
            None => println!(
                "This server's data moved to {} on {}; it is switching: keep this server running until it says done.",
                m.to,
                moving::old::when(m.at_ms)
            ),
        }
    }
    match &st.code {
        Some(c) => match &c.claimed {
            Some(cl) => {
                let pct = percent_of(cl.sent_bytes, cl.bytes).min(99);
                println!(
                    "A move to {} (Sentry Mate {}) started {}, move code {}: about {pct} % copied; last heard from {}.",
                    cl.from,
                    cl.app,
                    moving::old::when(cl.at_ms),
                    c.id,
                    moving::old::when(cl.last_seen_ms)
                );
            }
            None => {
                let reach = if c.anywhere { "from anywhere" } else { "from this home network only" };
                println!(
                    "A move code (id {}) waits until {}, {reach}; {} wrong tries so far.",
                    c.id,
                    moving::old::when(c.expires_ms),
                    c.wrong
                );
            }
        },
        None if st.moved.is_none() && moved_in.is_none() => println!("No move code waiting and no move under way."),
        None => {}
    }
    if let Some((at, why)) = &st.last_cancelled {
        println!("The last move that ended without finishing: {why} ({}).", moving::old::when(*at));
    }
    Ok(())
}

async fn move_cmd(data: &Path, keys: &Path, port: u16, cmd: MoveCmd) -> Result<ExitCode, Box<dyn std::error::Error>> {
    match cmd {
        MoveCmd::Code { anywhere, replace } => {
            let v = Vault::open(data, keys)?;
            let made = match moving::old::make_code(data, &v, anywhere, false) {
                Err(moving::Error::MoveUnderWay(msg)) if replace => {
                    if !confirm_yes(&format!(
                        "{msg}. A new code ends that move: the new server keeps what it copied but must start over with the new code."
                    ))? {
                        eprintln!("nothing was changed");
                        return Ok(ExitCode::FAILURE);
                    }
                    moving::old::make_code(data, &v, anywhere, true)?
                }
                Err(moving::Error::MoveUnderWay(msg)) => {
                    eprintln!("{msg}.");
                    eprintln!(
                        "To end it and make a new code: `sentry-mate move code --replace`; to end it only: `sentry-mate move cancel`."
                    );
                    return Ok(ExitCode::FAILURE);
                }
                r => r?,
            };
            let reach = if made.anywhere { "from anywhere" } else { "from this home network only" };
            println!(
                "Move code (shown once; valid until {}; works once; {reach}):\n",
                moving::old::when(made.expires_ms)
            );
            println!("  {}\n", made.code.as_str());
            println!("On the new server (empty data and keys folders), set:");
            println!("  SENTRY_MATE_MOVE_FROM=<this server's address: the one the CAN Bridge uses>");
            println!("  SENTRY_MATE_MOVE_CODE=<the code above>");
            println!("or run there: sentry-mate move from <this server's address> <the code above>");
            println!("\nKeep this server running until its log says `move: done`. Don't share the code.");
        }
        MoveCmd::Status => show_status(data, keys)?,
        MoveCmd::Cancel => {
            if data.join(moving::pull::STAGING).exists() {
                match moving::pull::cancel(data, keys) {
                    Ok(_) => println!(
                        "The move coming in here was stopped and what was copied removed; the old server was told (it takes uploads as before)."
                    ),
                    Err(moving::Error::Finished(m)) => {
                        eprintln!("{m}: nothing to cancel.");
                        return Ok(ExitCode::FAILURE);
                    }
                    Err(e) => return Err(e.into()),
                }
                return Ok(ExitCode::SUCCESS);
            }
            let v = Vault::open(data, keys)?;
            match moving::old::cancel(data, &v) {
                Ok(moving::old::Cancelled::Nothing) => {
                    println!("No move code waiting and no move under way: nothing to cancel.")
                }
                Ok(moving::old::Cancelled::Revoked { id }) => println!("The move code {id} no longer works."),
                Ok(moving::old::Cancelled::Ended { to }) => println!(
                    "The move to {to} ended: the code no longer works, and this server takes uploads as before. What {to} copied stays there until it is cancelled there too."
                ),
                Err(moving::Error::Finished(m)) => {
                    eprintln!("{m}");
                    return Ok(ExitCode::FAILURE);
                }
                Err(e) => return Err(e.into()),
            }
        }
        MoveCmd::Undo => {
            let v = Vault::open(data, keys)?;
            let Some(m) = moving::old::moved_info(data, &v)? else {
                println!("This server's data didn't move: nothing to undo.");
                return Ok(ExitCode::SUCCESS);
            };
            if !confirm_yes(&format!(
                "This server's data moved to {} on {}. Undo makes this server take uploads again. If the new server also takes uploads, the two drift apart: stop it first.",
                m.to,
                moving::old::when(m.at_ms)
            ))? {
                eprintln!("nothing was changed");
                return Ok(ExitCode::FAILURE);
            }
            moving::old::undo(data, &v)?;
            tracing::warn!("move: undone by `move undo`: this server takes uploads again");
            println!("Undone: this server takes uploads again (a running server within seconds).");
        }
        MoveCmd::From { address, code } => return move_from_cmd(data, keys, port, &address, &code).await,
        MoveCmd::Finish { .. } => {
            if moving::pull::status(data, keys)?.is_none() {
                eprintln!("There is no move coming in here.");
                return Ok(ExitCode::FAILURE);
            }
            if !confirm_yes(
                "This switches to what was copied from the old server, opened with its recovery key. The last changes may be missing, the copy may not be of one moment, and the old server doesn't know it moved: don't start it again with the CAN Bridge pointed at it.",
            )? {
                eprintln!("nothing was changed");
                return Ok(ExitCode::FAILURE);
            }
            let key = read_recovery_key()?;
            eprintln!("move: opening the copy with the recovery key (a few seconds) ...");
            moving::pull::force_finish(data, keys, &key)?;
            return finish_here_or_wait(data, keys, port).await;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// With no server running, this command carries the move to its end; with one, the server does and this waits.
async fn finish_here_or_wait(data: &Path, keys: &Path, port: u16) -> Result<ExitCode, Box<dyn std::error::Error>> {
    match gate::ServerLock::try_take(data)? {
        Some(lock) => {
            let opts = Arc::new(move_options());
            let (d, k, o) = (data.to_path_buf(), keys.to_path_buf(), opts.clone());
            let run = tokio::task::spawn_blocking(move || moving::pull::run(&d, &k, &o));
            tokio::pin!(run);
            let r = tokio::select! {
                r = &mut run => r?,
                _ = shutdown() => {
                    opts.stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    let _ = run.await;
                    eprintln!("move: stopped; it carries on from here when the server (or this command) runs again");
                    return Ok(ExitCode::FAILURE);
                }
            };
            drop(lock);
            match r {
                Ok(done) => {
                    report_moved_in(&done, port);
                    eprintln!("move: start the server now: it tells the old server it serves.");
                    Ok(ExitCode::SUCCESS)
                }
                Err(p) => {
                    eprintln!("move: {}", p.message);
                    Ok(ExitCode::FAILURE)
                }
            }
        }
        None => {
            eprintln!(
                "move: the server running here carries the move on; its log shows the progress. Closing this command doesn't stop it."
            );
            let mut last = String::new();
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                match moving::pull::status(data, keys) {
                    Ok(Some(s)) => {
                        let p = &s.progress;
                        let line = match &s.problem {
                            Some(pr) if !pr.retry => {
                                eprintln!("move: {}", pr.message);
                                return Ok(ExitCode::FAILURE);
                            }
                            Some(pr) => format!("move: {}", pr.message),
                            None if p.bytes_total > 0 => format!(
                                "move: round {}: {} % ({} of {})",
                                p.round.max(1),
                                percent_of(p.bytes_done, p.bytes_total),
                                moving::size(p.bytes_done),
                                moving::size(p.bytes_total)
                            ),
                            None => "move: starting".into(),
                        };
                        if line != last {
                            eprintln!("{line}");
                            last = line;
                        }
                    }
                    Ok(None) => match moved_in_note(data, keys) {
                        Some(n) => {
                            eprintln!("move: done: this server holds the data from {} and serves it.", n.from);
                            eprintln!(
                                "move: if this server has another address, change it in the CAN Bridge app; the token and the certificate SHA-256 stay as they are."
                            );
                            return Ok(ExitCode::SUCCESS);
                        }
                        None => {
                            eprintln!("move: the move is gone but didn't arrive here: see the server's log");
                            return Ok(ExitCode::FAILURE);
                        }
                    },
                    Err(_) => {}
                }
            }
        }
    }
}

async fn move_from_cmd(
    data: &Path,
    keys: &Path,
    port: u16,
    address: &str,
    code: &str,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(data)?;
    let opts = move_options();
    eprintln!("move: proving the move code to {address} ...");
    let (d, k, a, c) = (data.to_path_buf(), keys.to_path_buf(), address.to_string(), code.to_string());
    let r = tokio::task::spawn_blocking(move || moving::pull::begin(&d, &k, &a, &c, &opts)).await?;
    match r {
        Ok(()) => eprintln!("move: the old server took the code; copying"),
        Err(p) if p.retry => eprintln!("move: {} (it tries again by itself)", p.message),
        Err(p) => {
            eprintln!("move: {}", p.message);
            return Ok(ExitCode::FAILURE);
        }
    }
    finish_here_or_wait(data, keys, port).await
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
