//! Full-stack session test: viewer client ↔ host agent over a real TCP
//! socket, exercising auth, streaming, input, chat, file transfer and the
//! remote terminal — the complete OpacusR MVP pipeline.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;

use opacusr_agent::session::{HostCommand, HostEvent, SessionConfig, SessionHost};
use opacusr_agent::tunnel::TunnelPolicy;
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
            totp_secret: None,
            record: false,
            record_dir: std::env::temp_dir(),
            tunnel: TunnelPolicy::Loopback,
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
            if let ClientEvent::Frame { frame: f, .. } = events.recv().await.unwrap() {
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

// ---------------------------------------------------------------------------
// TOTP second factor
// ---------------------------------------------------------------------------

/// The RFC 6238 reference secret (base32 of ASCII "12345678901234567890").
const TOTP_SECRET: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

fn totp_code_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    opacusr_crypto::totp::totp_at(TOTP_SECRET, now).unwrap()
}

/// Host with `totp_secret` set: a wrong 6-digit code must be rejected even
/// with the correct password; the current code must let the session in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn totp_second_factor_is_enforced() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    // Host: handle both connection attempts sequentially.
    let host_task = tokio::spawn(async move {
        for _ in 0..2 {
            let (sock, _) = listener.accept().await.unwrap();
            let cfg = SessionConfig {
                password: Some(PASSWORD.into()),
                display_name: "e2e-host".into(),
                fps: 60,
                jpeg_quality: 80,
                scale_percent: 100,
                download_dir: std::env::temp_dir(),
                auto_accept_files: true,
                codec: opacusr_agent::session::CodecChoice::Auto,
                view_only: false,
                clipboard_sync: true,
                adaptive_quality: false,
                audio: false,
                audio_device: String::new(),
                lock_input: false,
                totp_secret: Some(TOTP_SECRET.into()),
                record: false,
                record_dir: std::env::temp_dir(),
                tunnel: TunnelPolicy::Off,
                auth_gate: None,
            };
            let (ev_tx, _ev_rx) = mpsc::unbounded_channel();
            let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();
            let session = SecureSession::new(Box::new(TcpTransport::from_stream(sock)), None);
            SessionHost {
                cfg,
                events: ev_tx,
                commands: cmd_rx,
            }
            .run(
                session,
                Box::new(TestSource::new(320, 200)),
                Box::new(opacusr_input::RecordingInjector::new()),
            )
            .await;
        }
    });

    // Attempt 1: correct password, wrong code → rejected with a 2FA error.
    let wrong = SessionSettings {
        password: PASSWORD.into(),
        display_name: "e2e-viewer".into(),
        download_dir: std::env::temp_dir(),
        auto_accept_files: true,
        totp: Some("000000".into()),
        ..Default::default()
    };
    match SessionClient::connect_direct(&addr, wrong).await {
        Ok(_) => panic!("wrong 2FA code must be rejected"),
        Err(e) => assert!(
            e.to_string().contains("2FA"),
            "expected a 2FA error, got: {e}"
        ),
    }

    // Attempt 2: correct password + current code → session established.
    let good = SessionSettings {
        password: PASSWORD.into(),
        display_name: "e2e-viewer".into(),
        download_dir: std::env::temp_dir(),
        auto_accept_files: true,
        totp: Some(totp_code_now()),
        ..Default::default()
    };
    let (client, mut events) = SessionClient::connect_direct(&addr, good)
        .await
        .expect("valid 2FA code must connect");
    let runner = tokio::spawn(client.run(tokio::sync::mpsc::unbounded_channel().1));
    let ev = tokio::time::timeout(Duration::from_secs(15), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(ev, ClientEvent::Connected { .. }),
        "expected Connected, got {ev:?}"
    );
    drop(runner);

    host_task.await.unwrap();
}

// ---------------------------------------------------------------------------
// Opt-in session recording (host-side MP4)
// ---------------------------------------------------------------------------

