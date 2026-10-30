//! OpacusR video codecs.
//!
//! JPEG: self-contained full-frame pipeline (quality 1..=100), testable
//! everywhere. H.264 (feature `h264`, on by default): Cisco OpenH264 with
//! inter-frame prediction, screen-content tuning and RGBA↔I420 conversion.
//! The [`VideoEncoder`] / [`VideoDecoder`] traits remain the extension
//! point for VP9/AV1 (docs/roadmap.md).

use opacusr_capture::Frame;
use opacusr_proto::message::CodecId;
use thiserror::Error;

#[cfg(feature = "h264")]
pub mod h264;

/// `true` when this binary can encode/decode H.264 (feature `h264`).
/// Transport layers use it to decide which [`CodecId`] to announce.
pub const H264_AVAILABLE: bool = cfg!(feature = "h264");

/// Codec failures.
#[derive(Debug, Error)]
pub enum CodecError {
    #[error("encoding failed: {0}")]
    Encode(String),
    #[error("decoding failed: {0}")]
    Decode(String),
    #[error("unsupported codec id {0:?}")]
    Unsupported(CodecId),
}

/// Frame → compressed bytes.
pub trait VideoEncoder: Send {
    fn encode(&mut self, frame: &Frame, quality: u8) -> Result<Vec<u8>, CodecError>;
    fn codec(&self) -> CodecId;
}

/// Compressed bytes → RGBA frame (`w`/`h` provided by the stream header).
pub trait VideoDecoder: Send {
    fn decode(&mut self, data: &[u8], width: u32, height: u32) -> Result<Frame, CodecError>;
    fn codec(&self) -> CodecId;
}

/// Build an encoder/decoder pair for the negotiated codec.
pub fn make_pair(codec: CodecId) -> (Box<dyn VideoEncoder>, Box<dyn VideoDecoder>) {
    match codec {
        CodecId::Jpeg => (Box::new(JpegEncoder), Box::new(JpegDecoder)),
        #[cfg(feature = "h264")]
        CodecId::H264 => (
            Box::new(h264::H264Encoder::new()),
            Box::new(h264::H264Decoder::new()),
        ),
        #[cfg(not(feature = "h264"))]
        CodecId::H264 => {
            tracing::warn!(target: "opacusr", "H.264 requested but built without the `h264` feature; falling back to JPEG");
            (Box::new(JpegEncoder), Box::new(JpegDecoder))
        }
    }
}

// ---------------------------------------------------------------------------
// JPEG
// ---------------------------------------------------------------------------

/// JPEG full-frame encoder (`image` crate, baseline).
#[derive(Debug, Default)]
pub struct JpegEncoder;

impl VideoEncoder for JpegEncoder {
    fn encode(&mut self, frame: &Frame, quality: u8) -> Result<Vec<u8>, CodecError> {
        let quality = quality.clamp(1, 100);
        // JPEG has no alpha: fold RGBA onto RGB.
        let px = frame.width as usize * frame.height as usize;
        if frame.rgba.len() != px * 4 {
            return Err(CodecError::Encode(format!(
                "buffer size {} does not match {}x{}x4",
                frame.rgba.len(),
                frame.width,
                frame.height
            )));
        }
        let mut rgb = Vec::with_capacity(px * 3);
        for p in frame.rgba.as_chunks::<4>().0 {
            rgb.extend_from_slice(&p[..3]);
        }
        let mut out = Vec::with_capacity(px / 2 + 1024);
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
        enc.encode(
            &rgb,
            frame.width,
            frame.height,
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| CodecError::Encode(e.to_string()))?;
        Ok(out)
    }

    fn codec(&self) -> CodecId {
        CodecId::Jpeg
    }
}

/// JPEG decoder producing RGBA frames.
#[derive(Debug, Default)]
pub struct JpegDecoder;

impl VideoDecoder for JpegDecoder {
    fn decode(&mut self, data: &[u8], width: u32, height: u32) -> Result<Frame, CodecError> {
        let img = image::load_from_memory(data)
            .map_err(|e| CodecError::Decode(e.to_string()))?
            .to_rgba8();
        let (w, h) = (img.width(), img.height());
        if (w, h) != (width, height) {
            // Encoders may have sent a downscaled frame; the wire header is
            // authoritative for the *stream*, but the frame itself defines
            // its own size — trust the pixels, the viewer scales.
            tracing::debug!(target: "opacusr", "frame {w}x{h} differs from session {width}x{height}");
        }
        Ok(Frame::new(w, h, img.into_raw()))
    }

    fn codec(&self) -> CodecId {
        CodecId::Jpeg
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opacusr_capture::Frame;

    fn gradient(w: u32, h: u32) -> Frame {
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&[x as u8, y as u8, ((x + y) % 256) as u8, 255]);
            }
        }
        Frame::new(w, h, rgba)
    }

    #[test]
    fn jpeg_round_trip() {
        let frame = gradient(320, 200);
        let mut enc = JpegEncoder;
        let mut dec = JpegDecoder;
        let bytes = enc.encode(&frame, 75).unwrap();
        assert!(!bytes.is_empty());
        let back = dec.decode(&bytes, frame.width, frame.height).unwrap();
        assert_eq!((back.width, back.height), (320, 200));
        assert_eq!(back.rgba.len(), frame.rgba.len());
        // JPEG is lossy — check the picture is still *mostly* the gradient.
        let diff: usize = back
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .zip(frame.rgba.as_chunks::<4>().0)
            .map(|(a, b)| (a[0].abs_diff(b[0]) as usize) + (a[1].abs_diff(b[1]) as usize))
            .sum();
        let avg = diff / (320 * 200);
        assert!(avg < 12, "average per-pixel channel error too high: {avg}");
    }

    #[test]
    fn quality_affects_size() {
        let frame = gradient(512, 512);
        let mut enc = JpegEncoder;
        let hi = enc.encode(&frame, 95).unwrap();
        let lo = enc.encode(&frame, 25).unwrap();
        assert!(lo.len() < hi.len(), "low quality must compress better");
    }

    #[test]
    fn garbage_rejected() {
        let mut dec = JpegDecoder;
        assert!(dec.decode(&[0xFF, 0x00, 0x12], 10, 10).is_err());
    }

    #[test]
    fn size_mismatch_rejected_on_encode() {
        let mut enc = JpegEncoder;
        let bad = Frame {
            width: 100,
            height: 100,
            rgba: vec![0; 10 * 10 * 4],
        };
        assert!(enc.encode(&bad, 50).is_err());
    }
}
