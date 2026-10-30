//! WebRTC transport (webrtc-rs): two SCTP data channels ("control",
//! "media") inside one DTLS-protected peer connection.
//!
//! The viewer (offerer) creates both channels and ships the offer; the host
//! (answerer) captures the incoming channels via `on_data_channel`.
//! Signaling is non-trickle: each side waits for ICE gathering to complete
//! and ships the full SDP through the OpacusR signaling server. Trickle ICE
//! is on the roadmap (see docs/roadmap.md).

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::mpsc;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_gathering_state::RTCIceGatheringState;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

use opacusr_proto::Channel;

use crate::chunk::{Chunker, Reassembler};
use crate::{Result, Transport, TransportError};

/// One STUN/TURN server entry.
#[derive(Debug, Clone, Default)]
pub struct IceServerConfig {
    pub urls: Vec<String>,
    pub username: Option<String>,
    pub credential: Option<String>,
}

impl IceServerConfig {
    pub fn stun(urls: &[&str]) -> Self {
        Self {
            urls: urls.iter().map(|s| s.to_string()).collect(),
            username: None,
            credential: None,
        }
    }

    pub fn turn(urls: &[&str], username: &str, credential: &str) -> Self {
        Self {
            urls: urls.iter().map(|s| s.to_string()).collect(),
            username: Some(username.to_string()),
            credential: Some(credential.to_string()),
        }
    }
}

