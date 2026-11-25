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
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
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
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let (v, _) =
        vault::Vault::create(&dir.path().join("data"), &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let id = Identity::load_or_create(&dir.path().join("tls"), &["sentry-mate.local".into()], &v).unwrap();
    let fp = id.fingerprint();
    assert_eq!(fp.len(), 64);
    assert_eq!(
        Identity::load_or_create(&dir.path().join("tls"), &[], &v).unwrap().fingerprint(),
        fp,
        "kept across restarts"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("tls/key.sealed")).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the key is readable by its owner only");
        assert!(!dir.path().join("tls/key.pem").exists(), "no plain key");
        assert!(vault::is_sealed(&std::fs::read(dir.path().join("tls/key.sealed")).unwrap()));
    }

    let can = dir.path().join("can");
    let tokens = Tokens::open(dir.path().join("tokens.json"), v.clone()).await.unwrap();
    let (token, _) = tokens.create("Model Y").await.unwrap();
    let store = Store::new(can, v.clone());
    let up = Uploads::new(store.clone(), tokens, Judge::new(store));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve_tls(listener, id.tls_config().unwrap(), router(up)));

    let req = format!(
        "GET /api/can/logs/ping HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    let res = client(addr, &id, &[&rustls::version::TLS13], &req).await.unwrap();
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert!(res.contains("\"formats\""), "{res}");

    let old = client(addr, &id, &[&rustls::version::TLS12], &req).await;
    assert!(old.is_err() || old.as_ref().is_ok_and(String::is_empty), "TLS 1.2 must be refused: {old:?}");

    // plain HTTP on the TLS port gets nothing
    let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    tcp.write_all(req.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), tcp.read_to_end(&mut out)).await;
    assert!(!String::from_utf8_lossy(&out).contains("200"), "plain HTTP answered");
}

/// A client that takes any certificate and says which one it got (its SHA-256, lowercase hex) and what it answered.
#[derive(Debug)]
struct AnyCert;

impl rustls::client::danger::ServerCertVerifier for AnyCert {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &rustls::pki_types::CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            m,
            c,
            d,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
    }
}

async fn served_sha(addr: std::net::SocketAddr) -> String {
    use sha2::Digest as _;
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCert))
        .with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let tls = tokio_rustls::TlsConnector::from(Arc::new(cfg))
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap();
    let cert = tls.get_ref().1.peer_certificates().unwrap()[0].clone();
    hex::encode(sha2::Sha256::digest(cert.as_ref()))
}

/// A new certificate made while the port runs: served at once to new connections (a connection open already
/// carries on), with the names asked for, its files in place of the old ones and kept across a restart.
#[tokio::test]
async fn a_new_certificate_is_served_without_a_restart() {
    use ingest::server::{LiveCert, RenewError};
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let (v, _) =
        vault::Vault::create(&dir.path().join("data"), &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let v = Arc::new(v);
    let tls = dir.path().join("tls");
    let id = Identity::load_or_create(&tls, &["old.example".into()], &v).unwrap();
    assert!(id.made_here());
    let live = LiveCert::new(&id, &tls, v.clone()).unwrap();
    assert_eq!(live.fingerprint(), id.fingerprint());

    let tokens = Tokens::open(dir.path().join("tokens.json"), v.clone()).await.unwrap();
    let (token, _) = tokens.create("Model Y").await.unwrap();
    let store = Store::new(dir.path().join("can"), v.clone());
    let up = Uploads::new(store.clone(), tokens, Judge::new(store));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve_tls(listener, live.tls_config().unwrap(), router(up)));
    assert_eq!(served_sha(addr).await, id.fingerprint());
    // a connection open before the change
    let mut before = {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(id.chain[0].clone()).unwrap();
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        tokio_rustls::TlsConnector::from(Arc::new(cfg))
            .connect(ServerName::try_from("localhost").unwrap(), tcp)
            .await
            .unwrap()
    };

    let names = vec!["new.example".to_string(), "192.0.2.7".to_string()];
    let (l2, n2) = (live.clone(), names.clone());
    let sha = tokio::task::spawn_blocking(move || l2.renew(&n2)).await.unwrap().unwrap();
    assert_ne!(sha, id.fingerprint());
    assert_eq!(live.fingerprint(), sha);
    assert_eq!(served_sha(addr).await, sha, "new connections get the new certificate");
    let req = format!(
        "GET /api/can/logs/ping HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    before.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    let _ = before.read_to_string(&mut out).await;
    assert!(out.starts_with("HTTP/1.1 200"), "the open connection carries on: {out}");

    // on disk: the new pair, nothing left beside it, the same after a restart; the names are the ones asked for
    let again = Identity::load_or_create(&tls, &[], &v).unwrap();
    assert_eq!(again.fingerprint(), sha);
    let mut left: Vec<String> =
        std::fs::read_dir(&tls).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, ["cert.pem", "key.sealed"]);
    let pem = std::fs::read_to_string(tls.join("cert.pem")).unwrap();
    let der = rustls_pemfile::certs(&mut pem.as_bytes()).next().unwrap().unwrap();
    for n in ["localhost", "new.example"] {
        assert!(der.windows(n.len()).any(|w| w == n.as_bytes()), "{n} in the certificate");
    }
    assert!(der.windows(4).any(|w| w == [192, 0, 2, 7]), "the address in the certificate");
    assert!(!der.windows(11).any(|w| w == b"old.example"), "the old name is gone");

    // the owner's own certificate is never replaced
    let own = rcgen::CertificateParams::new(vec!["own.example".to_string()]).unwrap();
    let mut own = own;
    own.distinguished_name = rcgen::DistinguishedName::new();
    own.distinguished_name.push(rcgen::DnType::CommonName, "Owner CA cert");
    let kp = rcgen::KeyPair::generate().unwrap();
    let c = own.self_signed(&kp).unwrap();
    std::fs::write(tls.join("cert.pem"), c.pem()).unwrap();
    std::fs::write(tls.join("key.pem"), kp.serialize_pem()).unwrap();
    let id = Identity::load_or_create(&tls, &[], &v).unwrap();
    assert!(!id.made_here(), "the owner's own");
    let live = LiveCert::new(&id, &tls, v.clone()).unwrap();
    assert!(matches!(live.renew(&names), Err(RenewError::OwnCertificate)));
    assert_eq!(Identity::load_or_create(&tls, &[], &v).unwrap().fingerprint(), id.fingerprint(), "left alone");
}

