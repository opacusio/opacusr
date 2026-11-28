//! macOS host capture — ScreenCaptureKit stream first (hardware-backed,
//! push-based `SCStream` via the C shim `shim/sck_shim.m`, see
//! [`super::macos_sck`]), CoreGraphics snapshot fallback (`macos-cg`).
//! The Rust side of BOTH paths is pure C FFI — no objc_msgSend calls
//! anywhere in Rust: the platform glue stays crash-free without an
//! ObjC runtime audit, same discipline as the raw-vtable JNI backend
//! on Android.
//!
//! The ladder mirrors Windows (DXGI → GDI): SCK is tried at connect
//! time and on every display switch; any failure — OS < 12.3, missing
//! Screen Recording TCC permission, unexpected pixel format, a delegate
//! stop mid-session — drops SCK and the CG path takes over (per frame
//! when SCK dies mid-session). CG itself requires the Screen Recording
//! permission: without it `CGDisplayCreateImage` returns NULL (10.15+)
//! and the backend fails with an explicit user-facing hint instead of a
//! black stream — so the fallback never masks a permission problem with
//! a silently broken stream.
//!
//! Coordinate contract (the tricky macOS part):
//! * CG *display geometry* (`CGDisplayBounds`) is bottom-left origin;
//! * Quartz *events* (`CGEventGetLocation`, what enigo injects) are
//!   top-left origin — both measured in POINTS;
//! * captured frames are DEVICE pixels (2x on Retina).
//!
//! Everything crossing the protocol — [`ScreenCapture::origin`] and the
//! cursor overlay positions — is global top-left DEVICE pixels so it
//! lines up with the streamed frame exactly like the Windows/X11
//! backends, and remote mouse input is converted back to points by
//! [`px_to_pt_global`] (used by `opacusr-input` on macOS).

use std::ffi::c_void;

use crate::macos_sck::{SckDeath, SckStream};
use crate::source::{ScreenCapture, ScreenDesc};
use crate::{CaptureError, Frame};

type CGDirectDisplayID = u32;
type CGError = i32;
type CFIndex = isize;
type CGImageRef = *mut c_void;
type CGDataProviderRef = *mut c_void;
type CFDataRef = *mut c_void;
type CGDisplayModeRef = *mut c_void;
type CGEventRef = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGMainDisplayID() -> CGDirectDisplayID;
    fn CGGetOnlineDisplayList(
        max_displays: u32,
        online_displays: *mut CGDirectDisplayID,
        display_count: *mut u32,
    ) -> CGError;
    fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
    fn CGDisplayCopyDisplayMode(display: CGDirectDisplayID) -> CGDisplayModeRef;
    fn CGDisplayModeGetPixelWidth(mode: CGDisplayModeRef) -> usize;
    fn CGDisplayModeGetPixelHeight(mode: CGDisplayModeRef) -> usize;
    fn CGDisplayModeRelease(mode: CGDisplayModeRef);
    fn CGDisplayCreateImage(display: CGDirectDisplayID) -> CGImageRef;
    fn CGImageGetWidth(image: CGImageRef) -> usize;
    fn CGImageGetHeight(image: CGImageRef) -> usize;
    fn CGImageGetBitmapInfo(image: CGImageRef) -> u32;
    fn CGImageGetDataProvider(image: CGImageRef) -> CGDataProviderRef;
    fn CGDataProviderCopyData(provider: CGDataProviderRef) -> CFDataRef;
    fn CGEventCreate(source: *mut c_void) -> CGEventRef;
    fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    fn CGGetDisplaysWithPoint(
        point: CGPoint,
        max_displays: u32,
        displays: *mut CGDirectDisplayID,
        display_count: *mut u32,
    ) -> CGError;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;
    fn CFDataGetLength(data: CFDataRef) -> CFIndex;
    fn CFRelease(cf: *mut c_void);
}

