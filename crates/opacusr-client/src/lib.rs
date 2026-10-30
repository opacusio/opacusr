//! OpacusR viewer client: establish sessions, decode frames, send input,
//! exchange chat/files/terminal — as a library used by the GUI and tests.

pub mod audio;
pub mod session;

pub use session::{ClientEvent, Command, SessionClient, TransportChoice};
