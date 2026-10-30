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
    /// Unattended access password (plain — see module docs).
    pub password: Option<String>,
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
    /// STUN/TURN servers (space- or comma-separated URLs inside each entry
    /// is not needed — one URL per entry; `user:pass@host` style TURN URLs
    /// are parsed by the app layer).
    pub ice_urls: Vec<String>,
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
            fps: opacusr_capture::source::CAPTURE_FPS_DEFAULT,
            jpeg_quality: 70,
            scale_percent: 100,
            auto_accept_files: true,
            download_dir,
            lan_port: 54000,
            ice_urls: vec!["stun:stun.l.google.com:19302".into()],
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
