//! Byte-level reader for the PipeWire cursor buffer metadata.
//!
//! The portal's screencast stream negotiated with `CursorMode::Metadata`
//! carries a `spa_meta_cursor` on every buffer: the pointer position and,
//! when the shape changed, an inline bitmap. The C layout of PipeWire
//! 1.0.x (`spa/buffer/meta.h`) is parsed here directly from raw bytes
//! instead of via bindgen structs, which keeps the logic testable without
//! a PipeWire build:
//!
//! ```c
//! struct spa_point     { int32_t x, y; };           // 8 bytes
//! struct spa_rectangle { uint32_t width, height; }; // 8 bytes
//! struct spa_meta_cursor {                  // 28 bytes
//!     uint32_t id;                          //   0 — 0 = no new cursor data
//!     uint32_t flags;                       //   4
//!     struct spa_point position;            //   8 — hotspot position on screen
//!     struct spa_point hotspot;             //  16 — hotspot inside the bitmap
//!     uint32_t bitmap_offset;               //  24 — 0 = no new bitmap info,
//!                                           //        >= 28 = spa_meta_bitmap there
//! };
//! struct spa_meta_bitmap {                  // 20 bytes
//!     uint32_t format;                      //   0 — 0 = invalid ("no new bitmap")
//!     struct spa_rectangle size;            //   4
//!     int32_t stride;                       //  12
//!     uint32_t offset;                      //  16 — 0 = no image data (invisible),
//!                                           //        >= 20 = pixels at this offset
//! };
//! ```
//!
//! `position` is where the cursor's *hotspot* sits on screen — the same
//! convention as XFixes and Win32, so no translation is needed before the
//! value reaches [`crate::cursor::CursorPosition`].

/// `enum spa_meta_type` index of `SPA_META_Cursor` (PipeWire 1.0.x,
/// `spa/buffer/meta.h`: Invalid, Header, VideoCrop, VideoDamage, Bitmap,
/// Cursor).
pub const SPA_META_CURSOR: u32 = 5;

/// `enum spa_video_format` values (PipeWire 1.0.x, `spa/param/video/raw.h`)
/// that appear as compositor cursor bitmaps.
pub const FMT_RGBX: u32 = 7;
/// See [`FMT_RGBX`].
pub const FMT_BGRX: u32 = 8;
/// See [`FMT_RGBX`].
pub const FMT_RGBA: u32 = 11;
/// See [`FMT_RGBX`].
pub const FMT_BGRA: u32 = 12;
/// See [`FMT_RGBX`].
pub const FMT_ARGB: u32 = 13;
/// See [`FMT_RGBX`].
pub const FMT_ABGR: u32 = 14;

/// Refuse absurd cursor bitmaps (the biggest sane cursor is 128×128×4).
pub const CURSOR_BITMAP_MAX_BYTES: usize = 256 * 1024;

const SIZEOF_META_CURSOR: usize = 28;
const SIZEOF_META_BITMAP: usize = 20;

/// One inline cursor bitmap as described by `spa_meta_bitmap`.
pub struct CursorBitmapMeta<'a> {
    /// `enum spa_video_format` value (see the [`FMT_*`](self) constants).
    pub format: u32,
    pub width: u32,
    pub height: u32,
    pub stride: i32,
    /// Pixel bytes when the bitmap carries image data; `None` means the
    /// compositor signalled an *empty* sprite (invisible cursor).
    pub pixels: Option<&'a [u8]>,
}

/// One parsed `spa_meta_cursor`.
pub struct CursorMeta<'a> {
    /// Non-zero: the buffer carries fresh cursor data.
    pub id: u32,
    /// Hotspot position on screen (stream pixels).
    pub x: i32,
    pub y: i32,
    /// Hotspot inside the bitmap.
    pub hotspot_x: i32,
    pub hotspot_y: i32,
    /// New bitmap info when the buffer updates the sprite (`None` =
    /// position-only update).
    pub bitmap: Option<CursorBitmapMeta<'a>>,
}

