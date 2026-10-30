//! Agent configuration file (TOML) with safe defaults.
//!
//! Location: `<config_dir>/opacusr/agent.toml`
//! (`~/.config/opacusr/agent.toml` on Linux,
//! `%APPDATA%\opacusr\agent.toml` on Windows, `~/Library/Application
//! Support/opacusr/agent.toml` on macOS).
//!
//! The unattended password is stored in this file with mode 0600 — it must
//! be recoverable to verify challenges. OS-keychain storage is a roadmap
//! item (docs/security.md).

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Agent settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    /// Signaling server URL, e.g. `ws://relay.example.com:8080/signal`.
    pub server_url: String,
    /// Name shown to viewers.
    pub name: String,
    /// Unattended access password (plaintext fallback; prefer the keychain
    /// via `password_in_keychain`). See `effective_password()`.
    pub password: Option<String>,
    /// `true` = the unattended password lives in the OS keychain and
    /// `password` is empty. Set by `opacusr-agent passwd`.
    pub password_in_keychain: bool,
    /// Streaming frame rate cap.
    pub fps: u8,
    /// JPEG quality 1..=100.
    pub jpeg_quality: u8,
    /// Downscale percent (100 = native).
    pub scale_percent: u16,
    /// Automatically accept incoming file transfers.
    pub auto_accept_files: bool,
    /// Where received files land.
    pub download_dir: PathBuf,
    /// TCP port for direct LAN sessions (`--allow-lan`).
    pub lan_port: u16,
    /// STUN/TURN servers (one URL per entry; `user:pass@host` style TURN
    /// URLs are parsed by the app layer).
    pub ice_urls: Vec<String>,
    /// Video codec: `auto` (H.264 when built with it, else JPEG), `jpeg`,
    /// or `h264` (falls back to JPEG when the feature is compiled out).
    pub codec: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        let download_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("OpacusR-Received");
        Self {
            server_url: "ws://127.0.0.1:8080/signal".into(),
            name: whoami_fallback(),
            password: None,
            password_in_keychain: false,
            fps: opacusr_capture::source::CAPTURE_FPS_DEFAULT,
            jpeg_quality: 70,
            scale_percent: 100,
            auto_accept_files: true,
            download_dir,
            lan_port: 54000,
            ice_urls: vec!["stun:stun.l.google.com:19302".into()],
            codec: "auto".into(),
        }
    }
}

fn whoami_fallback() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "OpacusR host".into())
}

impl AgentConfig {
    /// Default config file path.
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("opacusr")
            .join("agent.toml")
    }

    /// Load from the default path; missing file → defaults.
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &PathBuf) -> Self {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(target: "opacusr", "config {path:?} invalid ({e}); using defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Persist to the default path (0600 on unix).
    pub fn save(&self) -> std::io::Result<PathBuf> {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &PathBuf) -> std::io::Result<PathBuf> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        fs::write(path, &text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(path.clone())
    }

    /// The password the unattended mode actually verifies against:
    /// the file field first (legacy), then the OS keychain when
    /// `password_in_keychain` is set. `None` → attended mode.
    pub fn effective_password(&self) -> Option<String> {
        if let Some(p) = &self.password {
            return Some(p.clone());
        }
        if self.password_in_keychain {
            return match crate::keychain::load_password() {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(target: "opacusr", "{e}; unattended access disabled this run");
                    None
                }
            };
        }
        None
    }

    /// Whether unattended access is configured (password exists somewhere).
    pub fn is_unattended(&self) -> bool {
        self.password.is_some() || self.password_in_keychain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_via_toml() {
        let cfg = AgentConfig {
            name: "test-box".into(),
            fps: 24,
            ..Default::default()
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: AgentConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.name, "test-box");
        assert_eq!(back.fps, 24);
        assert_eq!(back.jpeg_quality, AgentConfig::default().jpeg_quality);
    }
}