/// `kCGBitmapByteOrderMask` and `kCGBitmapByteOrder32Little` from
/// `CGImage.h` — the two values this backend needs to tell the standard
/// macOS little-endian BGRA layout from a plain RGBA one.
const BITMAP_BYTE_ORDER_MASK: u32 = 0x7000;
const BITMAP_BYTE_ORDER_32_LITTLE: u32 = 4 << 12;

/// Every display currently online (mirrored/clamshell exclusions applied
/// by the OS itself). Empty = a headless Mac (rare; CI runners always
/// have a display).
pub(crate) fn online_displays() -> Vec<CGDirectDisplayID> {
    let mut ids = [0u32; 16];
    let mut n = 0u32;
    let err = unsafe { CGGetOnlineDisplayList(16, ids.as_mut_ptr(), &mut n) };
    if err != 0 {
        return Vec::new();
    }
    ids[..n.min(16) as usize].to_vec()
}

fn bounds(display: CGDirectDisplayID) -> CGRect {
    unsafe { CGDisplayBounds(display) }
}

/// Device-pixel dimensions of a display's current mode (2x on Retina).
fn display_px(display: CGDirectDisplayID) -> (u32, u32) {
    unsafe {
        let mode = CGDisplayCopyDisplayMode(display);
        if mode.is_null() {
            return (0, 0);
        }
        let (w, h) = (
            CGDisplayModeGetPixelWidth(mode) as u32,
            CGDisplayModeGetPixelHeight(mode) as u32,
        );
        CGDisplayModeRelease(mode);
        (w, h)
    }
}

/// Pixels per point of one display (1.0 on standard-density, 2.0 on
/// Retina). Falls back to 1.0 when the mode is unavailable — a 1.0 guess
/// degrades gracefully instead of throwing every coordinate to (0, 0).
fn px_per_pt(display: CGDirectDisplayID) -> f64 {
    let b = bounds(display);
    if b.size.width <= 0.0 {
        return 1.0;
    }
    let (w, _) = display_px(display);
    if w == 0 {
        return 1.0;
    }
    w as f64 / b.size.width
}

/// Top-left corner of a display in global top-left DEVICE pixels — the
/// exact space `origin()` and the cursor overlay must speak so the
/// viewer's pointer sits on the streamed frame where it belongs.
fn display_topleft_px(display: CGDirectDisplayID) -> (i32, i32) {
    let main = bounds(unsafe { CGMainDisplayID() });
    let b = bounds(display);
    let s = px_per_pt(display);
    // CG geometry: y measured up from the main display's bottom edge.
    // Top-left corner of this display in event space (y measured DOWN
    // from the main display's top edge) = main_height - origin.y -
    // height. Then scale points → device pixels.
    let x = b.origin.x * s;
    let y = (main.size.height - b.origin.y - b.size.height) * s;
    (x.round() as i32, y.round() as i32)
}

/// Cursor location in global top-left POINTS (the Quartz event space),
/// or `None` when the event stream is unavailable.
pub(crate) fn cursor_point() -> Option<(f64, f64)> {
    unsafe {
        let ev = CGEventCreate(std::ptr::null_mut());
        if ev.is_null() {
            return None;
        }
        let p = CGEventGetLocation(ev);
        CFRelease(ev);
        Some((p.x, p.y))
    }
}

/// Pixels-per-point scale of the display under the given top-left POINT,
/// falling back to the main display when no display matches (the point
/// sits beyond every screen — a transient during space switches).
pub(crate) fn px_per_pt_at_point(x: f64, y: f64) -> f64 {
    unsafe {
        let mut ids = [0u32; 4];
        let mut n = 0u32;
        let p = CGPoint { x, y };
        let err = CGGetDisplaysWithPoint(p, 4, ids.as_mut_ptr(), &mut n);
        if err == 0 && n > 0 {
            px_per_pt(ids[0])
        } else {
            px_per_pt(CGMainDisplayID())
        }
    }
}