fn u32_at(bytes: &[u8], off: usize) -> Option<u32> {
    bytes
        .get(off..off + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
}

fn i32_at(bytes: &[u8], off: usize) -> Option<i32> {
    bytes
        .get(off..off + 4)
        .map(|b| i32::from_le_bytes(b.try_into().unwrap()))
}

/// Parse the metadata region of a `SPA_META_Cursor` entry. Returns `None`
/// when the region is too small or offsets are out of bounds — the buffer
/// is then ignored, never trusted.
pub fn parse_cursor_meta(bytes: &[u8]) -> Option<CursorMeta<'_>> {
    if bytes.len() < SIZEOF_META_CURSOR {
        return None;
    }
    let id = u32_at(bytes, 0)?;
    let x = i32_at(bytes, 8)?;
    let y = i32_at(bytes, 12)?;
    let hotspot_x = i32_at(bytes, 16)?;
    let hotspot_y = i32_at(bytes, 20)?;
    let bitmap_offset = u32_at(bytes, 24)? as usize;

    // "When the offset is 0, there is no new bitmap information. When the
    // offset is >= sizeof(struct spa_meta_cursor) there is a struct
    // spa_meta_bitmap at the offset." Anything in between is garbage.
    let bitmap = if bitmap_offset >= SIZEOF_META_CURSOR {
        let rest = bytes.get(bitmap_offset..)?;
        let (hdr, pixels) = parse_bitmap_meta(rest)?;
        if hdr.format == 0 {
            // "0 is an invalid format and should be handled as if there
            // is no new bitmap information" — the position still applies.
            None
        } else {
            Some(CursorBitmapMeta {
                format: hdr.format,
                width: hdr.width,
                height: hdr.height,
                stride: hdr.stride,
                pixels,
            })
        }
    } else {
        None
    };

    Some(CursorMeta {
        id,
        x,
        y,
        hotspot_x,
        hotspot_y,
        bitmap,
    })
}

/// Raw bitmap header fields (`spa_meta_bitmap`).
struct BitmapHeader {
    format: u32,
    width: u32,
    height: u32,
    stride: i32,
}

/// Returns the header plus the pixel slice (when image data is present).
fn parse_bitmap_meta(bytes: &[u8]) -> Option<(BitmapHeader, Option<&[u8]>)> {
    if bytes.len() < SIZEOF_META_BITMAP {
        return None;
    }
    let format = u32_at(bytes, 0)?;
    let width = u32_at(bytes, 4)?;
    let height = u32_at(bytes, 8)?;
    let stride = i32_at(bytes, 12)?;
    let offset = u32_at(bytes, 16)? as usize;

    // "An offset of 0 means no image data (invisible); an offset >=
    // sizeof(struct spa_meta_bitmap) contains valid bitmap info."
    let pixels = if offset >= SIZEOF_META_BITMAP {
        let w = width as usize;
        let h = height as usize;
        if w == 0 || h == 0 || w.checked_mul(4)?.checked_mul(h)? > CURSOR_BITMAP_MAX_BYTES {
            return None;
        }
        let stride = usize::try_from(stride).ok()?;
        if stride < w * 4 {
            return None;
        }
        let needed = stride.checked_mul(h)?;
        let data = bytes.get(offset..)?.get(..needed)?;
        Some(data)
    } else {
        None
    };
    Some((
        BitmapHeader {
            format,
            width,
            height,
            stride,
        },
        pixels,
    ))
}

