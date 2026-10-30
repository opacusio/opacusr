//! H.264 codec (Cisco OpenH264, compiled from the bundled BSD-licensed
//! source — see `openh264-sys2`).
//!
//! Screen-content tuned: `ScreenContentRealTime` usage, bitrate mode with a
//! pixels-per-frame×quality heuristic, forced IDR on the first frame and
//! every [`H264_KEYFRAME_INTERVAL`] frames. RGBA↔I420 conversion is done
//! here (BT.601, full range) so the rest of the pipeline stays `Frame`-based.

use opacusr_capture::Frame;
use opacusr_proto::message::CodecId;

use openh264::formats::YUVSource;

use crate::{CodecError, VideoDecoder, VideoEncoder};

/// Emit an IDR frame at least this often (frames). Bounds how much of the
/// stream a mid-session viewer must wait for, and bounds damage after loss.
pub const H264_KEYFRAME_INTERVAL: u64 = 120;

/// Map JPEG-style quality (1..=100) to a bits-per-pixel-per-frame figure for
/// the rate controller. Screen content compresses very well, so even the top
/// of the range is modest in absolute bitrate.
fn bits_per_pixel(quality: u8) -> f64 {
    let q = quality.clamp(1, 100) as f64 / 100.0;
    0.02 + q * 0.18
}

fn target_bitrate(width: u32, height: u32, fps: u8, quality: u8) -> u32 {
    let bps = width as f64 * height as f64 * fps.max(1) as f64 * bits_per_pixel(quality);
    (bps as u32).clamp(200_000, 50_000_000)
}

// ---------------------------------------------------------------------------
// RGBA ↔ I420 (BT.601, full range)
// ---------------------------------------------------------------------------

/// Convert RGBA8 (4 bytes/px) into planar I420 (Y full-res; U, V quarter).
pub fn rgba_to_i420(rgba: &[u8], width: u32, height: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let w = width as usize;
    let h = height as usize;
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut y = vec![0u8; w * h];
    let mut u = vec![128u8; cw * ch];
    let mut v = vec![128u8; cw * ch];

    for row in 0..h {
        for col in 0..w {
            let i = (row * w + col) * 4;
            let (r, g, b) = (rgba[i] as i32, rgba[i + 1] as i32, rgba[i + 2] as i32);
            // BT.601 full range, fixed-point, rounded (symmetric with
            // `i420_to_rgba` below so a lossless round trip is ±1).
            let yy = ((19_595 * r + 38_470 * g + 7_471 * b + 32_768) >> 16).clamp(0, 255);
            y[row * w + col] = yy as u8;
            if row.is_multiple_of(2) && col.is_multiple_of(2) {
                // Average the 2×2 block before subsampling chroma.
                let (mut rs, mut gs, mut bs, mut n) = (0i64, 0i64, 0i64, 0i64);
                for dy in [0, 1] {
                    for dx in [0, 1] {
                        let (r2, c2) = (row + dy, col + dx);
                        if r2 < h && c2 < w {
                            let j = (r2 * w + c2) * 4;
                            rs += rgba[j] as i64;
                            gs += rgba[j + 1] as i64;
                            bs += rgba[j + 2] as i64;
                            n += 1;
                        }
                    }
                }
                let (r, g, b) = ((rs / n) as i32, (gs / n) as i32, (bs / n) as i32);
                let uu = ((-11_059 * r - 21_709 * g + 32_768 * b + (128 << 16) + 32_768) >> 16)
                    .clamp(0, 255);
                let vv = ((32_768 * r - 27_440 * g - 5_329 * b + (128 << 16) + 32_768) >> 16)
                    .clamp(0, 255);
                let ci = (row / 2) * cw + col / 2;
                u[ci] = uu as u8;
                v[ci] = vv as u8;
            }
        }
    }
    (y, u, v)
}

/// Convert planar I420 (with strides) into RGBA8. `u`/`v` are sampled at
/// (x/2, y/2) — the standard 4:2:0 layout openh264 produces.
pub fn i420_to_rgba(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    y_stride: usize,
    uv_stride: usize,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let mut rgba = vec![0u8; w * h * 4];
    for row in 0..h {
        for col in 0..w {
            let yp = row * y_stride + col;
            let cp = (row / 2) * uv_stride + col / 2;
            let (yy, uu, vv) = (y[yp] as i32, u[cp] as i32, v[cp] as i32);
            // BT.601 full-range reconstruction (symmetric with the forward
            // conversion above — openh264 stores YUV as given).
            let d = uu - 128;
            let e = vv - 128;
            let r = ((65_536 * yy + 91_881 * e + 32_768) >> 16).clamp(0, 255);
            let g = ((65_536 * yy - 22_554 * d - 46_802 * e + 32_768) >> 16).clamp(0, 255);
            let b = ((65_536 * yy + 116_130 * d + 32_768) >> 16).clamp(0, 255);
            let i = (row * w + col) * 4;
            rgba[i] = r as u8;
            rgba[i + 1] = g as u8;
            rgba[i + 2] = b as u8;
            rgba[i + 3] = 255;
        }
    }
    rgba
}

