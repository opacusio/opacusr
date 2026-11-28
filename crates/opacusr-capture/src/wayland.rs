//! Linux/Wayland backend: xdg-desktop-portal ScreenCast + PipeWire.
//!
//! Experimental, behind the `pipewire` cargo feature (not default: it
//! needs `libpipewire-0.3-dev` + `libspa-0.2-dev` at build time):
//!
//! ```sh
//! cargo build --release --features pipewire
//! ```
//!
//! Flow: the agent asks the desktop portal (org.freedesktop.portal.ScreenCast)
//! for monitors — the consent dialog multi-selects them — then consumes
//! one PipeWire stream per picked monitor (BGRx/RGBA frames, raw memory
//! buffers). Everything runs on a dedicated thread so the sync
//! [`ScreenCapture`] trait stays thread-safe and the session future `Send`.
//!
//! Multi-monitor: every negotiated stream is a selectable screen (ids are
//! the PipeWire node ids, stable within the capture instance); `capture()`
//! serves the latest frame of the *selected* node and `select_screen`
//! switches instantly between the live streams — no renegotiation, no
//! second consent dialog. The output layout (wl_output + xdg-output probe,
//! see [`crate::wayland_layout`]) names the screens and feeds
//! [`ScreenCapture::output_mapping`], the per-output translation the
//! uinput injector needs on compositors that map absolute pointers over
//! the whole desktop.
//!
//! Frames arrive only when the desktop changes, exactly like DXGI
//! duplication: idle timeouts re-serve the last frame. When the portal is
//! unavailable (headless, X11 session), [`platform_capture`] falls back to
//! the X11 backend (XWayland covers most apps).
//!
//! The streams are negotiated with `CursorMode::Metadata`, so the
//! compositor attaches a `spa_meta_cursor` to every buffer (position +
//! inline bitmap when the sprite changes). Those updates land in a
//! per-node slot of the shared [`PwSessionState`]; [`PwCursor`] — the
//! session's `CursorSource` on Wayland — reads the *selected* node's slot,
//! closing the cursor gap with the X11 and Windows backends.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use pipewire as pw;

use crate::cursor::{CursorBitmap, CursorPosition, CursorSource};
use crate::cursor_meta::{
    bitmap_to_rgba, parse_cursor_meta, CursorMeta, CURSOR_BITMAP_MAX_BYTES, SPA_META_CURSOR,
};
use crate::source::{output_mapping_from_layout, OutputInfo, ScreenCapture, ScreenDesc};
use crate::wayland_layout;
use crate::{bgra_to_rgba, CaptureError, Frame};
use opacusr_proto::message::OutputMapping;

/// One portal-negotiated monitor stream.
#[derive(Debug, Clone)]
struct PwStreamInfo {
    /// PipeWire node id — the screen id on the wire.
    node: u32,
    /// Negotiated stream size (device px); refreshed on mode changes.
    w: u32,
    h: u32,
    /// Screen name from the layout probe (`DP-1`, monitor model, …).
    name: String,
}

/// What the worker thread sends the capture object.
enum PwMsg {
    /// Portal negotiation finished: the picked streams + probed layout.
    Streams {
        streams: Vec<PwStreamInfo>,
        layout: Vec<OutputInfo>,
    },
    /// Renegotiated stream geometry (mode change).
    Size {
        node: u32,
        w: u32,
        h: u32,
    },
    Frame {
        node: u32,
        frame: Frame,
    },
}

pub struct PipeWireCapture {
    rx: Receiver<PwMsg>,
    /// Latest frame per stream (key = node id); `capture()` serves the
    /// selected one so switching screens is instant.
    frames: HashMap<u32, Frame>,
    streams: Vec<PwStreamInfo>,
    layout: Vec<OutputInfo>,
    /// node id → index into `layout` (best-effort device-size match).
    matched: HashMap<u32, usize>,
    /// Node id streamed right now.
    selected: u32,
}

