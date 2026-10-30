//! OpacusR host agent: accepts sessions, streams the screen, executes
//! input, serves file transfers and terminals, supports unattended access.

pub mod adaptive;
pub mod config;
pub mod keychain;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod pty;
pub mod service;
pub mod session;

pub use config::AgentConfig;
pub use keychain::{delete_password, load_password, store_password};
pub use service::{AgentService, SessionDecision};
pub use session::{CodecChoice, HostEvent, SessionConfig, SessionHost};
