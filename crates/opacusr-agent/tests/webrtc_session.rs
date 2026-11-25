//! Full-stack WebRTC session test: viewer client ↔ host agent through
//! the REAL signaling server (registry, session request, consent, SDP
//! routing) with the WebRTC transport FORCED (no relay fallback).
//!
//! This is the exact path that died for phone viewers with «data
//! channel send: DataChannel is not opened»: the offerer used to return
//! its transport while the SCTP association (and therefore the data
//! channels) was still coming up after DTLS. Since
//! `webrtc::wait_channels_open` the offerer waits for both channels to
//! report Open; this test is the regression guard for that handshake on
//! the live pipeline (demo capture, full access).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use opacusr_agent::config::AgentConfig;
use opacusr_agent::service::{AgentEvent, AgentService, SessionDecision};
use opacusr_client::session::{
    ClientEvent, Command, SessionClient, SessionSettings, TransportChoice,
};
use opacusr_server::{serve, AppState};
use opacusr_transport::webrtc::WebRtcConfig;

const PASSWORD: &str = "e2e-webrtc-password";

/// Real signaling server on ephemeral ports (same helper as the
/// opacusr-server integration tests).
async fn start_server() -> u16 {
    let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_port = http.local_addr().unwrap().port();
    let relay_port = relay.local_addr().unwrap().port();
    let state = Arc::new(AppState::new(relay_port));
    tokio::spawn(async move {
        serve(http, relay, state).await.unwrap();
    });
    http_port
}

#[tokio::test]
async fn full_session_over_webrtc_via_signaling() {
    // Isolated config dir: the agent service persists its assigned
    // peer id (set_last_peer_id) and must not touch the developer's or
    // CI runner's real state.
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("OPACUSR_CONFIG_DIR", tmp.path());

    let http_port = start_server().await;
    let url = format!("ws://127.0.0.1:{http_port}/signal");

    // Host agent in demo mode (deterministic test capture), unattended
    // (auto-accept with the default "full" permission) on the real
    // signaling server.
    let cfg = AgentConfig {
        server_url: url.clone(),
        name: "e2e-webrtc-host".into(),
        password: Some(PASSWORD.into()),
        mdns_advertise: false,
        ..Default::default()
    };

    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let svc = AgentService {
        cfg,
        events: ev_tx,
        demo: true,
        unattended: true,
    };
    tokio::spawn(svc.run(|_, _, _| SessionDecision::Full, shutdown_rx));

    // Wait for the agent to register and capture its assigned PeerId.
    let host_id = loop {
        let ev = tokio::time::timeout(Duration::from_secs(20), ev_rx.recv())
            .await
            .expect("agent service did not register in time");
        match ev {
            Some(AgentEvent::Registered { id, .. }) => break id,
            Some(_) => continue,
            None => panic!("agent service died before registering"),
        }
    };

    // Viewer: WebRTC ONLY (no relay fallback) — if the data channels
    // never open, this connect FAILS instead of silently relaying, so
    // the test genuinely guards the wait_channels_open handshake.
    let settings = SessionSettings {
        password: PASSWORD.into(),
        display_name: "e2e-viewer".into(),
        // Host candidates only: no external STUN needed (both peers are
        // this machine), and CI runners may have no outbound UDP.
        webrtc: WebRtcConfig::default(),
        ..Default::default()
    };

    let (client, mut events) = tokio::time::timeout(
        Duration::from_secs(30),
        SessionClient::connect_via_server(&url, host_id, settings, TransportChoice::WebRtcOnly),
    )
    .await
    .expect("webrtc connect timed out")
    .expect("webrtc connect failed");

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    tokio::spawn(client.run(cmd_rx));

    // Connected → first frame → prove the media channel really works.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let (mut connected, mut got_frame) = (false, false);
    loop {
        if connected && got_frame {
            break;
        }
        let ev = tokio::time::timeout_at(deadline, events.recv()).await;
        match ev {
            Ok(Some(ClientEvent::Connected { .. })) => connected = true,
            Ok(Some(ClientEvent::Frame { .. })) => got_frame = true,
            Ok(Some(_)) => {}
            Ok(None) => panic!("event stream closed (connected={connected})"),
            Err(_) => {
                panic!("timeout waiting over webrtc (connected={connected}, frame={got_frame})")
            }
        }
    }

    // Clean shutdown from the viewer side.
    let _ = cmd_tx.send(Command::Disconnect);
}
