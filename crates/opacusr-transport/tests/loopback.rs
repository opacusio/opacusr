//! End-to-end transport tests: TCP loopback, secure session, and an
//! in-process WebRTC handshake (both peer connections in one process).

use opacusr_crypto::SessionCrypto;
use opacusr_proto::{Channel, Message};
use opacusr_transport::secure::SecureSession;
use opacusr_transport::tcp::TcpTransport;
use opacusr_transport::webrtc::{
    accept_answer, create_channels, make_offer, new_peer_connection, AnswerPending, WebRtcConfig,
    WebRtcTransport,
};
use opacusr_transport::Transport;

#[tokio::test]
async fn tcp_round_trip_with_and_without_crypto() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut sess = SecureSession::new(
            Box::new(TcpTransport::from_stream(sock)),
            Some(SessionCrypto::new(&[42u8; 32])),
        );
        let msg = sess.recv_msg().await.unwrap();
        sess.send_msg(&Message::Pong(1)).await.unwrap();
        msg
    });

    let mut client = SecureSession::new(
        Box::new(TcpTransport::connect(&addr).await.unwrap()),
        Some(SessionCrypto::new(&[42u8; 32])),
    );
    client
        .send_msg(&Message::ChatMessage(opacusr_proto::message::ChatMessage {
            id: 5,
            text: "привет, OpacusR!".into(),
            ts_ms: 123,
        }))
        .await
        .unwrap();
    let reply = client.recv_msg().await.unwrap();
    let served = server.await.unwrap();
    assert!(matches!(served, Message::ChatMessage(ref c) if c.text.contains("OpacusR")));
    assert!(matches!(reply, Message::Pong(1)));
    assert!(client.app_encrypted() && !client.wire_confidential());
}

#[tokio::test]
async fn tcp_large_media_frame() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let big = vec![0xCDu8; 300_000];

    let server = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let mut t = TcpTransport::from_stream(sock);
        let (ch, payload) = t.recv().await.unwrap();
        (ch, payload)
    });

    let mut t = TcpTransport::connect(&addr).await.unwrap();
    t.send(Channel::Media, &big).await.unwrap();
    let (ch, payload) = server.await.unwrap();
    assert_eq!(ch, Channel::Media);
    assert_eq!(payload, big);
}

#[tokio::test]
async fn webrtc_loopback_round_trip() {
    let cfg = WebRtcConfig::default();
    let pc_a = new_peer_connection(&cfg).await.unwrap();
    let (ca, ma) = create_channels(&pc_a).await.unwrap();
    let offer = make_offer(&pc_a).await.unwrap();

    let pc_b = new_peer_connection(&cfg).await.unwrap();
    let mut pending = AnswerPending::new(pc_b);
    let answer = pending.accept_offer(&offer).await.unwrap();
    accept_answer(&pc_a, &answer).await.unwrap();

    let mut ta = WebRtcTransport::from_offerer(pc_a, ca, ma).await.unwrap();
    let mut tb = pending.finish().await.unwrap();

    assert!(ta.wire_confidential());

    ta.send(Channel::Control, b"hello").await.unwrap();
    ta.send(Channel::Media, &vec![0xABu8; 100_000])
        .await
        .unwrap();

    let (ch, payload) = tb.recv().await.unwrap();
    assert_eq!(
        (ch, payload.as_slice()),
        (Channel::Control, b"hello".as_slice())
    );
    let (ch, payload) = tb.recv().await.unwrap();
    assert_eq!(ch, Channel::Media);
    assert_eq!(payload.len(), 100_000);

    ta.close().await;
    tb.close().await;
}
