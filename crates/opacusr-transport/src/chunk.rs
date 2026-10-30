//! Chunking for message-oriented transports with a per-message size limit
//! (WebRTC SCTP data channels cap messages, typically at 64 KiB).
//!
//! A payload ≤ [`CHUNK_THRESHOLD`] travels as a single data-channel message.
//! Larger payloads are split: the first fragment carries an 8-byte header
//! `[0x01][total_len: u32 LE][reserved: u32 LE = 0]`, all following
//! fragments are raw bytes. Channels are reliable *and ordered*, so plain
//! accumulation is sufficient and replay-free.

/// Payloads larger than this are fragmented (leave head-room under the
/// typical 64 KiB SCTP message limit).
pub const CHUNK_THRESHOLD: usize = 30_000;

const FLAG_SINGLE: u8 = 0x00;
const FLAG_STREAM_START: u8 = 0x01;

/// Outgoing fragmenter state (stateless apart from nothing — the protocol
/// is self-describing, so sending needs no state).
#[derive(Debug, Default, Clone, Copy)]
pub struct Chunker;

impl Chunker {
    /// Split `payload` into data-channel messages.
    pub fn split(payload: &[u8]) -> Vec<Vec<u8>> {
        if payload.len() <= CHUNK_THRESHOLD {
            let mut msg = Vec::with_capacity(1 + payload.len());
            msg.push(FLAG_SINGLE);
            msg.extend_from_slice(payload);
            return vec![msg];
        }
        let mut out = Vec::new();
        let mut msg = Vec::with_capacity(9 + CHUNK_THRESHOLD);
        msg.push(FLAG_STREAM_START);
        msg.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        msg.extend_from_slice(&0u32.to_le_bytes()); // reserved
        let first = CHUNK_THRESHOLD.min(payload.len());
        msg.extend_from_slice(&payload[..first]);
        out.push(msg);
        let mut off = first;
        while off < payload.len() {
            let end = (off + CHUNK_THRESHOLD).min(payload.len());
            out.push(payload[off..end].to_vec());
            off = end;
        }
        out
    }
}

/// Incoming reassembly state, one per channel.
#[derive(Debug, Default)]
pub struct Reassembler {
    total: Option<usize>,
    acc: Vec<u8>,
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one data-channel message; returns a complete payload when the
    /// stream finished. Errors on protocol violations.
    pub fn feed(&mut self, msg: &[u8]) -> Result<Option<Vec<u8>>, crate::TransportError> {
        if msg.is_empty() {
            return Err(crate::TransportError::Other(
                "empty data channel message".into(),
            ));
        }
        match msg[0] {
            FLAG_SINGLE => {
                if self.total.is_some() {
                    return Err(crate::TransportError::Other(
                        "unexpected single message mid-stream".into(),
                    ));
                }
                Ok(Some(msg[1..].to_vec()))
            }
            FLAG_STREAM_START => {
                if self.total.is_some() || msg.len() < 9 {
                    return Err(crate::TransportError::Other("bad stream start".into()));
                }
                let total = u32::from_le_bytes(msg[1..5].try_into().unwrap()) as usize;
                if total > opacusr_proto::framing::MAX_FRAME_LEN {
                    return Err(crate::TransportError::Other(format!(
                        "chunked payload {total} exceeds frame limit"
                    )));
                }
                self.total = Some(total);
                self.acc.extend_from_slice(&msg[9..]);
                self.maybe_complete()
            }
            _ => {
                if self.total.is_none() {
                    return Err(crate::TransportError::Other(
                        "stream data without start".into(),
                    ));
                }
                self.acc.extend_from_slice(msg);
                self.maybe_complete()
            }
        }
    }

    fn maybe_complete(&mut self) -> Result<Option<Vec<u8>>, crate::TransportError> {
        let total = self.total.unwrap_or(0);
        if self.acc.len() == total {
            self.total = None;
            Ok(Some(std::mem::take(&mut self.acc)))
        } else if self.acc.len() > total {
            Err(crate::TransportError::Other("chunk stream overflow".into()))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_payload_is_single_message() {
        let parts = Chunker::split(b"hello");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0][0], FLAG_SINGLE);
        let mut r = Reassembler::new();
        assert_eq!(r.feed(&parts[0]).unwrap().unwrap(), b"hello");
    }

    #[test]
    fn large_payload_round_trips() {
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let parts = Chunker::split(&payload);
        assert!(parts.len() >= 7);
        let mut r = Reassembler::new();
        let mut done = None;
        for (i, p) in parts.iter().enumerate() {
            let res = r.feed(p).unwrap();
            if i + 1 == parts.len() {
                done = res;
            } else {
                assert!(res.is_none());
            }
        }
        assert_eq!(done.unwrap(), payload);
    }

    #[test]
    fn interleaved_channels_are_independent() {
        let big: Vec<u8> = vec![7; 70_000];
        let a = Chunker::split(&big);
        let b = Chunker::split(b"tiny");
        let mut ra = Reassembler::new();
        let mut rb = Reassembler::new();
        // Feed one fragment of A between fragments of B — must not confuse.
        assert!(ra.feed(&a[0]).unwrap().is_none());
        assert_eq!(rb.feed(&b[0]).unwrap().unwrap(), b"tiny");
        let mut out = None;
        for p in &a[1..] {
            out = ra.feed(p).unwrap().or(out);
        }
        assert_eq!(out.unwrap(), big);
    }

    #[test]
    fn garbage_is_rejected() {
        let mut r = Reassembler::new();
        assert!(r.feed(&[]).is_err());
        assert!(r.feed(&[0x02, 1, 2]).is_err());
        assert!(r.feed(&[0x00]).is_ok_and(|x| x.unwrap().is_empty()));
        assert!(r.feed(&[0x01, 0xFF, 0xFF, 0xFF, 0x7F, 0, 0, 0, 0]).is_err()); // absurd length
    }
}
