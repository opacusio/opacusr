//! `GET /metrics` over a real [`serve`] loopback listener: raw HTTP in,
//! Prometheus text out.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use opacusr_server::AppState;

#[tokio::test]
async fn metrics_endpoint_serves_prometheus_text() {
    let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = http.local_addr().unwrap();
    let state = Arc::new(AppState::new(0));
    let server_state = state.clone();
    let task = tokio::spawn(async move {
        opacusr_server::serve(http, relay, server_state)
            .await
            .unwrap();
    });

    // Bump a counter and check the value surfaces in the exposition.
    state.metrics.registrations.fetch_add(2, Ordering::Relaxed);
    state.metrics.peers.fetch_add(1, Ordering::Relaxed);

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut sock = tokio::net::TcpStream::connect(addr).await.unwrap();
    sock.write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    sock.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8(buf).unwrap();

    assert!(text.starts_with("HTTP/1.1 200 OK"), "no 200 in: {text}");
    assert!(text.contains("text/plain; version=0.0.4"), "content type");
    for expected in [
        "# TYPE opacusr_registrations_total counter",
        "opacusr_registrations_total 2",
        "opacusr_peers 1",
        "opacusr_relay_bytes_total 0",
    ] {
        assert!(text.contains(expected), "missing {expected} in:\n{text}");
    }

    task.abort();
}
