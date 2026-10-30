//! One host session: handshake → capabilities → stream/input/files/terminal
//! pump until `Bye` or error.

use std::collections::HashMap;
use std::path::PathBuf;

use tokio::sync::mpsc;

use opacusr_capture::{resize_nearest, ScreenCapture};
use opacusr_codec::make_pair;
use opacusr_input::InputInjector;
use opacusr_proto::message::{self, ByeReason, CodecId, SessionAccept};
use opacusr_proto::{Message, Platform};
use opacusr_transport::auth::host_handshake;
use opacusr_transport::ft::{FtEngine, FtEvent};
use opacusr_transport::secure::SecureSession;
use opacusr_transport::TransportError;

/// Tunables for one host session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub password: String,
    pub display_name: String,
    pub fps: u8,
    pub jpeg_quality: u8,
    pub scale_percent: u16,
    pub download_dir: PathBuf,
    pub auto_accept_files: bool,
}

/// Session lifecycle events for the UI / CLI.
#[derive(Debug, Clone)]
pub enum HostEvent {
    Started { viewer: String, platform: Platform },
    Ended { reason: String },
    Chat(String),
    Ft(FtEvent),
    Notice(String),
}

/// Everything needed to run one session.
pub struct SessionHost {
    pub cfg: SessionConfig,
    pub events: mpsc::UnboundedSender<HostEvent>,
}

impl SessionHost {
    /// Drive the session to completion. `capture` and `injector` are
    /// created by the caller so tests can plug mocks in.
    pub async fn run(
        self,
        mut session: SecureSession,
        mut capture: Box<dyn ScreenCapture>,
        mut injector: Box<dyn InputInjector>,
    ) {
        let cfg = self.cfg;
        let events = self.events;

        let outcome = run_session(
            &cfg,
            &events,
            &mut session,
            capture.as_mut(),
            injector.as_mut(),
        )
        .await;

        // Politely close (best effort — the wire may already be gone).
        let _ = session.send_msg(&Message::Bye(ByeReason::Normal)).await;
        session.close().await;

        let reason = match outcome {
            Ok(r) => r,
            Err(e) => format!("{e}"),
        };
        let _ = events.send(HostEvent::Ended { reason });
    }
}

