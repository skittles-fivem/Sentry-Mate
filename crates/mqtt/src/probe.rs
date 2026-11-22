//! One test of the saved settings, for Settings → MQTT's "Test" button: connect with exactly what publishing would
//! use (the same checks, the same TLS trust), publish one message (not retained) on `<prefix>/test`, wait for the
//! broker to acknowledge it, and disconnect. Its own client id (`<client id>-test`), so a running publisher keeps its
//! connection, and no last will, so the broker never says `offline` for the test.

use crate::render::iso_utc;
use crate::security;
use crate::settings::Settings;
use rumqttc::{AsyncClient, ConnectReturnCode, ConnectionError, MqttOptions, NetworkOptions, Packet, QoS, Transport};
use std::time::{Duration, Instant};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// How a test went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub ok: bool,
    /// `no_host`, `invalid_settings`, `connection_refused`, `auth_failed`, `tls_failed`, `pin_mismatch` or `timeout`.
    pub error: Option<&'static str>,
    /// In words, for the page.
    pub message: String,
    /// From the start of the connection to the broker's acknowledgement of the test message.
    pub latency_ms: Option<u64>,
    /// The topic the test message went to.
    pub topic: Option<String>,
    pub tls: bool,
}

fn failed(error: &'static str, message: impl Into<String>, tls: bool) -> Probe {
    Probe { ok: false, error: Some(error), message: message.into(), latency_ms: None, topic: None, tls }
}

/// Which of the page's error codes a failed connection is (the publisher's `last_error_code` too).
pub(crate) fn classify(e: &ConnectionError) -> &'static str {
    use rumqttc::TlsError;
    let tls = match e {
        ConnectionError::Tls(TlsError::TLS(t)) => Some(Some(t)),
        ConnectionError::Tls(TlsError::Io(io)) => Some(io.get_ref().and_then(|r| r.downcast_ref::<rustls::Error>())),
        ConnectionError::Tls(_) => Some(None),
        _ => None,
    };
    match tls {
        Some(Some(rustls::Error::InvalidCertificate(rustls::CertificateError::Other(o))))
            if o.0.downcast_ref::<security::PinMismatch>().is_some() =>
        {
            "pin_mismatch"
        }
        Some(_) => "tls_failed",
        None => match e {
            ConnectionError::ConnectionRefused(
                ConnectReturnCode::BadUserNamePassword | ConnectReturnCode::NotAuthorized,
            ) => "auth_failed",
            ConnectionError::NetworkTimeout | ConnectionError::FlushTimeout => "timeout",
            _ => "connection_refused",
        },
    }
}

/// The connection options for a one-off connection with `settings` (client id `<client id><suffix>`, no last
/// will), with the broker as the page shows it and whether it uses TLS; else the error code, in words, and TLS.
pub(crate) fn options(
    settings: &Settings,
    suffix: &str,
) -> Result<(MqttOptions, crate::settings::Broker, bool), (&'static str, String, bool)> {
    let checked = settings.checked().map_err(|e| ("invalid_settings", e.to_string(), true))?;
    let broker = checked.broker.clone();
    let tls = broker.tls;
    let mut options = MqttOptions::new(format!("{}{suffix}", settings.client_id), broker.host.clone(), broker.port);
    options.set_keep_alive(Duration::from_secs(30)).set_clean_session(true);
    if let Some(user) = settings.username.as_deref().filter(|u| !u.is_empty()) {
        options.set_credentials(user, settings.password.clone().unwrap_or_default());
    }
    if tls {
        let c = security::client_config(checked.pin, settings.allow_tls12)
            .map_err(|e| ("tls_failed", e.to_string(), tls))?;
        options.set_transport(Transport::tls_with_config(rumqttc::TlsConfiguration::Rustls(c)));
    }
    Ok((options, broker, tls))
}

/// Where the test message goes: `<prefix>/test`, beside the connection's own `<prefix>/status`.
fn test_topic(prefix: &str) -> String {
    format!("{prefix}/test")
}

/// Tests `settings` (whether or not publishing is on). Never takes longer than the connect timeout plus a few seconds.
pub async fn probe(settings: &Settings) -> Probe {
    if settings.broker.trim().is_empty() {
        return failed("no_host", "Set a broker host first.", true);
    }
    let (options, broker, tls) = match options(settings, "-test") {
        Ok(o) => o,
        Err((code, why, tls)) => return failed(code, why, tls),
    };
    let topic = test_topic(&settings.prefix);
    let (client, mut eventloop) = AsyncClient::new(options, 8);
    let mut net = NetworkOptions::new();
    let connect_s = u64::from(settings.limits.connect_timeout_s.max(1));
    net.set_connection_timeout(connect_s);
    eventloop.set_network_options(net);
    let started = Instant::now();
    let at = Duration::from_secs(connect_s + 5);
    let payload = format!("Sentry Mate test message, {}", iso_utc(now_ms()));
    let run = async {
        let mut sent = false;
        loop {
            match eventloop.poll().await {
                Ok(rumqttc::Event::Incoming(Packet::ConnAck(_))) if !sent => {
                    sent = true;
                    if client.try_publish(topic.clone(), QoS::AtLeastOnce, false, payload.clone()).is_err() {
                        return Err(failed("connection_refused", "The test message could not be queued.", tls));
                    }
                }
                Ok(rumqttc::Event::Incoming(Packet::PubAck(_))) => {
                    let latency = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    let _ = client.try_disconnect();
                    // let the DISCONNECT go out (bounded: the caller's timeout covers this too)
                    let _ = tokio::time::timeout(Duration::from_secs(1), eventloop.poll()).await;
                    return Ok(latency);
                }
                Ok(_) => {}
                Err(e) => {
                    let code = classify(&e);
                    let why = crate::worker::explain(&e);
                    return Err(failed(code, format!("{broker}: {why}"), tls));
                }
            }
        }
    };
    match tokio::time::timeout(at, run).await {
        Ok(Ok(latency)) => Probe {
            ok: true,
            error: None,
            message: format!("Connected to {broker} and published one test message."),
            latency_ms: Some(latency),
            topic: Some(topic),
            tls,
        },
        Ok(Err(p)) => p,
        Err(_) => failed("timeout", format!("{broker} did not answer within {} s.", at.as_secs()), tls),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_test_topic_is_the_prefix_and_test() {
        assert_eq!(test_topic("sentry-mate"), "sentry-mate/test");
        // a level named status inside the prefix stays as it is
        assert_eq!(test_topic("a/status/b"), "a/status/b/test");
    }

    #[tokio::test]
    async fn no_host_and_bad_settings_never_connect() {
        let p = probe(&Settings::default()).await;
        assert_eq!(p.error, Some("no_host"));
        let s = Settings { broker: "mqtt://broker.lan".into(), ..Settings::default() };
        assert_eq!(probe(&s).await.error, Some("invalid_settings"), "plain MQTT not allowed");
    }

    #[tokio::test]
    async fn a_closed_port_is_refused() {
        // a port nothing listens on (bound, then closed)
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let s = Settings { broker: format!("mqtt://127.0.0.1:{port}"), allow_unencrypted: true, ..Settings::default() };
        let p = probe(&s).await;
        assert!(!p.ok);
        assert_eq!(p.error, Some("connection_refused"), "{p:?}");
        assert!(!p.tls);
    }
}
