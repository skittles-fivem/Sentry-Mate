//! `sentry-mate`: receives the Screenmate CAN Bridge's logs and (later) turns them into the car's history.
//!
//! Everything it stores is sealed with the vault: the data key opens with the key file in the keys folder (a volume
//! of its own), or with the recovery key shown once when the vault is made.
//!
//! Commands:
//!   serve                         run the upload server (TLS 1.3) until stopped (makes the vault on first start)
//!   init                          make the vault now and print the recovery key
//!   restore                       new machine or lost key file: type the recovery key, a new key file is written
//!   recovery-key new              replace the recovery key (the old one stops working; the new one also goes in RECOVERY-KEY.txt)
//!   token new --car NAME          make an upload token for a car (shown once)
//!   token list | token revoke ID
//!   fingerprint                   the certificate SHA-256 the app pins
//!   log export NAME               a stored log or sidecar, unsealed, to standard output
//!   log decode NAME               a stored log's signals as CSV: only a log its check allows (--any: any), with the
//!                                 signal set from the car's VIN (--model 3y or sx-legacy: that one)
//!   log candump FILE              a log file of either format as candump text
//!   log check FILE [SIDECAR]      what its signature says
//!   chain KEY                     the chain report of a car key's logs
//!   export > FILE                 the whole server as one export file (opens elsewhere with the recovery key)
//!   import FILE [--cancel]        a new server: bring an export file in (asks for the recovery key)
//!   move code [--anywhere]        the old server: a one-time move code for the new one
//!   move from ADDRESS CODE        a new server: bring the data here from the old one
//!   move status | cancel | undo   what a move does; stop it; after one, take uploads here again
//!   move finish --recovery-key    a new server, last resort: switch to what was copied
//!   keys KEY                      the signing keys a car key's logs name, and which one is trusted
//!   keys trust KEY SIGNING-KEY    trust another signing key for a car key (asks first; --yes doesn't)
//!   health                        exit 0 when the upload port answers, or a move comes in (for Docker)
//!   gps-layout --car C [--days N] [--wait MIN]
//!                                 where the Screenmate's GPS frame keeps latitude and longitude, from the stored
//!                                 logs and the other inputs' positions (only reads; prints no positions; with the
//!                                 server running, the server runs it)
//!   log-checks --car C            every log of a car as the CAN Bridge app is told it now, counted per check_reason,
//!                                 and when the failed ones were (only reads; prints no names or keys)
//!
//! A new server can also move in with no shell at all: `SENTRY_MATE_MOVE_FROM` and `SENTRY_MATE_MOVE_CODE` in its
//! environment, and it brings the data in as it starts (`docs/plans/move-server.md`).

mod api;
mod decode;
#[cfg(test)]
mod e2e;
mod folders;
mod gps_layout;
#[cfg(any(test, feature = "dev-seed"))]
mod history;
mod keys;
mod log_checks;
mod migrate;
#[cfg(test)]
mod perf;
#[cfg(test)]
mod perf_engine;
#[cfg(any(test, feature = "dev-seed"))]
mod seed;
mod site;
#[cfg(test)]
mod testkit;
#[cfg(test)]
mod window_batch;

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

/// This version as a move or an export says it (an import refuses an export from a newer one): the crate's alone,
/// without the image build's part `--version` shows.
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// The web UI's port by default (beside the upload port, 10100; Sentry Six Manager's are 9000 and 10000).
const WEB_PORT: u16 = web::PORT;
/// How long a command waits for a pause of the writes (an export's snapshot) to end, or a snapshot for the writes
/// under way.
const GATE_WAIT: Duration = Duration::from_secs(60);

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
    /// The web UI's port (HTTPS).
    #[arg(long, env = "SENTRY_MATE_WEB_PORT", default_value_t = WEB_PORT, global = true)]
    web_port: u16,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server until stopped: the upload port, the history and the web UI.
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
    /// The chain report of a car key's signed logs (JSON), with the signing key it covers.
    Chain {
        /// The car key (the last column of `token list`).
        key: String,
    },
    /// The signing keys a car key's logs name, and which one is trusted; `keys trust` trusts another.
    #[command(args_conflicts_with_subcommands = true)]
    Keys {
        #[command(subcommand)]
        cmd: Option<KeysCmd>,
        /// The car key (the last column of `token list`).
        key: Option<String>,
    },
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
    /// Exit 0 when the upload port and the web port accept connections (or a move comes in, while it copies).
    Health,
    /// Where the Screenmate's own GPS frame (0x04F on its internal bus) keeps latitude and longitude, worked out from
    /// a car's stored logs and the positions its other inputs (Tesla API, TeslaFi, TeslaMate) reported at the same
    /// moments. Prints layouts and errors in metres only (no positions or times), so it can be passed on. Runs beside
    /// the server and only reads: `docker exec <container> /sentry-mate gps-layout --car c1`. With the server
    /// running, the server runs the fit (as Data health's Find GPS layout does) and this shows its progress.
    GpsLayout {
        /// The car, as its pages' address names it (`c1`).
        #[arg(long)]
        car: String,
        /// How many days back from now to read.
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=366))]
        days: u32,
        /// How many minutes to wait for a busy server at most.
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=1440))]
        wait: u64,
    },
    /// Every log of a car's CAN Bridge keys as the app is told it now (`verified`, `pending`, `failed` and its
    /// `check_reason`), counted, and the runs of failed logs by their start times (UTC). Prints no log name, car key or
    /// signing key, so it can be passed on. Runs beside the server and only reads:
    /// `docker exec <container> /sentry-mate log-checks --car c1`.
    LogChecks {
        /// The car, as its pages' address names it (`c1`).
        #[arg(long)]
        car: String,
    },
    /// Development only (not in the help, and only in a build with the `dev-seed` feature): made-up cars for trying
    /// the server without anyone's car.
    #[cfg(feature = "dev-seed")]
    #[command(hide = true)]
    Dev {
        #[command(subcommand)]
        cmd: DevCmd,
    },
}

