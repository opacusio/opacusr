//! Stream framing for byte-stream transports (TCP, relay, ...).
//!
//! Frame layout: `[channel: 1 byte][length: u32 BE][payload]`.
//! WebRTC data channels preserve message boundaries, so they skip this
//! layer; the [`crate::Channel`] tag still travels as the first byte when
//! needed by simpler transports.
//!
//! Payloads are bincode 2.x (`bincode::serde` layer over the existing
//! serde types, `config::standard()` — varint integers). This differs
//! from the bincode 1.x byte format: peers must agree on
//! [`crate::PROTO_VERSION`] ≥ 9, which is exactly what the handshake
//! (and the mDNS TXT `proto` record) check.

use std::collections::VecDeque;

use crate::{message::CryptoEnvelope, Channel, Message, ProtoError, PROTO_VERSION};

/// Hard cap for a single frame payload (8 MiB). Guards against malformed or
/// malicious length prefixes.
pub const MAX_FRAME_LEN: usize = 8 * 1024 * 1024;

/// bincode configuration used on every hop (the wire format is a
/// protocol property — one definition, one config). Callers outside this
/// module (e.g. sealed-envelope paths in `opacusr-transport`) reuse it so
/// advertiser and receiver can never drift apart.
pub fn bincode_config() -> bincode::config::Configuration {
    bincode::config::standard()
}

/// Serialize a session message to a bincode payload (no framing).
pub fn encode_message(msg: &Message) -> Result<Vec<u8>, ProtoError> {
    bincode::serde::encode_to_vec(msg, bincode_config())
        .map_err(|e| ProtoError::Encode(e.to_string()))
}

/// Deserialize a session message from a bincode payload.
pub fn decode_message(payload: &[u8]) -> Result<Message, ProtoError> {
    bincode::serde::decode_from_slice(payload, bincode_config())
        .map(|(msg, _)| msg)
        .map_err(|e| ProtoError::Decode(e.to_string()))
}

/// Serialize a signaling message to a bincode payload.
pub fn encode_signal(msg: &crate::SignalMessage) -> Result<Vec<u8>, ProtoError> {
    bincode::serde::encode_to_vec(msg, bincode_config())
        .map_err(|e| ProtoError::Encode(e.to_string()))
}

/// Deserialize a signaling message from a bincode payload.
pub fn decode_signal(payload: &[u8]) -> Result<crate::SignalMessage, ProtoError> {
    bincode::serde::decode_from_slice(payload, bincode_config())
        .map(|(msg, _)| msg)
        .map_err(|e| ProtoError::Decode(e.to_string()))
}

/// Build one framed record for a stream transport.
pub fn encode_frame(channel: Channel, payload: &[u8]) -> Result<Vec<u8>, ProtoError> {
    if payload.len() > MAX_FRAME_LEN {
        return Err(ProtoError::TooLarge {
            size: payload.len(),
            limit: MAX_FRAME_LEN,
        });
    }
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(channel.tag());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Build a frame whose payload is a sealed [`CryptoEnvelope`].
pub fn encode_envelope_frame(
    channel: Channel,
    envelope: &CryptoEnvelope,
) -> Result<Vec<u8>, ProtoError> {
    encode_frame(
        channel,
        &bincode::serde::encode_to_vec(envelope, bincode_config())
            .map_err(|e| ProtoError::Encode(e.to_string()))?,
    )
}

/// Incremental frame decoder: feed arbitrary byte chunks, pull out complete
/// frames. Keeps a compact internal buffer.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: VecDeque<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append raw bytes received from the stream.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend(bytes.iter().copied());
    }

    /// Try to extract the next complete frame, if any.
    pub fn poll(&mut self) -> Result<Option<(Channel, Vec<u8>)>, ProtoError> {
        if self.buf.len() < 5 {
            return Ok(None);
        }
        let tag = *self.buf.front().unwrap();
        let len_bytes: Vec<u8> = self.buf.iter().skip(1).take(4).copied().collect();
        let mut arr = [0u8; 4];
        arr.copy_from_slice(&len_bytes);
        let len = u32::from_be_bytes(arr) as usize;
        if len > MAX_FRAME_LEN {
            return Err(ProtoError::BadFrame(format!(
                "frame length {len} exceeds limit"
            )));
        }
        if self.buf.len() < 5 + len {
            return Ok(None);
        }
        let channel = Channel::from_tag(tag)?;
        // Drain header + payload.
        for _ in 0..5 {
            self.buf.pop_front();
        }
        let payload: Vec<u8> = self.buf.drain(..len).collect();
        Ok(Some((channel, payload)))
    }

    /// Number of buffered bytes not yet forming a complete frame.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }
}

