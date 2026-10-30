//! Plain TCP transport with stream framing (used for LAN connections,
//! relay fallback and in-process tests).

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use opacusr_proto::framing::{encode_frame, FrameDecoder};
use opacusr_proto::Channel;

use crate::{Result, Transport, TransportError};

const READ_BUF: usize = 16 * 1024;

pub struct TcpTransport {
    stream: TcpStream,
    decoder: FrameDecoder,
}

impl TcpTransport {
    /// Connect to a listener (LAN mode: `ip:port` of `opacusr-agent --listen`).
    pub async fn connect(addr: &str) -> Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        stream.set_nodelay(true).ok();
        Ok(Self {
            stream,
            decoder: FrameDecoder::new(),
        })
    }

    /// Wrap an already accepted stream (server side of a LAN session).
    pub fn from_stream(stream: TcpStream) -> Self {
        stream.set_nodelay(true).ok();
        Self {
            stream,
            decoder: FrameDecoder::new(),
        }
    }
}

#[async_trait]
impl Transport for TcpTransport {
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
            let n = self.stream.read(&mut buf).await.map_err(|e| {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    TransportError::Closed
                } else {
                    TransportError::Io(e)
                }
            })?;
            if n == 0 {
                return Err(TransportError::Closed);
            }
            self.decoder.push(&buf[..n]);
        }
    }

    fn describe(&self) -> String {
        self.stream
            .peer_addr()
            .map(|a| format!("tcp({a})"))
            .unwrap_or_else(|_| "tcp(?)".into())
    }

    async fn close(&mut self) {
        // Dropping TcpStream shuts the socket down; nothing else to do.
    }
}