/// Convert a compositor cursor bitmap into straight-alpha RGBA8
/// (row-major, tightly packed) — the format
/// [`crate::cursor::CursorBitmap`] carries.
pub fn bitmap_to_rgba(
    format: u32,
    width: u32,
    height: u32,
    stride: i32,
    pixels: &[u8],
) -> Option<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || w.checked_mul(4)?.checked_mul(h)? > CURSOR_BITMAP_MAX_BYTES {
        return None;
    }
    let stride = usize::try_from(stride).ok()?;
    if stride < w * 4 {
        return None;
    }
    let needed = stride.checked_mul(h)?;
    if pixels.len() < needed {
        return None;
    }
    let mut rgba = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        let row_bytes = &pixels[row * stride..row * stride + w * 4];
        match format {
            FMT_BGRA | FMT_BGRX => {
                for px in row_bytes.as_chunks::<4>().0 {
                    let a = if format == FMT_BGRX { 255 } else { px[3] };
                    rgba.extend_from_slice(&[px[2], px[1], px[0], a]);
                }
            }
            FMT_RGBA | FMT_RGBX => {
                for px in row_bytes.as_chunks::<4>().0 {
                    let a = if format == FMT_RGBX { 255 } else { px[3] };
                    rgba.extend_from_slice(&[px[0], px[1], px[2], a]);
                }
            }
            FMT_ARGB => {
                // bytes a,r,g,b → r,g,b,a
                for px in row_bytes.as_chunks::<4>().0 {
                    rgba.extend_from_slice(&[px[1], px[2], px[3], px[0]]);
                }
            }
            FMT_ABGR => {
                // bytes a,b,g,r → r,g,b,a
                for px in row_bytes.as_chunks::<4>().0 {
                    rgba.extend_from_slice(&[px[3], px[2], px[1], px[0]]);
                }
            }
            _ => return None,
        }
    }
    Some(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (format, width, height, stride, pixels) as fed to [`build_meta`].
    type BitmapSpec = (u32, u32, u32, i32, Option<Vec<u8>>);

    /// Build a `spa_meta_cursor` region: header + optional bitmap + pixels.
    fn build_meta(
        id: u32,
        x: i32,
        y: i32,
        hotspot: (i32, i32),
        bitmap: Option<BitmapSpec>,
    ) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&id.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // flags (unused)
        b.extend_from_slice(&x.to_le_bytes());
        b.extend_from_slice(&y.to_le_bytes());
        b.extend_from_slice(&hotspot.0.to_le_bytes());
        b.extend_from_slice(&hotspot.1.to_le_bytes());
        match bitmap {
            None => b.extend_from_slice(&0u32.to_le_bytes()),
            Some((fmt, w, h, stride, pixels)) => {
                let off = SIZEOF_META_CURSOR as u32;
                b.extend_from_slice(&off.to_le_bytes());
                let bmp_off = if pixels.is_some() {
                    SIZEOF_META_BITMAP as u32
                } else {
                    0
                };
                b.extend_from_slice(&fmt.to_le_bytes());
                b.extend_from_slice(&w.to_le_bytes());
                b.extend_from_slice(&h.to_le_bytes());
                b.extend_from_slice(&stride.to_le_bytes());
                b.extend_from_slice(&bmp_off.to_le_bytes());
                if let Some(px) = pixels {
                    b.extend_from_slice(&px);
                }
            }
        }
        b
    }

    #[test]
    fn position_only_meta_parses() {
        let b = build_meta(7, 100, 50, (3, 2), None);
        let m = parse_cursor_meta(&b).expect("parses");
        assert_eq!(m.id, 7);
        assert_eq!((m.x, m.y), (100, 50));
        assert_eq!((m.hotspot_x, m.hotspot_y), (3, 2));
        assert!(m.bitmap.is_none());
    }

    #[test]
    fn bitmap_meta_parses_with_pixels() {
        let px = vec![1u8; 8 * 8 * 4];
        let b = build_meta(
            1,
            10,
            20,
            (0, 0),
            Some((FMT_BGRA, 8, 8, 8 * 4, Some(px.clone()))),
        );
        let m = parse_cursor_meta(&b).expect("parses");
        let bm = m.bitmap.expect("bitmap present");
        assert_eq!(bm.format, FMT_BGRA);
        assert_eq!((bm.width, bm.height), (8, 8));
        assert_eq!(bm.stride, 32);
        assert_eq!(bm.pixels.expect("pixels"), &px[..]);
    }

    #[test]
    fn empty_bitmap_means_invisible_sprite() {
        // offset == 0 inside the bitmap → no image data (invisible).
        let b = build_meta(1, 5, 5, (0, 0), Some((FMT_BGRA, 8, 8, 32, None)));
        let m = parse_cursor_meta(&b).expect("parses");
        let bm = m.bitmap.expect("bitmap info present");
        assert!(bm.pixels.is_none(), "empty sprite carries no pixels");
    }

    #[test]
    fn invalid_id_and_short_regions_are_rejected() {
        assert!(parse_cursor_meta(&[0u8; 27]).is_none(), "too short");
        // id == 0 → "no new cursor data" (still parses; caller checks id).
        let b = build_meta(0, 1, 1, (0, 0), None);
        assert_eq!(parse_cursor_meta(&b).unwrap().id, 0);
        // format 0 → "no new bitmap information".
        let b = build_meta(1, 1, 1, (0, 0), Some((0, 8, 8, 32, None)));
        assert!(parse_cursor_meta(&b).unwrap().bitmap.is_none());
        // bitmap_offset in (0, sizeof(cursor)) → garbage → no bitmap.
        let mut b = build_meta(1, 1, 1, (0, 0), None);
        let last = b.len() - 4;
        b[last..].copy_from_slice(&4u32.to_le_bytes());
        assert!(parse_cursor_meta(&b).unwrap().bitmap.is_none());
    }

    #[test]
    fn oversize_and_misaligned_bitmaps_are_rejected() {
        // Corrupt bitmap regions reject the whole buffer (defensive).
        // stride smaller than the row:
        let px = vec![0u8; 8 * 8 * 4];
        let b = build_meta(1, 1, 1, (0, 0), Some((FMT_BGRA, 8, 8, 16, Some(px))));
        assert!(parse_cursor_meta(&b).is_none());
        // pixel data truncated:
        let b = build_meta(
            1,
            1,
            1,
            (0, 0),
            Some((FMT_BGRA, 8, 8, 32, Some(vec![0u8; 32]))),
        );
        assert!(parse_cursor_meta(&b).is_none());
    }

    #[test]
    fn bgra_pixels_convert_to_rgba() {
        // One row, two pixels: BGRA (10,20,30,40) → RGBA (30,20,10,40).
        let row = [10u8, 20, 30, 40, 50, 60, 70, 80];
        let rgba = bitmap_to_rgba(FMT_BGRA, 2, 1, 8, &row).expect("converts");
        assert_eq!(rgba, vec![30, 20, 10, 40, 70, 60, 50, 80]);
    }

    #[test]
    fn argb_and_abgr_pixels_convert_to_rgba() {
        // ARGB: a,r,g,b → r,g,b,a.
        let argb = [255u8, 1, 2, 3];
        let rgba = bitmap_to_rgba(FMT_ARGB, 1, 1, 4, &argb).unwrap();
        assert_eq!(rgba, vec![1, 2, 3, 255]);
        // ABGR: a,b,g,r → r,g,b,a.
        let abgr = [255u8, 3, 2, 1];
        let rgba = bitmap_to_rgba(FMT_ABGR, 1, 1, 4, &abgr).unwrap();
        assert_eq!(rgba, vec![1, 2, 3, 255]);
    }

    #[test]
    fn rgbx_forces_opaque_alpha() {
        let row = [10u8, 20, 30, 0];
        let rgba = bitmap_to_rgba(FMT_RGBX, 1, 1, 4, &row).unwrap();
        assert_eq!(rgba, vec![10, 20, 30, 255]);
    }

    #[test]
    fn stride_padding_is_skipped() {
        // Width 2 (8 bytes), stride 12 → 4 padding bytes per row, two rows.
        let row = [
            10u8, 20, 30, 40, 50, 60, 70, 80, 0, 0, 0, 0, // row 0 + padding
            10, 20, 30, 40, 50, 60, 70, 80, 0, 0, 0, 0, // row 1 + padding
        ];
        let rgba = bitmap_to_rgba(FMT_RGBA, 2, 2, 12, &row).expect("converts");
        assert_eq!(rgba.len(), 2 * 2 * 4);
        assert_eq!(
            &rgba[8..12],
            &[10, 20, 30, 40],
            "second row reads at stride"
        );
    }

    #[test]
    fn unsupported_format_and_bad_geometry_fail() {
        let px = vec![0u8; 16];
        assert!(bitmap_to_rgba(999, 1, 1, 4, &px).is_none());
        assert!(bitmap_to_rgba(FMT_BGRA, 0, 1, 4, &px).is_none());
        assert!(bitmap_to_rgba(FMT_BGRA, 4, 1, 2, &px).is_none());
        assert!(bitmap_to_rgba(FMT_BGRA, 4, 1, 4, &[]).is_none());
    }
}