// ---------------------------------------------------------------------------
// YUVSource adapter
// ---------------------------------------------------------------------------

struct PlanarI420 {
    w: usize,
    h: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl PlanarI420 {
    fn new(w: u32, h: u32) -> Self {
        let (w, h) = (w as usize, h as usize);
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
        Self {
            w,
            h,
            y: vec![0u8; w * h],
            u: vec![128u8; cw * ch],
            v: vec![128u8; cw * ch],
        }
    }
}

impl openh264::formats::YUVSource for PlanarI420 {
    fn dimensions(&self) -> (usize, usize) {
        (self.w, self.h)
    }
    fn strides(&self) -> (usize, usize, usize) {
        (self.w, self.w.div_ceil(2), self.w.div_ceil(2))
    }
    fn y(&self) -> &[u8] {
        &self.y
    }
    fn u(&self) -> &[u8] {
        &self.u
    }
    fn v(&self) -> &[u8] {
        &self.v
    }
}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

/// H.264 encoder backed by Cisco OpenH264. Stateful (inter-frame prediction),
/// re-created when quality changes, auto-reinitialized by openh264 on resize.
pub struct H264Encoder {
    enc: Option<openh264::encoder::Encoder>,
    last_quality: u8,
    frames: u64,
}

impl Default for H264Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl H264Encoder {
    pub fn new() -> Self {
        Self {
            enc: None,
            last_quality: u8::MAX,
            frames: 0,
        }
    }

    fn rebuild(&mut self, width: u32, height: u32, fps: u8, quality: u8) -> Result<(), CodecError> {
        let config = openh264::encoder::EncoderConfig::new()
            .bitrate(openh264::encoder::BitRate::from_bps(target_bitrate(
                width, height, fps, quality,
            )))
            .rate_control_mode(openh264::encoder::RateControlMode::Bitrate)
            .max_frame_rate(openh264::encoder::FrameRate::from_hz(fps.max(1) as f32))
            .usage_type(openh264::encoder::UsageType::ScreenContentRealTime)
            .intra_frame_period(openh264::encoder::IntraFramePeriod::from_num_frames(
                H264_KEYFRAME_INTERVAL as u32,
            ));
        let enc = openh264::encoder::Encoder::with_api_config(
            openh264::OpenH264API::from_source(),
            config,
        )
        .map_err(|e| CodecError::Encode(format!("openh264 init: {e}")))?;
        self.enc = Some(enc);
        self.last_quality = quality;
        // Fresh encoder → the first frame must be an IDR.
        self.frames = 0;
        Ok(())
    }
}

impl VideoEncoder for H264Encoder {
    fn encode(&mut self, frame: &Frame, quality: u8) -> Result<Vec<u8>, CodecError> {
        if self.enc.is_none() || self.last_quality != quality {
            // fps is unknowable here; the encoder mainly uses it for rate
            // control smoothing. 30 is the pipeline's nominal ceiling.
            self.rebuild(frame.width, frame.height, 30, quality)?;
        }
        let encoder = self
            .enc
            .as_mut()
            .expect("encoder is rebuilt above when missing");

        let mut planar = PlanarI420::new(frame.width, frame.height);
        let (y, u, v) = rgba_to_i420(&frame.rgba, frame.width, frame.height);
        planar.y = y;
        planar.u = u;
        planar.v = v;

        if self.frames.is_multiple_of(H264_KEYFRAME_INTERVAL) {
            encoder.force_intra_frame();
        }
        let bitstream = encoder
            .encode(&planar)
            .map_err(|e| CodecError::Encode(format!("openh264 encode: {e}")))?;
        self.frames += 1;

        let mut out = Vec::with_capacity(frame.rgba.len() / 8 + 64);
        bitstream.write_vec(&mut out);
        if out.is_empty() {
            return Err(CodecError::Encode("openh264 produced no bytes".into()));
        }
        Ok(out)
    }

