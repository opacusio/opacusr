//! OpacusR host agent: accepts sessions, streams the screen, executes
//! input, serves file transfers and terminals, supports unattended access.

pub mod config;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod pty;
pub mod service;
pub mod session;

pub use config::AgentConfig;
pub use service::AgentService;
pub use session::{HostEvent, SessionConfig, SessionHost};