impl PipeWireCapture {
    /// Start the portal + PipeWire worker. Does not block on user consent —
    /// the portal dialog may sit unanswered while `capture()` keeps
    /// returning `Unsupported`/cached frames.
    pub fn connect() -> Result<Self, CaptureError> {
        // Make sure the shared cursor slot exists as soon as a portal
        // capture is created, so `platform_cursor` and the worker agree on
        // the same state no matter which is constructed first.
        let _ = pw_session();
        let (tx, rx) = channel::<PwMsg>();
        std::thread::Builder::new()
            .name("opacusr-pw".into())
            .spawn(move || {
                if let Err(e) = run_screencast(tx) {
                    tracing::warn!(target: "opacusr", "pipewire capture: {e}");
                }
            })
            .map_err(|e| CaptureError::Other(format!("pipewire thread: {e}")))?;
        Ok(Self {
            rx,
            frames: HashMap::new(),
            streams: Vec::new(),
            layout: Vec::new(),
            matched: HashMap::new(),
            selected: 0,
        })
    }

    /// Fold every pending worker message into the caches.
    fn drain(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                PwMsg::Streams { streams, layout } => {
                    self.frames
                        .retain(|node, _| streams.iter().any(|s| s.node == *node));
                    self.streams = streams;
                    self.layout = layout;
                    self.match_outputs();
                    if let Some(first) = self.streams.first() {
                        self.selected = first.node;
                        set_shared_selected(first.node);
                    }
                }
                PwMsg::Size { node, w, h } => {
                    if let Some(s) = self.streams.iter_mut().find(|s| s.node == node) {
                        s.w = w;
                        s.h = h;
                    }
                }
                PwMsg::Frame { node, frame } => {
                    self.frames.insert(node, frame);
                }
            }
        }
    }

    /// Name the streams and match each one to a probed output. The
    /// compositor streams an output upright at its native mode size, so
    /// device size is the (best-effort, first-unclaimed-wins) match key;
    /// identical monitors may swap — harmless for the mapping math, which
    /// only needs *an* output of that size.
    fn match_outputs(&mut self) {
        self.matched.clear();
        let mut claimed: Vec<usize> = Vec::new();
        for (n, s) in self.streams.iter_mut().enumerate() {
            s.name = format!("Monitor {}", n + 1);
            let hit =
                self.layout.iter().enumerate().find(|(idx, o)| {
                    o.device_w == s.w && o.device_h == s.h && !claimed.contains(idx)
                });
            if let Some((idx, o)) = hit {
                claimed.push(idx);
                self.matched.insert(s.node, idx);
                if !o.name.is_empty() {
                    s.name = o.name.clone();
                }
            }
        }
    }
}

impl ScreenCapture for PipeWireCapture {
    fn capture(&mut self) -> Result<Frame, CaptureError> {
        self.drain();
        match self.frames.get(&self.selected) {
            Some(f) => Ok(f.clone()),
            None => Err(CaptureError::Unsupported),
        }
    }

    fn size(&mut self) -> Result<(u32, u32), CaptureError> {
        self.drain();
        match self.streams.iter().find(|s| s.node == self.selected) {
            Some(s) if s.w > 0 && s.h > 0 => Ok((s.w, s.h)),
            Some(s) => self
                .frames
                .get(&s.node)
                .map(|f| (f.width, f.height))
                .ok_or(CaptureError::Unsupported),
            None => Err(CaptureError::Unsupported),
        }
    }

    fn screens(&mut self) -> Vec<ScreenDesc> {
        self.drain();
        self.streams
            .iter()
            .map(|s| ScreenDesc {
                id: s.node,
                name: s.name.clone(),
                w: s.w,
                h: s.h,
            })
            .collect()
    }

    fn select_screen(&mut self, id: u32) -> Result<bool, CaptureError> {
        self.drain();
        if !self.streams.iter().any(|s| s.node == id) {
            return Err(CaptureError::Other(format!(
                "no portal stream for screen {id}"
            )));
        }
        if self.selected == id {
            return Ok(false);
        }
        self.selected = id;
        set_shared_selected(id);
        Ok(true)
    }

    fn output_mapping(&mut self) -> Option<OutputMapping> {
        self.drain();
        let idx = *self.matched.get(&self.selected)?;
        output_mapping_from_layout(&self.layout, idx)
    }

    fn backend(&self) -> &'static str {
        "pipewire"
    }
}

// ---------------------------------------------------------------------------
// Cursor metadata (portal CursorMode::Metadata)
// ---------------------------------------------------------------------------