    fn codec(&self) -> CodecId {
        CodecId::H264
    }
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

/// H.264 decoder backed by Cisco OpenH264.
#[derive(Default)]
pub struct H264Decoder {
    dec: Option<openh264::decoder::Decoder>,
}

impl H264Decoder {
    pub fn new() -> Self {
        Self { dec: None }
    }
}

impl VideoDecoder for H264Decoder {
    fn decode(&mut self, data: &[u8], width: u32, height: u32) -> Result<Frame, CodecError> {
        let dec = self.dec.get_or_insert_with(|| {
            openh264::decoder::Decoder::new()
                .expect("openh264 decoder construction cannot fail with default config")
        });
        let decoded = dec
            .decode(data)
            .map_err(|e| CodecError::Decode(format!("openh264 decode: {e}")))?
            .ok_or_else(|| {
                CodecError::Decode("decoder buffered the frame; no output yet".into())
            })?;

        let (dw, dh) = decoded.dimensions();
        let (ys, us, vs) = decoded.strides();
        if (dw as u32, dh as u32) != (width, height) {
            return Err(CodecError::Decode(format!(
                "decoded size {dw}x{dh} does not match header {width}x{height}"
            )));
        }
        if vs != us {
            return Err(CodecError::Decode("U/V strides differ".into()));
        }
        let rgba = i420_to_rgba(decoded.y(), decoded.u(), decoded.v(), ys, us, width, height);
        Ok(Frame {
            width,
            height,
            rgba,
        })
    }

    fn codec(&self) -> CodecId {
        CodecId::H264
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic gradient + blocks frame (same generator the JPEG tests
    /// use, adapted for 320×200).
    fn test_frame(width: u32, height: u32) -> Frame {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let r = (x * 255 / width.max(1)) as u8;
                let g = (y * 255 / height.max(1)) as u8;
                let b = ((x + y) % 256) as u8;
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        Frame {
            width,
            height,
            rgba,
        }
    }

    fn avg_diff(a: &[u8], b: &[u8]) -> usize {
        let sum: usize = a
            .iter()
            .zip(b.iter())
            .map(|(x, y)| x.abs_diff(*y) as usize)
            .sum();
        sum / a.len()
    }

    #[test]
    fn i420_round_trip_is_close() {
        let f = test_frame(64, 64);
        let (y, u, v) = rgba_to_i420(&f.rgba, 64, 64);
        let back = i420_to_rgba(&y, &u, &v, 64, 32, 64, 64);
        // Chroma subsampling alone should keep average error small.
        assert!(
            avg_diff(&f.rgba, &back) < 10,
            "pure conversion error too high"
        );
    }

    #[test]
    fn h264_round_trip_preserves_picture() {
        let f = test_frame(320, 200);
        let mut enc = H264Encoder::new();
        let mut dec = H264Decoder::new();

        // First frame is a forced IDR; decode must succeed immediately.
        let bytes = enc.encode(&f, 90).expect("encode idr");
        let back = dec.decode(&bytes, 320, 200).expect("decode idr");
        assert_eq!((back.width, back.height), (320, 200));

        // Inter frames after that.
        for _ in 0..3 {
            let bytes = enc.encode(&f, 90).expect("encode p");
            let back = dec.decode(&bytes, 320, 200).expect("decode p");
            assert_eq!(back.rgba.len(), f.rgba.len());
        }

        // Lossy codec: mean per-channel error must stay modest at high quality.
        let d = avg_diff(&f.rgba, &back.rgba);
        assert!(d < 60, "h264 round-trip average error too high: {d}");
    }

    #[test]
    fn quality_change_reinitializes() {
        let f = test_frame(160, 120);
        let mut enc = H264Encoder::new();
        let mut dec = H264Decoder::new();
        let b1 = enc.encode(&f, 40).unwrap();
        let _ = dec.decode(&b1, 160, 120).unwrap();
        let b2 = enc.encode(&f, 95).unwrap(); // triggers rebuild → new IDR
        let back = dec.decode(&b2, 160, 120).unwrap();
        assert_eq!(back.rgba.len(), f.rgba.len());
    }

    #[test]
    fn bitrate_scale_is_sane() {
        // 1080p30 q70 → roughly 10 Mbit/s, clamped range respected.
        assert!(target_bitrate(1920, 1080, 30, 70) > 5_000_000);
        assert!(target_bitrate(1920, 1080, 30, 70) < 20_000_000);
        assert_eq!(target_bitrate(320, 200, 1, 1), 200_000); // lower clamp
        assert_eq!(target_bitrate(4096, 2160, 60, 100), 50_000_000); // upper clamp
    }
}