/// Convenience: encode a message as a complete frame on its channel.
pub fn frame_message(msg: &Message) -> Result<Vec<u8>, ProtoError> {
    encode_frame(msg.channel(), &encode_message(msg)?)
}

/// Sanity check used by tests: current protocol version must be ≥ 1.
pub fn protocol_version_is_supported() -> bool {
    PROTO_VERSION >= 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::*;

    fn sample() -> Message {
        Message::VideoFrame(VideoFrame {
            seq: 42,
            width: 1920,
            height: 1080,
            keyframe: true,
            codec: CodecId::Jpeg,
            data: vec![0xAB; 50_000],
        })
    }

    #[test]
    fn message_round_trip() {
        let msg = sample();
        let bytes = encode_message(&msg).unwrap();
        let back = decode_message(&bytes).unwrap();
        assert_eq!(format!("{back:?}"), format!("{msg:?}"));
    }

    #[test]
    fn frame_round_trip_and_channel() {
        let msg = sample();
        assert_eq!(msg.channel(), Channel::Media);
        let frame = frame_message(&msg).unwrap();
        assert_eq!(frame[0], Channel::Media.tag());
        let mut dec = FrameDecoder::new();
        dec.push(&frame);
        let (ch, payload) = dec.poll().unwrap().unwrap();
        assert_eq!(ch, Channel::Media);
        let back = decode_message(&payload).unwrap();
        assert!(matches!(back, Message::VideoFrame(vf) if vf.seq == 42 && vf.data.len() == 50_000));
        assert!(dec.poll().unwrap().is_none());
    }

    #[test]
    fn decoder_handles_chunked_input() {
        let mut dec = FrameDecoder::new();
        let frame = frame_message(&Message::Ping(7)).unwrap();
        for b in &frame {
            dec.push(&[*b]);
            // Frame must not complete until the last byte arrives.
            if dec.buffered() < frame.len() {
                assert!(dec.poll().unwrap().is_none());
            }
        }
        let (ch, payload) = dec.poll().unwrap().unwrap();
        assert_eq!(ch, Channel::Control);
        assert!(matches!(
            decode_message(&payload).unwrap(),
            Message::Ping(7)
        ));
    }

    #[test]
    fn decoder_rejects_absurd_length() {
        let mut dec = FrameDecoder::new();
        let mut frame = vec![Channel::Control.tag()];
        frame.extend_from_slice(&u32::MAX.to_be_bytes());
        dec.push(&frame);
        assert!(dec.poll().is_err());
    }

    #[test]
    fn bincode2_reports_bytes_read_and_stays_compact() {
        // bincode 2.x standard: small messages shrink to a couple of
        // varint bytes, and the serde decode entry point reports how many
        // bytes it consumed. FrameDecoder hands decode_message exact
        // length-prefixed slices, so the count must equal the payload
        // length — anything else would mean a framing bug upstream.
        let bytes = encode_message(&Message::Ping(7)).unwrap();
        assert!(
            bytes.len() < 8,
            "varint keeps small messages tiny: {bytes:?}"
        );
        let (msg, read) =
            bincode::serde::decode_from_slice::<Message, _>(&bytes, bincode_config()).unwrap();
        assert!(matches!(msg, Message::Ping(7)));
        assert_eq!(read, bytes.len(), "an exact slice is consumed fully");
    }

    #[test]
    fn envelope_frame_round_trip() {
        let env = CryptoEnvelope {
            counter: 5,
            ciphertext: vec![1, 2, 3],
        };
        let frame = encode_envelope_frame(Channel::Control, &env).unwrap();
        let mut dec = FrameDecoder::new();
        dec.push(&frame);
        let (_, payload) = dec.poll().unwrap().unwrap();
        let back: CryptoEnvelope = bincode::serde::decode_from_slice(&payload, bincode_config())
            .unwrap()
            .0;
        assert_eq!(back.counter, 5);
    }
}
