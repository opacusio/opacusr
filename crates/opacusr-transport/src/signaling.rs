//! WebSocket client for the OpacusR signaling server.
//!
//! Transport: WebSocket binary frames carrying bincode-encoded
//! [`SignalMessage`]s. `wss://` is supported (rustls with webpki roots).

use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tokio_tungstenite::{client_async_tls, connect_async, MaybeTlsStream, WebSocketStream};

use opacusr_proto::framing::{decode_signal, encode_signal};
use opacusr_proto::signal::*;

use crate::{proxy::Proxy, Result, TransportError};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Signaling connection to `ws(s)://host:port/signal`.
pub struct SignalingClient {
    ws: WsStream,
    url: String,
}

impl SignalingClient {
    pub async fn connect(url: &str) -> Result<Self> {
        Self::connect_via(url, None).await
    }

    /// Connect, optionally dialing through an outbound proxy
    /// (`crate::proxy::Proxy`): the raw TCP connection is established via
    /// the proxy first, then the WebSocket (and TLS for `wss://`) runs
    /// over that tunnel.
    pub async fn connect_via(url: &str, proxy: Option<&Proxy>) -> Result<Self> {
        let request = url.into_client_request().map_err(|e| {
            TransportError::Signaling(format!("invalid signaling url {url:?}: {e}"))
        })?;
        let (ws, _resp) = match proxy {
            None => connect_async(request)
                .await
                .map_err(|e| TransportError::Signaling(format!("connect to {url} failed: {e}")))?,
            Some(p) => {
                let hostport = url_host_port(url).ok_or_else(|| {
                    TransportError::Signaling(format!("cannot derive host:port from {url:?}"))
                })?;
                let stream = p.connect(&hostport).await.map_err(|e| {
                    TransportError::Signaling(format!("connect to the proxy failed: {e}"))
                })?;
                client_async_tls(request, stream).await.map_err(|e| {
                    TransportError::Signaling(format!("ws handshake via proxy failed: {e}"))
                })?
            }
        };
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
            .send(WsMessage::Binary(bytes.into()))
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

/// Derive the TCP `host:port` a WebSocket URL dials (defaults: ws → 80,
/// wss → 443). Manual parsing keeps the dependency tree tiny.
fn url_host_port(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let default_port = match scheme {
        "ws" => 80,
        "wss" => 443,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    if let Some(close) = authority.rfind(']') {
        // Bracketed IPv6 literal, optionally followed by :port.
        let host = &authority[..=close];
        let port = match authority[close + 1..].strip_prefix(':') {
            Some(p) => p.to_string(),
            None => default_port.to_string(),
        };
        return Some(format!("{host}:{port}"));
    }
    match authority.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() => Some(format!("{h}:{p}")),
        _ => Some(format!("{authority}:{default_port}")),
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

    #[test]
    fn host_port_from_urls() {
        assert_eq!(
            url_host_port("ws://h:9000/signal"),
            Some("h:9000".to_string())
        );
        assert_eq!(url_host_port("wss://h/signal"), Some("h:443".to_string()));
        assert_eq!(
            url_host_port("ws://[::1]:80/"),
            Some("[::1]:80".to_string())
        );
        assert_eq!(url_host_port("nonsense"), None);
    }
}