/// A power cut during the swap never leaves the folder without a pair that matches: a start finishes a swap whose
/// new pair is whole and drops one that is not.
#[test]
fn a_swap_cut_short_is_finished_or_dropped_at_start() {
    use ingest::server::finish_swap;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    let (v, _) =
        vault::Vault::create(&dir.path().join("data"), &dir.path().join("keys"), vault::Kdf::FAST_FOR_TESTS).unwrap();
    let (old, new) = (dir.path().join("old"), dir.path().join("new"));
    let a = Identity::load_or_create(&old, &[], &v).unwrap().fingerprint();
    let b = Identity::load_or_create(&new, &["b.example".into()], &v).unwrap().fingerprint();
    let copy = |from: &str, to: &str| std::fs::copy(new.join(from), old.join(to)).unwrap();

    // only the new key written: dropped, the old pair stays
    copy("key.sealed", "next-key.sealed");
    assert_eq!(Identity::load_or_create(&old, &[], &v).unwrap().fingerprint(), a);
    assert!(!old.join("next-key.sealed").exists());
    // both written, nothing renamed yet: finished
    copy("key.sealed", "next-key.sealed");
    copy("cert.pem", "next-cert.pem");
    assert_eq!(Identity::load_or_create(&old, &[], &v).unwrap().fingerprint(), b);
    // the key renamed, the certificate not yet: finished
    let c = Identity::load_or_create(&old.join("x"), &[], &v).unwrap();
    std::fs::copy(old.join("x/key.sealed"), old.join("key.sealed")).unwrap();
    std::fs::copy(old.join("x/cert.pem"), old.join("next-cert.pem")).unwrap();
    finish_swap(&old).unwrap();
    assert_eq!(Identity::load_or_create(&old, &[], &v).unwrap().fingerprint(), c.fingerprint());
    assert!(!old.join("next-cert.pem").exists());
    // what a write that stopped before its rename left (its own temporary name, or an older version's) goes too
    for f in ["next-cert.pem.77-00ff.tmp", "next-key.sealed.77-00ff.tmp", "next-cert.tmp", "next-key.tmp"] {
        std::fs::write(old.join(f), b"half").unwrap();
    }
    std::fs::write(old.join("cert.pem.77-00ff.tmp"), b"not ours").unwrap();
    finish_swap(&old).unwrap();
    let mut left: Vec<String> =
        std::fs::read_dir(&old).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert!(!left.iter().any(|n| n.starts_with("next-")), "{left:?}");
    assert!(left.contains(&"cert.pem.77-00ff.tmp".to_string()), "{left:?}");
}
