#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The upload port can't be filled by connections that never show a token: they are closed, oldest first, to make
//! room, so the CAN Bridge's upload still gets in.

use ingest::server::{
    Identity, LINGER, MAX_ANONYMOUS, MAX_ANONYMOUS_PER_ADDRESS, MAX_CONNECTIONS, MAX_PER_ADDRESS, MAX_REFUSING,
    MAX_REFUSING_PER_ADDRESS, serve_tls,
};
use ingest::{Judge, Store, Tokens, Uploads, router};
use rustls::pki_types::ServerName;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn connector(id: &Identity) -> tokio_rustls::TlsConnector {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(id.chain[0].clone()).unwrap();
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tokio_rustls::TlsConnector::from(Arc::new(cfg))
}

async fn open_from(
    from: Ipv4Addr,
    addr: SocketAddr,
    tls: &tokio_rustls::TlsConnector,
) -> std::io::Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>> {
    let sock = tokio::net::TcpSocket::new_v4()?;
    sock.bind(SocketAddr::new(IpAddr::V4(from), 0))?;
    let tcp = sock.connect(addr).await?;
    tls.connect(ServerName::try_from("localhost").unwrap(), tcp).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connections_without_a_token_cannot_keep_the_bridge_out() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let (v, _) =
        vault::Vault::create(&dir.path().join("data"), &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let id = Identity::load_or_create(&dir.path().join("tls"), &[], &v).unwrap();
    let tokens = Tokens::open(dir.path().join("tokens.json"), v.clone()).await.unwrap();
    let (token, _) = tokens.create("Model Y").await.unwrap();
    let store = Store::new(dir.path().join("can"), v.clone());
    let up = Uploads::new(store.clone(), tokens, Judge::new(store));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve_tls(listener, id.tls_config().unwrap(), router(up)));
    let tls = connector(&id);

    // the attack: every slot, from 127.0.0.2 on, each a request whose headers never end
    let mut held = Vec::new();
    let mut n = 2u8;
    'fill: loop {
        for _ in 0..MAX_ANONYMOUS_PER_ADDRESS {
            if held.len() == MAX_CONNECTIONS {
                break 'fill;
            }
            let mut s = open_from(Ipv4Addr::new(127, 0, 0, n), addr, &tls).await.unwrap();
            s.write_all(b"POST /api/can/logs/status HTTP/1.1\r\nHost: x\r\n").await.unwrap();
            held.push(s);
        }
        n += 1;
    }

    // the bridge, from its own address, still gets in and is answered
    let req = format!(
        "GET /api/can/logs/ping HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    let answer = tokio::time::timeout(Duration::from_secs(5), async {
        let mut s = open_from(Ipv4Addr::LOCALHOST, addr, &tls).await?;
        s.write_all(req.as_bytes()).await?;
        let mut out = String::new();
        let _ = s.read_to_string(&mut out).await;
        Ok::<_, std::io::Error>(out)
    })
    .await;
    let out = match answer {
        Ok(Ok(out)) => out,
        other => panic!("the bridge was kept out: {other:?}"),
    };
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    drop(held);
}

struct Server {
    addr: SocketAddr,
    tls: tokio_rustls::TlsConnector,
    token: String,
    _dir: tempfile::TempDir,
}

async fn start() -> Server {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let (v, _) =
        vault::Vault::create(&dir.path().join("data"), &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let id = Identity::load_or_create(&dir.path().join("tls"), &[], &v).unwrap();
    let tokens = Tokens::open(dir.path().join("tokens.json"), v.clone()).await.unwrap();
    let (token, _) = tokens.create("Model Y").await.unwrap();
    let store = Store::with_limits(dir.path().join("can"), v.clone(), 0, 200_000, |_| u64::MAX);
    let up = Uploads::new(store.clone(), tokens, Judge::new(store));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve_tls(listener, id.tls_config().unwrap(), router(up)));
    Server { addr, tls: connector(&id), token, _dir: dir }
}

type Tls = tokio_rustls::client::TlsStream<tokio::net::TcpStream>;

