//! `keys` and `keys trust`: the signing keys a car key's logs name, and the owner trusting one of them.
//!
//! A key is shown, and taken, by its fingerprint as the CAN Bridge app shows it under "Log signing key" (the same id
//! the chain report names). Only a key that signed at least one of the car key's logs can be trusted, and only after
//! the owner says yes (or `--yes`): someone with the car's upload token could upload logs signed by a key of their own.

use ingest::Judge;
use ingest::judge::{Signer, Signers, Trusted};
use std::error::Error;
use std::fmt::Write as _;
use std::io::{BufRead, Write};
use std::process::ExitCode;

/// A time as the owner reads it: `2026-11-08 14:31 UTC`.
pub fn utc(ms: u64) -> String {
    let secs = ms / 1000;
    let (y, m, d) = trust::name::civil_from_days((secs / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", secs % 86_400 / 3600, secs % 3600 / 60)
}

/// A car key with no folder here: never uploaded, or mistyped.
pub fn unknown(key: &str, err: &mut dyn Write) -> std::io::Result<ExitCode> {
    writeln!(err, "no car key {key} here: no log has come from it. The car keys are the last column of `token list`.")?;
    Ok(ExitCode::FAILURE)
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn is_pin(s: &Signers, k: &Signer) -> bool {
    s.pin.as_ref().is_some_and(|p| trust::same_fingerprint(&p.fingerprint, &k.fingerprint))
}

fn state(s: &Signers, k: &Signer) -> String {
    if let Some(p) = s.pin.as_ref().filter(|_| is_pin(s, k)) {
        let how = if p.how == "owner" { "you trusted it" } else { "its first signed log paired the car key" };
        return format!("trusted, since {} ({how})", utc(p.pinned_ms));
    }
    if let Some(f) = s.former.iter().find(|f| trust::same_fingerprint(&f.fingerprint, &k.fingerprint)) {
        return format!("trusted until {}: its logs that came before then still count", utc(f.until_ms));
    }
    if k.signed == 0 {
        return "not trusted, and it can't be: none of its logs passed the check".into();
    }
    "not trusted: its logs are kept, not used".into()
}

/// The listing `keys` prints.
pub fn render(key: &str, s: &Signers) -> String {
    if s.keys.is_empty() {
        return format!("No log of car key {key} is signed yet.\n");
    }
    let mut t = String::new();
    let n = plural(s.keys.len() as u64, "signing key", "signing keys");
    let _ = writeln!(t, "Car key {key}: {n}. Compare them with \"Log signing key\" in the CAN Bridge app.");
    for k in &s.keys {
        let _ = writeln!(t);
        let _ = writeln!(t, "  {}", k.fingerprint);
        let _ = writeln!(t, "    {}", state(s, k));
        let mut logs = format!("{} signed", plural(k.signed, "log", "logs"));
        if k.failed > 0 {
            let _ = write!(logs, ", {} more that failed the check", k.failed);
        }
        let _ = writeln!(t, "    {logs}; first seen {}, last seen {}", utc(k.first_seen_ms), utc(k.last_seen_ms));
        if !is_pin(s, k) && k.signed > 0 {
            let _ = writeln!(t, "    To trust it: keys trust {key} {}", k.fingerprint);
        }
    }
    t
}

/// `keys <car key>`.
pub async fn list(
    judge: &Judge,
    key: &str,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExitCode, Box<dyn Error>> {
    if !judge.known(key) {
        return Ok(unknown(key, err)?);
    }
    let s = judge.signers(key).await?;
    out.write_all(render(key, &s).as_bytes())?;
    Ok(ExitCode::SUCCESS)
}

/// `keys trust <car key> <signing key>`: asks on `err`, reads the answer from `input` (unless `yes`).
pub async fn trust(
    judge: &Judge,
    key: &str,
    id: &str,
    yes: bool,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExitCode, Box<dyn Error>> {
    if !judge.known(key) {
        return Ok(unknown(key, err)?);
    }
    let s = judge.signers(key).await?;
    let Some(k) = s.keys.iter().find(|k| k.signed > 0 && trust::same_fingerprint(&k.fingerprint, id)) else {
        writeln!(err, "no log of car key {key} is signed by the key {id}. `keys {key}` lists its keys.")?;
        return Ok(ExitCode::FAILURE);
    };
    if is_pin(&s, k) {
        writeln!(out, "{} is the trusted key of car key {key} already.", k.fingerprint)?;
        return Ok(ExitCode::SUCCESS);
    }
    if !yes {
        writeln!(err, "Trust the signing key {} for car key {key}?", k.fingerprint)?;
        writeln!(
            err,
            "Check first that the CAN Bridge app shows this key under \"Log signing key\". If it doesn't, these logs \
             are not from your Screenmate: don't trust it."
        )?;
        writeln!(
            err,
            "Once it is trusted, its {} used, and so are its logs after that.",
            plural(k.signed, "log is", "logs are")
        )?;
        if let Some(p) = &s.pin {
            writeln!(
                err,
                "The key trusted now ({}) stays trusted for the logs that came before. Its later logs will fail the check.",
                p.fingerprint
            )?;
        }
        write!(err, "Type yes to trust it: ")?;
        err.flush()?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0 {
            writeln!(err)?;
            writeln!(err, "Not trusted: no answer came. Run it with docker exec -it, or add --yes.")?;
            return Ok(ExitCode::FAILURE);
        }
        if !answer.trim().eq_ignore_ascii_case("yes") {
            writeln!(err, "Not trusted. Nothing changed.")?;
            return Ok(ExitCode::FAILURE);
        }
    }
    match judge.trust_key(key, &k.fingerprint).await? {
        Trusted::Now { fingerprint, verified, before } => {
            writeln!(
                out,
                "Trusted {fingerprint} for car key {key}: {} verified now.",
                plural(verified, "log", "logs")
            )?;
            if let Some(b) = before {
                writeln!(out, "{b} stays trusted for its logs that came before now.")?;
            }
            Ok(ExitCode::SUCCESS)
        }
        Trusted::Already => {
            writeln!(out, "{} is the trusted key of car key {key} already.", k.fingerprint)?;
            Ok(ExitCode::SUCCESS)
        }
        Trusted::NotSeen => {
            writeln!(err, "no log of car key {key} is signed by the key {id}. `keys {key}` lists its keys.")?;
            Ok(ExitCode::FAILURE)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::testkit::{self, CAR, fp, genesis, kit, new_key, text};

    async fn listed(judge: &Judge, key: &str) -> (ExitCode, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = list(judge, key, &mut out, &mut err).await.unwrap();
        (code, text(&out), text(&err))
    }

    async fn trusted(judge: &Judge, id: &str, yes: bool, answer: &str) -> (ExitCode, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = trust(judge, CAR, id, yes, &mut answer.as_bytes(), &mut out, &mut err).await.unwrap();
        (code, text(&out), text(&err))
    }

    #[test]
    fn times_are_shown_in_utc_to_the_minute() {
        assert_eq!(utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc(1_794_148_259_999), "2026-11-08 14:30 UTC");
    }

    #[tokio::test]
    async fn keys_are_listed_and_another_is_trusted_only_after_a_yes() {
        let k = kit();
        let (first, second) = (new_key(), new_key());
        let (a, _) = testkit::chain(&k, &first, 0, 2, 0, genesis(), None).await;
        // the Screenmate's keystore made a new key: a new chain from it
        let (b, _) = testkit::chain(&k, &second, 2, 3, 0, genesis(), None).await;
        let (code, out, err) = listed(&k.judge, CAR).await;
        assert_eq!((code, err.as_str()), (ExitCode::SUCCESS, ""));
        assert!(out.starts_with(&format!("Car key {CAR}: 2 signing keys.")), "{out}");
        let at = |fp: &str| out.find(fp).unwrap();
        assert!(at(&fp(&first)) < at(&fp(&second)), "in the order first seen:\n{out}");
        let (one, two) = out.split_at(at(&fp(&second)));
        assert!(one.contains("trusted, since") && one.contains("its first signed log paired the car key"), "{one}");
        assert!(one.contains("2 logs signed; first seen 20") && !one.contains("To trust it"), "{one}");
        assert!(two.contains("not trusted: its logs are kept, not used") && two.contains("3 logs signed"), "{two}");
        assert!(two.contains(&format!("To trust it: keys trust {CAR} {}", fp(&second))), "{two}");
        // the id is the one the chain report names
        assert_eq!(k.judge.chain(CAR).await.signing_key, Some(fp(&first)));

        // asked first: anything but yes, or no answer at all, changes nothing
        let (code, out, err) = trusted(&k.judge, &fp(&second), false, "no\n").await;
        assert_eq!((code, out.as_str()), (ExitCode::FAILURE, ""));
        assert!(err.starts_with(&format!("Trust the signing key {} for car key {CAR}?", fp(&second))), "{err}");
        assert!(err.contains("\"Log signing key\"") && err.contains("its 3 logs are used"), "{err}");
        assert!(
            err.contains(&format!("The key trusted now ({})", fp(&first))) && err.contains("Not trusted."),
            "{err}"
        );
        let (code, _, err) = trusted(&k.judge, &fp(&second), false, "").await;
        assert_eq!(code, ExitCode::FAILURE);
        assert!(err.contains("no answer came") && err.contains("--yes"), "{err}");
        assert_eq!(k.judge.summary(CAR).await.unwrap().pin.unwrap().fingerprint, fp(&first));
        for name in &b {
            assert!(!k.judge.record(CAR, name).await.unwrap().usable(), "{name}");
        }

        let (code, out, _) = trusted(&k.judge, &fp(&second), false, "YES\n").await;
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(out.starts_with(&format!("Trusted {} for car key {CAR}: 3 logs verified now.", fp(&second))), "{out}");
        assert!(out.contains(&format!("{} stays trusted for its logs that came before now.", fp(&first))), "{out}");
        for name in a.iter().chain(&b) {
            assert!(k.judge.record(CAR, name).await.unwrap().usable(), "{name}");
        }
        assert_eq!(k.judge.chain(CAR).await.signing_key, Some(fp(&second)));
        let (_, out, _) = listed(&k.judge, CAR).await;
        let (one, two) = out.split_at(out.find(&fp(&second)).unwrap());
        assert!(one.contains("trusted until") && one.contains("its logs that came before then still count"), "{one}");
        assert!(one.contains(&format!("To trust it: keys trust {CAR} {}", fp(&first))), "{one}");
        assert!(two.contains("trusted, since") && two.contains("(you trusted it)"), "{two}");

        // --yes asks nothing; the key trusted already is said so; a key that signed nothing here can't be trusted
        let (code, _, err) = trusted(&k.judge, &fp(&first).replace(':', ""), true, "").await;
        assert_eq!((code, err.as_str()), (ExitCode::SUCCESS, ""));
        let (code, out, _) = trusted(&k.judge, &fp(&first), true, "").await;
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(out.contains("is the trusted key of car key abcdef123456 already"), "{out}");
        let (code, _, err) = trusted(&k.judge, &fp(&new_key()), true, "").await;
        assert_eq!(code, ExitCode::FAILURE);
        assert!(err.contains("is signed by the key") && err.contains(&format!("`keys {CAR}` lists")), "{err}");
    }

    #[tokio::test]
    async fn an_unknown_car_key_is_said_so() {
        let k = kit();
        let (code, out, err) = listed(&k.judge, "fedcba654321").await;
        assert_eq!((code, out.as_str()), (ExitCode::FAILURE, ""));
        assert!(err.starts_with("no car key fedcba654321 here") && err.contains("token list"), "{err}");
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = trust(&k.judge, "../x", "aa", true, &mut &b""[..], &mut out, &mut err).await.unwrap();
        assert_eq!(code, ExitCode::FAILURE);
        assert!(text(&err).starts_with("no car key ../x here"));
        assert!(!k.data.join("can").exists(), "nothing made for it");
        // a car key whose logs are all unsigned has no keys to list
        testkit::upload(&k, &testkit::log_name(0), &testkit::log(None)).await;
        testkit::upload(&k, &format!("{}.json", testkit::log_name(0)), b"{}").await;
        let (code, out, _) = listed(&k.judge, CAR).await;
        assert_eq!((code, out.as_str()), (ExitCode::SUCCESS, "No log of car key abcdef123456 is signed yet.\n"));
    }
}