/// Per-stream cursor state, written by the PipeWire worker threads and
/// read by [`PwCursor`] for the selected node.
#[derive(Default)]
struct PwCursorState {
    /// Last reported position (hotspot in stream pixels).
    pos: Option<CursorPosition>,
    /// Current sprite; keyed by `serial`.
    bitmap: Option<CursorBitmap>,
    /// Bumped whenever the sprite bitmap changes (compositor-independent:
    /// we diff the converted RGBA, not the metadata `id`).
    serial: u64,
    /// `false` after the compositor signals an empty sprite or parks the
    /// cursor at (-1, -1).
    visible: bool,
}

/// Shared session state: which node is streamed (the capture object owns
/// the truth, but [`PwCursor`] is constructed independently) + per-node
/// cursor metadata. Static because `platform_capture` and `platform_cursor`
/// are created separately — the same trick the X11/Windows cursor backends
/// use for their globals.
#[derive(Default)]
struct PwSessionState {
    selected: u32,
    cursors: HashMap<u32, PwCursorState>,
}

static PW_SESSION: OnceLock<Arc<Mutex<PwSessionState>>> = OnceLock::new();

/// The shared session slot (created on first use).
fn pw_session() -> Arc<Mutex<PwSessionState>> {
    PW_SESSION
        .get_or_init(|| Arc::new(Mutex::new(PwSessionState::default())))
        .clone()
}

/// The worker tells everyone which node is streamed now.
fn set_shared_selected(node: u32) {
    if let Ok(mut st) = pw_session().lock() {
        st.selected = node;
    }
}

/// `CursorSource` fed by the selected PipeWire stream's cursor metadata.
/// Always succeeds; `poll` yields `None` until that stream has produced
/// its first cursor update.
pub struct PwCursor;

impl PwCursor {
    /// Create the Wayland cursor poller.
    pub fn new() -> Self {
        Self
    }
}

impl Default for PwCursor {
    fn default() -> Self {
        Self::new()
    }
}

impl CursorSource for PwCursor {
    fn poll(&mut self) -> Option<CursorPosition> {
        let st = pw_session();
        let g = st.lock().ok()?;
        g.cursors.get(&g.selected)?.pos
    }

    fn shape(&mut self, serial: u64) -> Option<CursorBitmap> {
        let st = pw_session();
        let g = st.lock().ok()?;
        let cur = g.cursors.get(&g.selected)?;
        if cur.serial == serial {
            cur.bitmap.clone()
        } else {
            None
        }
    }

    fn backend(&self) -> &'static str {
        "pipewire-cursor"
    }
}

/// Extract the `spa_meta_cursor` of one dequeued buffer and fold it into
/// the shared state. The safe `Buffer` wrapper exposes only the data
/// planes, so the containing `spa_buffer` (and with it the metadata
/// array) is reached from element 0 of the `datas` array via the
/// (computed) offset of the `datas` field.
fn update_cursor_from_buffer(node: u32, datas: &[pipewire::spa::buffer::Data]) {
    use pipewire::spa::sys::{spa_buffer, spa_meta};

    let Some(first) = datas.first() else {
        return;
    };
    let probe = std::mem::MaybeUninit::<spa_buffer>::zeroed();
    let datas_off =
        unsafe { std::ptr::addr_of!((*probe.as_ptr()).datas) as usize } - probe.as_ptr() as usize;
    let Some(base) = (std::ptr::from_ref(first) as usize).checked_sub(datas_off) else {
        return;
    };
    if base == 0 {
        return;
    }
    // SAFETY: `first` points at element 0 of this buffer's `datas` array,
    // so `base` is the start of the `spa_buffer` owning it; both stay
    // valid for the duration of the process callback.
    let sbuf = unsafe { &*(base as *const spa_buffer) };
    if sbuf.n_metas == 0 || sbuf.n_metas > 64 || sbuf.metas.is_null() {
        return;
    }
    // SAFETY: `metas` is `n_metas` valid `spa_meta` entries.
    let metas =
        unsafe { std::slice::from_raw_parts(sbuf.metas as *const spa_meta, sbuf.n_metas as usize) };
    for m in metas {
        if m.type_ != SPA_META_CURSOR || m.data.is_null() || m.size == 0 {
            continue;
        }
        let len = (m.size as usize).min(CURSOR_BITMAP_MAX_BYTES);
        // SAFETY: `m.data` covers `m.size` writable bytes of this buffer's
        // metadata region.
        let bytes = unsafe { std::slice::from_raw_parts(m.data as *const u8, len) };
        if let Some(cur) = parse_cursor_meta(bytes) {
            if cur.id == 0 {
                continue; // "an id of 0 is an invalid id and means that
                          // there is no new cursor data"
            }
            apply_cursor_meta(node, cur);
        }
    }
}