async fn run_session(
    cfg: &SessionConfig,
    events: &mpsc::UnboundedSender<HostEvent>,
    session: &mut SecureSession,
    capture: &mut dyn ScreenCapture,
    injector: &mut dyn InputInjector,
) -> Result<String, TransportError> {
    // --- handshake ---------------------------------------------------------
    let auth = host_handshake(session, Some(&cfg.password), &cfg.display_name).await?;
    let _ = events.send(HostEvent::Started {
        viewer: auth.viewer_hello.display_name.clone(),
        platform: auth.viewer_hello.platform,
    });

    // --- capabilities ------------------------------------------------------
    let (mut cur_w, mut cur_h) = capture.size().unwrap_or((1280, 720));
    let accept_w = cur_w;
    let accept_h = cur_h;
    session
        .send_msg(&Message::SessionAccept(SessionAccept {
            screen_w: accept_w,
            screen_h: accept_h,
            codec: CodecId::Jpeg,
            fps_hint: cfg.fps,
        }))
        .await?;

    let (mut encoder, _decoder) = make_pair(CodecId::Jpeg);
    let mut ft = FtEngine::new();
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let mut terms: HashMap<u32, crate::pty::TermSession> = HashMap::new();
    #[allow(unused_mut)]
    let mut next_term_id: u32 = 1;

    let mut fps = cfg.fps.max(1);
    let mut quality = cfg.jpeg_quality.clamp(1, 100);
    let mut scale = cfg.scale_percent.clamp(10, 100);
    let mut paused = false;
    let mut seq: u64 = 0;

    // Outbound queue (terminal output, engine replies, ...).
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(1000 / fps as u64));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    #[allow(unused_assignments)]
    let mut reason = String::from("session ended");

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if paused { continue; }
                match capture.capture() {
                    Ok(frame) => {
                        cur_w = frame.width;
                        cur_h = frame.height;
                        let frame = if scale < 100 {
                            resize_nearest(&frame, frame.width * scale as u32 / 100, frame.height * scale as u32 / 100)
                        } else { frame };
                        match encoder.encode(&frame, quality) {
                            Ok(data) => {
                                seq += 1;
                                let msg = Message::VideoFrame(message::VideoFrame {
                                    seq,
                                    width: frame.width,
                                    height: frame.height,
                                    keyframe: true,
                                    codec: CodecId::Jpeg,
                                    data,
                                });
                                session.send_msg(&msg).await?;
                            }
                            Err(e) => { let _ = events.send(HostEvent::Notice(format!("encode: {e}"))); }
                        }
                    }
                    Err(e) => { let _ = events.send(HostEvent::Notice(format!("capture: {e}"))); }
                }
            }
            msg = session.recv_msg() => {
                match msg? {
                    Message::MouseMove(m) => {
                        let (x, y) = rescale(m.x, m.y, accept_w, accept_h, cur_w, cur_h);
                        let _ = injector.mouse_move(x, y);
                    }
                    Message::MouseButton(m) => {
                        let (x, y) = rescale(m.x, m.y, accept_w, accept_h, cur_w, cur_h);
                        let _ = injector.mouse_button(x, y, m.button, m.down);
                    }
                    Message::MouseWheel(w) => {
                        let _ = injector.wheel(w.dx, w.dy);
                    }
                    Message::KeyEvent(k) => {
                        if let Err(e) = injector.key(k.key, k.mods, k.down) {
                            tracing::debug!(target: "opacusr", "key injection: {e}");
                        }
                    }
                    Message::TextInput(t) => {
                        let _ = injector.text(&t.text);
                    }
                    Message::ChatMessage(c) => {
                        let _ = events.send(HostEvent::Chat(c.text));
                    }
                    Message::ClipboardText(c) => set_clipboard(&c.text),
                    Message::QualitySet(q) => {
                        let old_fps = fps;
                        fps = q.fps.max(1);
                        quality = q.jpeg_quality.clamp(1, 100);
                        scale = q.scale_percent.clamp(10, 100);
                        if fps != old_fps {
                            ticker = tokio::time::interval(std::time::Duration::from_millis(1000 / fps as u64));
                            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                        }
                    }
                    Message::Pause(p) => paused = p,
                    Message::Ping(n) => session.send_msg(&Message::Pong(n)).await?,
                    Message::Bye(_) => { reason = "viewer disconnected".into(); break; }
                    other => {
                        // File transfer + terminal messages.
                        let (mut replies, ft_events) = ft.on_message(&other, &cfg.download_dir, cfg.auto_accept_files);
                        for ev in ft_events {
                            let _ = events.send(HostEvent::Ft(ev));
                        }
                        for r in replies.drain(..) {
                            session.send_msg(&r).await?;
                        }
                        // Our outgoing pushes advance after every message.
                        for r in ft.pump() {
                            session.send_msg(&r).await?;
                        }

                        #[cfg(not(any(target_os = "android", target_os = "ios")))]
                        match other {
                            Message::TermOpen(t) => {
                                let id = next_term_id;
                                next_term_id += 1;
                                let (res, opened, out_rx_term) =
                                    crate::pty::TermSession::spawn(id, t.rows, t.cols, None);
                                match res {
                                    Ok(term) => {
                                        let tx = out_tx.clone();
                                        tokio::spawn(async move {
                                            let mut out_rx_term = out_rx_term;
                                            while let Some(mut data) = out_rx_term.recv().await {
                                                data.id = id;
                                                if tx.send(Message::TermData(data)).is_err() { break; }
                                            }
                                        });
                                        terms.insert(id, term);
                                    }
                                    Err(_) => { /* reply already carries the error */ }
                                }
                                session.send_msg(&Message::TermOpened(opened)).await?;
                            }
                            Message::TermData(t) => {
                                if let Some(term) = terms.get(&t.id) {
                                    if let Err(e) = term.write(&t.data) {
                                        let _ = events.send(HostEvent::Notice(format!("term write: {e}")));
                                    }
                                }
                            }
                            Message::TermResize(t) => {
                                if let Some(term) = terms.get(&t.id) {
                                    let _ = term.resize(t.rows, t.cols);
                                }
                            }
                            Message::TermClose(t) => {
                                if let Some(term) = terms.remove(&t.id) {
                                    term.kill();
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Some(msg) = out_rx.recv() => {
                session.send_msg(&msg).await?;
            }
        }
    }

    // Cleanup terminals.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    for (_, t) in terms.drain() {
        t.kill();
    }
    Ok(reason)
}

/// Map viewer coordinates (reported-resolution space) onto the current
/// physical screen size.
fn rescale(x: u32, y: u32, aw: u32, ah: u32, cw: u32, ch: u32) -> (u32, u32) {
    if aw == 0 || ah == 0 || (aw == cw && ah == ch) {
        return (x, y);
    }
    (
        ((x as u64 * cw as u64) / aw as u64) as u32,
        ((y as u64 * ch as u64) / ah as u64) as u32,
    )
}

fn set_clipboard(text: &str) {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_string())) {
            Ok(_) => {}
            Err(e) => tracing::debug!(target: "opacusr", "clipboard set failed: {e}"),
        }
    }
}