/// WebRTC endpoint configuration.
#[derive(Debug, Clone, Default)]
pub struct WebRtcConfig {
    pub ice_servers: Vec<IceServerConfig>,
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const GATHER_TIMEOUT: Duration = Duration::from_secs(5);
const CHANNELS_TIMEOUT: Duration = Duration::from_secs(5);

/// Create a peer connection with the given ICE configuration.
pub async fn new_peer_connection(cfg: &WebRtcConfig) -> Result<Arc<RTCPeerConnection>> {
    let api = APIBuilder::new().build();
    let rtc = RTCConfiguration {
        ice_servers: cfg
            .ice_servers
            .iter()
            .map(|s| RTCIceServer {
                urls: s.urls.clone(),
                username: s.username.clone().unwrap_or_default(),
                credential: s.credential.clone().unwrap_or_default(),
            })
            .collect(),
        ..Default::default()
    };
    let pc = api
        .new_peer_connection(rtc)
        .await
        .map_err(|e| TransportError::WebRtc(format!("new peer connection: {e}")))?;
    Ok(Arc::new(pc))
}

/// Offerer: create the two channels ("control", "media") with default
/// (reliable, ordered) options.
pub async fn create_channels(
    pc: &Arc<RTCPeerConnection>,
) -> Result<(Arc<RTCDataChannel>, Arc<RTCDataChannel>)> {
    let control = pc
        .create_data_channel("control", None)
        .await
        .map_err(|e| TransportError::WebRtc(format!("control channel: {e}")))?;
    let media = pc
        .create_data_channel("media", None)
        .await
        .map_err(|e| TransportError::WebRtc(format!("media channel: {e}")))?;
    Ok((control, media))
}

/// Offerer: create the offer and wait for ICE gathering, return the full
/// SDP (JSON-serialized `RTCSessionDescription`).
pub async fn make_offer(pc: &Arc<RTCPeerConnection>) -> Result<String> {
    let offer = pc
        .create_offer(None)
        .await
        .map_err(|e| TransportError::WebRtc(format!("create offer: {e}")))?;
    pc.set_local_description(offer)
        .await
        .map_err(|e| TransportError::WebRtc(format!("set local desc: {e}")))?;
    wait_gathered(pc).await;
    let desc = pc
        .local_description()
        .await
        .ok_or_else(|| TransportError::WebRtc("local description missing".into()))?;
    serde_json::to_string(&desc).map_err(|e| TransportError::WebRtc(format!("encode offer: {e}")))
}

/// Offerer: consume the remote answer.
pub async fn accept_answer(pc: &Arc<RTCPeerConnection>, answer_json: &str) -> Result<()> {
    let answer: RTCSessionDescription = serde_json::from_str(answer_json)
        .map_err(|e| TransportError::WebRtc(format!("decode answer: {e}")))?;
    pc.set_remote_description(answer)
        .await
        .map_err(|e| TransportError::WebRtc(format!("set remote desc: {e}")))?;
    Ok(())
}

fn attach_channel_pump(dc: &Arc<RTCDataChannel>, tx: mpsc::Sender<Vec<u8>>) {
    dc.on_message(Box::new(move |msg: DataChannelMessage| {
        let tx = tx.clone();
        Box::pin(async move {
            let _ = tx.send(msg.data.to_vec()).await;
        })
    }));
}

async fn wait_gathered(pc: &Arc<RTCPeerConnection>) {
    let deadline = Instant::now() + GATHER_TIMEOUT;
    while pc.ice_gathering_state() != RTCIceGatheringState::Complete {
        if Instant::now() >= deadline {
            tracing::warn!(target: "opacusr", "ICE gathering did not complete in time, continuing");
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Wait until the peer connection is fully established (or failed).
pub async fn wait_connected(pc: &Arc<RTCPeerConnection>) -> Result<()> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match pc.connection_state() {
            RTCPeerConnectionState::Connected => return Ok(()),
            RTCPeerConnectionState::Failed
            | RTCPeerConnectionState::Closed
            | RTCPeerConnectionState::Disconnected => {
                return Err(TransportError::WebRtc(format!(
                    "peer connection state: {:?}",
                    pc.connection_state()
                )));
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(TransportError::Timeout);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Answerer state machine: attach channel capture *before* applying the
/// remote offer (required by the WebRTC API), then finish the handshake.
pub struct AnswerPending {
    pc: Arc<RTCPeerConnection>,
    incoming: mpsc::Receiver<(String, Arc<RTCDataChannel>)>,
    control: Option<(Arc<RTCDataChannel>, mpsc::Receiver<Vec<u8>>)>,
    media: Option<(Arc<RTCDataChannel>, mpsc::Receiver<Vec<u8>>)>,
}

impl AnswerPending {
    pub fn new(pc: Arc<RTCPeerConnection>) -> Self {
        let (tx, rx) = mpsc::channel::<(String, Arc<RTCDataChannel>)>(4);
        pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
            let tx = tx.clone();
            Box::pin(async move {
                let _ = tx.send((dc.label().to_string(), dc)).await;
            })
        }));
        Self {
            pc,
            incoming: rx,
            control: None,
            media: None,
        }
    }

    /// Apply the remote offer and produce the gathered answer SDP.
    pub async fn accept_offer(&mut self, offer_json: &str) -> Result<String> {
        let offer: RTCSessionDescription = serde_json::from_str(offer_json)
            .map_err(|e| TransportError::WebRtc(format!("decode offer: {e}")))?;
        self.pc
            .set_remote_description(offer)
            .await
            .map_err(|e| TransportError::WebRtc(format!("set remote desc: {e}")))?;

        let answer = self
            .pc
            .create_answer(None)
            .await
            .map_err(|e| TransportError::WebRtc(format!("create answer: {e}")))?;
        self.pc
            .set_local_description(answer)
            .await
            .map_err(|e| TransportError::WebRtc(format!("set local desc: {e}")))?;
        wait_gathered(&self.pc).await;

        // NOTE: the remote-created data channels are announced only after
        // DTLS/SCTP come up — that happens once the offerer applies our
        // answer, so channel collection happens in [`Self::finish`], not
        // here.

        let desc = self
            .pc
            .local_description()
            .await
            .ok_or_else(|| TransportError::WebRtc("local description missing".into()))?;
        serde_json::to_string(&desc)
            .map_err(|e| TransportError::WebRtc(format!("encode answer: {e}")))
    }

    /// Collect the incoming data channels, wait for the connection to
    /// establish, and return the ready transport.
    pub async fn finish(mut self) -> Result<WebRtcTransport> {
        let deadline = Instant::now() + CHANNELS_TIMEOUT;
        while self.control.is_none() || self.media.is_none() {
            let timeout = deadline.saturating_duration_since(Instant::now());
            if timeout.is_zero() {
                return Err(TransportError::WebRtc(
                    "data channels not announced in time".into(),
                ));
            }
            match tokio::time::timeout(timeout, self.incoming.recv()).await {
                Ok(Some((label, dc))) => match label.as_str() {
                    "control" => {
                        let (tx, rx) = mpsc::channel(64);
                        attach_channel_pump(&dc, tx);
                        self.control = Some((dc, rx));
                    }
                    "media" => {
                        let (tx, rx) = mpsc::channel(256);
                        attach_channel_pump(&dc, tx);
                        self.media = Some((dc, rx));
                    }
                    other => tracing::debug!(target: "opacusr", "ignoring data channel {other:?}"),
                },
                Ok(None) => return Err(TransportError::Closed),
                Err(_) => return Err(TransportError::Timeout),
            }
        }

        let (control, rx_ctl) = self.control.take().expect("channels collected above");
        let (media, rx_media) = self.media.take().expect("channels collected above");
        wait_connected(&self.pc).await?;
        Ok(WebRtcTransport {
            pc: self.pc,
            control,
            media,
            rx_ctl,
            rx_media,
            ctl_re: Reassembler::new(),
            med_re: Reassembler::new(),
        })
    }
}

/// [`Transport`] implementation over the two data channels.
pub struct WebRtcTransport {
    pc: Arc<RTCPeerConnection>,
    control: Arc<RTCDataChannel>,
    media: Arc<RTCDataChannel>,
    rx_ctl: mpsc::Receiver<Vec<u8>>,
    rx_media: mpsc::Receiver<Vec<u8>>,
    ctl_re: Reassembler,
    med_re: Reassembler,
}

impl WebRtcTransport {
    /// Finalize the offerer side: attach pumps and await the handshake.
    pub async fn from_offerer(
        pc: Arc<RTCPeerConnection>,
        control: Arc<RTCDataChannel>,
        media: Arc<RTCDataChannel>,
    ) -> Result<Self> {
        let (tx_ctl, rx_ctl) = mpsc::channel(64);
        let (tx_med, rx_media) = mpsc::channel(256);
        attach_channel_pump(&control, tx_ctl);
        attach_channel_pump(&media, tx_med);
        wait_connected(&pc).await?;
        Ok(Self {
            pc,
            control,
            media,
            rx_ctl,
            rx_media,
            ctl_re: Reassembler::new(),
            med_re: Reassembler::new(),
        })
    }
}

#[async_trait]
impl Transport for WebRtcTransport {
    async fn send(&mut self, channel: Channel, payload: &[u8]) -> Result<()> {
        let dc = match channel {
            Channel::Control => &self.control,
            Channel::Media => &self.media,
        };
        for part in Chunker::split(payload) {
            dc.send(&Bytes::from(part))
                .await
                .map_err(|e| TransportError::WebRtc(format!("data channel send: {e}")))?;
        }
        Ok(())
    }

    async fn recv(&mut self) -> Result<(Channel, Vec<u8>)> {
        tokio::select! {
            msg = self.rx_ctl.recv() => {
                let msg = msg.ok_or(TransportError::Closed)?;
                if let Some(payload) = self.ctl_re.feed(&msg)? {
                    return Ok((Channel::Control, payload));
                }
                // Incomplete chunk stream — recurse (channels are ordered,
                // so this terminates).
                self.recv().await
            }
            msg = self.rx_media.recv() => {
                let msg = msg.ok_or(TransportError::Closed)?;
                if let Some(payload) = self.med_re.feed(&msg)? {
                    return Ok((Channel::Media, payload));
                }
                self.recv().await
            }
        }
    }

    fn wire_confidential(&self) -> bool {
        true
    }

    fn describe(&self) -> String {
        "webrtc(dtls)".into()
    }

    async fn close(&mut self) {
        let _ = self.pc.close().await;
    }
}
