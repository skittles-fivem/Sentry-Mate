//! `log decode`: a stored log's signals as CSV. Only a log whose check allows its data is decoded (unless `--any`),
//! with the signal set the car's VIN calls for (unless `--model`).
//!
//! The VIN is looked for in the log's own frames (0x405, or 0x508 on a Model S or X before 2021: signed with the log),
//! then in its sidecar (`vin` or `car.vin`, as Sentry Six Manager reads it: not signed), then in the car key's newest
//! logs whose data may be used. Without one, the Model 3/Y set.

use ingest::judge::{Record, Status};
use ingest::{Judge, Store};
use model::VinInfo;
use std::error::Error;
use std::io::Write;
use std::process::ExitCode;

/// How many of the car key's other logs are looked in for the VIN, newest first.
pub const OTHER_LOGS: usize = 10;

/// `--model`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Model {
    /// From the car's VIN, else Model 3/Y.
    Auto,
    /// Model 3 and Y, and Model S and X from 2021 (tesla-3y).
    #[value(name = "3y", alias = "tesla-3y")]
    ThreeY,
    /// Model S and X before 2021 (tesla-sx-legacy).
    #[value(name = "sx-legacy", alias = "tesla-sx-legacy")]
    SxLegacy,
}

/// What a log's check says, in the words the README uses, and whether its data may be used.
pub fn verdict(r: Option<&Record>) -> (bool, String) {
    let Some(r) = r else { return (false, "not checked yet".into()) };
    let word = match r.status {
        Status::Verified => "verified",
        Status::Unsigned => "unsigned",
        Status::CouldNotSign => "could not sign",
        Status::Failed => "failed",
        Status::Waiting => "waiting (its sidecar hasn't come)",
    };
    let mut said = match &r.reason {
        Some(why) => format!("{word} ({why})"),
        None => word.to_string(),
    };
    if r.override_use && !matches!(r.status, Status::Verified | Status::Unsigned) {
        said.push_str(", used as you said");
    }
    (r.usable(), said)
}

/// What a set is for.
pub fn set_name(p: dbc::Profile) -> &'static str {
    match p {
        dbc::Profile::Tesla3Y => "Model 3 and Y, and Model S and X from 2021",
        dbc::Profile::TeslaSxLegacy => "Model S and X before 2021",
    }
}

fn profile_of(v: &VinInfo) -> dbc::Profile {
    dbc::Profile::from_name(v.profile().dbc_name())
}

/// What a VIN says, without any of it: `Model Y of 2024`.
fn car_of(v: &VinInfo) -> String {
    let car = match (v.model_name(), v.year) {
        (Some(m), Some(y)) => format!("{m} of {y}"),
        (Some(m), None) => m.to_string(),
        (None, Some(y)) => format!("a car of {y}"),
        (None, None) => "a car it doesn't name".into(),
    };
    if v.via == 0x508 { format!("{car}, sent as a Model S or X before 2021 sends it") } else { car }
}

/// The VIN a sidecar names (`vin`, or `car.vin`).
pub fn sidecar_vin(side: &serde_json::Value) -> Option<VinInfo> {
    let text = side.get("vin").or_else(|| side.get("car").and_then(|c| c.get("vin")))?.as_str()?;
    timeline::decode_vin(text, 0)
}

/// The VIN in a log's own frames.
pub fn log_vin(log: &[u8]) -> Option<VinInfo> {
    let mut c = timeline::VinCollector::new();
    // a log that can't be read to its end still gives what came before
    let _ = logfmt::read_log(log, logfmt::Limits::default(), |l| {
        if c.vin().is_none()
            && let logfmt::Line::Frame(f) = logfmt::Line::parse(l)
        {
            c.add_frame(&f);
        }
    });
    c.vin().cloned()
}

fn sidecar_of(bytes: Option<&[u8]>) -> serde_json::Value {
    bytes.and_then(|b| serde_json::from_slice(b).ok()).unwrap_or_default()
}