/// One captured display as RGBA (`CGDataProviderCopyData` + the
/// BGRA→RGBA swap macOS's little-endian bitmap layout needs). The image
/// is released on every return path.
unsafe fn capture_display(display: CGDirectDisplayID) -> Result<Frame, CaptureError> {
    let img = CGDisplayCreateImage(display);
    if img.is_null() {
        // macOS 10.15+: NULL without the Screen Recording permission.
        return Err(CaptureError::Other(
            "macOS не даёт снять экран: Системные настройки → Конфиденциальность \
             и безопасность → Запись экрана → включите OpacusR и перезапустите \
             приложение"
                .into(),
        ));
    }

    let (w, h) = (CGImageGetWidth(img), CGImageGetHeight(img));
    if w == 0 || h == 0 {
        CFRelease(img);
        return Err(CaptureError::Other(
            "capture returned an empty image".into(),
        ));
    }

    let provider = CGImageGetDataProvider(img);
    let data = if provider.is_null() {
        std::ptr::null_mut()
    } else {
        CGDataProviderCopyData(provider)
    };
    if data.is_null() {
        CFRelease(img);
        return Err(CaptureError::Other("image data provider failed".into()));
    }

    let expected = w * h * 4;
    let len = CFDataGetLength(data) as usize;
    let result = if len == expected {
        let mut rgba = std::slice::from_raw_parts(CFDataGetBytePtr(data), expected).to_vec();
        if CGImageGetBitmapInfo(img) & BITMAP_BYTE_ORDER_MASK == BITMAP_BYTE_ORDER_32_LITTLE {
            crate::bgra_to_rgba(&mut rgba);
        }
        Ok(Frame::new(w as u32, h as u32, rgba))
    } else {
        Err(CaptureError::Other(format!(
            "captured {len} bytes, expected {expected} ({w}x{h})"
        )))
    };

    CFRelease(data);
    CFRelease(img);
    result
}

/// Start a ScreenCaptureKit stream for the display, or `None` with a
/// warn log when unavailable (OS < 12.3, no Screen Recording
/// permission, or any other SCK failure) — the CG snapshot path stays
/// fully functional as the fallback.
fn try_start_sck(disp: CGDirectDisplayID) -> Option<SckStream> {
    if !SckStream::available() {
        return None;
    }
    let (w, h) = display_px(disp);
    if w == 0 || h == 0 {
        return None;
    }
    match SckStream::start(disp, w, h) {
        Ok(s) => {
            tracing::info!(
                target: "opacusr",
                // `display` in a format string is the tracing field
                // helper, not a variable name — the argument stays `disp`
                // (see the connect() comment).
                "ScreenCaptureKit stream active ({w}x{h}, display {disp})"
            );
            Some(s)
        }
        Err(e) => {
            tracing::warn!(
                target: "opacusr",
                "ScreenCaptureKit unavailable ({e}); using CoreGraphics snapshots"
            );
            None
        }
    }
}

// ---------------------------------------------------------------------------
// ScreenCapture backend
// ---------------------------------------------------------------------------

/// ScreenCaptureKit stream with a CoreGraphics snapshot fallback
/// (multi-monitor aware, Retina-aware). [`MacCapture::connect`] tries
/// SCK first; every failure path degrades to the CG path that worked
/// before v2, and `backend()` reports which one is actually streaming.
pub struct MacCapture {
    display: CGDirectDisplayID,
    online: Vec<CGDirectDisplayID>,
    /// Live ScreenCaptureKit stream (`None` = CoreGraphics snapshots).
    sck: Option<SckStream>,
}

impl MacCapture {
    pub fn connect() -> Result<Self, CaptureError> {
        let online = online_displays();
        if online.is_empty() {
            return Err(CaptureError::Other(
                "no online displays found (headless mac?)".into(),
            ));
        }
        // `display` is a reserved tracing-macro helper name; the local
        // variable stays `disp` so log arguments never shadow it.
        let disp = unsafe { CGMainDisplayID() };
        // A stale main-display id would stream nothing; fall back to the
        // first online display instead.
        let disp = if online.contains(&disp) {
            disp
        } else {
            online[0]
        };
        tracing::info!(
            target: "opacusr",
            "macOS capture: display {} of {} online",
            disp,
            online.len()
        );
        let sck = try_start_sck(disp);
        Ok(Self {
            display: disp,
            online,
            sck,
        })
    }

