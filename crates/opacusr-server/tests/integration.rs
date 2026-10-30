//! Full-stack server tests: registry, routing, and the TCP relay pairing.

use std::sync::Arc;

use opacusr_proto::signal::*;
use opacusr_proto::Platform;
use opacusr_server::{serve, AppState, PeerInfo};
use opacusr_transport::signaling::SignalingClient;

async fn start_server() -> (u16, u16) {
    let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_port = http.local_addr().unwrap().port();
    let relay_port = relay.local_addr().unwrap().port();
    let state = Arc::new(AppState::new(relay_port));
    tokio::spawn(async move {
        serve(http, relay, state).await.unwrap();
    });
    (http_port, relay_port)
}

async fn client(port: u16) -> (SignalingClient, Welcome) {
    let mut c = SignalingClient::connect(&format!("ws://127.0.0.1:{port}/signal"))
        .await
        .unwrap();
    c.send(&SignalMessage::Register(Register {
        platform: Platform::Linux,
        display_name: "tester".into(),
        is_agent: false,
        unattended: false,
    }))
    .await
    .unwrap();
    match c.recv().await.unwrap() {
        SignalMessage::Welcome(w) => (c, w),
        other => panic!("expected Welcome, got {other:?}"),
    }
}

#[tokio::test]
async fn register_and_forward_between_peers() {
    let (port, _) = start_server().await;
    let (mut host, host_w) = client(port).await;
    let (mut viewer, viewer_w) = client(port).await;
    assert_ne!(host_w.id, viewer_w.id);
    assert_eq!(host_w.heartbeat_secs, opacusr_server::HEARTBEAT_SECS);

    // Viewer asks the host for a session.
    viewer
        .send(&SignalMessage::RequestSession(RequestSession {
            peer: host_w.id,
            display_name: "me".into(),
        }))
        .await
        .unwrap();
    match host.recv().await.unwrap() {
        SignalMessage::SessionIncoming(s) => {
            assert_eq!(s.from, viewer_w.id);
            assert_eq!(s.display_name, "me");
        }
        other => panic!("expected SessionIncoming, got {other:?}"),
    }

    // Host answers with an opaque payload.
    host.send(&SignalMessage::PeerSend(PeerSend {
        to: viewer_w.id,
        payload: PeerPayload::Reject {
            reason: "not now".into(),
        },
    }))
    .await
    .unwrap();
    match viewer.recv().await.unwrap() {
        SignalMessage::PeerRecv(p) => {
            assert_eq!(p.from, host_w.id);
            assert!(matches!(p.payload, PeerPayload::Reject { .. }));
        }
        other => panic!("expected PeerRecv, got {other:?}"),
    }
}

#[tokio::test]
async fn request_to_offline_peer_fails() {
    let (port, _) = start_server().await;
    let (mut viewer, _w) = client(port).await;
    viewer
        .send(&SignalMessage::RequestSession(RequestSession {
            peer: 123_456_789, // nobody registered
            display_name: "me".into(),
        }))
        .await
        .unwrap();
    match viewer.recv().await.unwrap() {
        SignalMessage::SessionFailed(f) => assert_eq!(f.peer, 123_456_789),
        other => panic!("expected SessionFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn relay_pairs_two_connections() {
    let (port, relay_port) = start_server().await;
    let (mut a, _) = client(port).await;
    let (b, _) = client(port).await;
    let _ = b;

    // One side allocates a ticket and shares it with the peer out of band
    // (in real sessions via PeerPayload); the server pairs the two raw
    // connections by ticket and pipes them byte-for-byte.
    a.send(&SignalMessage::RelayAlloc(
        opacusr_proto::signal::RelayAlloc,
    ))
    .await
    .unwrap();
    let ticket = match a.recv().await.unwrap() {
        SignalMessage::RelayTicket(t) => t.ticket,
        other => panic!("expected RelayTicket, got {other:?}"),
    };

    let host_task = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", relay_port))
            .await
            .unwrap();
        let mut preamble = Vec::with_capacity(17);
        preamble.extend_from_slice(&ticket);
        preamble.push(1u8); // host role
        s.write_all(&preamble).await.unwrap();
        s.write_all(b"ping-from-host").await.unwrap();
        let mut buf = [0u8; 4]; // receives the viewer's "pong"
        s.read_exact(&mut buf).await.unwrap();
        buf
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let viewer_task = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", relay_port))
            .await
            .unwrap();
        let mut preamble = Vec::with_capacity(17);
        preamble.extend_from_slice(&ticket);
        preamble.push(0u8); // viewer role
        s.write_all(&preamble).await.unwrap();
        let mut buf = vec![0u8; 14]; // receives the host's "ping-from-host"
        s.read_exact(&mut buf).await.unwrap();
        s.write_all(b"pong").await.unwrap();
        buf
    });

    let host_read = host_task.await.unwrap();
    let viewer_read = viewer_task.await.unwrap();
    assert_eq!(&host_read, b"pong");
    assert_eq!(&viewer_read, b"ping-from-host");
}

#[tokio::test]
async fn registry_prune_keeps_fresh_peers() {
    let registry = opacusr_server::Registry::default();
    let _ = registry.register(PeerInfo {
        platform: Platform::Linux,
        display_name: "fresh".into(),
        is_agent: true,
        unattended: false,
    });
    assert_eq!(registry.len(), 1);
    let removed = registry.prune();
    assert_eq!(removed, 0);
    assert_eq!(registry.len(), 1);
}
