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

/// Permission level granted to a viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Permission {
    /// Full control (input, files, terminal).
    Full,
    /// Watch only: the host ignores input, file and terminal messages.
    ViewOnly,
    /// Reject the connection outright.
    Deny,
}

impl Permission {
    /// Parse a config string (`full` | `view-only` | `view` | `deny`).
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "view-only" | "view_only" | "view" | "viewonly" => Self::ViewOnly,
            "deny" | "reject" | "blocked" => Self::Deny,
            _ => Self::Full,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Permission::Full => "full",
            Permission::ViewOnly => "view-only",
            Permission::Deny => "deny",
        }
    }
}

/// One ACL entry: viewers whose self-reported display name matches
/// `peer` (case-insensitive) get `permission`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AclRule {
    pub peer: String,
    pub permission: String,
}

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
    /// Synchronize the clipboard with connected viewers (both directions).
    pub clipboard_sync: bool,
    /// Adapt fps/quality/scale to the measured link quality.
    pub adaptive_quality: bool,
    /// Permission for unattended connections not matched by `acl_rules`:
    /// `full` | `view-only` | `deny`.
    pub default_permission: String,
    /// Per-viewer overrides (matched by display name, case-insensitive).
    pub acl_rules: Vec<AclRule>,
    /// Offer audio streaming (`AudioControl` from viewers).
    pub audio: bool,
    /// Audio capture device substring (`""` = system default; on Windows
    /// an empty value prefers WASAPI loopback of the default output).
    pub audio_device: String,
    /// Windows capture backend: `auto` (DXGI, GDI fallback) | `gdi` | `dxgi`.
    /// Ignored on other platforms.
    pub capture: String,
    /// Auth lockout: failed password proofs allowed within
    /// `auth_window_secs` before new handshakes are refused for
    /// `auth_lockout_secs` (doubling per consecutive lockout, capped at
    /// 15 minutes). `0` disables the gate (not recommended).
    pub auth_max_attempts: u32,
    /// Sliding window (seconds) over which failed proofs are counted.
    pub auth_window_secs: u64,
    /// Base lockout (seconds) once the failure budget is spent.
    pub auth_lockout_secs: u64,
    /// Fetch time-limited TURN credentials from the signaling server's
    /// `GET /turn` (coturn REST API) for WebRTC sessions. Harmless when
    /// the server does not mint (STUN-only fallback).
    pub turn_api: bool,
    /// Extra signaling endpoints to register on simultaneously (fan-out).
    /// When empty, only `server_url` is used. Every endpoint assigns its
    /// own ID; viewers try their endpoints in order until one connects.
    pub signaling_urls: Vec<String>,
    /// Serve the signaling registry + TCP relay inside this agent
    /// (`signal_listen_port` / +1). Zero-infra mode: expose it behind a
    /// free tunnel (cloudflared / Tailscale Funnel) or a mesh VPN — see
    /// docs/serverless.md. The agent registers on the local endpoint too.
    pub signal_listen: bool,
    /// HTTP/WS port for the embedded signaling server; the TCP relay
    /// binds the next port (`+1`).
    pub signal_listen_port: u16,
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
            clipboard_sync: true,
            adaptive_quality: true,
            default_permission: "full".into(),
            acl_rules: Vec::new(),
            audio: true,
            audio_device: String::new(),
            capture: "auto".into(),
            auth_max_attempts: 5,
            auth_window_secs: 300,
            auth_lockout_secs: 60,
            turn_api: true,
            signaling_urls: Vec::new(),
            signal_listen: false,
            signal_listen_port: 8080,
        }
    }
}

impl AgentConfig {
    /// Outbound signaling endpoints in registration order (fan-out):
    /// the explicit `signaling_urls` list when set, otherwise the
    /// single `server_url`. Does not include the embedded listener —
    /// the service layer appends it when `signal_listen` succeeded.
    pub fn signaling_endpoints(&self) -> Vec<String> {
        if self.signaling_urls.is_empty() {
            vec![self.server_url.clone()]
        } else {
            let mut urls = self.signaling_urls.clone();
            // The primary URL (config field / UI text box) always stays
            // first so upgrades of old configs keep their priority.
            if !urls.iter().any(|u| u == &self.server_url) {
                urls.insert(0, self.server_url.clone());
            }
            urls
        }
    }

    /// `ws://127.0.0.1:port/signal` URL of the embedded signaling server.
    pub fn embedded_signal_url(&self) -> String {
        format!("ws://127.0.0.1:{}/signal", self.signal_listen_port)
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

    /// Permission for a viewer with the given self-reported display name.
    /// Explicit `acl_rules` win over `default_permission`. Note that names
    /// are self-reported — the real gate is the session password; the ACL
    /// is a convenience policy layer, see docs/security.md.
    pub fn permission_for(&self, peer_name: &str) -> Permission {
        for rule in &self.acl_rules {
            if rule.peer.trim().eq_ignore_ascii_case(peer_name.trim()) {
                return Permission::parse(&rule.permission);
            }
        }
        Permission::parse(&self.default_permission)
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

    #[test]
    fn acl_rules_win_over_default() {
        let cfg = AgentConfig {
            default_permission: "view-only".into(),
            acl_rules: vec![
                AclRule {
                    peer: "alice".into(),
                    permission: "full".into(),
                },
                AclRule {
                    peer: "bob".into(),
                    permission: "deny".into(),
                },
            ],
            ..Default::default()
        };
        assert_eq!(cfg.permission_for("Alice"), Permission::Full);
        assert_eq!(cfg.permission_for("BOB"), Permission::Deny);
        assert_eq!(cfg.permission_for("stranger"), Permission::ViewOnly);
    }

    #[test]
    fn unknown_permission_falls_back_to_full() {
        assert_eq!(Permission::parse("nonsense"), Permission::Full);
        assert_eq!(Permission::parse("view-only"), Permission::ViewOnly);
    }

    #[test]
    fn endpoints_fall_back_to_server_url() {
        let mut cfg = AgentConfig::default();
        cfg.server_url = "ws://a/signal".into();
        cfg.signaling_urls.clear();
        assert_eq!(cfg.signaling_endpoints(), vec!["ws://a/signal".to_string()]);
    }

    #[test]
    fn endpoints_keep_primary_first() {
        let mut cfg = AgentConfig::default();
        cfg.server_url = "ws://a/signal".into();
        cfg.signaling_urls = vec!["ws://b/signal".into(), "ws://c/signal".into()];
        assert_eq!(
            cfg.signaling_endpoints(),
            vec![
                "ws://a/signal".to_string(),
                "ws://b/signal".to_string(),
                "ws://c/signal".to_string()
            ]
        );
    }

    #[test]
    fn endpoints_do_not_duplicate_primary() {
        let mut cfg = AgentConfig::default();
        cfg.server_url = "ws://a/signal".into();
        cfg.signaling_urls = vec!["ws://a/signal".into(), "ws://b/signal".into()];
        assert_eq!(
            cfg.signaling_endpoints(),
            vec!["ws://a/signal".to_string(), "ws://b/signal".to_string()]
        );
    }

    #[test]
    fn embedded_url_follows_port() {
        let mut cfg = AgentConfig::default();
        cfg.signal_listen_port = 9090;
        assert_eq!(cfg.embedded_signal_url(), "ws://127.0.0.1:9090/signal");
    }
}
