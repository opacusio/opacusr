//! Clipboard access from a dedicated OS thread.
//!
//! `arboard::Clipboard` is thread-affine on X11 (and costly to re-create on
//! every poll), while our session loops are `tokio` futures that must stay
//! `Send`. This module pins the clipboard to one worker thread and talks to
//! it with plain channels:
//!
//! ```text
//! session task ── request_get()/set() ──► worker thread (owns arboard)
//!       ▲                                        │
//!       └──────────── poll() result ─────────────┘
//! ```
//!
//! The same helper is used by the host agent (share my clipboard) and by
//! the viewer (receive the remote one), so both get identical echo
//! suppression behaviour at their call sites.
//!
//! Text and images: [`poll`] returns a [`ClipContent`] — text when the
//! clipboard holds any (cheap read, covers the common case), otherwise
//! the image (screenshot copies usually carry no text). The caller does
//! change detection; for images it should hash the RGBA buffer (see
//! [`hash_clip`]) instead of comparing bytes.

/// Refuse to move absurd amounts of data through the clipboard channel.
pub const CLIPBOARD_MAX_BYTES: usize = 512 * 1024;

/// Cap on a single image clipboard transfer, **PNG-encoded**. Must stay
/// well below the transport frame limit (8 MiB).
pub const CLIPBOARD_IMAGE_MAX_PNG_BYTES: usize = 6 * 1024 * 1024;

/// Cap on the decoded RGBA payload accepted from the wire (decompression
/// bomb guard; a 4K frame is ~33 MB).
pub const CLIPBOARD_IMAGE_MAX_RGBA_BYTES: usize = 64 * 1024 * 1024;

/// One clipboard snapshot, either side of the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipContent {
    Text(String),
    Image {
        width: u32,
        height: u32,
        /// Tight RGBA8, row-major (`width * height * 4` bytes).
        rgba: Vec<u8>,
    },
}

/// Stable per-content hash for echo-suppression markers. Text goes
/// through the same hasher so callers can keep one marker type.
pub fn hash_clip(content: &ClipContent) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match content {
        ClipContent::Text(t) => {
            0u8.hash(&mut h);
            t.hash(&mut h);
        }
        ClipContent::Image {
            width,
            height,
            rgba,
        } => {
            1u8.hash(&mut h);
            width.hash(&mut h);
            height.hash(&mut h);
            rgba.hash(&mut h);
        }
    }
    h.finish()
}

/// Encode a tight RGBA8 buffer as PNG. `None` when the dimensions do not
/// match the buffer length.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    use image::ImageEncoder;
    if (width as usize) * (height as usize) * 4 != rgba.len() || width == 0 || height == 0 {
        return None;
    }
    let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut out))
        .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
        .ok()
        .map(|_| out)
}

/// Decode a PNG into a tight RGBA8 buffer. `None` on malformed input or
/// when the decoded payload exceeds [`CLIPBOARD_IMAGE_MAX_RGBA_BYTES`].
pub fn decode_png(png: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let reader = image::ImageReader::new(std::io::Cursor::new(png))
        .with_guessed_format()
        .ok()?;
    let (width, height) = reader.into_dimensions().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    if (width as usize) * (height as usize) * 4 > CLIPBOARD_IMAGE_MAX_RGBA_BYTES {
        return None;
    }
    let img = image::ImageReader::new(std::io::Cursor::new(png))
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    Some((width, height, img.to_rgba8().into_raw()))
}

enum Req {
    Get,
    Set(ClipContent),
}

/// A handle to the clipboard worker thread. Dropping it lets the worker
/// exit (the channel disconnects).
pub struct ClipboardHandle {
    tx: std::sync::mpsc::Sender<Req>,
    rx: std::sync::mpsc::Receiver<Option<ClipContent>>,
}

impl ClipboardHandle {
    /// Spawn the worker. Returns `None` on platforms without clipboard
    /// support (mobile) or when the backend cannot initialise (headless
    /// CI, Wayland without a portal, …) — callers must treat clipboard
    /// sync as a best-effort feature.
    pub fn spawn() -> Option<Self> {
        spawn_impl()
    }

    /// Ask the worker to read the clipboard; the result arrives as a
    /// [`Self::poll`] event. Cheap and non-blocking.
    pub fn request_get(&self) {
        let _ = self.tx.send(Req::Get);
    }

    /// Drain one completed read request, if any. The caller does the
    /// change detection (compare text / hash images against markers).
    pub fn poll(&mut self) -> Option<ClipContent> {
        self.rx.try_recv().ok().flatten()
    }

    /// Write text to the system clipboard (best effort, size-capped).
    pub fn set(&self, text: &str) {
        if text.len() <= CLIPBOARD_MAX_BYTES {
            let _ = self.tx.send(Req::Set(ClipContent::Text(text.to_string())));
        }
    }