#[cfg(feature = "dev-seed")]
#[derive(Subcommand)]
enum DevCmd {
    /// Upload made-up cars' logs (signed, chained, sealed and judged like real uploads) into the data folder.
    Seed {
        #[arg(long, default_value_t = 3)]
        days: u32,
        #[arg(long, default_value_t = 1)]
        cars: u32,
        /// The same seed makes the same cars and logs.
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// At most a frame a second per message (smaller, quicker).
        #[arg(long)]
        light: bool,
        /// No trouble: no lost log, dropped frames, recording pause or app update.
        #[arg(long)]
        clean: bool,
        /// Candump text logs (/1) instead of columns (/2).
        #[arg(long)]
        candump: bool,
        /// Put made-up cars in a data folder that has real cars too (they show in its fleet, and anyone can forge
        /// their logs: their signing keys come from a public seed).
        #[arg(long, env = "SENTRY_MATE_DEV")]
        with_real_cars: bool,
        /// The first made-up day (YYYY-MM-DD, UTC), for example a few days ago so the pages' recent periods show
        /// them; synth's own first day (2026-03-02) when left out.
        #[arg(long, value_parser = seed::parse_day)]
        start: Option<u64>,
        /// Minutes the car stays awake after it was last used, logging all the while (Sentry Mode); synth's own 10
        /// when left out. 360 gives about 22,000 logs in 30 days.
        #[arg(long)]
        awake_min: Option<u32>,
    },
    /// A long made-up history for the first car (or a new one): a TeslaMate import of about a year (about 3 million
    /// positions), a TeslaFi import, and TeslaFi's API, local TeslaMate and the Tesla API as live inputs over its
    /// last weeks. Run after `dev seed` (its car gets the history). For timing the pages
    /// (docs/plans/performance.md).
    History {
        /// The day the history ends (YYYY-MM-DD, UTC, not included).
        #[arg(long, value_parser = seed::parse_day)]
        end: u64,
        #[arg(long, default_value_t = 330)]
        days: u32,
        #[arg(long, default_value_t = 1300)]
        drives: u32,
        #[arg(long, default_value_t = 500)]
        charges: u32,
        /// Seconds between TeslaMate's positions while driving.
        #[arg(long, default_value_t = 1)]
        drive_step: u64,
        #[arg(long, default_value_t = 60)]
        teslafi_days: u32,
        #[arg(long, default_value_t = 30)]
        api_days: u32,
        /// Days of the Tesla API as a live input, before the end.
        #[arg(long, default_value_t = 30)]
        tesla_days: u32,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// What the data folder takes on disk by category, as a table (docs/plans/storage.md). With `--compact`, packs
    /// everything that can be packed first (what the server does when idle) and prints what that saved.
    Storage {
        #[arg(long)]
        compact: bool,
    },
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
        /// The old server's address, as the CAN Bridge uses it (192.0.2.20, nas.local:10100, ...).
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
    /// Make a new recovery key; the old one stops working. It is printed and written to RECOVERY-KEY.txt.
    New,
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Make a token for a car; it is shown once.
    New {
        #[arg(long)]
        car: String,
        /// A car key it may take over although another token used it (a replacement for that car's lost or
        /// revoked token; the last column of `token list`). Without it, only a car key no one uses yet.
        // its own id: `keys` is the global --keys folder, and a clash panics when the command runs
        #[arg(long = "key", id = "car_keys")]
        keys: Vec<String>,
    },
    /// One line per token: id, car name, in use or revoked, the car key it is tied to.
    List,
    /// Revoke a token; it stops working at its next request.
    Revoke {
        /// The token's id (the first column of `token list`).
        id: String,
    },
}

#[derive(Subcommand)]
enum KeysCmd {
    /// Trust a signing key for a car key: the logs it signed are used from then on. Asks first.
    Trust {
        /// The car key (the last column of `token list`).
        key: String,
        /// The signing key, as `keys` lists it (the CAN Bridge app's "Log signing key").
        signing_key: String,
        /// Don't ask.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum LogCmd {
    /// A stored log or sidecar (by its name), unsealed, to standard output.
    Export { name: String },
    /// A stored log's decoded signals as CSV (each value when it changes): time, signal, value, unit, value name.
    /// Only a log whose check allows its data (verified or unsigned), unless --any.
    Decode {
        name: String,
        /// Decode it whatever its check says (failed, could not sign, waiting, not checked yet).
        #[arg(long)]
        any: bool,
        /// The signal set: auto (from the car's VIN, else Model 3/Y), 3y or sx-legacy.
        #[arg(long, value_enum, default_value_t = decode::Model::Auto)]
        model: decode::Model,
    },
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

/// What `keys` without a car key says.
const KEYS_USAGE: &str = "sentry-mate: which car key? The car keys are the last column of `token list`.\n\nUsage: \
                          sentry-mate keys <KEY>\n       sentry-mate keys trust <KEY> <SIGNING-KEY>";

/// The version, and the image build's number, commit and date when it was given them (build.rs).
const VERSION: &str = env!("SENTRY_MATE_VERSION");

/// This build, for the release check: read here, in the last crate built, so a new build number, commit or date
/// builds again only this crate (and links), not web and the crates below it.
pub(crate) fn this_build() -> release::Build {
    release::Build::from_parts(
        env!("CARGO_PKG_VERSION"),
        option_env!("SENTRY_MATE_BUILD"),
        option_env!("SENTRY_MATE_COMMIT"),
        option_env!("SENTRY_MATE_DATE"),
    )
}

#[tokio::main]
async fn main() -> ExitCode {
    vault::harden_process();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();
    let cli = Cli::parse();
    if let Cmd::Keys { cmd: None, key: None } = cli.cmd {
        // (not clap's own message: its usage line shows --data and --keys as if they had to be given)
        eprintln!("{KEYS_USAGE}");
        return ExitCode::from(2);
    }
    let (data, keys) = (cli.data.clone(), cli.keys.clone());
    let mounts = folders::mounts_required(std::env::var_os("SENTRY_MATE_REQUIRE_MOUNTS").as_deref());
    match run(cli, mounts).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sentry-mate: {}", said(&*e));
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

/// An error as the owner reads it: an upload API error without its HTTP status and code.
fn said(e: &(dyn std::error::Error + 'static)) -> String {
    match e.downcast_ref::<ingest::ApiError>() {
        Some(a) => a.message.clone(),
        None => e.to_string(),
    }
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
        // a new recovery key waits in the keys folder's RECOVERY-KEY.txt, like the first one
        Cmd::RecoveryKey { .. } => Some((true, true)),
        Cmd::Token { .. } | Cmd::Fingerprint => Some((true, false)),
        Cmd::Keys { cmd: Some(KeysCmd::Trust { .. }), .. } => Some((true, false)),
        // an export's snapshot is made in the data folder; a move code, an undo and the old server's cancel write
        // there too
        Cmd::Export | Cmd::Move { cmd: MoveCmd::Code { .. } | MoveCmd::Undo } => Some((true, false)),
        // a new server taking data in writes its own key file; a cancel removes the staged one
        Cmd::Import { .. } | Cmd::Move { cmd: MoveCmd::From { .. } | MoveCmd::Finish { .. } | MoveCmd::Cancel } => {
            Some((true, true))
        }
        Cmd::Move { cmd: MoveCmd::Status } => Some((false, false)),
        // (the GPS layout fit only reads: the running server's job, or the folder read without its write gate)
        Cmd::Log { cmd: LogCmd::Export { .. } | LogCmd::Decode { .. } }
        | Cmd::Chain { .. }
        | Cmd::Keys { .. }
        | Cmd::GpsLayout { .. }
        | Cmd::LogChecks { .. } => Some((false, false)),
        #[cfg(feature = "dev-seed")]
        Cmd::Dev { cmd: DevCmd::Seed { .. } | DevCmd::History { .. } | DevCmd::Storage { .. } } => Some((true, false)),
        Cmd::Log { .. } | Cmd::Health => None,
    }
}

/// Runs a command. `mounts`: the data and keys folders must each be a mount of their own (`SENTRY_MATE_REQUIRE_MOUNTS`,
/// set in the image), checked before any command that uses them.
async fn run(cli: Cli, mounts: bool) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let (data, keys) = (cli.data, cli.keys);
    if let Some(writes) = needs(&cli.cmd) {
        let mountinfo = mounts.then(|| std::fs::read_to_string(folders::MOUNTINFO)).transpose().map_err(|e| {
            format!(
                "can't read {} to check that the data and keys folders are mounted: {e} (SENTRY_MATE_REQUIRE_MOUNTS=0 \
                 turns that check off)",
                folders::MOUNTINFO
            )
        })?;
        folders::check_all(&data, &keys, writes, mountinfo.as_deref())?;
    }
    match cli.cmd {
        Cmd::Serve { bind, names, move_from, move_code, offer_move } => {
            let env = MoveEnv {
                from: move_from.filter(|s| !s.trim().is_empty()),
                code: move_code.filter(|s| !s.trim().is_empty()),
                offer: offer_move.filter(|s| !s.trim().is_empty()),
            };
            let net = Net { bind, upload: cli.upload_port, web: cli.web_port };
            serve(&data, &keys, &net, &names, &env, accounts::Hashing::STRONG).await?
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
        Cmd::RecoveryKey { cmd: RecoveryCmd::New } => recovery_key_new(&data, &keys, Kdf::STRONG)?,
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
                TokenCmd::New { car, keys } => {
                    let (token, t) = tokens.create_for(&car, &keys).await?;
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
        Cmd::Log { cmd: LogCmd::Decode { name, any, model } } => {
            let store = Store::new(can_dir(&data), Arc::new(Vault::open(&data, &keys)?));
            let judge = Judge::new(store.clone());
            let out = std::io::stdout();
            return decode::run(&store, &judge, &name, any, model, &mut out.lock(), &mut std::io::stderr()).await;
        }
        Cmd::GpsLayout { car, days, wait } => {
            let Some(car) = gps_layout::parse_car(&car) else {
                eprintln!("--car: a car as its pages' address names it, like c1");
                return Ok(ExitCode::FAILURE);
            };
            let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
            let wait = Duration::from_secs(wait * 60);
            return gps_layout::run(&data, &keys, car, days, wait, &mut out, &mut err).await;
        }
        Cmd::LogChecks { car } => {
            let Some(car) = gps_layout::parse_car(&car) else {
                eprintln!("--car: a car as its pages' address names it, like c1");
                return Ok(ExitCode::FAILURE);
            };
            let out = std::io::stdout();
            return log_checks::run(&data, &keys, car, &mut out.lock(), &mut std::io::stderr()).await;
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
            let judge = Judge::new(Store::new(can_dir(&data), Arc::new(Vault::open(&data, &keys)?)));
            if !judge.known(&key) {
                return Ok(keys::unknown(&key, &mut std::io::stderr())?);
            }
            println!("{}", serde_json::to_string_pretty(&judge.chain(&key).await)?);
        }
        Cmd::Keys { cmd, key } => {
            let judge = Judge::new(Store::new(can_dir(&data), Arc::new(Vault::open(&data, &keys)?)));
            let (out, err) = (&mut std::io::stdout(), &mut std::io::stderr());
            return match (cmd, key) {
                (Some(KeysCmd::Trust { key, signing_key, yes }), _) => {
                    keys::trust(&judge, &key, &signing_key, yes, &mut std::io::stdin().lock(), out, err).await
                }
                (None, Some(key)) => keys::list(&judge, &key, out, err).await,
                // main() stops before this
                (None, None) => {
                    writeln!(err, "{KEYS_USAGE}")?;
                    Ok(ExitCode::from(2))
                }
            };
        }
        Cmd::Export => return export_cmd(&data, &keys),
        Cmd::Import { file, cancel } => return import_cmd(&data, &keys, file.as_deref(), cancel).await,
        Cmd::Move { cmd } => return move_cmd(&data, &keys, cli.upload_port, cmd).await,
        #[cfg(feature = "dev-seed")]
        Cmd::Dev {
            cmd: DevCmd::Seed { days, cars, seed, light, clean, candump, with_real_cars, start, awake_min },
        } => {
            let v = Arc::new(Vault::open(&data, &keys)?);
            refuse_writes(&data, &v)?;
            let store = Store::new(can_dir(&data), v.clone());
            let judge = Judge::new(store.clone());
            let tokens = Tokens::open(tokens_file(&data), v.clone()).await?;
            let o =
                seed::Options { days, cars, seed, light, clean, candump, with_real_cars, start_ms: start, awake_min };
            // each file's writes go through the data folder's gate, as an upload's chunk does (an export or a move
            // started meanwhile waits for one file, not for the whole run)
            let gate = gate::Gate::open(&data)?;
            let enter = || -> Result<gate::Pass, Box<dyn std::error::Error>> {
                let pass = gate.enter(GATE_WAIT)?;
                refuse_writes(&data, &v)?;
                Ok(pass)
            };
            seed::run(&store, &judge, &tokens, &o, &enter, &mut std::io::stdout()).await?;
        }
        #[cfg(feature = "dev-seed")]
        Cmd::Dev {
            cmd: DevCmd::History { end, days, drives, charges, drive_step, teslafi_days, api_days, tesla_days, seed },
        } => {
            let _lock = take_server_lock(&data).await?;
            let v = Arc::new(Vault::open(&data, &keys)?);
            refuse_writes(&data, &v)?;
            let db = db::Db::open(&data, v.clone(), db::Options::default())?;
            let engine = engine::Engine::new(db, Store::new(can_dir(&data), v), engine::Options::default());
            engine.start()?;
            let o = history::Options {
                days,
                end_ms: end,
                seed,
                drives,
                charges,
                drive_step_s: drive_step.max(1),
                teslafi_days,
                api_days,
                tesla_days,
            };
            let t = Instant::now();
            let c = history::run(&engine, &o).await?;
            println!("{c:?} in {:.1} s", t.elapsed().as_secs_f64());
        }
        #[cfg(feature = "dev-seed")]
        Cmd::Dev { cmd: DevCmd::Storage { compact } } => {
            let _lock = take_server_lock(&data).await?;
            let v = Arc::new(Vault::open(&data, &keys)?);
            let db = db::Db::open(&data, v.clone(), db::Options::default())?;
            let engine = engine::Engine::new(db, Store::new(can_dir(&data), v), engine::Options::default());
            if compact {
                refuse_writes(&data, engine.store().vault())?;
                let t = Instant::now();
                let r = engine.compact(&|| false, false)?;
                println!("compacted in {:.1} s: {r:?}", t.elapsed().as_secs_f64());
            }
            let cats = engine::query::storage::categories(&engine)?;
            println!("| category | files | bytes | on disk |\n|---|---:|---:|---:|");
            let (mut f, mut b, mut d) = (0, 0, 0);
            for c in &cats {
                println!("| `{}` | {} | {} | {} |", c.key, c.files, c.bytes, c.disk_bytes);
                (f, b, d) = (f + c.files, b + c.bytes, d + c.disk_bytes);
            }
            println!("| **total** | {f} | {b} | {d} |");
        }
        Cmd::Health => {
            // a move coming in keeps the upload port closed until it switches: healthy meanwhile
            if data.join(moving::pull::STAGING).exists() {
                return Ok(ExitCode::SUCCESS);
            }
            // the upload port, and the web port unless the data moved away (the web UI then runs on the new server)
            let mut ports = vec![cli.upload_port];
            if !moving::old::is_moved(&data) {
                ports.push(cli.web_port);
            }
            for port in ports {
                let addr = format!("127.0.0.1:{port}");
                let ok = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(&addr))
                    .await
                    .is_ok_and(|r| r.is_ok());
                if !ok {
                    return Ok(ExitCode::FAILURE);
                }
            }
            return Ok(ExitCode::SUCCESS);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// A file's bytes; an error names the file.
fn read_named(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("can't read {}: {e}", path.display()))
}

fn log_cmd(cmd: LogCmd) -> Result<ExitCode, Box<dyn std::error::Error>> {
    match cmd {
        LogCmd::Candump { file } => {
            let bytes = read_named(&file)?;
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
            let bytes = read_named(&file)?;
            let side_path = sidecar.unwrap_or_else(|| PathBuf::from(format!("{}.json", file.display())));
            let side: serde_json::Value = serde_json::from_slice(&read_named(&side_path)?)
                .map_err(|e| format!("{} is not a sidecar (JSON): {e}", side_path.display()))?;
            let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let sha = hex::encode(Sha256::digest(&bytes));
            match trust::check(name, &sha, bytes.len() as u64, &side) {
                trust::Check::Signed(l) => {
                    println!("signed and intact: key {}, chain index {}", l.key_fingerprint, l.index)
                }
                trust::Check::NotSigned { bridge_error, .. } => {
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

/// Where `serve` listens.
struct Net {
    bind: String,
    upload: u16,
    web: u16,
}

/// Runs the server until stopped: the upload port, the engine and the web port, started again after an import is
/// switched to, a move undone or a move coming in. `hashing`: the accounts' password hashing (tests make it fast).
async fn serve(
    data: &Path,
    keys: &Path,
    net: &Net,
    names: &[String],
    env: &MoveEnv,
    hashing: accounts::Hashing,
) -> Result<(), Box<dyn std::error::Error>> {
    let port = net.upload;
    tracing::info!("Sentry Mate {VERSION}");
    refuse_key_file_over_empty_data(data, keys)?;
    std::fs::create_dir_all(data)?;
    let _lock = take_server_lock(data).await?;
    let gate = gate::Gate::open(data)?;
    // before the vault is opened or made: what a crash left, an import ready to switch to, a move coming in
    moving::import::tidy(data, keys);
    moving::pull::tidy(data, keys);
    let mut refused: Option<String> = None;
    let mut tried: Option<String> = None;
    // the web pages' set-up claim, kept across the starts below (a restore's switch starts the web side again)
    let claim = web::moving::ClaimKeep::default();
    loop {
        if !carry_on_import(data, keys, &mut refused).await {
            tracing::info!("stopping");
            return Ok(());
        }
        if !carry_on_move(data, keys, env, port, &mut tried).await {
            tracing::info!("stopping");
            return Ok(());
        }
        let Instance { mut server, housekeeping, offer, ticker, confirm, mut side } =
            start(data, keys, &gate, net, names, env, hashing, &claim).await?;
        let mut housekeeping = Some(housekeeping);
        let mut moved = offer.watch();
        let was_moved = offer.is_moved();
        let ready = import_ready(data.to_path_buf(), keys.to_path_buf(), refused.clone());
        tokio::pin!(ready);
        // a move into this server runs here, with the web side up (its page shows how far it is)
        let mut incoming = MoveRun::new();
        let mut look = tokio::time::interval(Duration::from_secs(1));
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
                    incoming.stop().await;
                    side.stop(site::ENGINE_STOP).await;
                    r?;
                    return Ok(());
                }
                r = &mut side.web => {
                    stop(&mut housekeeping);
                    incoming.stop().await;
                    side.stop(site::ENGINE_STOP).await;
                    r?;
                    return Ok(());
                }
                _ = shutdown() => break Next::Stop,
                _ = &mut ready => break Next::Import,
                _ = look.tick(), if !incoming.running() => {
                    if incoming.look(data, keys).await {
                        break Next::Restart;
                    }
                }
                r = MoveRun::ended(&mut incoming.task) => {
                    incoming.task = None;
                    if incoming.after(r, data, keys, port).await {
                        break Next::Restart;
                    }
                }
                c = moved.changed() => {
                    if c.is_err() {
                        continue;
                    }
                    let now = *moved.borrow_and_update();
                    if now {
                        // the data moved away: nothing writes here from now on (the history included); the web
                        // side stays up, read only, to show the move and undo it
                        if let Some(h) = housekeeping.take() {
                            h.abort();
                        }
                        side.stop_engine(site::ENGINE_STOP_RESTART).await;
                        tracing::info!("move: the data moved; the web UI here shows it, read only");
                    } else if was_moved || housekeeping.is_none() {
                        // `move undo`: this server takes uploads again, with its housekeeping
                        break Next::Restart;
                    }
                }
                _ = hourly.tick() => {
                    if let Some(r) = offer.last_refused()
                        && said_refused != Some(r.at_ms) && ingest::store::now_ms().saturating_sub(r.at_ms) < 3_600_000 {
                            said_refused = Some(r.at_ms);
                            let car = r.car_key.as_deref().map(|k| format!(" (car key {k})")).unwrap_or_default();
                            tracing::warn!(
                                "move: a CAN Bridge{car} still sends here, last on {}: change its server address to the new server",
                                moving::old::when(r.at_ms)
                            );
                        }
                }
            }
        };
        incoming.stop().await;
        match next {
            Next::Stop => {
                stop(&mut housekeeping);
                side.stop(site::ENGINE_STOP).await;
                tracing::info!("stopping");
                return Ok(());
            }
            Next::Restart => {
                stop(&mut housekeeping);
                side.stop(site::ENGINE_STOP_RESTART).await;
                drop(server);
                continue;
            }
            Next::Import => {}
        }
        // an import was read and checked by `import` while this server ran: stop the writes, switch, start again
        tracing::info!("import: the imported data is checked; this server stops its uploads and switches to it");
        let g = gate.clone();
        // the history and the web side first: they write to the data folder without the gate
        side.stop(site::ENGINE_STOP_RESTART).await;
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
    /// The engine and the web port.
    side: Side,
}

/// The engine and the web port of one start (neither on a server whose data moved away).
struct Side {
    engine: Option<site::EngineRun>,
    /// The web port (pending forever when it isn't served).
    web: std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>> + Send>>,
    app: Option<web::AppState>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    /// The Tesla API input's pollers (running only when a Tesla account exists).
    tesla: Option<Arc<web::resources::tesla::TeslaService>>,
    /// The TeslaFi input's pollers (running only when a TeslaFi account exists).
    teslafi: Option<Arc<web::resources::teslafi::TeslafiService>>,
    /// The local TeslaMate input (running only when an admin added its settings).
    teslamate_local: Option<Arc<web::resources::teslamate_local::TeslamateLocalService>>,
}

impl Side {
    /// Stops the engine's background work only (its data stays readable for the web side).
    async fn stop_engine(&mut self, wait: Duration) {
        if let Some(t) = &self.tesla {
            t.stop();
        }
        if let Some(t) = &self.teslafi {
            t.stop();
        }
        if let Some(t) = &self.teslamate_local {
            t.stop();
        }
        if let Some(e) = self.engine.as_mut() {
            e.stop(wait).await;
        }
    }

    /// Stops the web port (its connections end) and then the engine (waiting at most `wait` for a log under way).
    async fn stop(&mut self, wait: Duration) {
        self.web = Box::pin(std::future::pending());
        if let Some(t) = self.tesla.take() {
            t.stop();
        }
        if let Some(t) = self.teslafi.take() {
            t.stop();
        }
        if let Some(t) = self.teslamate_local.take() {
            t.stop();
        }
        for t in self.tasks.drain(..) {
            t.abort();
        }
        if let Some(app) = self.app.take() {
            site::flush(&app).await;
        }
        if let Some(mut e) = self.engine.take() {
            e.stop(wait).await;
        }
    }
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

/// `recovery-key new`: the file first, then the vault, so a failed write leaves the old key working (the error says
/// so) and nothing holds a key that doesn't open the vault.
fn recovery_key_new(data: &Path, keys: &Path, kdf: Kdf) -> Result<(), Box<dyn std::error::Error>> {
    let v = Vault::open(data, keys)?;
    refuse_writes(data, &v)?;
    let (rk, file) = moving::import::new_recovery_key(&v, data, keys, kdf)?;
    print_recovery(&rk);
    println!("The old recovery key no longer works. The new one is also in {} until you delete it.", file.display());
    Ok(())
}

/// Commands that change the data refuse on a server whose data moved away, and on one a move is coming into.
fn refuse_writes(data: &Path, v: &Vault) -> Result<(), Box<dyn std::error::Error>> {
    moving::old::refuse_if_moved(data, v)?;
    if data.join(moving::pull::STAGING).exists() {
        return Err("a move is coming in here (see `sentry-mate move status`): wait until it is done, or `sentry-mate move cancel`".into());
    }
    Ok(())
}

type PullTask = tokio::task::JoinHandle<Result<moving::pull::Done, moving::pull::Problem>>;

/// A move into this server, carried on while the server runs with its web side up (begun from the web page, by
/// `move from`, by the two lines, or staged before a restart). It stops before the switch: the server then starts
/// again, and the switch runs before anything else does.
struct MoveRun {
    task: Option<PullTask>,
    stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// After a problem it tries again by itself, not before this.
    not_before: tokio::time::Instant,
}

impl MoveRun {
    fn new() -> MoveRun {
        MoveRun { task: None, stop: None, not_before: tokio::time::Instant::now() }
    }

    fn running(&self) -> bool {
        self.task.is_some()
    }

    /// Resolves when the run ends (never while there is none).
    async fn ended(task: &mut Option<PullTask>) -> Result<Result<moving::pull::Done, moving::pull::Problem>, String> {
        match task.as_mut() {
            Some(t) => t.await.map_err(|e| e.to_string()),
            None => std::future::pending().await,
        }
    }

    /// Looks whether a staged move wants carrying on, and starts it. `true`: a switch waits (start again for it).
    async fn look(&mut self, data: &Path, keys: &Path) -> bool {
        if tokio::time::Instant::now() < self.not_before || !data.join(moving::pull::STAGING).exists() {
            return false;
        }
        let (d, k) = (data.to_path_buf(), keys.to_path_buf());
        let Ok(p) = tokio::task::spawn_blocking(move || moving::pull::pending(&d, &k)).await else { return false };
        if let moving::pull::Pending::Moving { phase: moving::pull::Phase::Switching, .. } = &p {
            return true;
        }
        if !pull_wanted(&p) {
            return false;
        }
        if let moving::pull::Pending::Moving { from, code_id, percent, .. } = &p {
            if *percent > 0 {
                tracing::info!("move: carrying on the move from {from} (move code {code_id}; {percent} % was copied)");
            } else {
                tracing::info!("move: bringing the data from {from} (move code {code_id})");
            }
        }
        let mut o = move_options();
        o.stop_before_switch = true;
        self.stop = Some(o.stop.clone());
        let (d, k) = (data.to_path_buf(), keys.to_path_buf());
        self.task = Some(tokio::task::spawn_blocking(move || moving::pull::run(&d, &k, &o)));
        false
    }

    /// What a run that ended means. `true`: the copy is done (start again: the switch runs first).
    async fn after(
        &mut self,
        r: Result<Result<moving::pull::Done, moving::pull::Problem>, String>,
        data: &Path,
        keys: &Path,
        port: u16,
    ) -> bool {
        self.stop = None;
        match r {
            Ok(Ok(done)) => {
                report_moved_in(&done, port);
                true
            }
            Ok(Err(p)) if p.code == moving::pull::SWITCH_NEXT => {
                tracing::info!("move: the copy is done; this server starts again to switch to it");
                true
            }
            Ok(Err(p)) if p.code == "stopped" => false,
            Ok(Err(p)) if p.code == "cancelled_here" => {
                // `move cancel` removes the copy; what a stop part way left is removed here
                let (d, k) = (data.to_path_buf(), keys.to_path_buf());
                let _ = tokio::task::spawn_blocking(move || moving::pull::tidy(&d, &k)).await;
                tracing::info!("move: the move was cancelled here; this server goes on as a new one");
                false
            }
            Ok(Err(p)) => {
                tracing::warn!("move: {}", p.message);
                if p.retry {
                    tracing::warn!("move: trying again in {} s", SWITCH_AGAIN.as_secs());
                    self.not_before = tokio::time::Instant::now() + SWITCH_AGAIN;
                }
                false
            }
            Err(e) => {
                tracing::warn!("move: the move stopped: {e}; trying again in {} s", SWITCH_AGAIN.as_secs());
                self.not_before = tokio::time::Instant::now() + SWITCH_AGAIN;
                false
            }
        }
    }

    /// Stops the run at its next step (it carries on from there on the next start).
    async fn stop(&mut self) {
        if let Some(s) = self.stop.take() {
            s.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(t) = self.task.take() {
            let _ = t.await;
        }
    }
}

/// Whether a staged move is one to carry on now (not one waiting for the owner).
fn pull_wanted(p: &moving::pull::Pending) -> bool {
    match p {
        moving::pull::Pending::Moving { phase, problem, cancelled, forced, .. } => {
            *phase == moving::pull::Phase::Switching
                || *forced
                || (!cancelled && problem.as_ref().is_none_or(|p| p.retry))
        }
        _ => false,
    }
}

fn code_id(code: &str) -> Option<String> {
    moving::code::MoveCode::parse(code).ok().map(|c| c.keys().id)
}

/// On start, a move into this server: started if the two lines are set and this server is new, and switched to if
/// its copy is done. The copy itself runs once the server is up ([`MoveRun`], the web side showing it); a problem
/// only the owner can solve is said in the log, and the server starts as usual (it has no tokens, so nothing reaches
/// it). `false`: told to stop meanwhile.
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
                if let Some(note) = moved_in_note(data, keys)
                    && id.as_deref() == Some(note.code_id.as_str())
                {
                    tracing::info!(
                        "move: the move from {} is done; you can remove SENTRY_MATE_MOVE_FROM and SENTRY_MATE_MOVE_CODE",
                        note.from
                    );
                    return true;
                }
                let (d, k) = (data.to_path_buf(), keys.to_path_buf());
                let also = opts.first_start_also.clone();
                if let Ok(Err(e)) = tokio::task::spawn_blocking(move || moving::import::unused_but(&d, &k, &also)).await
                {
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
            moving::pull::Pending::Moving { phase, .. } if *phase != moving::pull::Phase::Switching => {
                // the running server carries it on, with its web side up ([`MoveRun`])
                let _ = just_begun;
                return true;
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

/// What `import` runs with: as the pages' restore, what a first start of the server wrote before anyone set it up
/// is not the owner's data (docs/moving.md: "start it with empty folders, then import").
fn import_options() -> moving::Options {
    let mut o = moving::Options::new(APP_VERSION);
    o.first_start_also = moving::import::WEB_FIRST_START.iter().map(|n| n.to_string()).collect();
    o
}

fn move_options() -> moving::pull::Options {
    let mut o = moving::pull::Options::new(APP_VERSION);
    o.min_free = ingest::store::MIN_FREE;
    // what the web side writes before there is an account isn't the owner's data
    o.first_start_also = moving::import::WEB_FIRST_START.iter().map(|n| n.to_string()).collect();
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

/// A key file over an empty data folder: refused before the server's lock and gate files are made, so a refused
/// start writes nothing. (An import or a move coming in keeps its state in the data folder: an empty one has none.)
fn refuse_key_file_over_empty_data(data: &Path, keys: &Path) -> Result<(), vault::Error> {
    let empty = match std::fs::read_dir(data) {
        Ok(mut d) => d.next().is_none(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => return Err(e.into()),
    };
    match Vault::open(data, keys) {
        Err(e @ vault::Error::KeyFileButNoVault { .. }) if empty => Err(e),
        _ => Ok(()),
    }
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
                    "import: an import is half done ({percent} %): choose the same file again on the web page (or run `sentry-mate import FILE` again) to carry it on, or give it up there (or `sentry-mate import --cancel`)"
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
            && refused.as_deref() != Some(export_id.as_str())
        {
            return;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn start(
    data: &Path,
    keys: &Path,
    gate: &gate::Gate,
    net: &Net,
    names: &[String],
    env: &MoveEnv,
    hashing: accounts::Hashing,
    claim: &web::moving::ClaimKeep,
) -> Result<Instance, Box<dyn std::error::Error>> {
    let (bind, port) = (net.bind.as_str(), net.upload);
    let moved = moving::old::is_moved(data);
    // the start's own writes go through the gate too (an export's snapshot from the command line may be under way);
    // a server whose data moved away writes nothing
    let pass = if moved { None } else { Some(ingest::api::enter_gate(gate, GATE_WAIT).await?) };
    let (v, made) = Vault::open_or_create(data, keys, Kdf::STRONG)?;
    // only once the vault opened: a start that is refused writes nothing
    std::fs::create_dir_all(can_dir(data))?;
    if let Some(rk) = made {
        // not in the log (container logs are kept in the clear): in a file of the keys folder, for the owner to take
        let file = moving::import::write_recovery_file(keys, &rk)?;
        tracing::warn!(
            "new vault made: its recovery key is in {} — copy it somewhere safe off this server, then delete that file",
            file.display()
        );
    } else if keys.join(moving::import::RECOVERY_FILE).exists() {
        tracing::warn!(
            "{} is still there: copy the recovery key somewhere safe off this server, then delete it",
            keys.join(moving::import::RECOVERY_FILE).display()
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
    // what both ports serve: a new certificate made from the web pages is swapped in here, without a restart
    let live = ingest::server::LiveCert::new(&id, &tls_dir(data), v.clone())?;
    let tokens = Tokens::open(tokens_file(data), v.clone()).await?;
    drop(pass);
    let any_token = tokens.has_any().await;
    let store = Store::new(can_dir(data), v.clone());
    // the move's side of this server: a moved one closes its gate and holds it from here on
    let (d, v2, g, fp) = (data.to_path_buf(), v.clone(), gate.clone(), id.fingerprint());
    let offer = tokio::task::spawn_blocking(move || moving::old::Offer::open(&d, v2, g, APP_VERSION, &fp)).await??;
    // the history: told of every log the uploads' judge checks (not on a server whose data moved away)
    let serving = !offer.is_moved();
    // the engine's own view of the tokens (the same files; the uploads' instance ties them), with the uploads' last
    // contacts: the pages show each bridge's last request as it happens
    let engine_tokens =
        Arc::new(Tokens::open(tokens_file(data), v.clone()).await?.with_contacts(tokens.contacts().clone()));
    let (d, v2, s2) = (data.to_path_buf(), v.clone(), store.clone());
    let engine = tokio::task::spawn_blocking(move || site::EngineRun::open(&d, v2, s2, Some(engine_tokens), serving))
        .await?
        .map_err(|e| e as Box<dyn std::error::Error>)?;
    let judge = Judge::new(store.clone()).on_judged(engine.engine.listener());
    let up = Uploads::with_gate(store, tokens, judge, gate.clone(), ingest::api::GATE_WAIT);
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
        tracing::info!(
            "no upload token yet: make one with `docker exec sentry-mate /sentry-mate token new --car \"My car\"`"
        );
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
            if n.is_multiple_of(60) {
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
    let server = Box::pin(serve_tls(listener, live.tls_config()?, app));
    if !serving {
        tracing::info!(
            "the data moved away: the web UI here shows the move, read only (the data is on the new server)"
        );
    }
    let renewer = Arc::new(site::Renewer { live, offer: Some(offer.clone()), gate: gate.clone() });
    let side = start_web(data, keys, &v, engine, renewer, net, hashing, claim.clone(), offer.clone(), gate).await?;
    let offer = Arc::new(OfferAndUploads { offer, uploads: up });
    Ok(Instance { server, housekeeping, offer, ticker, confirm, side })
}

/// The web port, over the engine (its cars, its live changes), with the certificate the upload port uses.
#[allow(clippy::too_many_arguments)]
async fn start_web(
    data: &Path,
    keys: &Path,
    v: &Arc<Vault>,
    engine: site::EngineRun,
    renewer: Arc<site::Renewer>,
    net: &Net,
    hashing: accounts::Hashing,
    claim: web::moving::ClaimKeep,
    offer: Arc<moving::old::Offer>,
    gate: &gate::Gate,
) -> Result<Side, Box<dyn std::error::Error>> {
    let moved = offer.is_moved();
    let (d, k, v2, e2) = (data.to_path_buf(), keys.to_path_buf(), v.clone(), engine.engine.clone());
    let ports = web::resources::mqtt::Network { web_port: net.web, upload_port: net.upload };
    let live = renewer.live.clone();
    let certs: Arc<dyn web::resources::certificate::CertificateMaker> = renewer;
    let app = tokio::task::spawn_blocking(move || {
        site::web_state(&d, &k, v2, &e2, hashing, Some(ports), claim, Some(offer), Some(certs))
    })
    .await?
    .map_err(|e| e as Box<dyn std::error::Error>)?;
    // a CAN export's logs are stored through the data folder's gate, as uploads are
    if let Ok(im) = app.service::<web::resources::imports::Imports>() {
        im.set_gate(gate.clone());
        // imported rows an older server put on the wrong car are moved, once
        drop(web::resources::imports::spawn_rehome(&app));
    }
    let listener = tokio::net::TcpListener::bind((net.bind.as_str(), net.web)).await?;
    tracing::info!("web UI on https://{}:{}/ (TLS 1.3), the same certificate", net.bind, net.web);
    let a2 = app.clone();
    let announced =
        tokio::task::spawn_blocking(move || a2.announce_setup().map(|c| c.is_some()).map_err(|e| e.message))
            .await?
            .map_err(|e| format!("the setup code: {e}"))?;
    let tasks = vec![
        site::forward(&engine.engine, app.events.clone()),
        site::spawn_upkeep(app.clone(), announced),
        web::resources::mqtt::spawn(app.clone()),
        // the live figures of the open pages (jobs, health, connections, storage) onto the live stream
        web::live::spawn(app.clone()),
    ];
    // the logs the server lost are looked for after the start and once a day (not after a move away: no uploads)
    let mut tasks = tasks;
    if !moved {
        tasks.push(site::spawn_missing_scan(app.clone(), gate.clone()));
        // a full rebuild's end into the audit log, and the one a new server version calls for started
        tasks.push(web::resources::full_rebuild::spawn_watch(app.clone()));
    }
    // a place's prices worked out from Supercharger invoices into the audit log
    tasks.push(web::resources::places::spawn_invoice_audit(app.clone()));
    // the Tesla API input reads cars only on a server that serves its data (not after a move away), and only for an
    // account an admin added: with none, nothing is started and nothing connects to Tesla
    let tesla = app.service::<web::resources::tesla::TeslaService>().ok();
    if let Some(t) = &tesla
        && !moved
    {
        t.start();
        let n = t.accounts().len();
        if n > 0 {
            tracing::info!("Tesla API: reading the cars of {n} Tesla account(s)");
        }
    }
    // the same for TeslaFi: only for a token an admin added
    let teslafi = app.service::<web::resources::teslafi::TeslafiService>().ok();
    if let Some(t) = &teslafi
        && !moved
    {
        t.start();
        let n = t.accounts().len();
        if n > 0 {
            tracing::info!("TeslaFi: reading the car(s) of {n} TeslaFi account(s)");
        }
    }
    // the same for the local TeslaMate: only once an admin added its settings (no file: nothing starts)
    let teslamate_local = app.service::<web::resources::teslamate_local::TeslamateLocalService>().ok();
    if let Some(t) = &teslamate_local
        && !moved
    {
        t.start();
        if t.store().exists() {
            tracing::info!("local TeslaMate: reading it");
        }
    }
    let web = Box::pin(web::serve(listener, live.tls_config()?, web::router_with(app.clone(), api::routes())));
    Ok(Side { engine: Some(engine), web, app: Some(app), tasks, tesla, teslafi, teslamate_local })
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
    let snap = moving::Snapshot::make(data, &v, &g, APP_VERSION, GATE_WAIT)?;
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
    let ex = moving::import::open_file(file, APP_VERSION)?;
    std::fs::create_dir_all(data)?;
    let h = &ex.header;
    eprintln!(
        "import: an export made by Sentry Mate {} on {}: {} files, {}",
        h.app,
        moving::export::day_of(h.made_ms),
        h.files,
        moving::size(h.bytes)
    );
    let opts = import_options();
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
            moving::import::unused_but(data, keys, &opts.first_start_also)?;
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
                Ok(moving::old::Cancelled::Ended { to, .. }) => println!(
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
        let cli = Cli { data, keys, upload_port: 0, web_port: 0, cmd: Cmd::Fingerprint };
        let Err(e) = run(cli, false).await else { panic!("it ran") };
        let said = e.to_string();
        assert!(said.contains("the data folder") && said.contains("the keys folder"), "{said}");
    }

    /// With the mounts required (the image), this process's own mounts are read: a temporary folder is no mount.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn with_the_mounts_required_folders_that_are_not_mounted_are_named() {
        let dir = tempfile::tempdir().unwrap();
        let (data, keys) = (dir.path().join("data"), dir.path().join("keys"));
        let cli = Cli { data: data.clone(), keys: keys.clone(), upload_port: 0, web_port: 0, cmd: Cmd::Init };
        let Err(e) = run(cli, true).await else { panic!("it ran") };
        let said = e.to_string();
        assert!(said.starts_with(&format!("the data folder {} and the keys folder", data.display())), "{said}");
        assert!(said.contains("are not mounted"), "{said}");
        // nothing made: no folder, no vault
        assert!(!data.exists() && !keys.exists());
    }

    /// `recovery-key new` whose key file can't be written ends with an error and leaves the old key working.
    #[test]
    fn a_new_recovery_key_that_cannot_be_saved_keeps_the_old_one() {
        let k = testkit::kit();
        let v = Vault::open(&k.data, &k.keys).unwrap();
        let first = v.new_recovery_key(&k.data, Kdf::FAST_FOR_TESTS).unwrap();
        let before = std::fs::read(k.data.join(vault::VAULT_FILE)).unwrap();
        std::fs::create_dir(k.keys.join("RECOVERY-KEY.tmp")).unwrap();
        let said = recovery_key_new(&k.data, &k.keys, Kdf::FAST_FOR_TESTS).err().unwrap().to_string();
        assert!(said.contains("the old recovery key still works"), "{said}");
        assert!(std::fs::read(k.data.join(vault::VAULT_FILE)).unwrap() == before, "the vault file is the same");
        let t = tempfile::tempdir().unwrap();
        let (d, ks) = (t.path().join("d"), t.path().join("k"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::copy(k.data.join(vault::VAULT_FILE), d.join(vault::VAULT_FILE)).unwrap();
        assert!(Vault::restore(&d, &ks, first.as_str()).is_ok(), "the old key still opens the vault");
        // once it can be written, the new key is in the file
        std::fs::remove_dir(k.keys.join("RECOVERY-KEY.tmp")).unwrap();
        recovery_key_new(&k.data, &k.keys, Kdf::FAST_FOR_TESTS).unwrap();
        assert!(k.keys.join(moving::import::RECOVERY_FILE).exists());
    }

    /// `chain` for a car key with no logs here says so (exit 1) rather than print an empty report.
    #[tokio::test]
    async fn the_chain_of_an_unknown_car_key_fails() {
        let k = testkit::kit();
        let chain = |key: &str| Cli {
            data: k.data.clone(),
            keys: k.keys.clone(),
            upload_port: 0,
            web_port: 0,
            cmd: Cmd::Chain { key: key.to_string() },
        };
        assert_eq!(run(chain("fedcba654321"), false).await.unwrap(), ExitCode::FAILURE);
        assert_eq!(run(chain("../../etc"), false).await.unwrap(), ExitCode::FAILURE);
        testkit::chain(&k, &testkit::new_key(), 0, 1, 0, testkit::genesis(), None).await;
        assert_eq!(run(chain(testkit::CAR), false).await.unwrap(), ExitCode::SUCCESS);
        let keys = Cli {
            data: k.data.clone(),
            keys: k.keys.clone(),
            upload_port: 0,
            web_port: 0,
            cmd: Cmd::Keys { cmd: None, key: Some("fedcba654321".into()) },
        };
        assert_eq!(run(keys, false).await.unwrap(), ExitCode::FAILURE);
    }

    #[test]
    fn the_command_line_takes_keys_and_keys_trust() {
        let cli = Cli::try_parse_from(["sentry-mate", "keys", "abcdef123456"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Keys { cmd: None, key: Some(k) } if k == "abcdef123456"));
        let cli = Cli::try_parse_from(["sentry-mate", "keys", "trust", "abcdef123456", "aa:bb", "--yes"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Keys { cmd: Some(KeysCmd::Trust { yes: true, .. }), key: None }));
        // no car key: main() says which usage, not clap (its usage line would show --data and --keys as needed)
        let cli = Cli::try_parse_from(["sentry-mate", "keys"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Keys { cmd: None, key: None }));
        assert!(KEYS_USAGE.contains("keys <KEY>") && !KEYS_USAGE.contains("--data"));
        assert!(Cli::try_parse_from(["sentry-mate", "keys", "trust", "abcdef123456"]).is_err());
        let cli =
            Cli::try_parse_from(["sentry-mate", "log", "decode", "x", "--any", "--model", "tesla-sx-legacy"]).unwrap();
        let Cmd::Log { cmd: LogCmd::Decode { any: true, model, .. } } = cli.cmd else { panic!("not decode") };
        assert_eq!(model, decode::Model::SxLegacy);
        let cli = Cli::try_parse_from(["sentry-mate", "log", "decode", "x"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Log { cmd: LogCmd::Decode { any: false, model: decode::Model::Auto, .. } }));
        assert!(Cli::try_parse_from(["sentry-mate", "log", "decode", "x", "--model", "z"]).is_err());
    }

    #[test]
    fn the_command_line_is_consistent_and_token_new_takes_car_keys() {
        // clap's own check of the definitions: two arguments with one id (the global --keys) fail here, not at run
        <Cli as clap::CommandFactory>::command().debug_assert();
        let cli =
            Cli::try_parse_from(["sentry-mate", "token", "new", "--car", "Model Y", "--key", "abc", "--key", "def"])
                .unwrap();
        let Cmd::Token { cmd: TokenCmd::New { car, keys } } = cli.cmd else { panic!("not token new") };
        assert_eq!((car.as_str(), keys), ("Model Y", vec!["abc".to_string(), "def".to_string()]));
        let cli = Cli::try_parse_from(["sentry-mate", "--keys", "/k", "token", "new", "--car", "X"]).unwrap();
        assert_eq!(cli.keys, PathBuf::from("/k"));
        assert!(matches!(cli.cmd, Cmd::Token { cmd: TokenCmd::New { keys, .. } } if keys.is_empty()));
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
    fn errors_read_plainly() {
        let e: Box<dyn std::error::Error> = ingest::ApiError::new(400, "bad_name", "That is not a log name.").into();
        assert_eq!(said(&*e), "That is not a log name.");
        let e = read_named(Path::new("/no/such/file.log.xz")).unwrap_err();
        assert!(e.starts_with("can't read /no/such/file.log.xz: "), "{e}");
    }

    #[test]
    fn the_version_starts_with_the_crates_own() {
        assert!(VERSION.starts_with(env!("CARGO_PKG_VERSION")), "{VERSION}");
    }

    /// A new server that takes data in (an import; a direct move ends with the same switch) writes a key file of its
    /// own. The folder checks pass on its empty folders, and the refusal of a new vault next to a key file doesn't stop
    /// it starting afterwards. A key file with no vault is still refused, by the vault and by the import.
    #[tokio::test]
    async fn a_key_file_over_an_empty_data_folder_is_refused_and_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let (d, k) = (dir.path().join("data"), dir.path().join("keys"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::create_dir_all(&k).unwrap();
        std::fs::write(k.join("sentry-mate.key"), b"old-key").unwrap();
        let net = Net { bind: "127.0.0.1".into(), upload: 0, web: 0 };
        let e = serve(&d, &k, &net, &[], &MoveEnv::default(), accounts::Hashing::FAST_FOR_TESTS).await.unwrap_err();
        assert!(e.to_string().contains("has a key file"), "{e}");
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0, "something was written to the data folder");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_import_into_an_empty_server_passes_the_checks_and_starts() {
        let dir = tempfile::tempdir().unwrap();
        let (od, ok) = (dir.path().join("old-data"), dir.path().join("old-keys"));
        std::fs::create_dir_all(&od).unwrap();
        let (ov, rk) = Vault::create(&od, &ok, Kdf::FAST_FOR_TESTS).unwrap();
        let ov = Arc::new(ov);
        let fp = Identity::load_or_create(&tls_dir(&od), &[], &ov).unwrap().fingerprint();
        Tokens::open(tokens_file(&od), ov.clone()).await.unwrap().create("Test car").await.unwrap();
        let file = dir.path().join("x.smexport");
        {
            let g = gate::Gate::open(&od).unwrap();
            let snap = moving::Snapshot::make(&od, &ov, &g, APP_VERSION, Duration::from_secs(5)).unwrap();
            let mut out = Vec::new();
            snap.write_to(&ov, &mut out).unwrap();
            std::fs::write(&file, out).unwrap();
        }

        let (nd, nk) = (dir.path().join("new-data"), dir.path().join("new-keys"));
        std::fs::create_dir_all(&nd).unwrap();
        std::fs::create_dir_all(&nk).unwrap();
        folders::check_all(&nd, &nk, needs(&Cmd::Import { file: None, cancel: false }).unwrap(), None).unwrap();
        moving::import::unused(&nd, &nk).unwrap();
        let mut opts = moving::Options::new(APP_VERSION);
        opts.min_free = 0;
        let ex = moving::import::open_file(&file, APP_VERSION).unwrap();
        let mut imp = moving::import::begin(&nd, &nk, &ex, rk.as_str(), &opts).unwrap();
        assert_eq!(imp.run(|_| std::ops::ControlFlow::Continue(())).unwrap(), moving::Outcome::Ready);
        drop(imp);
        moving::import::switch(&nd, &nk).unwrap();
        assert!(nk.join(vault::KEY_FILE).exists(), "the new server's own key file");
        assert_ne!(std::fs::read(nk.join(vault::KEY_FILE)).unwrap(), std::fs::read(ok.join(vault::KEY_FILE)).unwrap());

        // the start's vault step opens it and makes nothing; a command reads the same certificate
        let (nv, made) = Vault::open_or_create(&nd, &nk, Kdf::FAST_FOR_TESTS).unwrap();
        assert!(made.is_none());
        assert_eq!(nv.key_id(), ov.key_id());
        let cli = Cli { data: nd.clone(), keys: nk.clone(), upload_port: 0, web_port: 0, cmd: Cmd::Fingerprint };
        assert_eq!(run(cli, false).await.unwrap(), ExitCode::SUCCESS);
        assert_eq!(Identity::load_or_create(&tls_dir(&nd), &[], &nv).unwrap().fingerprint(), fp);

        // a key file with no vault: neither a new vault nor an import goes there
        let (d2, k2) = (dir.path().join("d2"), dir.path().join("k2"));
        std::fs::create_dir_all(&d2).unwrap();
        std::fs::create_dir_all(&k2).unwrap();
        std::fs::write(k2.join(vault::KEY_FILE), [7u8; 32]).unwrap();
        assert!(matches!(
            Vault::open_or_create(&d2, &k2, Kdf::FAST_FOR_TESTS),
            Err(vault::Error::KeyFileButNoVault { .. })
        ));
        assert!(moving::import::unused(&d2, &k2).is_err());
    }

    /// A new server that was started once (no account, nothing uploaded) still takes data in: what its first start
    /// wrote by itself (the set-up code, the audit log, a certificate, the database's own key) is not the owner's,
    /// for the pages' "Move here" and "Restore", the move lines and `import` alike, and is set aside at the switch.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_server_that_started_once_still_takes_data_in() {
        let dir = tempfile::tempdir().unwrap();
        let (od, ok) = (dir.path().join("old-data"), dir.path().join("old-keys"));
        std::fs::create_dir_all(&od).unwrap();
        let (ov, rk) = Vault::create(&od, &ok, Kdf::FAST_FOR_TESTS).unwrap();
        let ov = Arc::new(ov);
        Tokens::open(tokens_file(&od), ov.clone()).await.unwrap().create("Test car").await.unwrap();
        let file = dir.path().join("x.smexport");
        {
            let g = gate::Gate::open(&od).unwrap();
            let snap = moving::Snapshot::make(&od, &ov, &g, APP_VERSION, Duration::from_secs(5)).unwrap();
            let mut out = Vec::new();
            snap.write_to(&ov, &mut out).unwrap();
            std::fs::write(&file, out).unwrap();
        }

        // the new server's first start, through the real start (the engine, the web side and its start-up passes)
        let (nd, nk) = (dir.path().join("new-data"), dir.path().join("new-keys"));
        std::fs::create_dir_all(&nd).unwrap();
        std::fs::create_dir_all(&nk).unwrap();
        let net = Net { bind: "127.0.0.1".into(), upload: 0, web: 0 };
        let gate = gate::Gate::open(&nd).unwrap();
        let claim = web::moving::ClaimKeep::default();
        let hashing = accounts::Hashing::FAST_FOR_TESTS;
        let mut inst = start(&nd, &nk, &gate, &net, &[], &MoveEnv::default(), hashing, &claim).await.unwrap();
        // (the start's pass over imported rows, once more and to its end: what it writes is written now)
        web::resources::imports::spawn_rehome(inst.side.app.as_ref().unwrap()).await.unwrap();
        assert!(nd.join("accounts").exists(), "the first start's set-up code");
        // the pages' "Move here" and "Restore", and the move lines
        moving::import::unused_but(&nd, &nk, &move_options().first_start_also).unwrap();
        // `import` beside the running server, and after it stopped
        moving::import::unused_but(&nd, &nk, &import_options().first_start_also).unwrap();
        inst.housekeeping.abort();
        inst.ticker.abort();
        inst.side.stop(Duration::from_secs(10)).await;
        drop(inst);
        let mut opts = import_options();
        opts.min_free = 0;
        moving::import::unused_but(&nd, &nk, &opts.first_start_also).unwrap();
        let ex = moving::import::open_file(&file, APP_VERSION).unwrap();
        let mut imp = moving::import::begin(&nd, &nk, &ex, rk.as_str(), &opts).unwrap();
        assert_eq!(imp.run(|_| std::ops::ControlFlow::Continue(())).unwrap(), moving::Outcome::Ready);
        drop(imp);
        assert!(moving::import::switch(&nd, &nk).unwrap().replaced_first_start);
        // nothing sealed with the first start's vault is left beside the imported data
        let nv = Arc::new(Vault::open(&nd, &nk).unwrap());
        assert_eq!(nv.key_id(), ov.key_id());
        assert!(!nd.join("accounts").exists() && !nd.join("audit").exists(), "the first start's own files stayed");
        assert_eq!(Tokens::open(tokens_file(&nd), nv.clone()).await.unwrap().list().await.len(), 1);
        // and the server starts on it
        let gate = gate::Gate::open(&nd).unwrap();
        let mut inst = start(&nd, &nk, &gate, &net, &[], &MoveEnv::default(), hashing, &claim).await.unwrap();
        inst.housekeeping.abort();
        inst.ticker.abort();
        inst.side.stop(Duration::from_secs(10)).await;

        // a server someone set up is in use all the same: an account is the owner's
        std::fs::create_dir_all(nd.join("accounts")).unwrap();
        std::fs::write(nd.join("accounts").join("account-1.sealed"), b"x").unwrap();
        assert!(moving::import::unused_but(&nd, &nk, &import_options().first_start_also).is_err());
    }

    #[test]
    fn commands_that_need_no_folder_dont_check_one() {
        assert_eq!(needs(&Cmd::Health), None);
        assert_eq!(needs(&Cmd::Log { cmd: LogCmd::Candump { file: PathBuf::from("x") } }), None);
        let serve =
            Cmd::Serve { bind: String::new(), names: vec![], move_from: None, move_code: None, offer_move: None };
        assert_eq!(needs(&serve), Some((true, true)));
        assert_eq!(needs(&Cmd::Chain { key: String::new() }), Some((false, false)));
        assert_eq!(needs(&Cmd::Keys { cmd: None, key: Some(String::new()) }), Some((false, false)));
        let trust = KeysCmd::Trust { key: String::new(), signing_key: String::new(), yes: true };
        assert_eq!(needs(&Cmd::Keys { cmd: Some(trust), key: None }), Some((true, false)));
        // every command that writes is checked (and so, in the image, needs both folders mounted)
        assert_eq!(needs(&Cmd::Init), Some((true, true)));
        assert_eq!(needs(&Cmd::Restore), Some((true, true)));
        assert_eq!(needs(&Cmd::RecoveryKey { cmd: RecoveryCmd::New }), Some((true, true)));
        assert_eq!(
            needs(&Cmd::Token { cmd: TokenCmd::New { car: String::new(), keys: Vec::new() } }),
            Some((true, false))
        );
        assert_eq!(needs(&Cmd::Token { cmd: TokenCmd::Revoke { id: String::new() } }), Some((true, false)));
        assert_eq!(needs(&Cmd::Fingerprint), Some((true, false)));
        // the move: the old server writes in its data folder, a new one taking data in writes its own key file
        assert_eq!(needs(&Cmd::Export), Some((true, false)));
        assert_eq!(needs(&Cmd::Move { cmd: MoveCmd::Code { anywhere: false, replace: false } }), Some((true, false)));
        assert_eq!(needs(&Cmd::Move { cmd: MoveCmd::Undo }), Some((true, false)));
        assert_eq!(needs(&Cmd::Move { cmd: MoveCmd::Status }), Some((false, false)));
        assert_eq!(needs(&Cmd::Import { file: None, cancel: false }), Some((true, true)));
        let from = MoveCmd::From { address: String::new(), code: String::new() };
        assert_eq!(needs(&Cmd::Move { cmd: from }), Some((true, true)));
        assert_eq!(needs(&Cmd::Move { cmd: MoveCmd::Finish { recovery_key: true } }), Some((true, true)));
        assert_eq!(needs(&Cmd::Move { cmd: MoveCmd::Cancel }), Some((true, true)));
    }
}
