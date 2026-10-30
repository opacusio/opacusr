//! Session messages exchanged between the viewing client and the host agent
//! (after signaling, over `Channel::Control` / `Channel::Media`).

use serde::{Deserialize, Serialize};

/// Crypto payload wrapper used when the underlying transport does not
/// provide confidentiality (plain TCP / relay). The whole bincode-encoded
/// [`Message`] is sealed into `ciphertext` (which includes the 16-byte
/// Poly1305 tag). `counter` is a strict monotonic per-direction nonce.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CryptoEnvelope {
    pub counter: u64,
    pub ciphertext: Vec<u8>,
}

/// Protocol codec negotiated for the video stream. v0.1 ships JPEG full
/// frames; v0.2 adds H.264 (Cisco OpenH264). New variants must be appended
/// so bincode discriminants stay stable across versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodecId {
    Jpeg,
    /// H.264 via the BSD-licensed Cisco OpenH264 implementation.
    H264,
}

/// Modifier-key bitmask for [`KeyEvent`].
pub mod mods {
    pub const SHIFT: u8 = 1 << 0;
    pub const CTRL: u8 = 1 << 1;
    pub const ALT: u8 = 1 << 2;
    pub const META: u8 = 1 << 3;
}

/// A logical key. Printable characters ride in [`OpKey::Char`]; everything
/// else is an explicit variant so both sides map to their native keycodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpKey {
    Char(char),
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Space,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    CapsLock,
    NumLock,
    PrintScreen,
    ContextMenu,
}

/// Mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButtonId {
    Left,
    Right,
    Middle,
}

/// KDF parameters chosen by the host for password derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    /// Argon2 memory cost, KiB.
    pub m_cost_kib: u32,
    /// Argon2 time cost (iterations).
    pub t_cost: u32,
    /// Argon2 parallelism (lanes).
    pub parallelism: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        // ~19 MiB, 3 passes, 1 lane: fast enough on phones, strong enough
        // for an interactive remote-desktop password.
        Self {
            m_cost_kib: 19_456,
            t_cost: 3,
            parallelism: 1,
        }
    }
}

/// First message sent by both peers on the control channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub proto_version: u32,
    pub app_version: String,
    pub platform: crate::Platform,
    pub display_name: String,
    /// `true` = host agent, `false` = viewer.
    pub is_agent: bool,
}

/// Host → viewer: prove you know the password.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthChallenge {
    /// Per-session random salt for PSK derivation.
    pub salt: [u8; 16],
    /// Per-session random nonce to be signed.
    pub nonce: [u8; 16],
    pub kdf: KdfParams,
}

/// Viewer → host: HMAC proof over the challenge nonce.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResponse {
    pub proof: [u8; 32],
}

/// Host → viewer: authentication verdict + session salt for key derivation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResult {
    pub ok: bool,
    pub error: Option<String>,
    /// Random salt used with the verified PSK to derive the session key.
    pub session_salt: [u8; 16],
}

/// Host → viewer: session capabilities (sent after successful auth).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionAccept {
    pub screen_w: u32,
    pub screen_h: u32,
    pub codec: CodecId,
    pub fps_hint: u8,
}

/// Host → viewer: session refused.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionReject {
    pub reason: String,
}

/// Viewer → host: change streaming parameters.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct QualitySet {
    pub fps: u8,
    /// JPEG quality 1..=100.
    pub jpeg_quality: u8,
    /// Downscale percentage (100 = native resolution).
    pub scale_percent: u16,
}

/// Host → viewer: one compressed video frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoFrame {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub keyframe: bool,
    pub codec: CodecId,
    pub data: Vec<u8>,
}

/// Viewer → host: move cursor to absolute remote-screen pixel.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MouseMove {
    pub x: u32,
    pub y: u32,
}

/// Viewer → host: press/release a mouse button at a position.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MouseButton {
    pub x: u32,
    pub y: u32,
    pub button: MouseButtonId,
    pub down: bool,
}

/// Viewer → host: scroll wheel delta (positive = up / right).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MouseWheel {
    pub x: u32,
    pub y: u32,
    pub dx: i32,
    pub dy: i32,
}

