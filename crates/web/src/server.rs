//! Serving the web side: TLS 1.3 only (the rustls settings come from the server's certificate, the same one the
//! upload port uses: `ingest::server::Identity::tls_config`), HTTP/1.1, with connection guards: a TLS handshake
//! within 15 s, headers within 20 s, a JSON body within [`BODY_TIMEOUT`] (`408` and the connection closed: see
//! [`crate::ctx::read_body`]), a request answered within 10 minutes (a stream, once started, runs on), idle
//! connections closed after the keep-alive.
//!
//! Connections are counted per address, an IPv4 address or an IPv6 /64 ([`crate::policy::address_key`]): at most
//! [`MAX_PER_ADDRESS`] each and [`MAX_CONNECTIONS`] in all. A connection counts as anonymous until a request on it
//! carries a valid session; at most [`MAX_ANONYMOUS`] are, and a new one beyond closes the oldest anonymous one, so
//! connections nobody signed in on can't fill the port and keep signed-in people out.

use crate::error::ApiError;
use axum::Router;
use axum::response::IntoResponse;
use hyper_util::rt::{TokioIo, TokioTimer};
use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tower::ServiceExt;

/// A browser opens about six connections per site, plus the live stream, per tab.
pub const MAX_PER_ADDRESS: usize = 32;
pub const MAX_CONNECTIONS: usize = 1024;
/// Connections no request has signed in on yet, at most (the rest of [`MAX_CONNECTIONS`] stays for signed-in
/// people).
pub const MAX_ANONYMOUS: usize = MAX_CONNECTIONS * 3 / 4;
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
pub const HEADER_TIMEOUT: Duration = Duration::from_secs(20);
/// A JSON request body arrives within this ([`crate::Builder::body_timeout`] changes it).
pub const BODY_TIMEOUT: Duration = Duration::from_secs(30);
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Where a request came from, set on every request by [`serve`]. A request without it (not from [`serve`]) is
/// refused by the guard: the API answers over TLS only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Conn {
    pub ip: IpAddr,
    pub tls: bool,
}

#[derive(Default)]
struct Slots {
    per: HashMap<IpAddr, usize>,
    total: usize,
    /// Anonymous connections, oldest first, with what closes each.
    anonymous: BTreeMap<u64, Arc<Notify>>,
    next: u64,
}

struct Slot {
    slots: Arc<Mutex<Slots>>,
    key: IpAddr,
    seq: u64,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut s = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        s.total = s.total.saturating_sub(1);
        s.anonymous.remove(&self.seq);
        if let Some(n) = s.per.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                s.per.remove(&self.key);
            }
        }
    }
}

/// The connection a request came on (a request extension, set by [`serve`]).
#[derive(Clone)]
pub struct ConnSlot(Arc<Slot>);

impl ConnSlot {
    /// A request on this connection carried a valid session: it no longer counts as anonymous (and is never closed
    /// to make room for another).
    pub fn signed_in(&self) {
        let mut s = self.0.slots.lock().unwrap_or_else(|e| e.into_inner());
        s.anonymous.remove(&self.0.seq);
    }
}

/// A slot for a new connection from `ip`, and what tells it to close (to make room), or `None`: refused.
fn take_slot(slots: &Arc<Mutex<Slots>>, ip: IpAddr) -> Option<(ConnSlot, Arc<Notify>)> {
    let key = crate::policy::address_key(ip);
    let mut s = slots.lock().unwrap_or_else(|e| e.into_inner());
    if s.per.get(&key).copied().unwrap_or(0) >= MAX_PER_ADDRESS {
        return None;
    }
    if s.anonymous.len() >= MAX_ANONYMOUS
        && let Some((_, oldest)) = s.anonymous.pop_first()
    {
        oldest.notify_one();
    }
    if s.total >= MAX_CONNECTIONS {
        return None;
    }
    s.total += 1;
    *s.per.entry(key).or_default() += 1;
    let seq = s.next;
    s.next += 1;
    let close = Arc::new(Notify::new());
    s.anonymous.insert(seq, close.clone());
    Some((ConnSlot(Arc::new(Slot { slots: slots.clone(), key, seq })), close))
}

