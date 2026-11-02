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
//! for a monitor — the user picks one in the system dialog — then consumes
//! the PipeWire stream the compositor publishes (BGRx/RGBA frames, raw
//! memory buffers). Everything runs on a dedicated thread so the sync
//! [`ScreenCapture`] trait stays thread-safe and the session future `Send`.
//!
//! Frames arrive only when the desktop changes, exactly like DXGI
//! duplication: idle timeouts re-serve the last frame. When the portal is
//! unavailable (headless, X11 session), [`platform_capture`] falls back to
//! the X11 backend (XWayland covers most apps).
//!
//! The stream is negotiated with `CursorMode::Metadata`, so the compositor
//! attaches a `spa_meta_cursor` to every buffer (position + inline bitmap
//! when the sprite changes). Those updates land in a shared
//! [`PwCursorState`]; [`PwCursor`] — the session's `CursorSource` on
//! Wayland — reads them from there, closing the cursor gap with the X11
//! and Windows backends.

use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, OnceLock};

use crate::cursor::{CursorBitmap, CursorPosition, CursorSource};
use crate::cursor_meta::{
    bitmap_to_rgba, parse_cursor_meta, CURSOR_BITMAP_MAX_BYTES, SPA_META_CURSOR,
};
use crate::source::{ScreenCapture, ScreenDesc};
use crate::{bgra_to_rgba, CaptureError, Frame};

/// What the worker thread sends the capture object.
enum PwMsg {
    Frame(Frame),
    /// Negotiated stream geometry (before the first frame).
    Size(u32, u32),
}

pub struct PipeWireCapture {
    rx: Receiver<PwMsg>,
    last: Option<Frame>,
    size_hint: (u32, u32),
}

impl PipeWireCapture {
    /// Start the portal + PipeWire worker. Does not block on user consent —
    /// the portal dialog may sit unanswered while `capture()` keeps
    /// returning `Unsupported`/cached frames.
    pub fn connect() -> Result<Self, CaptureError> {
        // Make sure the shared cursor slot exists as soon as a portal
        // capture is created, so `platform_cursor` and the worker agree on
        // the same state no matter which is constructed first.
        let _ = pw_cursor_state();
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
            last: None,
            size_hint: (0, 0),
        })
    }
}

impl ScreenCapture for PipeWireCapture {
    fn capture(&mut self) -> Result<Frame, CaptureError> {
        loop {
            match self.rx.try_recv() {
                Ok(PwMsg::Frame(f)) => {
                    self.size_hint = (f.width, f.height);
                    self.last = Some(f);
                    return Ok(self.last.clone().unwrap());
                }
                Ok(PwMsg::Size(w, h)) => self.size_hint = (w, h),
                Err(TryRecvError::Empty) => {
                    return self.last.clone().ok_or(CaptureError::Unsupported)
                }
                Err(TryRecvError::Disconnected) => {
                    return self.last.clone().ok_or_else(|| {
                        CaptureError::Other(
                            "pipewire worker exited (portal denied or unavailable)".into(),
                        )
                    });
                }
            }
        }
    }

    fn size(&mut self) -> Result<(u32, u32), CaptureError> {
        // Drain pending messages first so the hint is fresh.
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                PwMsg::Frame(f) => {
                    self.size_hint = (f.width, f.height);
                    self.last = Some(f);
                }
                PwMsg::Size(w, h) => self.size_hint = (w, h),
            }
        }
        if self.size_hint.0 > 0 {
            Ok(self.size_hint)
        } else {
            Err(CaptureError::Unsupported)
        }
    }

    fn screens(&mut self) -> Vec<ScreenDesc> {
        match self.size() {
            Ok((w, h)) => vec![ScreenDesc {
                id: 0,
                name: "Portal screencast".into(),
                w,
                h,
            }],
            Err(_) => Vec::new(),
        }
    }

    fn backend(&self) -> &'static str {
        "pipewire"
    }
}

// ---------------------------------------------------------------------------
// Cursor metadata (portal CursorMode::Metadata)
// ---------------------------------------------------------------------------

/// Latest Wayland cursor state, written by the PipeWire worker thread and
/// read by [`PwCursor`] on the session thread.
#[derive(Default)]
pub struct PwCursorState {
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

static PW_CURSOR_STATE: OnceLock<Arc<Mutex<PwCursorState>>> = OnceLock::new();

/// The shared cursor state slot (created on first use).
fn pw_cursor_state() -> Arc<Mutex<PwCursorState>> {
    PW_CURSOR_STATE
        .get_or_init(|| Arc::new(Mutex::new(PwCursorState::default())))
        .clone()
}

/// `CursorSource` fed by the PipeWire stream's cursor metadata. Always
/// succeeds; `poll` yields `None` until the portal capture has produced
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
        let state = pw_cursor_state();
        let st = state.lock().ok()?;
        st.pos
    }

    fn shape(&mut self, serial: u64) -> Option<CursorBitmap> {
        let state = pw_cursor_state();
        let st = state.lock().ok()?;
        if st.serial == serial {
            st.bitmap.clone()
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
fn update_cursor_from_buffer(datas: &[pipewire::spa::buffer::Data]) {
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
            apply_cursor_meta(cur);
        }
    }
}