/// Viewer → host: key press/release with modifiers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct KeyEvent {
    pub key: OpKey,
    pub mods: u8,
    pub down: bool,
}

/// Viewer → host: inject a text string (mobile keyboards, IME).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextInput {
    pub text: String,
}

/// Either side → other: sync a text clipboard snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardText {
    pub text: String,
}

/// Either side → other: one chat bubble.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: u64,
    pub text: String,
    pub ts_ms: u64,
}

/// Sender → receiver: offer a file. `sha256` covers the whole file so the
/// receiver can verify integrity on completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtOffer {
    pub id: u32,
    pub name: String,
    pub size: u64,
    pub chunk_size: u32,
    pub sha256: [u8; 32],
}

/// Receiver → sender: accept transfer, possibly resuming from `offset`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FtAccept {
    pub id: u32,
    pub offset: u64,
}

/// Receiver → sender: refuse the transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtReject {
    pub id: u32,
    pub reason: String,
}

/// Sender → receiver: one file chunk. `seq` counts chunks from 0.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtChunk {
    pub id: u32,
    pub seq: u64,
    pub data: Vec<u8>,
}

/// Receiver → sender: cumulative ack — all chunks `seq <= acked_seq` are
/// durably written.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FtAck {
    pub id: u32,
    pub acked_seq: u64,
}

/// Sender → receiver: transfer finished.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FtComplete {
    pub id: u32,
    /// Receiver-side sha256 matched the offer.
    pub verified: bool,
}

/// Either side → other: abort transfer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtCancel {
    pub id: u32,
    pub reason: String,
}

/// Viewer → host: open a remote terminal of the given size.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TermOpen {
    pub id: u32,
    pub rows: u16,
    pub cols: u16,
}

/// Host → viewer: terminal spawned or failed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermOpened {
    pub id: u32,
    pub ok: bool,
    pub error: Option<String>,
    pub shell: String,
}

/// Both directions: raw PTY bytes (output host→viewer, input viewer→host).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TermData {
    pub id: u32,
    pub data: Vec<u8>,
}

/// Viewer → host: terminal resized.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TermResize {
    pub id: u32,
    pub rows: u16,
    pub cols: u16,
}

/// Either side → other: close terminal.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TermClose {
    pub id: u32,
}

/// Reason a session ended.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ByeReason {
    Normal,
    Error(String),
}

/// Every application-level message of an established session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    // --- handshake / auth ---
    Hello(Hello),
    AuthChallenge(AuthChallenge),
    AuthResponse(AuthResponse),
    AuthResult(AuthResult),

    // --- session control ---
    SessionAccept(SessionAccept),
    SessionReject(SessionReject),
    QualitySet(QualitySet),
    /// `true` = pause the stream, `false` = resume.
    Pause(bool),
    /// Transport-level liveness probe (carries a monotonic counter).
    Ping(u64),
    Pong(u64),
    Bye(ByeReason),

    // --- desktop streaming ---
    VideoFrame(VideoFrame),

    // --- input ---
    MouseMove(MouseMove),
    MouseButton(MouseButton),
    MouseWheel(MouseWheel),
    KeyEvent(KeyEvent),
    TextInput(TextInput),

    // --- clipboard ---
    ClipboardText(ClipboardText),

    // --- chat ---
    ChatMessage(ChatMessage),

    // --- file transfer ---
    FtOffer(FtOffer),
    FtAccept(FtAccept),
    FtReject(FtReject),
    FtChunk(FtChunk),
    FtAck(FtAck),
    FtComplete(FtComplete),
    FtCancel(FtCancel),

    // --- terminal ---
    TermOpen(TermOpen),
    TermOpened(TermOpened),
    TermData(TermData),
    TermResize(TermResize),
    TermClose(TermClose),
}

impl Message {
    /// `true` for messages that belong on [`Channel::Media`].
    pub fn channel(&self) -> crate::Channel {
        match self {
            Message::VideoFrame(_) => crate::Channel::Media,
            _ => crate::Channel::Control,
        }
    }
}