fn apply_cursor_meta(node: u32, cur: CursorMeta<'_>) {
    let slot = pw_session();
    let Ok(mut g) = slot.lock() else {
        return;
    };
    let st = g.cursors.entry(node).or_default();

    if let Some(bm) = &cur.bitmap {
        match bm.pixels {
            Some(pixels) => {
                if let Some(rgba) =
                    bitmap_to_rgba(bm.format, bm.width, bm.height, bm.stride, pixels)
                {
                    let bitmap = CursorBitmap {
                        width: bm.width,
                        height: bm.height,
                        hotspot_x: cur.hotspot_x.clamp(0, bm.width as i32) as u32,
                        hotspot_y: cur.hotspot_y.clamp(0, bm.height as i32) as u32,
                        rgba,
                    };
                    if st.bitmap.as_ref() != Some(&bitmap) {
                        st.serial = st.serial.wrapping_add(1);
                        st.bitmap = Some(bitmap);
                    }
                    st.visible = true;
                }
            }
            // Valid bitmap header without image data: the cursor became
            // invisible (compositor's "empty sprite").
            None => st.visible = false,
        }
    }

    let visible = st.visible && !(cur.x == -1 && cur.y == -1);
    st.pos = Some(CursorPosition {
        x: cur.x,
        y: cur.y,
        visible,
        serial: st.serial,
    });
}

// ---------------------------------------------------------------------------
// Worker: portal handshake + one PipeWire stream per monitor
// ---------------------------------------------------------------------------

fn run_screencast(tx: Sender<PwMsg>) -> Result<(), String> {
    pipewire::init();

    // Probe the compositor layout BEFORE the consent dialog: it names the
    // screens and drives the per-output input mapping, and it must not
    // wait behind a dialog the user may leave unanswered for minutes.
    let layout = wayland_layout::probe_outputs();

    let (fd, streams) = portal_negotiate()?;
    tracing::info!(target: "opacusr", "portal streams: {}",
        streams.iter().map(|s| format!("[{}] {} {}x{}", s.node, s.name, s.w, s.h)).collect::<Vec<_>>().join(", "));
    let _ = tx.send(PwMsg::Streams {
        streams: streams.clone(),
        layout,
    });

    pipewire_loop(tx, fd, &streams)
}

/// Ask xdg-desktop-portal for monitor streams. Blocks on the user's
/// consent dialog (the portal UI) — with multi-select, every picked
/// monitor comes back as its own stream. Returns (fd, per-monitor info).
fn portal_negotiate() -> Result<(std::os::fd::OwnedFd, Vec<PwStreamInfo>), String> {
    use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
    use ashpd::desktop::PersistMode;
    use ashpd::WindowIdentifier;

    async fn inner() -> Result<(std::os::fd::OwnedFd, Vec<PwStreamInfo>), String> {
        let proxy = Screencast::new()
            .await
            .map_err(|e| format!("portal connect: {e}"))?;
        let session = proxy
            .create_session()
            .await
            .map_err(|e| format!("portal session: {e}"))?;
        proxy
            .select_sources(
                &session,
                // Metadata mode: the compositor attaches spa_meta_cursor
                // to every buffer, giving us the real pointer even though
                // capture APIs see none.
                CursorMode::Metadata,
                SourceType::Monitor.into(),
                // Multiple: the dialog lists every monitor; each picked
                // one becomes its own stream + selectable screen.
                true,
                None,
                PersistMode::DoNot,
            )
            .await
            .map_err(|e| format!("portal select: {e}"))?;
        let response = proxy
            .start(&session, &WindowIdentifier::default())
            .await
            .map_err(|e| format!("portal start: {e}"))?
            .response()
            .map_err(|e| format!("portal response: {e}"))?;
        let streams: Vec<PwStreamInfo> = response
            .streams()
            .iter()
            .map(|s| {
                let (w, h) = s.size().unwrap_or((0, 0));
                PwStreamInfo {
                    node: s.pipe_wire_node_id(),
                    w: w.max(0) as u32,
                    h: h.max(0) as u32,
                    name: String::new(),
                }
            })
            .filter(|s| s.node != 0)
            .collect();
        if streams.is_empty() {
            return Err("portal returned no streams".to_string());
        }
        let fd = proxy
            .open_pipe_wire_remote(&session)
            .await
            .map_err(|e| format!("portal fd: {e}"))?;
        Ok((fd, streams))
    }

    async_std::task::block_on(inner())
}