/// Host with `record = true`: the viewer is told the session is recorded
/// (`Connected.recording`) and the host leaves a parseable MP4 with the
/// streamed H.264 samples in `record_dir`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_recording_writes_mp4() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let record_dir = tempfile::tempdir().expect("record dir");

    let dir_for_host = record_dir.path().to_path_buf();
    let host_task = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let cfg = SessionConfig {
            password: Some(PASSWORD.into()),
            display_name: "e2e-host".into(),
            fps: 60,
            jpeg_quality: 80,
            scale_percent: 100,
            download_dir: std::env::temp_dir(),
            auto_accept_files: true,
            codec: opacusr_agent::session::CodecChoice::Auto,
            view_only: false,
            clipboard_sync: true,
            adaptive_quality: false,
            audio: false,
            audio_device: String::new(),
            lock_input: false,
            totp_secret: None,
            record: true,
            record_dir: dir_for_host,
            tunnel: TunnelPolicy::Off,
            auth_gate: None,
        };
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
        let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let session = SecureSession::new(Box::new(TcpTransport::from_stream(sock)), None);
        let host = SessionHost {
            cfg,
            events: ev_tx,
            commands: cmd_rx,
        };
        let host = tokio::spawn(host.run(
            session,
            Box::new(TestSource::new(320, 200)),
            Box::new(opacusr_input::RecordingInjector::new()),
        ));
        // The host should announce the recording file.
        let mut announced = false;
        let _ = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(ev) = ev_rx.recv().await {
                if let HostEvent::Notice(text) = ev {
                    if text.starts_with("recording →") {
                        break;
                    }
                }
            }
        })
        .await
        .map(|_| announced = true);
        assert!(announced, "host must announce the recording file");

        let _ = host.await;
    });

    let (client, mut events) = SessionClient::connect_direct(
        &addr,
        SessionSettings {
            password: PASSWORD.into(),
            display_name: "e2e-viewer".into(),
            download_dir: std::env::temp_dir(),
            auto_accept_files: true,
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    // The command sender must stay alive: the client treats a closed
    // command channel as "ui closed" and ends the session.
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let runner = tokio::spawn(client.run(cmd_rx));
    let ev = tokio::time::timeout(Duration::from_secs(15), events.recv())
        .await
        .unwrap()
        .unwrap();
    let ClientEvent::Connected { recording, .. } = ev else {
        panic!("expected Connected, got {ev:?}");
    };
    assert!(recording, "viewer must be told the session is recorded");

    // Let some frames flow so the recorder has samples, then hang up.
    // (Non-frame events like Rtt may interleave — count Frame only.)
    let mut frames = 0;
    while frames < 3 {
        let ev = tokio::time::timeout(Duration::from_secs(15), events.recv())
            .await
            .unwrap()
            .unwrap();
        if matches!(ev, ClientEvent::Frame { .. }) {
            frames += 1;
        }
    }
    drop(runner);
    drop(cmd_tx);

    // Wait for the host session to end so the recorder is finalized.
    let ended = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if host_task.is_finished() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(ended.is_ok(), "host never finished");
    host_task.await.unwrap();

    // Exactly one MP4 in the record dir, with video samples muxed.
    let files: Vec<_> = std::fs::read_dir(record_dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "expected one recording, got {files:?}");
    let file = std::fs::File::open(&files[0]).unwrap();
    let mut reader = mp4::read_mp4(file).expect("recording must parse as MP4");
    let n = reader.sample_count(1).expect("video track expected");
    assert!(n >= 3, "expected several muxed frames, got {n}");
    let s1 = reader.read_sample(1, 1).unwrap().unwrap();
    assert!(s1.is_sync, "first sample must be a keyframe");
}

// ---------------------------------------------------------------------------
// TCP tunneling (SSH-style local forwarding)
// ---------------------------------------------------------------------------

/// Host with the loopback tunnel policy: the viewer binds a local port,
/// an app connects to it, and bytes round-trip to an echo server on the
/// host through the session channel. The server closes after the echo so
/// the viewer sees the host-side EOF as `TunnelClosed`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tcp_tunnel_echo_roundtrip() {
    // Single-shot echo server: echo exactly one payload, then close.
    const PAYLOAD: &[u8] = b"e2e-tunnel-payload";
    let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let echo_port = echo.local_addr().unwrap().port();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut sock, _) = echo.accept().await.unwrap();
        let mut buf = [0u8; PAYLOAD.len()];
        sock.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, PAYLOAD);
        let _ = sock.write_all(&buf).await;
        drop(sock); // close → host pump sees EOF → TunnelClose
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let host_task = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let cfg = SessionConfig {
            password: Some(PASSWORD.into()),
            display_name: "e2e-host".into(),
            fps: 60,
            jpeg_quality: 80,
            scale_percent: 100,
            download_dir: std::env::temp_dir(),
            auto_accept_files: true,
            codec: opacusr_agent::session::CodecChoice::Auto,
            view_only: false,
            clipboard_sync: true,
            adaptive_quality: false,
            audio: false,
            audio_device: String::new(),
            lock_input: false,
            totp_secret: None,
            record: false,
            record_dir: std::env::temp_dir(),
            tunnel: TunnelPolicy::Loopback,
            auth_gate: None,
        };
        let (ev_tx, _ev_rx) = mpsc::unbounded_channel();
        let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let session = SecureSession::new(Box::new(TcpTransport::from_stream(sock)), None);
        SessionHost {
            cfg,
            events: ev_tx,
            commands: cmd_rx,
        }
        .run(
            session,
            Box::new(TestSource::new(320, 200)),
            Box::new(opacusr_input::RecordingInjector::new()),
        )
        .await;
    });

    let (client, mut events) = SessionClient::connect_direct(
        &addr,
        SessionSettings {
            password: PASSWORD.into(),
            display_name: "e2e-viewer".into(),
            download_dir: std::env::temp_dir(),
            auto_accept_files: true,
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let runner = tokio::spawn(client.run(cmd_rx));

    // Connected must announce tunnel support.
    let ev = tokio::time::timeout(Duration::from_secs(15), events.recv())
        .await
        .unwrap()
        .unwrap();
    let ClientEvent::Connected { tunnel, .. } = ev else {
        panic!("expected Connected, got {ev:?}");
    };
    assert!(tunnel, "loopback policy must announce tunnels");

    // Bind an ephemeral local listener.
    cmd_tx
        .send(Command::TunnelListen {
            listen_port: 0,
            target_host: "127.0.0.1".into(),
            target_port: echo_port,
        })
        .unwrap();
    let listen_port = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let ClientEvent::TunnelListening { listen_port, .. } = events.recv().await.unwrap() {
                return listen_port;
            }
        }
    })
    .await
    .expect("no TunnelListening event");
    assert!(listen_port > 0);

    // A local "app" connects through the forwarded port.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut app = tokio::net::TcpStream::connect(("127.0.0.1", listen_port))
        .await
        .expect("local tunnel connect");
    app.write_all(PAYLOAD).await.unwrap();

    // Echo comes back through the session.
    let mut buf = [0u8; PAYLOAD.len()];
    tokio::time::timeout(Duration::from_secs(10), app.read_exact(&mut buf))
        .await
        .expect("echo timed out")
        .expect("read failed");
    assert_eq!(&buf, PAYLOAD, "echo mismatch");

    // Host answered the open with success.
    let opened = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let ClientEvent::TunnelOpened { ok, error, .. } = events.recv().await.unwrap() {
                return (ok, error);
            }
        }
    })
    .await
    .expect("no TunnelOpened event");
    assert!(opened.0, "tunnel must open: {:?}", opened.1);

    // The server closes → host-side EOF → TunnelClosed event.
    let closed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let ClientEvent::TunnelClosed { id } = events.recv().await.unwrap() {
                return id;
            }
        }
    })
    .await
    .expect("no TunnelClosed event");
    assert!(closed > 0);

    // Local app hangs up cleanly; the session stays alive (the pump
    // completed the half-close handshake both ways).
    drop(app);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Stopping the listener removes it.
    cmd_tx.send(Command::TunnelStop { listen_port }).unwrap();
    // A new connect to the stopped port must fail.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", listen_port))
            .await
            .is_err(),
        "listener must be closed after TunnelStop"
    );

    drop(runner);
    drop(cmd_tx);
    host_task.abort();
}