    /// Restart the SCK stream when the display mode changed
    /// mid-session: SCK frames keep the configured size, so a stale
    /// stream would desync the frame from the cursor/input coordinate
    /// mapping (the CG path re-reads the mode every frame instead).
    fn maybe_restart_stream(&mut self) {
        let stale = match &self.sck {
            Some(s) => {
                let (w, h) = display_px(self.display);
                w > 0 && h > 0 && s.cfg_size() != (w, h)
            }
            None => false,
        };
        if stale {
            tracing::info!(
                target: "opacusr",
                "display mode changed; restarting the ScreenCaptureKit stream"
            );
            // The old stream drops AFTER the new one started (the
            // assignment moves the replacement in first): handles are
            // object pointers in the shim, so the old stop() cannot
            // tear down the new stream.
            self.sck = try_start_sck(self.display);
        }
    }
}

impl ScreenCapture for MacCapture {
    fn capture(&mut self) -> Result<Frame, CaptureError> {
        self.maybe_restart_stream();
        if self.sck.is_some() {
            // take() first so the mutable borrow is released before the
            // error path reassigns; a dead stream drops (stopping the
            // SCStream in Drop) and the CG path answers this frame.
            let mut s = self.sck.take().expect("checked is_some");
            match s.frame() {
                Ok(f) => {
                    self.sck = Some(s);
                    return Ok(f);
                }
                Err(death) => {
                    let why = match death {
                        SckDeath::WrongFormat => "unexpected pixel format".to_string(),
                        SckDeath::Failed(m) => m,
                    };
                    tracing::warn!(
                        target: "opacusr",
                        "ScreenCaptureKit stream ended ({why}); falling back to CoreGraphics snapshots"
                    );
                }
            }
        }
        unsafe { capture_display(self.display) }
    }

    fn size(&mut self) -> Result<(u32, u32), CaptureError> {
        let (w, h) = display_px(self.display);
        if w == 0 || h == 0 {
            return Err(CaptureError::Other("display size unavailable".into()));
        }
        Ok((w, h))
    }

    fn screens(&mut self) -> Vec<ScreenDesc> {
        let main = unsafe { CGMainDisplayID() };
        self.online
            .iter()
            .enumerate()
            .map(|(i, &d)| {
                let (w, h) = display_px(d);
                ScreenDesc {
                    id: d,
                    name: if d == main {
                        format!("Display {} (основной)", i + 1)
                    } else {
                        format!("Display {}", i + 1)
                    },
                    w,
                    h,
                }
            })
            .collect()
    }

    fn select_screen(&mut self, id: u32) -> Result<bool, CaptureError> {
        if !self.online.contains(&id) {
            return Err(CaptureError::Other(format!("unknown display id {id}")));
        }
        if id == self.display {
            return Ok(false);
        }
        self.display = id;
        // Stop the old stream BEFORE starting the new one: the
        // assignment drops it (Drop → shim stop), so the new start
        // never briefly overlaps two live SCStreams for one backend.
        self.sck = None;
        self.sck = try_start_sck(id);
        Ok(true)
    }

    fn origin(&mut self) -> (i32, i32) {
        display_topleft_px(self.display)
    }

    fn backend(&self) -> &'static str {
        if self.sck.is_some() {
            "macos-sck"
        } else {
            "macos-cg"
        }
    }
}

// ---------------------------------------------------------------------------
// Coordinate bridge for remote input (opacusr-input, macOS only)
// ---------------------------------------------------------------------------

/// One display's rect in global top-left DEVICE pixels plus its
/// pixels-per-point scale — the input the pure converter below needs.
struct PxRect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    /// Display's own top-left corner in POINTS (event space).
    pt_x: f64,
    pt_y: f64,
    scale: f64,
}