/// User data of one PipeWire stream (one per picked monitor).
struct PwUserData {
    tx: Sender<PwMsg>,
    node: u32,
    format: pw::spa::param::video::VideoInfoRaw,
    negotiated: bool,
}

fn pipewire_loop(
    tx: Sender<PwMsg>,
    fd: std::os::fd::OwnedFd,
    streams: &[PwStreamInfo],
) -> Result<(), String> {
    use pw::properties::properties;
    use pw::spa::param::format::{FormatProperties, MediaSubtype};
    use pw::spa::param::format_utils;
    use pw::spa::param::video::VideoFormat;
    use pw::spa::param::ParamType;
    use pw::spa::pod::Pod;
    use pw::spa::pod::{object, property};
    use pw::spa::utils::{Direction, SpaTypes};
    use pw::stream::StreamFlags;

    let mainloop = pw::main_loop::MainLoop::new(None).map_err(|e| format!("mainloop: {e}"))?;
    let context = pw::context::Context::new(&mainloop).map_err(|e| format!("context: {e}"))?;
    let core = context
        .connect_fd(fd, None)
        .map_err(|e| format!("connect fd: {e}"))?;

    // One pw stream per negotiated monitor — all live at once, so
    // switching screens never renegotiates with the portal.
    let mut alive: Vec<(pw::stream::Stream, pw::stream::StreamListener<PwUserData>)> = Vec::new();
    for s in streams {
        let stream = pw::stream::Stream::new(
            &core,
            &format!("opacusr-screencast-{}", s.node),
            properties! {
                *pw::keys::MEDIA_TYPE => "Video",
                *pw::keys::MEDIA_CATEGORY => "Capture",
                *pw::keys::MEDIA_ROLE => "Screen",
            },
        )
        .map_err(|e| format!("stream {}: {e}", s.node))?;

        let listener = stream
            .add_local_listener_with_user_data(PwUserData {
                tx: tx.clone(),
                node: s.node,
                format: Default::default(),
                negotiated: false,
            })
            .state_changed(|_, _, old, new| {
                tracing::debug!(target: "opacusr", "pw stream: {old:?} → {new:?}");
                if let pw::stream::StreamState::Error(e) = new {
                    tracing::warn!(target: "opacusr", "pw stream error: {e}");
                }
            })
            .param_changed(|_, ud, id, param| {
                if id != ParamType::Format.as_raw() {
                    return;
                }
                let Some(param) = param else { return };
                let Ok((media_type, media_subtype)) = format_utils::parse_format(param) else {
                    return;
                };
                if media_type != pw::spa::param::format::MediaType::Video
                    || media_subtype != MediaSubtype::Raw
                {
                    return;
                }
                if ud.format.parse(param).is_err() {
                    tracing::warn!(target: "opacusr", "pw: failed to parse video format");
                    return;
                }
                ud.negotiated = true;
                tracing::info!(
                    target: "opacusr",
                    "pw stream {}: format {:?} {}×{}",
                    ud.node,
                    ud.format.format().as_raw(),
                    ud.format.size().width,
                    ud.format.size().height
                );
                // Mode changes resize the stream — the capture object
                // tracks per-node sizes from these.
                let _ = ud.tx.send(PwMsg::Size {
                    node: ud.node,
                    w: ud.format.size().width,
                    h: ud.format.size().height,
                });
            })
            .process(|stream, ud| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let datas = buffer.datas_mut();
                if datas.is_empty() || !ud.negotiated {
                    return;
                }
                // Cursor metadata rides on the same buffer; the frame path
                // below only looks at `datas[0]`, so read the metadata first.
                update_cursor_from_buffer(ud.node, datas);
                let data = &mut datas[0];
                let w = ud.format.size().width;
                let h = ud.format.size().height;
                if w == 0 || h == 0 {
                    return;
                }
                let stride = {
                    let s = data.chunk().stride();
                    if s > 0 {
                        s as usize
                    } else {
                        (w as usize) * 4
                    }
                };
                let expected = stride * h as usize;
                let Some(bytes) = data.data() else {
                    return; // DmaBuf not supported in this backend
                };
                if bytes.len() < expected {
                    return;
                }

                let Some(rgba) = frame_rgba(ud.format.format(), w, h, stride, bytes) else {
                    // YUV planar formats are a roadmap item.
                    return;
                };
                let _ = ud.tx.send(PwMsg::Frame {
                    node: ud.node,
                    frame: Frame::new(w, h, rgba),
                });
            })
            .register()
            .map_err(|e| format!("listener {}: {e}", s.node))?;

        // Offer the raw formats we can consume, preferring BGRx
        // (compositors hand that out natively), at the negotiated size.
        let (w, h) = (s.w.max(1), s.h.max(1));
        let obj = object!(
            SpaTypes::ObjectParamFormat,
            ParamType::EnumFormat,
            property!(
                FormatProperties::MediaType,
                Id,
                pw::spa::param::format::MediaType::Video
            ),
            property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
            property!(
                FormatProperties::VideoFormat,
                Choice,
                Enum,
                Id,
                VideoFormat::BGRx,
                VideoFormat::BGRx,
                VideoFormat::RGBA,
                VideoFormat::RGBx,
                VideoFormat::RGB
            ),
            property!(
                FormatProperties::VideoSize,
                Choice,
                Range,
                Rectangle,
                pw::spa::utils::Rectangle {
                    width: w,
                    height: h
                },
                pw::spa::utils::Rectangle {
                    width: 1,
                    height: 1
                },
                pw::spa::utils::Rectangle {
                    width: 7680,
                    height: 4320
                }
            ),
            property!(
                FormatProperties::VideoFramerate,
                Choice,
                Range,
                Fraction,
                pw::spa::utils::Fraction { num: 30, denom: 1 },
                pw::spa::utils::Fraction { num: 0, denom: 1 },
                pw::spa::utils::Fraction { num: 60, denom: 1 }
            ),
        );
        let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
            std::io::Cursor::new(Vec::new()),
            &pw::spa::pod::Value::Object(obj),
        )
        .map_err(|e| format!("pod: {e}"))?
        .0
        .into_inner();
        let mut params = [Pod::from_bytes(&values).ok_or("bad pod")?];

        stream
            .connect(
                Direction::Input,
                Some(s.node),
                StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
                &mut params,
            )
            .map_err(|e| format!("connect {}: {e}", s.node))?;

        alive.push((stream, listener));
    }

    // The per-stream listeners and senders must stay alive; the capture
    // object holds `rx` for the session thread.
    drop(tx);

    mainloop.run();
    Ok(())
}

