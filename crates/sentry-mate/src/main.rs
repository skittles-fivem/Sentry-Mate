//! `sentry-mate`: receives the Screenmate CAN Bridge's logs and (later) turns them into the car's history.
//!
//! Commands:
//!   serve                         run the upload server (TLS 1.3) until stopped
//!   token new --car NAME          make an upload token for a car (shown once)
//!   token list | token revoke ID
//!   fingerprint                   the certificate SHA-256 the app pins
//!   log candump FILE              a log of either format as candump text
//!   log check FILE [SIDECAR]      what its signature says
//!   chain KEY                     the chain report of a car key's logs
//!   health                        exit 0 when the upload port answers (for Docker)

use clap::{Parser, Subcommand};
use ingest::server::{Identity, serve_tls, tls_dir};
use ingest::{Judge, Store, Tokens, Uploads, router};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "sentry-mate",
    version,
    about = "A car logger fed by the Screenmate CAN Bridge's uploads"
)]
struct Cli {
    /// Where everything is kept (logs, tokens, certificate).
    #[arg(long, env = "SENTRY_MATE_DATA", default_value = "/data", global = true)]
    data: PathBuf,
    /// The CAN Bridge upload port (HTTPS).
    #[arg(
        long,
        env = "SENTRY_MATE_UPLOAD_PORT",
        default_value_t = 10100,
        global = true
    )]
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
    /// Upload tokens (one per car).
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
    /// The SHA-256 of the upload port's certificate, for the app's "certificate SHA-256" field.
    Fingerprint,
    /// Read or check one log file.
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
    Candump {
        file: PathBuf,
    },
    Check {
        file: PathBuf,
        sidecar: Option<PathBuf>,
    },
}

fn can_dir(data: &Path) -> PathBuf {
    data.join("can")
}

fn tokens_file(data: &Path) -> PathBuf {
    data.join("tokens.json")
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
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

async fn run(cli: Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let data = cli.data;
    match cli.cmd {
        Cmd::Serve { bind, names } => {
            serve(&data, &bind, cli.upload_port, &names).await?;
        }
        Cmd::Token { cmd } => {
            let tokens = Tokens::open(tokens_file(&data)).await?;
            match cmd {
                TokenCmd::New { car } => {
                    let (token, t) = tokens.create(&car).await?;
                    let fp = Identity::load_or_create(&tls_dir(&data), &[])?.fingerprint();
                    println!(
                        "Token for {} (id {}), shown once:\n\n  {token}\n",
                        t.car, t.id
                    );
                    println!("In the CAN Bridge app, Second Sentry Six Manager:");
                    println!(
                        "  address          https://<this server>:{}/api/can/logs",
                        cli.upload_port
                    );
                    println!("  token            the one above");
                    println!("  certificate SHA-256  {fp}");
                }
                TokenCmd::List => {
                    for t in tokens.list().await {
                        let state = if t.revoked { "revoked" } else { "in use" };
                        let key = t
                            .vehicle_key
                            .as_deref()
                            .unwrap_or("not tied to a car key yet");
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
        Cmd::Fingerprint => println!(
            "{}",
            Identity::load_or_create(&tls_dir(&data), &[])?.fingerprint()
        ),
        Cmd::Log { cmd } => return log_cmd(cmd),
        Cmd::Chain { key } => {
            let report = Judge::new(can_dir(&data)).chain(&key).await;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Cmd::Health => {
            let addr = format!("127.0.0.1:{}", cli.upload_port);
            let ok = tokio::time::timeout(
                Duration::from_secs(5),
                tokio::net::TcpStream::connect(&addr),
            )
            .await
            .is_ok_and(|r| r.is_ok());
            return Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
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
            eprintln!(
                "{}: {} lines, {} long lines skipped{}",
                s.format.name(),
                s.lines,
                s.skipped_long_lines,
                if s.truncated {
                    ", cut at the limit"
                } else {
                    ""
                }
            );
        }
        LogCmd::Check { file, sidecar } => {
            let bytes = std::fs::read(&file)?;
            let side_path =
                sidecar.unwrap_or_else(|| PathBuf::from(format!("{}.json", file.display())));
            let side: serde_json::Value = serde_json::from_slice(&std::fs::read(&side_path)?)?;
            let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let sha = hex::encode(Sha256::digest(&bytes));
            match trust::check(name, &sha, bytes.len() as u64, &side) {
                trust::Check::Signed(l) => println!(
                    "signed and intact: key {}, chain index {}",
                    l.key_fingerprint, l.index
                ),
                trust::Check::NotSigned { bridge_error } => {
                    println!(
                        "not signed{}",
                        bridge_error
                            .map(|e| format!(" (the bridge says: {e})"))
                            .unwrap_or_default()
                    );
                }
                trust::Check::Bad { reason, .. } => {
                    println!("failed: {reason}");
                    return Ok(ExitCode::FAILURE);
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn serve(
    data: &Path,
    bind: &str,
    port: u16,
    names: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(can_dir(data))?;
    let id = Identity::load_or_create(&tls_dir(data), names)?;
    let tokens = Tokens::open(tokens_file(data)).await?;
    let in_use = tokens.list().await.iter().filter(|t| !t.revoked).count();
    let up = Uploads::new(Store::new(can_dir(data)), tokens, Judge::new(can_dir(data)));
    let listener = tokio::net::TcpListener::bind((bind, port)).await?;
    tracing::info!(
        "CAN Bridge uploads on https://{bind}:{port}/api/can/logs (TLS 1.3), certificate SHA-256 {}",
        id.fingerprint()
    );
    if in_use == 0 {
        tracing::info!(
            "no upload token yet: make one with `sentry-mate token new --car \"My car\"`"
        );
    }

    // logs whose sidecar never came are judged without it; unfinished uploads are dropped after two days
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
                let dropped = housekeeping
                    .store
                    .clean_stale(ingest::store::STALE_PARTIAL_MS)
                    .await;
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
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(_) => return std::future::pending().await,
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