/// Pure part of the device-px → event-points conversion (unit-tested):
/// find the display whose pixel rect contains the point, undo its
/// device-px scale, and add its point-space origin.
fn px_to_pt_in(x: u32, y: u32, rects: &[PxRect]) -> Option<(i32, i32)> {
    let (x, y) = (x as i32, y as i32);
    for r in rects {
        if r.scale <= 0.0 {
            continue;
        }
        let (rx, ry) = (x - r.x, y - r.y);
        if rx >= 0 && ry >= 0 && rx < r.w && ry < r.h {
            return Some((
                (r.pt_x + rx as f64 / r.scale).round() as i32,
                (r.pt_y + ry as f64 / r.scale).round() as i32,
            ));
        }
    }
    None
}

/// Convert global top-left DEVICE pixels (the space the remote input
/// path arrives in — streamed-frame pixels rescaled by the host session)
/// into global top-left POINTS (the Quartz event space enigo speaks).
/// Mixed-DPI multi-monitor setups resolve against the display under the
/// point; unmatched points fall back to the main display's scale.
pub fn px_to_pt_global(x: u32, y: u32) -> (i32, i32) {
    let main = unsafe { CGMainDisplayID() };
    let main_bounds = bounds(main);
    let main_topleft = display_topleft_px(main);
    let mut rects: Vec<PxRect> = online_displays()
        .iter()
        .map(|&d| {
            let (w, h) = display_px(d);
            let b = bounds(d);
            let s = px_per_pt(d);
            let (tx, ty) = display_topleft_px(d);
            PxRect {
                x: tx,
                y: ty,
                w: w as i32,
                h: h as i32,
                // Point-space top-left of this display (mirror of the
                // px-space computation above, unscaled).
                pt_x: b.origin.x,
                pt_y: main_bounds.size.height - b.origin.y - b.size.height,
                scale: s,
            }
        })
        .collect();
    if rects.is_empty() {
        rects.push(PxRect {
            x: main_topleft.0,
            y: main_topleft.1,
            w: 0,
            h: 0,
            pt_x: 0.0,
            pt_y: 0.0,
            scale: 1.0,
        });
    }
    if let Some(pt) = px_to_pt_in(x, y, &rects) {
        return pt;
    }
    // Point beyond every display rect (transient during rearrangements):
    // scale with the main display uniformly instead of dropping the event.
    let s = px_per_pt(main);
    ((x as f64 / s).round() as i32, (y as f64 / s).round() as i32)
}

// ---------------------------------------------------------------------------
// Tests (no display required — pure geometry + sprite generation)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x Retina laptop display at (0, 0) and a 1x external at
    /// (1512 px right of it): the converter must pick the right scale
    /// per point and keep 1x displays pixel-exact.
    #[test]
    fn px_to_pt_resolves_scale_per_display() {
        let rects = vec![
            PxRect {
                x: 0,
                y: 0,
                w: 3024,
                h: 1964,
                pt_x: 0.0,
                pt_y: 0.0,
                scale: 2.0,
            },
            PxRect {
                x: 3024,
                y: 0,
                w: 1920,
                h: 1080,
                pt_x: 1512.0,
                pt_y: 0.0,
                scale: 1.0,
            },
        ];
        // Retina: device px → points (÷2).
        let (x, y) = px_to_pt_in(1000, 500, &rects).expect("inside laptop");
        assert_eq!((x, y), (500, 250));
        // External 1x display: exact.
        let (x, y) = px_to_pt_in(3024 + 192, 108, &rects).expect("inside external");
        assert_eq!((x, y), (1512 + 192, 108));
        // Bottom-right corner of the laptop rect is OUTSIDE (half-open).
        assert!(px_to_pt_in(3023, 1963, &rects).is_some());
        assert!(px_to_pt_in(3024, 100, &rects).is_some());
    }

    #[test]
    fn px_to_pt_fallback_scales_by_main_display() {
        // The live path builds this fallback when the point sits beyond
        // every display; px_to_pt_in returning None triggers it.
        let rects = vec![PxRect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
            pt_x: 0.0,
            pt_y: 0.0,
            scale: 2.0,
        }];
        assert!(px_to_pt_in(5000, 5000, &rects).is_none());
    }
}