/// A view-only host never announces tunnels, and the viewer-side guard
/// refuses `TunnelListen` locally (no listener, explicit notice).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tunnel_refused_in_view_only() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let host_task = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let cfg = SessionConfig {
            password: Some(PASSWORD.into()),
            display_name: "e2e-host".into(),
            fps: 60,
            jpeg_quality: 80,
            scale_percent: 100,
            download_dir: std::env::temp_dir(),
            auto_accept_files: true,
            codec: opacusr_agent::session::CodecChoice::Auto,
            view_only: true,
            clipboard_sync: true,
            adaptive_quality: false,
            audio: false,
            audio_device: String::new(),
            lock_input: false,
            totp_secret: None,
            record: false,
            record_dir: std::env::temp_dir(),
            tunnel: TunnelPolicy::Loopback,
            auth_gate: None,
        };
        let (ev_tx, _ev_rx) = mpsc::unbounded_channel();
        let (_cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let session = SecureSession::new(Box::new(TcpTransport::from_stream(sock)), None);
        SessionHost {
            cfg,
            events: ev_tx,
            commands: cmd_rx,
        }
        .run(
            session,
            Box::new(TestSource::new(320, 200)),
            Box::new(opacusr_input::RecordingInjector::new()),
        )
        .await;
    });

    let (client, mut events) = SessionClient::connect_direct(
        &addr,
        SessionSettings {
            password: PASSWORD.into(),
            display_name: "e2e-viewer".into(),
            download_dir: std::env::temp_dir(),
            auto_accept_files: true,
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let runner = tokio::spawn(client.run(cmd_rx));

    let ev = tokio::time::timeout(Duration::from_secs(15), events.recv())
        .await
        .unwrap()
        .unwrap();
    let ClientEvent::Connected { tunnel, .. } = ev else {
        panic!("expected Connected, got {ev:?}");
    };
    assert!(!tunnel, "view-only sessions must not announce tunnels");

    // The viewer-side guard refuses before anything hits the wire.
    cmd_tx
        .send(Command::TunnelListen {
            listen_port: 0,
            target_host: "127.0.0.1".into(),
            target_port: 80,
        })
        .unwrap();
    let refused = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await.unwrap() {
                ClientEvent::Notice(t) => return t,
                ClientEvent::Frame { .. } => {}
                _ => {}
            }
        }
    })
    .await
    .expect("no refusal notice");
    assert!(
        refused.contains("tunnel"),
        "expected a tunnel refusal notice, got: {refused}"
    );

    drop(runner);
    drop(cmd_tx);
    host_task.abort();
}
