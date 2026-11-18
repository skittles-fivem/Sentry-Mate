//! Serving the web side: TLS 1.3 only (the rustls settings come from the server's certificate, the same one the
//! upload port uses: `ingest::server::Identity::tls_config`), HTTP/1.1, with connection guards: a TLS handshake
//! within 15 s, headers within 20 s, a request answered within 10 minutes (a stream, once started, runs on), idle
//! connections closed after the keep-alive, at most [`MAX_PER_ADDRESS`] connections per address and
//! [`MAX_CONNECTIONS`] in all.

use crate::error::ApiError;
use axum::Router;
use axum::response::IntoResponse;
use hyper_util::rt::{TokioIo, TokioTimer};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tower::ServiceExt;

/// A browser opens about six connections per site, plus the live stream, per tab.
pub const MAX_PER_ADDRESS: usize = 32;
pub const MAX_CONNECTIONS: usize = 1024;
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
pub const HEADER_TIMEOUT: Duration = Duration::from_secs(20);
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
}

struct Slot(Arc<Mutex<Slots>>, IpAddr);

impl Drop for Slot {
    fn drop(&mut self) {
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        s.total = s.total.saturating_sub(1);
        if let Some(n) = s.per.get_mut(&self.1) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                s.per.remove(&self.1);
            }
        }
    }
}

fn take_slot(slots: &Arc<Mutex<Slots>>, ip: IpAddr) -> Option<Slot> {
    let mut s = slots.lock().unwrap_or_else(|e| e.into_inner());
    if s.total >= MAX_CONNECTIONS || s.per.get(&ip).copied().unwrap_or(0) >= MAX_PER_ADDRESS {
        return None;
    }
    s.total += 1;
    *s.per.entry(ip).or_default() += 1;
    Some(Slot(slots.clone(), ip))
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
        let Some(slot) = take_slot(&slots, ip) else {
            drop(tcp);
            continue;
        };
        let (acceptor, app) = (acceptor.clone(), app.clone());
        conns.spawn(async move {
            let _slot = slot;
            let _ = tcp.set_nodelay(true);
            let Ok(Ok(stream)) = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await else {
                return;
            };
            let conn = Conn { ip, tls: true };
            let service = hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
                req.extensions_mut().insert(conn);
                let app = app.clone();
                async move {
                    let req = req.map(axum::body::Body::new);
                    match tokio::time::timeout(REQUEST_TIMEOUT, app.oneshot(req)).await {
                        Ok(r) => r,
                        Err(_) => Ok(ApiError::new(408, "timeout", "The request took too long.").into_response()),
                    }
                }
            });
            let mut http = hyper::server::conn::http1::Builder::new();
            http.timer(TokioTimer::new()).header_read_timeout(HEADER_TIMEOUT).keep_alive(true).max_buf_size(64 * 1024);
            let _ = http.serve_connection(TokioIo::new(stream), service).await;
        });
    }
}
