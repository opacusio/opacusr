//! WebSocket client for the OpacusR signaling server.
//!
//! Transport: WebSocket binary frames carrying bincode-encoded
//! [`SignalMessage`]s. `wss://` is supported (rustls with webpki roots).

use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use opacusr_proto::framing::{decode_signal, encode_signal};
use opacusr_proto::signal::*;

use crate::{Result, TransportError};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Signaling connection to `ws(s)://host:port/signal`.
pub struct SignalingClient {
    ws: WsStream,
    url: String,
}

impl SignalingClient {
    pub async fn connect(url: &str) -> Result<Self> {
        let request = url.into_client_request().map_err(|e| {
            TransportError::Signaling(format!("invalid signaling url {url:?}: {e}"))
        })?;
        let (ws, _resp) = connect_async(request)
            .await
            .map_err(|e| TransportError::Signaling(format!("connect to {url} failed: {e}")))?;
        Ok(Self {
            ws,
            url: url.to_string(),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Send one signaling message.
    pub async fn send(&mut self, msg: &SignalMessage) -> Result<()> {
        let bytes = encode_signal(msg)?;
        self.ws
            .send(WsMessage::Binary(bytes))
            .await
            .map_err(|e| TransportError::Signaling(format!("send failed: {e}")))
    }

    /// Receive the next signaling message (skips control frames).
    pub async fn recv(&mut self) -> Result<SignalMessage> {
        loop {
            match self.ws.next().await {
                Some(Ok(WsMessage::Binary(bytes))) => {
                    return decode_signal(&bytes).map_err(TransportError::Proto);
                }
                Some(Ok(WsMessage::Text(t))) => {
                    tracing::debug!(target: "opacusr", "ignoring text frame: {t}");
                }
                Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_))) => {}
                Some(Ok(WsMessage::Close(c))) => {
                    return Err(TransportError::Signaling(format!(
                        "server closed the connection: {c:?}"
                    )));
                }
                Some(Err(e)) => return Err(TransportError::Signaling(format!("ws error: {e}"))),
                None => return Err(TransportError::Closed),
            }
        }
    }

    /// Close the WebSocket politely.
    pub async fn close(&mut self) {
        let _ = self.ws.close(None).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_rejects_bad_url() {
        let res = SignalingClient::connect("not-a-websocket-url").await;
        assert!(res.is_err());
    }
}