/// The signal set for the log `name` of car key `key`, and why (for standard error).
pub async fn signal_set(
    model: Model,
    store: &Store,
    judge: &Judge,
    key: &str,
    name: &str,
    log: &[u8],
    side: &serde_json::Value,
) -> (dbc::Profile, String) {
    match model {
        Model::ThreeY => return (dbc::Profile::Tesla3Y, "chosen with --model".into()),
        Model::SxLegacy => return (dbc::Profile::TeslaSxLegacy, "chosen with --model".into()),
        Model::Auto => {}
    }
    if let Some(v) = log_vin(log) {
        return (profile_of(&v), format!("the VIN in the log says {}", car_of(&v)));
    }
    if let Some(v) = sidecar_vin(side) {
        return (profile_of(&v), format!("the VIN in its sidecar says {}", car_of(&v)));
    }
    let others = judge.latest_usable(key, OTHER_LOGS + 1).await.unwrap_or_default();
    for other in others.into_iter().filter(|o| o != name).take(OTHER_LOGS) {
        let found = match store.read(&other).await {
            Ok(Some(bytes)) => log_vin(&bytes),
            _ => None,
        };
        let found = match found {
            Some(v) => Some(v),
            None => {
                let side = store.read(&format!("{other}.json")).await.ok().flatten();
                sidecar_vin(&sidecar_of(side.as_deref().map(|b| &b[..])))
            }
        };
        if let Some(v) = found {
            return (profile_of(&v), format!("the VIN in {other}, a log of the same car key, says {}", car_of(&v)));
        }
    }
    let why = format!(
        "no VIN in the log, its sidecar or the car key's {OTHER_LOGS} newest logs, so the Model 3/Y set (--model \
         chooses another)"
    );
    (dbc::Profile::Tesla3Y, why)
}

