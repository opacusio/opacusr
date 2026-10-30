//! Stream framing for byte-stream transports (TCP, relay, ...).
//!
//! Frame layout: `[channel: 1 byte][length: u32 BE][payload]`.
//! WebRTC data channels preserve message boundaries, so they skip this
//! layer; the [`crate::Channel`] tag still travels as the first byte when
//! needed by simpler transports.

use std::collections::VecDeque;

use crate::{message::CryptoEnvelope, Channel, Message, ProtoError, PROTO_VERSION};

/// Hard cap for a single frame payload (8 MiB). Guards against malformed or
/// malicious length prefixes.
pub const MAX_FRAME_LEN: usize = 8 * 1024 * 1024;

/// Serialize a session message to a bincode payload (no framing).
pub fn encode_message(msg: &Message) -> Result<Vec<u8>, ProtoError> {
    bincode::serialize(msg).map_err(|e| ProtoError::Encode(e.to_string()))
}

/// Deserialize a session message from a bincode payload.
pub fn decode_message(payload: &[u8]) -> Result<Message, ProtoError> {
    bincode::deserialize(payload).map_err(|e| ProtoError::Decode(e.to_string()))
}

/// Serialize a signaling message to a bincode payload.
pub fn encode_signal(msg: &crate::SignalMessage) -> Result<Vec<u8>, ProtoError> {
    bincode::serialize(msg).map_err(|e| ProtoError::Encode(e.to_string()))
}

/// Deserialize a signaling message from a bincode payload.
pub fn decode_signal(payload: &[u8]) -> Result<crate::SignalMessage, ProtoError> {
    bincode::deserialize(payload).map_err(|e| ProtoError::Decode(e.to_string()))
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
        &bincode::serialize(envelope).map_err(|e| ProtoError::Encode(e.to_string()))?,
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
    fn envelope_frame_round_trip() {
        let env = CryptoEnvelope {
            counter: 5,
            ciphertext: vec![1, 2, 3],
        };
        let frame = encode_envelope_frame(Channel::Control, &env).unwrap();
        let mut dec = FrameDecoder::new();
        dec.push(&frame);
        let (_, payload) = dec.poll().unwrap().unwrap();
        let back: CryptoEnvelope = bincode::deserialize(&payload).unwrap();
        assert_eq!(back.counter, 5);
    }
}
