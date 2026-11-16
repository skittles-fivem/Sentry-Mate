#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use ingest::server::{Identity, serve_tls};
use ingest::{Judge, Store, Tokens, Uploads, router};
use rustls::pki_types::ServerName;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn client(
    addr: std::net::SocketAddr,
    id: &Identity,
    versions: &[&'static rustls::SupportedProtocolVersion],
    request: &str,
) -> std::io::Result<String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(id.chain[0].clone()).unwrap();
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(versions)
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(addr).await?;
    let mut tls = tokio_rustls::TlsConnector::from(Arc::new(cfg))
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await?;
    tls.write_all(request.as_bytes()).await?;
    let mut out = String::new();
    let _ = tls.read_to_string(&mut out).await;
    Ok(out)
}

#[tokio::test]
async fn tls_1_3_only_and_the_api_answers() {
    let dir = tempfile::tempdir().unwrap();
    let id =
        Identity::load_or_create(&dir.path().join("tls"), &["sentry-mate.local".into()]).unwrap();
    let fp = id.fingerprint();
    assert_eq!(fp.len(), 64);
    assert_eq!(
        Identity::load_or_create(&dir.path().join("tls"), &[])
            .unwrap()
            .fingerprint(),
        fp,
        "kept across restarts"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("tls/key.pem"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "the key is readable by its owner only");
    }

    let can = dir.path().join("can");
    let tokens = Tokens::open(dir.path().join("tokens.json")).await.unwrap();
    let (token, _) = tokens.create("Model Y").await.unwrap();
    let up = Uploads::new(Store::new(can.clone()), tokens, Judge::new(can));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve_tls(listener, id.tls_config().unwrap(), router(up)));

    let req = format!(
        "GET /api/can/logs/ping HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    let res = client(addr, &id, &[&rustls::version::TLS13], &req)
        .await
        .unwrap();
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert!(res.contains("\"formats\""), "{res}");

    let old = client(addr, &id, &[&rustls::version::TLS12], &req).await;
    assert!(
        old.is_err() || old.as_ref().is_ok_and(String::is_empty),
        "TLS 1.2 must be refused: {old:?}"
    );

    // plain HTTP on the TLS port gets nothing
    let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    tcp.write_all(req.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ =
        tokio::time::timeout(std::time::Duration::from_secs(5), tcp.read_to_end(&mut out)).await;
    assert!(
        !String::from_utf8_lossy(&out).contains("200"),
        "plain HTTP answered"
    );
}
