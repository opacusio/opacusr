//! Integration: the agent-side auth gate refuses handshakes once the
//! failure budget is spent — even when the *correct* password arrives —
//! and recovers after the lockout expires.

use std::sync::Arc;
use std::time::Duration;

use opacusr_transport::auth::{host_handshake, viewer_handshake, HostAuth, ViewerHandshake};
use opacusr_transport::lockout::{AuthGate, AuthGateConfig};
use opacusr_transport::secure::SecureSession;
use opacusr_transport::tcp::TcpTransport;
use opacusr_transport::TransportError;

const PW: &str = "gate-test-password";

type HostResult = Result<HostAuth, TransportError>;
type ViewerResult = Result<ViewerHandshake, TransportError>;

/// Error text of a handshake result (`"Ok"` on success) — `TransportError`
/// implements `Display`, which keeps the assertions readable without
/// requiring `Debug` on the handshake outputs.
fn text<T>(r: &Result<T, TransportError>) -> String {
    match r {
        Ok(_) => "Ok".into(),
        Err(e) => e.to_string(),
    }
}

/// One connect + handshake round: host side runs on a spawned task
/// (accepts the next connection on the shared listener), viewer side runs
/// inline with `password`.
async fn attempt(
    addr: &str,
    listener: Arc<tokio::net::TcpListener>,
    gate: Arc<AuthGate>,
    password: &str,
) -> (HostResult, ViewerResult) {
    let host = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut session = SecureSession::new(Box::new(TcpTransport::from_stream(sock)), None);
        host_handshake(&mut session, Some(PW), None, "gate-test-host", Some(&gate)).await
    });
    let mut vs = SecureSession::new(Box::new(TcpTransport::connect(addr).await.unwrap()), None);
    let viewer = viewer_handshake(&mut vs, password, "gate-test-viewer", None).await;
    let host = host.await.unwrap();
    (host, viewer)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lockout_refuses_even_the_correct_password() {
    let listener = Arc::new(tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap());
    let addr = listener.local_addr().unwrap().to_string();
    let gate = Arc::new(AuthGate::new(AuthGateConfig {
        max_attempts: 2,
        window: Duration::from_secs(60),
        lockout: Duration::from_secs(1),
        max_lockout: Duration::from_secs(60),
    }));

    // Two wrong-password attempts spend the budget.
    for _ in 0..2 {
        let (host, viewer) = attempt(&addr, listener.clone(), gate.clone(), "wrong").await;
        assert!(
            host.is_err(),
            "host must reject a wrong proof: {}",
            text(&host)
        );
        assert!(
            text(&host).contains("failed authentication"),
            "unexpected host error: {}",
            text(&host)
        );
        assert!(
            viewer.is_err() && text(&viewer).contains("wrong password"),
            "unexpected viewer error: {}",
            text(&viewer)
        );
    }

    // Third attempt: the correct password, but the gate is locked now.
    let (host, viewer) = attempt(&addr, listener.clone(), gate.clone(), PW).await;
    assert!(
        text(&host).contains("locked out"),
        "host side must be refused by the lockout: {}",
        text(&host)
    );
    // The viewer must fail as well. Usually it sees the SessionReject
    // reason, but the host closing the socket right after the reject can
    // surface as a connection-level i/o error instead (Windows); the
    // host-side refusal above is the authoritative lockout assertion.
    assert!(
        viewer.is_err(),
        "viewer must be refused too: {}",
        text(&viewer)
    );

    // After the (short) lockout expires the correct password goes through.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let (host, viewer) = attempt(&addr, listener, gate, PW).await;
    assert!(
        host.is_ok(),
        "host handshake failed after lockout: {}",
        text(&host)
    );
    assert!(
        viewer.is_ok(),
        "viewer handshake failed after lockout: {}",
        text(&viewer)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn success_resets_the_budget() {
    let listener = Arc::new(tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap());
    let addr = listener.local_addr().unwrap().to_string();
    let gate = Arc::new(AuthGate::new(AuthGateConfig {
        max_attempts: 2,
        window: Duration::from_secs(60),
        lockout: Duration::from_secs(60),
        max_lockout: Duration::from_secs(60),
    }));

    // One failure, then a successful authentication clears the counter:
    // two more failures are needed before the lockout engages.
    let (host, viewer) = attempt(&addr, listener.clone(), gate.clone(), "wrong").await;
    assert!(host.is_err() && viewer.is_err());
    let (host, viewer) = attempt(&addr, listener.clone(), gate.clone(), PW).await;
    assert!(host.is_ok(), "correct password must pass: {}", text(&host));
    assert!(viewer.is_ok());

    // Budget is full again: one failure is not enough…
    let (host, viewer) = attempt(&addr, listener.clone(), gate.clone(), "wrong").await;
    assert!(host.is_err() && viewer.is_err());
    // …and the correct password still works (no lockout yet).
    let (host, viewer) = attempt(&addr, listener, gate, PW).await;
    assert!(host.is_ok(), "gate locked too early: {}", text(&host));
    assert!(viewer.is_ok());
}