/// Serves `app` ([`crate::router`]) on `listener` over TLS until this future is dropped, which also ends every
/// connection it opened. `tls` must allow TLS 1.3 only and offer `http/1.1`.
pub async fn serve(listener: TcpListener, tls: Arc<rustls::ServerConfig>, app: Router) -> std::io::Result<()> {
    let acceptor = tokio_rustls::TlsAcceptor::from(tls);
    let slots = Arc::new(Mutex::new(Slots::default()));
    let mut conns = tokio::task::JoinSet::new();
    loop {
        while conns.try_join_next().is_some() {}
        let (tcp, peer): (_, SocketAddr) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!("web port: accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let ip = accounts::net::canonical(peer.ip());
        let Some((slot, close)) = take_slot(&slots, ip) else {
            drop(tcp);
            continue;
        };
        let (acceptor, app) = (acceptor.clone(), app.clone());
        conns.spawn(async move {
            let run = async move {
                let _ = tcp.set_nodelay(true);
                let Ok(Ok(stream)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await else {
                    return;
                };
                let conn = Conn { ip, tls: true };
                let service = hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
                    req.extensions_mut().insert(conn);
                    req.extensions_mut().insert(slot.clone());
                    let app = app.clone();
                    async move {
                        let req = req.map(axum::body::Body::new);
                        match tokio::time::timeout(REQUEST_TIMEOUT, app.oneshot(req)).await {
                            Ok(r) => r,
                            Err(_) => Ok(ApiError::new(408, "timeout", "The request took too long.")
                                .header(http::header::CONNECTION, "close")
                                .into_response()),
                        }
                    }
                });
                let mut http = hyper::server::conn::http1::Builder::new();
                http.timer(TokioTimer::new())
                    .header_read_timeout(HEADER_TIMEOUT)
                    .keep_alive(true)
                    .max_buf_size(64 * 1024);
                let _ = http.serve_connection(TokioIo::new(stream), service).await;
            };
            // closed to make room for a newer anonymous connection: dropping `run` ends it
            tokio::select! {
                () = run => {}
                () = close.notified() => {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn one_ipv6_64_is_one_address() {
        let slots = Arc::new(Mutex::new(Slots::default()));
        let mut held = Vec::new();
        for i in 0..MAX_PER_ADDRESS {
            held.push(take_slot(&slots, ip(&format!("2001:db8:1:2::{:x}", i + 1))).unwrap());
        }
        assert!(take_slot(&slots, ip("2001:db8:1:2:ffff::9")).is_none(), "the same /64");
        assert!(take_slot(&slots, ip("2001:db8:1:3::1")).is_some(), "another /64");
        held.pop();
        assert!(take_slot(&slots, ip("2001:db8:1:2::abcd")).is_some(), "a slot freed is free again");
    }

    #[tokio::test]
    async fn anonymous_connections_make_room_and_signed_ones_stay() {
        let slots = Arc::new(Mutex::new(Slots::default()));
        let signed = take_slot(&slots, ip("192.0.2.1")).unwrap();
        signed.0.signed_in();
        let mut anon = Vec::new();
        for i in 0..MAX_ANONYMOUS {
            anon.push(take_slot(&slots, ip(&format!("10.{}.{}.1", i / 200, i % 200))).unwrap());
        }
        assert_eq!(slots.lock().unwrap().anonymous.len(), MAX_ANONYMOUS);
        // one more closes the oldest anonymous connection, never the signed-in one
        let newest = take_slot(&slots, ip("198.51.100.7")).unwrap();
        {
            let s = slots.lock().unwrap();
            assert_eq!(s.anonymous.len(), MAX_ANONYMOUS);
            assert!(!s.anonymous.contains_key(&anon[0].0.0.seq), "the oldest was told to close");
            assert!(!s.anonymous.contains_key(&signed.0.0.seq));
            assert!(s.anonymous.contains_key(&newest.0.0.seq));
            assert_eq!(s.total, MAX_ANONYMOUS + 2, "until it has closed it still counts");
        }
        // the oldest's notification is waiting for it
        let told = tokio::time::timeout(Duration::from_secs(1), anon[0].1.notified()).await;
        assert!(told.is_ok());
    }
}