    /// Write an image to the system clipboard (best effort, size-capped).
    /// `rgba` must be a tight RGBA8 buffer.
    pub fn set_image(&self, width: u32, height: u32, rgba: &[u8]) {
        if (width as usize) * (height as usize) * 4 != rgba.len()
            || rgba.len() > CLIPBOARD_IMAGE_MAX_RGBA_BYTES
        {
            return;
        }
        let _ = self.tx.send(Req::Set(ClipContent::Image {
            width,
            height,
            rgba: rgba.to_vec(),
        }));
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn spawn_impl() -> Option<ClipboardHandle> {
    use std::sync::mpsc::channel;

    let (tx, worker_rx) = channel::<Req>();
    let (res_tx, rx) = channel::<Option<ClipContent>>();

    let started = std::thread::Builder::new()
        .name("opacusr-clipboard".into())
        .spawn(move || {
            let Ok(mut cb) = arboard::Clipboard::new() else {
                // No clipboard backend: keep replying "unavailable" until
                // the owner drops the handle.
                while let Ok(req) = worker_rx.recv() {
                    if let Req::Get = req {
                        let _ = res_tx.send(None);
                    }
                }
                return;
            };
            while let Ok(req) = worker_rx.recv() {
                match req {
                    Req::Get => {
                        // Text first: cheap, and copied text must not be
                        // shadowed by an older image. Screenshot copies
                        // usually set no text — fall back to the image.
                        let content = match cb.get_text() {
                            Ok(t) if !t.is_empty() && t.len() <= CLIPBOARD_MAX_BYTES => {
                                Some(ClipContent::Text(t))
                            }
                            _ => cb.get_image().ok().and_then(|img| {
                                let len = img.width.checked_mul(img.height)? * 4;
                                if img.width == 0
                                    || img.height == 0
                                    || len > CLIPBOARD_IMAGE_MAX_RGBA_BYTES
                                {
                                    return None;
                                }
                                Some(ClipContent::Image {
                                    width: img.width as u32,
                                    height: img.height as u32,
                                    rgba: img.bytes.to_vec(),
                                })
                            }),
                        };
                        let _ = res_tx.send(content);
                    }
                    Req::Set(ClipContent::Text(t)) => {
                        let _ = cb.set_text(t);
                    }
                    Req::Set(ClipContent::Image {
                        width,
                        height,
                        rgba,
                    }) => {
                        let _ = cb.set_image(arboard::ImageData {
                            width: width as usize,
                            height: height as usize,
                            bytes: std::borrow::Cow::Owned(rgba),
                        });
                    }
                }
            }
        });

    started.ok().map(|_| ClipboardHandle { tx, rx })
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn spawn_impl() -> Option<ClipboardHandle> {
    None
}

#[cfg(all(test, not(any(target_os = "android", target_os = "ios"))))]
mod tests {
    use super::*;

    #[test]
    fn set_then_get_round_trip() {
        let Some(mut h) = ClipboardHandle::spawn() else {
            // Headless CI without X11/portal — nothing to assert.
            return;
        };
        h.set("opacusr-clipboard-test");
        // Wait for the set to land, then read back.
        std::thread::sleep(std::time::Duration::from_millis(150));
        h.request_get();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut got = None;
        while std::time::Instant::now() < deadline {
            if let Some(content) = h.poll() {
                got = Some(content);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // The clipboard may be shared with other processes; accept either
        // our value or "no backend".
        if let Some(ClipContent::Text(text)) = got {
            assert_eq!(text, "opacusr-clipboard-test");
        }
    }

    /// A 3×2 gradient: pixel (x, y) → (x·80, y·80, 160, 255).
    fn sample_rgba() -> (u32, u32, Vec<u8>) {
        let mut rgba = Vec::with_capacity(3 * 2 * 4);
        for y in 0..2u32 {
            for x in 0..3u32 {
                rgba.extend_from_slice(&[(x * 80) as u8, (y * 80) as u8, 160, 255]);
            }
        }
        (3, 2, rgba)
    }

    #[test]
    fn png_round_trip() {
        let (w, h, rgba) = sample_rgba();
        let png = encode_png(w, h, &rgba).expect("encode");
        assert!(png.len() > 8);
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        let (w2, h2, rgba2) = decode_png(&png).expect("decode");
        assert_eq!((w, h), (w2, h2));
        assert_eq!(rgba, rgba2);
    }

    #[test]
    fn png_helpers_reject_bad_input() {
        let (_, _, rgba) = sample_rgba();
        // Dimension/length mismatch.
        assert!(encode_png(4, 2, &rgba).is_none());
        assert!(encode_png(0, 2, &rgba).is_none());
        // Malformed payload.
        assert!(decode_png(b"not a png at all").is_none());
        assert!(decode_png(&[]).is_none());
    }

    #[test]
    fn hash_is_stable_and_discriminates() {
        let (w, h, rgba) = sample_rgba();
        let img = ClipContent::Image {
            width: w,
            height: h,
            rgba: rgba.clone(),
        };
        assert_eq!(hash_clip(&img), hash_clip(&img));
        let mut other = rgba.clone();
        other[0] ^= 0xff;
        assert_ne!(
            hash_clip(&img),
            hash_clip(&ClipContent::Image {
                width: w,
                height: h,
                rgba: other,
            })
        );
        assert_ne!(
            hash_clip(&ClipContent::Text("hi".into())),
            hash_clip(&ClipContent::Text("ho".into()))
        );
    }
}
