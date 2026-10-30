//! Client side of the OpacusR TCP relay.
//!
//! Used when direct connectivity (WebRTC ICE) fails: both peers open a TCP
//! connection to the server's relay port, present the same 16-byte ticket
//! (obtained from signaling via `RelayAlloc` → `RelayTicket`) and declare a
//! role. The server pairs viewer+host and pipes bytes. The server is
//! untrusted — [`crate::secure::SecureSession`] always encrypts over relay.

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use opacusr_proto::framing::{encode_frame, FrameDecoder};
use opacusr_proto::Channel;

use crate::{Result, Transport, TransportError};

const READ_BUF: usize = 16 * 1024;

/// Role marker sent right after the ticket so the server can pair peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayRole {
    Viewer = 0,
    Host = 1,
}

pub struct RelayTransport {
    stream: TcpStream,
    decoder: FrameDecoder,
    server: String,
}

impl RelayTransport {
    /// Connect to `server` (`host:port` of the relay listener) with a ticket
    /// obtained from signaling.
    pub async fn connect(server: &str, ticket: [u8; 16], role: RelayRole) -> Result<Self> {
        let mut stream = TcpStream::connect(server).await?;
        stream.set_nodelay(true).ok();
        // Preamble: ticket ‖ role byte. The server reads exactly 17 bytes
        // and starts piping once both sides of a ticket arrived.
        let mut preamble = Vec::with_capacity(17);
        preamble.extend_from_slice(&ticket);
        preamble.push(role as u8);
        stream.write_all(&preamble).await?;
        stream.flush().await?;
        Ok(Self {
            stream,
            decoder: FrameDecoder::new(),
            server: server.to_string(),
        })
    }
}

#[async_trait]
impl Transport for RelayTransport {
    async fn send(&mut self, channel: Channel, payload: &[u8]) -> Result<()> {
        let frame = encode_frame(channel, payload)?;
        self.stream.write_all(&frame).await?;
        self.stream.flush().await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<(Channel, Vec<u8>)> {
        loop {
            if let Some(frame) = self.decoder.poll()? {
                return Ok(frame);
            }
            let mut buf = vec![0u8; READ_BUF];
            let n = self.stream.read(&mut buf).await?;
            if n == 0 {
                return Err(TransportError::Closed);
            }
            self.decoder.push(&buf[..n]);
        }
    }

    fn describe(&self) -> String {
        format!("relay({})", self.server)
    }

    async fn close(&mut self) {}
}
