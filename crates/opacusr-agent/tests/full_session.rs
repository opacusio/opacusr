//! Full-stack session test: viewer client ↔ host agent over a real TCP
//! socket, exercising auth, streaming, input, chat, file transfer and the
//! remote terminal — the complete OpacusR MVP pipeline.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;

use opacusr_agent::session::{HostCommand, HostEvent, SessionConfig, SessionHost};
use opacusr_capture::test::TestSource;
use opacusr_client::session::{ClientEvent, Command, SessionClient, SessionSettings};
use opacusr_input::{InputError, InputInjector, RecordingInjector};
use opacusr_proto::message::{MouseButtonId, OpKey};
use opacusr_transport::lockout::{AuthGate, AuthGateConfig};
use opacusr_transport::secure::SecureSession;
use opacusr_transport::tcp::TcpTransport;

const PASSWORD: &str = "e2e-password-1";

/// Records input events into a shared log so the test can assert them
/// after the host owns the injector.
#[derive(Default)]
struct SharedInjector {
    inner: RecordingInjector,
    log: Arc<Mutex<Vec<String>>>,
}

impl SharedInjector {
    fn new(log: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            inner: RecordingInjector::new(),
            log,
        }
    }
}

impl InputInjector for SharedInjector {
    fn mouse_move(&mut self, x: u32, y: u32) -> Result<(), InputError> {
        self.log.lock().unwrap().push(format!("move {x} {y}"));
        self.inner.mouse_move(x, y)
    }
    fn mouse_button(
        &mut self,
        x: u32,
        y: u32,
        b: MouseButtonId,
        down: bool,
    ) -> Result<(), InputError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("button {x} {y} {b:?} {down}"));
        self.inner.mouse_button(x, y, b, down)
    }
    fn wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        self.log.lock().unwrap().push(format!("wheel {dx} {dy}"));
        self.inner.wheel(dx, dy)
    }
    fn key(&mut self, k: OpKey, m: u8, down: bool) -> Result<(), InputError> {
        self.log
            .lock()
            .unwrap()
            .push(format!("key {k:?} {m} {down}"));
        self.inner.key(k, m, down)
    }
    fn text(&mut self, t: &str) -> Result<(), InputError> {
        self.log.lock().unwrap().push(format!("text {t:?}"));
        self.inner.text(t)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_session_over_tcp() {
    let dir = std::env::temp_dir().join(format!("opacusr-e2e-{}", std::process::id()));
    let downloads = dir.join("downloads");
    std::fs::create_dir_all(&downloads).unwrap();

    // ------------------------------------------------------------------
    // Host side
    // ------------------------------------------------------------------
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    let (host_events_tx, mut host_events_rx) = mpsc::unbounded_channel();
    let (host_cmd_tx, host_cmd_rx) = mpsc::unbounded_channel();
    let input_log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let host_input_log = input_log.clone();
    let host_downloads = downloads.clone();
    let host = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let cfg = SessionConfig {
            password: Some(PASSWORD.into()),
            display_name: "e2e-host".into(),
            fps: 60,
            jpeg_quality: 80,
            scale_percent: 100,
            download_dir: host_downloads,
            auto_accept_files: true,
            codec: opacusr_agent::session::CodecChoice::Auto,
            view_only: false,
            clipboard_sync: true,
            adaptive_quality: true,
            audio: false,
            audio_device: String::new(),
            lock_input: false,
            auth_gate: Some(Arc::new(AuthGate::new(AuthGateConfig::default()))),
        };
        let session = SecureSession::new(Box::new(TcpTransport::from_stream(sock)), None);
        let injector = SharedInjector::new(host_input_log);
        SessionHost {
            cfg,
            events: host_events_tx,
            commands: host_cmd_rx,
        }
        .run(
            session,
            Box::new(TestSource::new(320, 200)),
            Box::new(injector),
        )
        .await;
    });

    // ------------------------------------------------------------------
    // Client side
    // ------------------------------------------------------------------
    let settings = SessionSettings {
        password: PASSWORD.into(),
        display_name: "e2e-viewer".into(),
        download_dir: downloads.clone(),
        auto_accept_files: true,
        ..Default::default()
    };
    let (client, mut events) = SessionClient::connect_direct(&addr, settings)
        .await
        .unwrap();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let runner = tokio::spawn(client.run(cmd_rx));

    // Connected + capabilities.
    let connected = tokio::time::timeout(Duration::from_secs(15), async {
        match events.recv().await.unwrap() {
            ClientEvent::Connected {
                host_name,
                screen,
                view_only,
                screens,
                audio_available,
                ..
            } => (host_name, screen, view_only, screens, audio_available),
            other => panic!("expected Connected, got {other:?}"),
        }
    })
    .await
    .unwrap();
    assert_eq!(connected.0, "e2e-host");
    assert_eq!(connected.1, (320, 200));
    assert!(!connected.2, "full-control session must not be view-only");
    assert_eq!(
        connected.3.len(),
        1,
        "test source exposes exactly one screen"
    );
    assert!(!connected.4, "audio is disabled in this test config");

    // Video frames flow with the expected geometry.
    let mut frames = 0u32;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let ClientEvent::Frame(f) = events.recv().await.unwrap() {
                assert_eq!((f.width, f.height), (320, 200));
                assert!(!f.rgba.is_empty());
                frames += 1;
                if frames >= 5 {
                    return;
                }
            }
        }
    })
    .await
    .unwrap();

    // Input: mouse move is executed on the host.
    cmd_tx.send(Command::MouseMove { x: 42, y: 24 }).unwrap();
    let moved = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if input_log.lock().unwrap().iter().any(|e| e == "move 42 24") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(moved.is_ok(), "mouse move never reached the injector");

    // Chat reaches the host.
    cmd_tx
        .send(Command::Chat("hello from viewer".into()))
        .unwrap();
    let got_chat = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let HostEvent::Chat(text) = host_events_rx.recv().await.unwrap() {
                assert!(text.contains("hello from viewer"));
                return;
            }
        }
    })
    .await;
    assert!(got_chat.is_ok(), "host never saw the chat message");

    // File transfer: viewer → host, verified by sha256 on the host.
    let payload: Vec<u8> = (0..(64 * 1024 * 2 + 999))
        .map(|i| (i % 253) as u8)
        .collect();
    let upload = dir.join("upload.bin");
    std::fs::write(&upload, &payload).unwrap();
    cmd_tx.send(Command::PushFile(upload.clone())).unwrap();
    let ft_done = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match host_events_rx.recv().await.unwrap() {
                HostEvent::Ft(opacusr_transport::ft::FtEvent::ReceivedDone(
                    _id,
                    path,
                    verified,
                )) => {
                    assert!(verified, "sha256 mismatch on the received file");
                    let received = std::fs::read(&path).unwrap();
                    assert_eq!(received, payload, "file contents differ");
                    return;
                }
                HostEvent::Ft(opacusr_transport::ft::FtEvent::Failed(id, reason)) => {
                    panic!("transfer failed ({id}): {reason}");
                }
                _ => {}
            }
        }
    })
    .await;
    assert!(ft_done.is_ok(), "file transfer did not complete in time");

    // File transfer: host → viewer, driven through the host command channel.
    host_cmd_tx
        .send(HostCommand::SendFile(upload.clone()))
        .unwrap();
    let (path, verified) = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match events.recv().await {
                Some(opacusr_client::ClientEvent::FtReceivedDone { path, verified }) => {
                    break (path, verified);
                }
                Some(_) => continue,
                None => break (std::path::PathBuf::new(), false),
            }
        }
    })
    .await
    .expect("host→viewer transfer did not complete in time");
    assert!(verified, "sha256 mismatch on the host→viewer transfer");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        payload,
        "host→viewer contents differ"
    );

    // Remote terminal: open, run a command, see its output.
    cmd_tx
        .send(Command::TermOpen {
            rows: 30,
            cols: 100,
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await.unwrap() {
                ClientEvent::TermOpened { ok, error, .. } => {
                    assert!(ok, "terminal failed to open: {error:?}");
                    return;
                }
                ClientEvent::TermData { .. } => {} // banner before TermOpened is possible
                _ => {}
            }
        }
    })
    .await
    .expect("terminal did not open in time");

    cmd_tx
        .send(Command::TermData {
            id: 1,
            data: b"echo opacusr-e2e-marker\n".to_vec(),
        })
        .unwrap();
    let marker_seen = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let ClientEvent::TermData { data, .. } = events.recv().await.unwrap() {
                if String::from_utf8_lossy(&data).contains("opacusr-e2e-marker") {
                    return;
                }
            }
        }
    })
    .await;
    assert!(
        marker_seen.is_ok(),
        "terminal output never showed the marker"
    );

    // Ping/pong RTT measurement.
    cmd_tx.send(Command::Ping).unwrap();
    let _rtt = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ClientEvent::Rtt(_) = events.recv().await.unwrap() {
                return;
            }
        }
    })
    .await;

    // Clean disconnect on both ends.
    cmd_tx.send(Command::Disconnect).unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ClientEvent::Closed(_) = events.recv().await.unwrap() {
                return;
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "client never closed");

    let host_ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let HostEvent::Ended { .. } = host_events_rx.recv().await.unwrap() {
                return;
            }
        }
    })
    .await;
    assert!(host_ended.is_ok(), "host never reported the session end");

    host.await.unwrap();
    drop(runner);
    let _ = std::fs::remove_dir_all(&dir);
}