/// Sends `head` (and `body`) on `s` and reads one answer, leaving the connection open: its status line, its headers
/// (lower case) and its body.
async fn ask(s: &mut Tls, head: &str, body: &[u8]) -> std::io::Result<(String, String, String)> {
    s.write_all(head.as_bytes()).await?;
    s.write_all(body).await?;
    let mut got = Vec::new();
    let mut b = [0u8; 4096];
    let end = loop {
        if let Some(i) = got.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = s.read(&mut b).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        got.extend_from_slice(&b[..n]);
    };
    let head = String::from_utf8_lossy(&got[..end]).to_ascii_lowercase();
    let len: usize =
        head.lines().find_map(|l| l.strip_prefix("content-length:")).map(|v| v.trim().parse().unwrap()).unwrap_or(0);
    while got.len() < end + len {
        let n = s.read(&mut b).await?;
        if n == 0 {
            break;
        }
        got.extend_from_slice(&b[..n]);
    }
    let status = head.lines().next().unwrap_or("").to_string();
    Ok((status, head, String::from_utf8_lossy(&got[end..]).into_owned()))
}

fn ping(token: &str) -> String {
    format!("GET /api/can/logs/ping HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n\r\n")
}

async fn upload(srv: &Server, i: usize) -> std::io::Result<(String, Tls)> {
    use sha2::Digest;
    let mut s = open_from(Ipv4Addr::LOCALHOST, srv.addr, &srv.tls).await?;
    let name = format!("abcdef123456_20261108T1430{i:02}Z_1.log.xz");
    let bytes = format!("log {i} ").repeat(1000).into_bytes();
    let sha = hex::encode(sha2::Sha256::digest(&bytes));
    let head = format!(
        "POST /api/can/logs/{name} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nUpload-Offset: 0\r\n\
         Upload-Length: {len}\r\nUpload-Sha256: {sha}\r\nContent-Length: {len}\r\n\r\n",
        srv.token,
        len = bytes.len()
    );
    let (status, _, _) = ask(&mut s, &head, &bytes).await?;
    Ok((status, s))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bridges_16_uploads_at_once_all_get_in_and_the_33rd_connection_is_told_to_wait() {
    let srv = Arc::new(start().await);
    // the bridge's ping: the address is a car's from now on
    let mut first = open_from(Ipv4Addr::LOCALHOST, srv.addr, &srv.tls).await.unwrap();
    let (status, _, _) = ask(&mut first, &ping(&srv.token), b"").await.unwrap();
    assert!(status.starts_with("http/1.1 200"), "{status}");

    // 16 uploads at once, each on a new connection (anonymous until its answer)
    let mut tasks = Vec::new();
    for i in 0..16 {
        let srv = srv.clone();
        tasks.push(tokio::spawn(async move { upload(&srv, i).await }));
    }
    let mut held = vec![first];
    for t in tasks {
        let (status, s) = t.await.unwrap().expect("an upload connection was closed");
        assert!(status.starts_with("http/1.1 201"), "{status}");
        held.push(s);
    }
    // more of the car's (anonymous) connections up to 32 from its address
    while held.len() < MAX_PER_ADDRESS {
        held.push(open_from(Ipv4Addr::LOCALHOST, srv.addr, &srv.tls).await.unwrap());
    }
    // the 33rd: answered 503 too_many_connections with Retry-After, not cut off
    let mut extra = open_from(Ipv4Addr::LOCALHOST, srv.addr, &srv.tls).await.unwrap();
    let (status, head, body) = ask(&mut extra, &ping(&srv.token), b"").await.unwrap();
    assert!(status.starts_with("http/1.1 503"), "{status}");
    assert!(head.contains("retry-after: 5\r\n"), "{head}");
    assert!(head.contains("connection: close"), "{head}");
    assert!(body.contains("\"code\":\"too_many_connections\""), "{body}");
    // an upload refused this way (a small one, sent whole before reading) still reads its answer, not a reset
    drop(extra);
    let (status, _) = upload(&srv, 20).await.unwrap();
    assert!(status.starts_with("http/1.1 503"), "{status}");

    // the authorized connections still work
    let (status, _, _) = ask(&mut held[0], &ping(&srv.token), b"").await.unwrap();
    assert!(status.starts_with("http/1.1 200"), "{status}");
    let (status, _, _) = ask(&mut held[5], &ping(&srv.token), b"").await.unwrap();
    assert!(status.starts_with("http/1.1 200"), "{status}");

    // a stranger from another address still gets only 6 anonymous connections
    let stranger = Ipv4Addr::new(127, 0, 0, 2);
    let mut theirs = Vec::new();
    for _ in 0..MAX_ANONYMOUS_PER_ADDRESS {
        let mut s = open_from(stranger, srv.addr, &srv.tls).await.unwrap();
        s.write_all(b"POST /api/can/logs/status HTTP/1.1\r\nHost: x\r\n").await.unwrap();
        theirs.push(s);
    }
    let mut seventh = open_from(stranger, srv.addr, &srv.tls).await.unwrap();
    let (status, _, body) = ask(&mut seventh, "GET /api/can/logs/ping HTTP/1.1\r\nHost: x\r\n\r\n", b"").await.unwrap();
    assert!(status.starts_with("http/1.1 503") && body.contains("too_many_connections"), "{status} {body}");

    // closing some of the car's connections makes room again
    held.truncate(20);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (status, _) = upload(&srv, 21).await.unwrap();
    assert!(status.starts_with("http/1.1 201"), "{status}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn before_any_token_an_address_gets_6_and_the_rest_are_told_to_wait() {
    let srv = start().await;
    let mut held = Vec::new();
    for _ in 0..MAX_ANONYMOUS_PER_ADDRESS {
        held.push(open_from(Ipv4Addr::LOCALHOST, srv.addr, &srv.tls).await.unwrap());
    }
    let mut s = open_from(Ipv4Addr::LOCALHOST, srv.addr, &srv.tls).await.unwrap();
    let (status, head, _) = ask(&mut s, &ping(&srv.token), b"").await.unwrap();
    assert!(status.starts_with("http/1.1 503") && head.contains("retry-after: 5"), "{status}");
    // one of them shows the token: the address is a car's, and the next connection gets in
    let (status, _, _) = ask(&mut held[0], &ping(&srv.token), b"").await.unwrap();
    assert!(status.starts_with("http/1.1 200"), "{status}");
    let mut s = open_from(Ipv4Addr::LOCALHOST, srv.addr, &srv.tls).await.unwrap();
    let (status, _, _) = ask(&mut s, &ping(&srv.token), b"").await.unwrap();
    assert!(status.starts_with("http/1.1 200"), "{status}");
}

/// The head of an upload of `len` bytes that sends no hash check worth anything (it is refused before that).
fn upload_head(srv: &Server, i: usize, len: usize, extra: &str) -> String {
    format!(
        "POST /api/can/logs/abcdef123456_20261108T1431{i:02}Z_1.log.xz HTTP/1.1\r\nHost: localhost\r\nAuthorization: \
         Bearer {}\r\nUpload-Offset: 0\r\nUpload-Length: {len}\r\nUpload-Sha256: {}\r\nContent-Length: {len}\r\n{extra}\r\n",
        srv.token,
        "0".repeat(64)
    )
}

/// Reads an answer's head, or the error that ended the connection first.
async fn read_head(s: &mut Tls) -> std::io::Result<String> {
    let mut got = Vec::new();
    let mut b = [0u8; 4096];
    loop {
        if got.windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(String::from_utf8_lossy(&got).into_owned());
        }
        let n = s.read(&mut b).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        got.extend_from_slice(&b[..n]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_16_mib_upload_is_answered_at_once_not_after_its_body() {
    let srv = start().await;
    let a = Ipv4Addr::new(127, 0, 0, 9);
    let mut held = Vec::new();
    for _ in 0..MAX_ANONYMOUS_PER_ADDRESS {
        held.push(open_from(a, srv.addr, &srv.tls).await.unwrap());
    }
    let len = 16usize << 20;

    // with Expect: 100-continue the answer comes before any of the body is sent
    let mut s = open_from(a, srv.addr, &srv.tls).await.unwrap();
    s.write_all(upload_head(&srv, 1, len, "Expect: 100-continue\r\n").as_bytes()).await.unwrap();
    let t = Instant::now();
    let head = tokio::time::timeout(Duration::from_secs(5), read_head(&mut s)).await.unwrap().unwrap();
    assert!(head.starts_with("HTTP/1.1 503"), "{head}");
    assert!(head.to_ascii_lowercase().contains("retry-after: 5\r\n"), "{head}");
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());

    // without it, a slow link (20 KB every 100 ms) reading as it sends gets the answer after the headers, at once,
    // not after 16 MiB
    let s = open_from(a, srv.addr, &srv.tls).await.unwrap();
    let (mut rd, mut wr) = tokio::io::split(s);
    wr.write_all(upload_head(&srv, 2, len, "").as_bytes()).await.unwrap();
    let t = Instant::now();
    let sender = tokio::spawn(async move {
        let chunk = [1u8; 20_000];
        let mut sent = 0usize;
        while sent < len {
            if wr.write_all(&chunk).await.is_err() {
                break;
            }
            sent += chunk.len();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        sent
    });
    let mut got = Vec::new();
    let mut b = [0u8; 4096];
    let answer = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match rd.read(&mut b).await {
                Ok(0) | Err(_) => break,
                Ok(n) => got.extend_from_slice(&b[..n]),
            }
        }
    })
    .await;
    assert!(answer.is_ok(), "the connection stayed open past the linger");
    let text = String::from_utf8_lossy(&got);
    assert!(text.starts_with("HTTP/1.1 503") && text.contains("too_many_connections"), "{text}");
    assert!(t.elapsed() < LINGER + Duration::from_secs(2), "{:?}", t.elapsed());
    // the sender is stopped soon (closed after the linger), far short of the whole piece
    let sent = tokio::time::timeout(Duration::from_secs(10), sender).await.unwrap().unwrap();
    assert!(sent < 4 << 20, "{sent}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn strangers_holding_refusals_cannot_keep_a_cars_refusal_unanswered() {
    let srv = start().await;
    let mut held = Vec::new();
    // two addresses hold 6 connections each and then as many refused ones as they may, each trickling a body
    for a in [Ipv4Addr::new(127, 0, 0, 20), Ipv4Addr::new(127, 0, 0, 21)] {
        for _ in 0..MAX_ANONYMOUS_PER_ADDRESS {
            held.push(open_from(a, srv.addr, &srv.tls).await.unwrap());
        }
        for _ in 0..MAX_REFUSING_PER_ADDRESS + 4 {
            if let Ok(mut s) = open_from(a, srv.addr, &srv.tls).await {
                let _ =
                    s.write_all(b"POST /api/can/logs/x HTTP/1.1\r\nHost: x\r\nContent-Length: 1000000\r\n\r\nab").await;
                held.push(s);
            }
        }
    }
    const { assert!(2 * MAX_REFUSING_PER_ADDRESS >= MAX_REFUSING) };
    tokio::time::sleep(Duration::from_millis(300)).await;
    // the car, before its token: 6 get in, and the rest are each answered 503, never closed unanswered
    let car = Ipv4Addr::LOCALHOST;
    for _ in 0..MAX_ANONYMOUS_PER_ADDRESS {
        held.push(open_from(car, srv.addr, &srv.tls).await.unwrap());
    }
    for _ in 0..10 {
        let mut s = open_from(car, srv.addr, &srv.tls).await.expect("closed before the TLS handshake");
        let (status, head, _) = ask(&mut s, &ping(&srv.token), b"").await.expect("closed unanswered");
        assert!(status.starts_with("http/1.1 503") && head.contains("retry-after: 5"), "{status}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_anonymous_flood_never_cuts_a_cars_uploads_in_flight() {
    let srv = start().await;
    let mut p = open_from(Ipv4Addr::LOCALHOST, srv.addr, &srv.tls).await.unwrap();
    let (status, _, _) = ask(&mut p, &ping(&srv.token), b"").await.unwrap();
    assert!(status.starts_with("http/1.1 200"), "{status}");
    // 16 uploads in flight: headers and the first 4 KB of 1 MiB each
    let mut ups = Vec::new();
    for i in 0..16 {
        let mut u = open_from(Ipv4Addr::LOCALHOST, srv.addr, &srv.tls).await.unwrap();
        u.write_all(upload_head(&srv, i, 1 << 20, "").as_bytes()).await.unwrap();
        u.write_all(&[7u8; 4096]).await.unwrap();
        ups.push(u);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    // anonymous connections from many addresses, more than the port keeps: they close each other's, oldest first
    let mut held = Vec::new();
    let mut n = 0u32;
    while held.len() < MAX_ANONYMOUS + 64 {
        let a = Ipv4Addr::new(127, 1, (n / 200) as u8, (n % 200 + 1) as u8);
        for _ in 0..MAX_ANONYMOUS_PER_ADDRESS {
            if let Ok(s) = open_from(a, srv.addr, &srv.tls).await {
                held.push(s);
            }
        }
        n += 1;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut cut = 0;
    for u in &mut ups {
        let mut ok = true;
        for _ in 0..64 {
            if u.write_all(&[7u8; 4096]).await.is_err() {
                ok = false;
                break;
            }
        }
        if ok {
            let mut b = [0u8; 1];
            if let Ok(Ok(0) | Err(_)) = tokio::time::timeout(Duration::from_millis(300), u.read(&mut b)).await {
                ok = false;
            }
        }
        if !ok {
            cut += 1;
        }
    }
    assert_eq!(cut, 0, "{cut} of 16 uploads in flight were cut by {} anonymous connections", held.len());
    // the first of the flood were closed to make room instead
    let mut b = [0u8; 1];
    let first = tokio::time::timeout(Duration::from_secs(2), held[0].read(&mut b)).await;
    assert!(matches!(first, Ok(Ok(0) | Err(_))), "{first:?}");
}