/// Convert one raw buffer into RGBA bytes per the negotiated format.
/// `None` for formats we cannot consume yet (YUV planar — roadmap).
fn frame_rgba(
    fmt: pw::spa::param::video::VideoFormat,
    w: u32,
    h: u32,
    stride: usize,
    bytes: &[u8],
) -> Option<Vec<u8>> {
    use pw::spa::param::video::VideoFormat;

    let expected = stride * h as usize;
    match fmt {
        VideoFormat::BGRA | VideoFormat::BGRx => {
            let mut v = bytes[..expected].to_vec();
            bgra_to_rgba(&mut v);
            Some(v)
        }
        VideoFormat::RGBA | VideoFormat::RGBx => {
            // RGBx: alpha byte may be garbage → force opaque.
            let mut v = bytes[..expected].to_vec();
            for px in v.as_chunks_mut::<4>().0 {
                px[3] = 255;
            }
            Some(v)
        }
        VideoFormat::RGB => {
            let mut v = Vec::with_capacity((w as usize) * (h as usize) * 4);
            for row in bytes[..expected].chunks_exact(stride) {
                for px in row[..(w as usize) * 3].as_chunks::<3>().0 {
                    v.extend_from_slice(&[px[0], px[1], px[2], 255]);
                }
            }
            Some(v)
        }
        _ => None,
    }
}
