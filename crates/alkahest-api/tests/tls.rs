//! HTTPS listener (ALK-US-0044, ALK-SP-0006.R6): scenario `tls`, certificate reload.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use alkahest_api::tls::{CertPaths, generate_self_signed, write_generated};
use alkahest_api::{ApiRouter, ApiSettings, AppState, Server, ServerInfo, TlsAcceptor, bind};
use tokio::sync::oneshot;

fn generate(dir: &Path, overwrite: bool) -> CertPaths {
    let paths = CertPaths {
        cert: dir.join("cert.pem"),
        key: dir.join("key.pem"),
        ca: dir.join("ca.pem"),
    };
    let names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    write_generated(
        &generate_self_signed(&names, 30).unwrap(),
        &paths,
        overwrite,
    )
    .unwrap();
    paths
}

async fn start(acceptor: TlsAcceptor) -> (SocketAddr, oneshot::Sender<()>) {
    let state = AppState::builder(ServerInfo::default()).build();
    let app = ApiRouter::new(state, ApiSettings::default()).into_router();
    let listener = bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, rx) = oneshot::channel::<()>();
    tokio::spawn(
        Server::new(listener, app)
            .tls(Some(acceptor))
            .run(async move {
                let _ = rx.await;
            }),
    );
    (addr, stop)
}

/// A client that trusts only `ca` (no system roots).
fn client(ca: &Path) -> reqwest::Client {
    let pem = std::fs::read(ca).unwrap();
    reqwest::Client::builder()
        .tls_certs_only([reqwest::Certificate::from_pem(&pem).unwrap()])
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

async fn healthz(client: &reqwest::Client, url: &str) -> reqwest::Result<String> {
    client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await
}

#[tokio::test]
async fn tls() {
    // Scenario ALK-SP-0006.R6 `tls`: WHEN TLS is configured THEN clients connect over HTTPS.
    let dir = tempfile::tempdir().unwrap();
    let paths = generate(dir.path(), false);
    let (addr, _stop) = start(TlsAcceptor::from_files(&paths.cert, &paths.key).unwrap()).await;
    let https = format!("https://localhost:{}/healthz", addr.port());
    assert!(
        healthz(&client(&paths.ca), &https)
            .await
            .unwrap()
            .contains("ok")
    );
    let by_ip = format!("https://127.0.0.1:{}/healthz", addr.port());
    assert!(healthz(&client(&paths.ca), &by_ip).await.is_ok());

    // Plain HTTP to the TLS port gets no HTTP answer.
    let plain = format!("http://127.0.0.1:{}/healthz", addr.port());
    assert!(healthz(&client(&paths.ca), &plain).await.is_err());

    // A client that does not trust the CA refuses the server.
    let stranger = tempfile::tempdir().unwrap();
    let other = generate(stranger.path(), false);
    assert!(healthz(&client(&other.ca), &https).await.is_err());
}

#[tokio::test]
async fn renewed_certificate_is_served_after_a_file_change() {
    let dir = tempfile::tempdir().unwrap();
    let old = generate(dir.path(), false);
    let old_ca = std::fs::read(&old.ca).unwrap();
    let old_ca_file = dir.path().join("old-ca.pem");
    std::fs::write(&old_ca_file, old_ca).unwrap();
    let acceptor = TlsAcceptor::from_files(&old.cert, &old.key)
        .unwrap()
        .with_reload_interval(Some(Duration::from_millis(50)));
    let (addr, stop) = start(acceptor).await;
    let url = format!("https://localhost:{}/healthz", addr.port());
    assert!(healthz(&client(&old_ca_file), &url).await.is_ok());

    let new = generate(dir.path(), true);
    let mut switched = false;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if healthz(&client(&new.ca), &url).await.is_ok() {
            switched = true;
            break;
        }
    }
    assert!(switched, "the renewed certificate was never served");
    assert!(healthz(&client(&old_ca_file), &url).await.is_err());
    stop.send(()).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn sighup_reloads_the_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let first = generate(dir.path(), false);
    let first_ca = dir.path().join("first-ca.pem");
    std::fs::copy(&first.ca, &first_ca).unwrap();
    // No polling: only the signal can trigger the reload.
    let acceptor = TlsAcceptor::from_files(&first.cert, &first.key).unwrap();
    let (addr, stop) = start(acceptor).await;
    let url = format!("https://localhost:{}/healthz", addr.port());
    // A served request proves the server task installed its SIGHUP handler.
    assert!(healthz(&client(&first_ca), &url).await.is_ok());

    let second = generate(dir.path(), true);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        healthz(&client(&second.ca), &url).await.is_err(),
        "reloaded without a signal"
    );
    let status = std::process::Command::new("kill")
        .args(["-HUP", &std::process::id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let mut switched = false;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if healthz(&client(&second.ca), &url).await.is_ok() {
            switched = true;
            break;
        }
    }
    assert!(switched, "SIGHUP did not reload the certificate");
    stop.send(()).unwrap();
}
