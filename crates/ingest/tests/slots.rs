#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]
//! The upload port can't be filled by connections that never show a token: they are closed, oldest first, to make
//! room, so the CAN Bridge's upload still gets in.

use ingest::server::{Identity, MAX_CONNECTIONS, MAX_PER_ADDRESS, serve_tls};
use ingest::{Judge, Store, Tokens, Uploads, router};
use rustls::pki_types::ServerName;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
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
        for _ in 0..MAX_PER_ADDRESS {
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
