//! Plain TCP transport with stream framing (used for LAN connections,
//! relay fallback and in-process tests).

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use opacusr_proto::framing::{encode_frame, FrameDecoder};
use opacusr_proto::Channel;

use crate::proxy::Proxy;
use crate::{Result, Transport, TransportError};

const READ_BUF: usize = 16 * 1024;

/// Hard cap on the dial itself. Without it a firewalled/black-holed
/// address hangs on the OS connect timeout (up to ~130 s on Linux,
/// longer through some proxies) while the UI shows «Подключение…».
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub struct TcpTransport {
    stream: TcpStream,
    decoder: FrameDecoder,
}

impl TcpTransport {
    /// Connect to a listener (LAN mode: `ip:port` of `opacusr-agent --listen`).
    pub async fn connect(addr: &str) -> Result<Self> {
        Self::connect_via(addr, None).await
    }

    /// Connect directly, or through an outbound proxy when one is given
    /// (see [`crate::proxy`]; LAN/direct use keeps `None`).
    pub async fn connect_via(addr: &str, proxy: Option<&Proxy>) -> Result<Self> {
        let stream = match proxy {
            None => tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
                .await
                .map_err(|_| TransportError::Other(format!("connect to {addr} timed out")))?
                .map_err(TransportError::Io)?,
            Some(p) => tokio::time::timeout(CONNECT_TIMEOUT, p.connect(addr))
                .await
                .map_err(|_| TransportError::Other(format!("connect to {addr} timed out")))?
                .map_err(|e| TransportError::Other(format!("proxy connect to {addr}: {e}")))?,
        };
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