fn apply_cursor_meta(cur: crate::cursor_meta::CursorMeta<'_>) {
    let state = pw_cursor_state();
    let Ok(mut st) = state.lock() else {
        return;
    };

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
// Worker: portal handshake + PipeWire stream
// ---------------------------------------------------------------------------

fn run_screencast(tx: Sender<PwMsg>) -> Result<(), String> {
    pipewire::init();

    let (fd, node, size) = portal_negotiate()?;
    let _ = tx.send(PwMsg::Size(size.0 as u32, size.1 as u32));
    tracing::info!(target: "opacusr", "portal stream: node {node}, {}×{}", size.0, size.1);

    pipewire_loop(tx, fd, node, size)
}

/// Ask xdg-desktop-portal for a monitor stream. Blocks on the user's
/// consent dialog (the portal UI). Returns (fd, node id, size).
fn portal_negotiate() -> Result<(std::os::fd::OwnedFd, u32, (i32, i32)), String> {
    use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
    use ashpd::desktop::PersistMode;
    use ashpd::WindowIdentifier;

    async fn inner() -> Result<(std::os::fd::OwnedFd, u32, (i32, i32)), String> {
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
                false,
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
        let stream = response
            .streams()
            .first()
            .cloned()
            .ok_or_else(|| "portal returned no streams".to_string())?;
        let node = stream.pipe_wire_node_id();
        let size = stream.size().unwrap_or((0, 0));
        let fd = proxy
            .open_pipe_wire_remote(&session)
            .await
            .map_err(|e| format!("portal fd: {e}"))?;
        Ok((fd, node, size))
    }

    async_std::task::block_on(inner())
}

fn pipewire_loop(
    tx: Sender<PwMsg>,
    fd: std::os::fd::OwnedFd,
    node: u32,
    size: (i32, i32),
) -> Result<(), String> {
    use pipewire as pw;
    use pw::properties::properties;
    use pw::spa::param::format::{FormatProperties, MediaSubtype};
    use pw::spa::param::format_utils;
    use pw::spa::param::video::VideoFormat;
    use pw::spa::param::ParamType;
    use pw::spa::pod::Pod;
    use pw::spa::pod::{object, property};
    use pw::spa::utils::{Direction, SpaTypes};
    use pw::stream::StreamFlags;

    struct UserData {
        tx: Sender<PwMsg>,
        format: pw::spa::param::video::VideoInfoRaw,
        negotiated: bool,
    }

    let mainloop = pw::main_loop::MainLoop::new(None).map_err(|e| format!("mainloop: {e}"))?;
    let context = pw::context::Context::new(&mainloop).map_err(|e| format!("context: {e}"))?;
    let core = context
        .connect_fd(fd, None)
        .map_err(|e| format!("connect fd: {e}"))?;

    let stream = pw::stream::Stream::new(
        &core,
        "opacusr-screencast",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(|e| format!("stream: {e}"))?;

    let _listener = stream
        .add_local_listener_with_user_data(UserData {
            tx,
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
                "pw format: {:?} {}×{}",
                ud.format.format().as_raw(),
                ud.format.size().width,
                ud.format.size().height
            );
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
            update_cursor_from_buffer(datas);
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

            let fmt = ud.format.format();
            let rgba: Vec<u8> = match fmt {
                VideoFormat::BGRA | VideoFormat::BGRx => {
                    let mut v = bytes[..expected].to_vec();
                    bgra_to_rgba(&mut v);
                    v
                }
                VideoFormat::RGBA | VideoFormat::RGBx => {
                    // RGBx: alpha byte may be garbage → force opaque.
                    let mut v = bytes[..expected].to_vec();
                    for px in v.as_chunks_mut::<4>().0 {
                        px[3] = 255;
                    }
                    v
                }
                VideoFormat::RGB => {
                    let mut v = Vec::with_capacity((w as usize) * (h as usize) * 4);
                    for row in bytes[..expected].chunks_exact(stride) {
                        for px in row[..(w as usize) * 3].as_chunks::<3>().0 {
                            v.extend_from_slice(&[px[0], px[1], px[2], 255]);
                        }
                    }
                    v
                }
                _ => {
                    // YUV planar formats are a roadmap item.
                    return;
                }
            };
            let _ = ud.tx.send(PwMsg::Frame(Frame::new(w, h, rgba)));
        })
        .register()
        .map_err(|e| format!("listener: {e}"))?;

    // Offer the raw formats we can consume, preferring BGRx (compositors
    // hand that out natively), at the portal-negotiated size.
    let (w, h) = (size.0.max(1) as u32, size.1.max(1) as u32);
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
            Some(node),
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|e| format!("connect: {e}"))?;

    mainloop.run();
    Ok(())
}
