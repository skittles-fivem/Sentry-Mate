//! Clearing what an earlier layout left on the broker, after a saved change of the settings: Home Assistant discovery
//! turned off, or its prefix or the topic prefix changed. Everything the publisher sends is retained, so without this
//! the broker would keep the old discovery configs (Home Assistant keeps the old devices) and the old values.
//!
//! One connection of its own with the earlier settings (client id `<client id>-cleanup`, no last will), empty retained
//! messages on the old topics, every one acknowledged, then a clean disconnect. Bounded in time: the connect timeout
//! plus a few seconds per thousand messages.

use crate::discovery;
use crate::error::Error;
use crate::render::{self, Layout, Message};
use crate::settings::Settings;
use model::CarId;
use rumqttc::{AsyncClient, NetworkOptions, Packet, QoS};
use std::time::Duration;

/// What an earlier layout left that the new one no longer covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Leftovers {
    /// The values (and the connection's status) under the old topic prefix.
    pub values: bool,
    /// The Home Assistant discovery configs under the old prefixes.
    pub discovery: bool,
}

/// What publishing with `old` left that `new` does not take over (`None`: nothing). Turning publishing off leaves
/// everything (Home Assistant shows the cars as unavailable); so does a change of broker.
pub fn leftovers(old: &Settings, new: &Settings) -> Option<Leftovers> {
    if !old.enabled || !new.enabled || old.broker != new.broker {
        return None;
    }
    let values = old.prefix != new.prefix;
    // the device ids come from the topic prefix, so a new one makes new devices
    let discovery = old.discovery && (!new.discovery || old.discovery_prefix != new.discovery_prefix || values);
    (values || discovery).then_some(Leftovers { values, discovery })
}

/// The empty retained messages that clear `what` of `cars` under `old`'s layout.
pub fn messages(old: &Settings, cars: &[CarId], what: Leftovers) -> Result<Vec<Message>, Error> {
    let layout = Layout::new(old)?;
    let mut out = Vec::new();
    for &car in cars {
        if what.discovery {
            out.extend(discovery::removals(&layout, car));
        }
        if what.values {
            out.extend(render::clear_messages(&layout, car));
        }
    }
    if what.values {
        out.push(Message { topic: layout.topics.status(), payload: String::new(), retain: true });
    }
    Ok(out)
}

/// Sends `msgs` with the connection settings of `old` and waits until the broker has acknowledged every one. The
/// number sent, or why it failed (a code as [`crate::probe::Probe::error`] gives them, and words).
pub async fn clear(old: &Settings, msgs: Vec<Message>) -> Result<usize, (&'static str, String)> {
    let total = msgs.len();
    if total == 0 {
        return Ok(0);
    }
    let (options, broker, _) = crate::probe::options(old, "-cleanup").map_err(|(code, why, _)| (code, why))?;
    let (client, mut eventloop) = AsyncClient::new(options, 64);
    let mut net = NetworkOptions::new();
    let connect_s = u64::from(old.limits.connect_timeout_s.max(1));
    net.set_connection_timeout(connect_s);
    eventloop.set_network_options(net);
    let within = Duration::from_secs(connect_s + 5 + 5 * (total as u64).div_ceil(1000));
    let mut sender: Option<tokio::task::JoinHandle<()>> = None;
    let mut waiting = Some(msgs);
    let run = async {
        let mut acked = 0usize;
        loop {
            match eventloop.poll().await {
                Ok(rumqttc::Event::Incoming(Packet::ConnAck(_))) => {
                    if let Some(msgs) = waiting.take() {
                        let c = client.clone();
                        sender = Some(tokio::spawn(async move {
                            for m in msgs {
                                if c.publish(m.topic, QoS::AtLeastOnce, m.retain, m.payload).await.is_err() {
                                    return;
                                }
                            }
                        }));
                    }
                }
                Ok(rumqttc::Event::Incoming(Packet::PubAck(_))) => {
                    acked += 1;
                    if acked >= total {
                        let _ = client.try_disconnect();
                        let _ = tokio::time::timeout(Duration::from_secs(1), eventloop.poll()).await;
                        return Ok(acked);
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    return Err((crate::probe::classify(&e), format!("{broker}: {}", crate::worker::explain(&e))));
                }
            }
        }
    };
    let r = match tokio::time::timeout(within, run).await {
        Ok(r) => r,
        Err(_) => Err(("timeout", format!("{broker} did not take the clean-up within {} s.", within.as_secs()))),
    };
    if let Some(s) = sender {
        s.abort();
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> Settings {
        Settings { enabled: true, broker: "mqtts://broker.lan".into(), discovery: true, ..Settings::default() }
    }

    #[test]
    fn what_a_change_leaves() {
        let old = on();
        assert_eq!(leftovers(&old, &old), None, "nothing changed");
        let off = Settings { discovery: false, ..on() };
        assert_eq!(leftovers(&old, &off), Some(Leftovers { values: false, discovery: true }));
        assert_eq!(leftovers(&off, &old), None, "turning discovery on leaves nothing");
        let renamed = Settings { prefix: "cars2".into(), ..on() };
        assert_eq!(leftovers(&old, &renamed), Some(Leftovers { values: true, discovery: true }));
        assert_eq!(
            leftovers(&off, &Settings { prefix: "cars2".into(), ..off.clone() }),
            Some(Leftovers { values: true, discovery: false })
        );
        let ha = Settings { discovery_prefix: "ha".into(), ..on() };
        assert_eq!(leftovers(&old, &ha), Some(Leftovers { values: false, discovery: true }));
        assert_eq!(leftovers(&old, &Settings { enabled: false, discovery: false, ..on() }), None, "publishing off");
        assert_eq!(leftovers(&Settings { enabled: false, ..on() }, &off), None, "nothing was published");
    }

    #[test]
    fn the_messages_are_empty_and_retained_on_the_old_topics() {
        let old = on();
        let m = messages(&old, &[1, 2], Leftovers { values: true, discovery: true }).unwrap();
        assert!(m.iter().all(|m| m.retain && m.payload.is_empty()));
        assert!(m.iter().any(|m| m.topic == "homeassistant/sensor/sentry-mate_c2/soc/config"));
        assert!(m.iter().any(|m| m.topic == "sentry-mate/cars/c1/soc"));
        assert_eq!(m.last().map(|m| m.topic.as_str()), Some("sentry-mate/status"));
        let only = messages(&old, &[1], Leftovers { values: false, discovery: true }).unwrap();
        assert!(only.iter().all(|m| m.topic.starts_with("homeassistant/")));
    }
}