/// `log decode NAME [--any] [--model ...]`: the CSV on `out`, what was checked and chosen on `err`.
pub async fn run(
    store: &Store,
    judge: &Judge,
    name: &str,
    any: bool,
    model: Model,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExitCode, Box<dyn Error>> {
    let Some(n) = trust::LogName::parse(name).filter(|n| !n.sidecar) else {
        writeln!(err, "{name} is not a log's name: a log is named <car key>_<UTC start>_<seq>.log.xz")?;
        return Ok(ExitCode::FAILURE);
    };
    let Some(log) = store.read(name).await? else {
        writeln!(err, "no stored log {name}")?;
        return Ok(ExitCode::FAILURE);
    };
    let (usable, said) = verdict(judge.record(n.vehicle_key, name).await.as_ref());
    if !usable && !any {
        writeln!(err, "{name} is not decoded: its check says {said}. Add --any to decode it anyway.")?;
        return Ok(ExitCode::FAILURE);
    }
    writeln!(err, "check: {said}{}", if usable { "" } else { ", decoded anyway (--any)" })?;
    let side = sidecar_of(store.read(&format!("{name}.json")).await?.as_deref().map(|b| &b[..]));
    let (profile, why) = signal_set(model, store, judge, n.vehicle_key, name, &log, &side).await;
    writeln!(err, "signal set {} ({}): {why}", profile.name(), set_name(profile))?;
    csv(&log, &side, profile, out)?;
    Ok(ExitCode::SUCCESS)
}

/// Decoded signals as CSV, a row when a value changes.
pub fn csv(
    log: &[u8],
    side: &serde_json::Value,
    profile: dbc::Profile,
    out: &mut dyn Write,
) -> Result<(), Box<dyn Error>> {
    let mut d = dbc::Decoder::new(profile, side);
    let mut last: std::collections::HashMap<&'static str, f64> = std::collections::HashMap::new();
    let mut out = std::io::BufWriter::new(out);
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::testkit::{self, CAR, VIN_S, VIN_Y, genesis, kit, log_name, new_key, text};

    async fn decoded(k: &testkit::Kit, name: &str, any: bool, model: Model) -> (ExitCode, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(&k.store, &k.judge, name, any, model, &mut out, &mut err).await.unwrap();
        (code, text(&out), text(&err))
    }

    #[tokio::test]
    async fn only_a_log_its_check_allows_is_decoded_unless_any() {
        let k = kit();
        let (first, second) = (new_key(), new_key());
        let (a, prev) = testkit::chain(&k, &first, 0, 1, 0, genesis(), Some((VIN_Y, 0x405))).await;
        let (code, out, err) = decoded(&k, &a[0], false, Model::Auto).await;
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(err.starts_with("check: verified\n"), "{err}");
        assert!(err.contains("signal set tesla-3y (Model 3 and Y, and Model S and X from 2021): the VIN in the log says Model Y of 2024"), "{err}");
        assert!(out.starts_with("timestamp_utc,signal,value,unit,value_name\n"), "{out}");
        assert!(out.contains(",DI_vehicleSpeed,8,kph,"), "{out}");

        // signed by another key: refused, and why
        let (b, _) = testkit::chain(&k, &second, 1, 1, 1, prev, Some((VIN_Y, 0x405))).await;
        let (code, out, err) = decoded(&k, &b[0], false, Model::Auto).await;
        assert_eq!((code, out.as_str()), (ExitCode::FAILURE, ""));
        assert_eq!(
            err,
            format!(
                "{} is not decoded: its check says failed (signed by another key). Add --any to decode it anyway.\n",
                b[0]
            )
        );
        let (code, out, err) = decoded(&k, &b[0], true, Model::Auto).await;
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(err.starts_with("check: failed (signed by another key), decoded anyway (--any)\n"), "{err}");
        assert!(out.contains("DI_vehicleSpeed"), "{out}");

        // waiting for its sidecar: not yet
        let c = log_name(5);
        testkit::upload(&k, &c, &testkit::log(None)).await;
        let (code, _, err) = decoded(&k, &c, false, Model::Auto).await;
        assert_eq!(code, ExitCode::FAILURE);
        assert!(err.contains("its check says waiting (its sidecar hasn't come)"), "{err}");

        // not a log's name, or not stored
        let (code, _, err) = decoded(&k, &format!("{}.json", a[0]), true, Model::Auto).await;
        assert_eq!(code, ExitCode::FAILURE);
        assert!(err.contains("is not a log's name"), "{err}");
        let (code, _, err) = decoded(&k, &log_name(40), true, Model::Auto).await;
        assert_eq!((code, err), (ExitCode::FAILURE, format!("no stored log {}\n", log_name(40))));
    }

    #[tokio::test]
    async fn the_signal_set_follows_the_vin_unless_chosen() {
        let k = kit();
        let key = new_key();
        // a Model S from before 2021 sends its VIN in 0x508: the legacy set, which has no Model 3/Y speed on can0
        let (a, prev) = testkit::chain(&k, &key, 0, 1, 0, genesis(), Some((VIN_S, 0x508))).await;
        let (code, out, err) = decoded(&k, &a[0], false, Model::Auto).await;
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(
            err.contains(
                "signal set tesla-sx-legacy (Model S and X before 2021): the VIN in the log says Model S of 2023"
            ),
            "{err}"
        );
        assert!(!out.contains("DI_vehicleSpeed"), "{out}");
        let (_, out, err) = decoded(&k, &a[0], false, Model::ThreeY).await;
        assert!(
            err.contains("signal set tesla-3y (Model 3 and Y, and Model S and X from 2021): chosen with --model"),
            "{err}"
        );
        assert!(out.contains("DI_vehicleSpeed"), "{out}");

        // a log without the VIN: from the car key's newest log that has it
        let (b, prev) = testkit::chain(&k, &key, 1, 1, 1, prev, None).await;
        let (_, _, err) = decoded(&k, &b[0], false, Model::Auto).await;
        assert!(err.contains(&format!("signal set tesla-sx-legacy (Model S and X before 2021): the VIN in {}, a log of the same car key, says Model S of 2023", a[0])), "{err}");
        let (_, _, err) = decoded(&k, &b[0], false, Model::SxLegacy).await;
        assert!(err.contains("signal set tesla-sx-legacy (Model S and X before 2021): chosen with --model"), "{err}");

        // the sidecar may name it (not signed: after the log's own frames)
        let c = log_name(2);
        let bytes = testkit::log(None);
        let mut side = testkit::signed(&key, &c, &bytes, 2, (&prev.0, &prev.1));
        side["vin"] = serde_json::Value::from(VIN_Y);
        testkit::upload(&k, &c, &bytes).await;
        testkit::upload(&k, &format!("{c}.json"), &serde_json::to_vec(&side).unwrap()).await;
        let (_, _, err) = decoded(&k, &c, false, Model::Auto).await;
        assert!(err.contains("signal set tesla-3y (Model 3 and Y, and Model S and X from 2021): the VIN in its sidecar says Model Y of 2024"), "{err}");
    }

    #[tokio::test]
    async fn without_a_vin_anywhere_it_is_the_model_3_y_set() {
        let k = kit();
        let (a, _) = testkit::chain(&k, &new_key(), 0, 2, 0, genesis(), None).await;
        let (code, out, err) = decoded(&k, &a[1], false, Model::Auto).await;
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(err.contains("signal set tesla-3y (Model 3 and Y, and Model S and X from 2021): no VIN in the log, its sidecar or the car key's 10 newest logs, so the Model 3/Y set (--model chooses another)"), "{err}");
        assert!(out.contains("DI_vehicleSpeed"));
        assert_eq!(k.judge.record(CAR, &a[1]).await.unwrap().status, ingest::judge::Status::Verified);
    }

    #[test]
    fn a_vin_in_the_sidecar_is_read_where_sentry_six_manager_reads_it() {
        let s = serde_json::json!({ "car": { "vin": VIN_S } });
        assert_eq!(sidecar_vin(&s).unwrap().profile(), model::CarProfile::TeslaSxLegacy);
        assert!(sidecar_vin(&serde_json::json!({ "vin": "not a vin" })).is_none());
        assert!(sidecar_vin(&serde_json::json!({ "vin": 5 })).is_none());
        assert_eq!(verdict(None), (false, "not checked yet".to_string()));
    }
}
