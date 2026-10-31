//! Time-limited TURN credentials from the signaling server.
//!
//! When the server runs with `OPACUSR_TURN_SECRET`, it mints coturn
//! "TURN REST API" credentials at `GET /turn` (see opacusr-server). Clients
//! fetch a pair at session setup and add the returned TURN URIs to their
//! ICE configuration, so media can relay through coturn when the direct
//! paths fail. Fetch failures are non-fatal: sessions keep working over
//! STUN, direct paths and the built-in relay.
//!
//! Note: fetching over plain `ws://` derives an `http://` endpoint and the
//! minted pair crosses the network unencrypted — terminate TLS at the
//! reverse proxy (`wss://` → `https://`) for internet-facing deployments.

use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One minted credential pair as returned by `GET /turn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnCredentials {
    /// `<unix expiry seconds>` — the coturn REST API username.
    pub username: String,
    /// `base64(HMAC-SHA1(secret, username))`.
    pub password: String,
    /// Server-declared validity window (seconds).
    pub ttl: u64,
    /// TURN URIs, e.g. `turn:host:3478?transport=udp`.
    pub uris: Vec<String>,
}

/// Why a fetch failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnFetchError {
    /// The server answered 503: minting is disabled there (no
    /// `OPACUSR_TURN_SECRET`). No point retrying until it is reconfigured.
    Disabled,
    /// Any other HTTP status.
    Http(u16),
    /// Transport or response-parse failure.
    Other(String),
}

impl std::fmt::Display for TurnFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => write!(f, "TURN minting disabled on the server (HTTP 503)"),
            Self::Http(code) => write!(f, "HTTP {code}"),
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

const FETCH_TIMEOUT: Duration = Duration::from_secs(4);

/// Derive the `/turn` endpoint from a signaling URL:
/// `ws(s)://host[:port]/path` → `http(s)://host[:port]/turn`.
/// Returns `None` for anything that is not a ws/wss URL.
pub fn turn_endpoint_of(server_url: &str) -> Option<String> {
    let url = server_url.trim();
    let (rest, scheme) = url
        .strip_prefix("wss://")
        .map(|rest| (rest, "https"))
        .or_else(|| url.strip_prefix("ws://").map(|rest| (rest, "http")))?;
    let host_port = rest.split('/').next().unwrap_or(rest);
    if host_port.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{host_port}/turn"))
}

/// Fetch credentials from an endpoint. Blocking for up to
/// [`FETCH_TIMEOUT`]; call from `tokio::task::spawn_blocking`.
pub fn fetch_turn_credentials(endpoint: &str) -> Result<TurnCredentials, TurnFetchError> {
    let resp = ureq::get(endpoint)
        .set("User-Agent", concat!("opacusr/", env!("CARGO_PKG_VERSION")))
        .timeout(FETCH_TIMEOUT)
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(503, _) => TurnFetchError::Disabled,
            ureq::Error::Status(code, _) => TurnFetchError::Http(code),
            other => TurnFetchError::Other(format!("request failed: {other}")),
        })?;
    let body = resp
        .into_string()
        .map_err(|e| TurnFetchError::Other(format!("read body: {e}")))?;
    parse_credentials(&body)
}

fn parse_credentials(body: &str) -> Result<TurnCredentials, TurnFetchError> {
    let json: Value = serde_json::from_str(body)
        .map_err(|e| TurnFetchError::Other(format!("unexpected JSON: {e}")))?;
    let username = str_field(&json, "username")?;
    let password = str_field(&json, "password")?;
    let ttl = json
        .get("ttl")
        .and_then(Value::as_u64)
        .ok_or_else(|| TurnFetchError::Other("response has no ttl".into()))?;
    let uris: Vec<String> = json
        .get("uris")
        .and_then(Value::as_array)
        .ok_or_else(|| TurnFetchError::Other("response has no uris".into()))?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    if uris.is_empty() {
        return Err(TurnFetchError::Other("response has empty uris".into()));
    }
    Ok(TurnCredentials {
        username,
        password,
        ttl,
        uris,
    })
}

fn str_field(json: &Value, key: &str) -> Result<String, TurnFetchError> {
    json.get(key)
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .ok_or_else(|| TurnFetchError::Other(format!("response has no {key}")))
}

/// Credential cache for long-lived services (the agent): within the
/// server-declared TTL the minted pair is reused, so `/turn` is not hit
/// once per session. A 503 disables the cache for the process lifetime;
/// any other failure starts a cooldown, after which one retry is allowed.
#[derive(Clone)]
pub struct TurnCredentialCache {
    endpoint: Option<String>,
    /// Shared: clones of the cache reuse the same minted pair.
    state: Arc<Mutex<CachedTurn>>,
}

#[derive(Debug, Clone)]
struct CachedTurn {
    creds: Option<TurnCredentials>,
    /// Trust horizon of `creds` (a safety margin below the server TTL).
    valid_until: Instant,
    /// Do not retry before this instant (after a transient failure).
    cooldown_until: Instant,
    /// Sticky disable after a 503 (minting off on the server).
    disabled: bool,
}

/// How long a transient failure parks the cache before one retry.
const COOLDOWN: Duration = Duration::from_secs(10 * 60);
/// Safety margin between the server TTL and the local trust horizon.
const TTL_MARGIN: u64 = 30;

impl TurnCredentialCache {
    /// Cache for a signaling URL (`ws://`/`wss://host:port/...`).
    pub fn from_server_url(server_url: &str) -> Self {
        Self {
            endpoint: turn_endpoint_of(server_url),
            state: Arc::new(Mutex::new(CachedTurn {
                creds: None,
                valid_until: Instant::now(),
                cooldown_until: Instant::now(),
                disabled: false,
            })),
        }
    }

    /// A permanently inert cache (feature turned off in the config).
    pub fn disabled() -> Self {
        Self {
            endpoint: None,
            state: Arc::new(Mutex::new(CachedTurn {
                creds: None,
                valid_until: Instant::now(),
                cooldown_until: Instant::now(),
                disabled: true,
            })),
        }
    }

    /// `true` when the cache may ever return credentials.
    pub fn enabled(&self) -> bool {
        self.endpoint.is_some()
    }

    /// Fetch-or-reuse. Blocking for up to ~[`FETCH_TIMEOUT`] on a miss —
    /// call from `tokio::task::spawn_blocking`. `None` = disabled,
    /// cooling down or the fetch failed; callers degrade to STUN-only.
    pub fn get(&self) -> Option<TurnCredentials> {
        let endpoint = self.endpoint.clone()?;
        let now = Instant::now();
        {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if st.disabled {
                return None;
            }
            if let Some(creds) = &st.creds {
                if now < st.valid_until {
                    return Some(creds.clone());
                }
            }
            if now < st.cooldown_until {
                return None;
            }
        }
        match fetch_turn_credentials(&endpoint) {
            Ok(creds) => {
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let valid = Duration::from_secs(creds.ttl.saturating_sub(TTL_MARGIN));
                if valid > Duration::ZERO {
                    st.creds = Some(creds.clone());
                    // `now` predates the fetch by at most FETCH_TIMEOUT —
                    // a conservative horizon either way.
                    st.valid_until = now + valid;
                }
                Some(creds)
            }
            Err(TurnFetchError::Disabled) => {
                tracing::info!(target: "opacusr",
                    "server does not mint TURN credentials (no OPACUSR_TURN_SECRET); STUN-only");
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                st.disabled = true;
                st.creds = None;
                None
            }
            Err(e) => {
                tracing::warn!(target: "opacusr",
                    "TURN credential fetch failed ({e}); STUN-only for {} min",
                    COOLDOWN.as_secs() / 60);
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                st.cooldown_until = Instant::now() + COOLDOWN;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn derives_endpoints_from_signaling_urls() {
        assert_eq!(
            turn_endpoint_of("ws://relay.example.com:8080/signal"),
            Some("http://relay.example.com:8080/turn".into())
        );
        assert_eq!(
            turn_endpoint_of("wss://relay.example.com/opacus"),
            Some("https://relay.example.com/turn".into())
        );
        assert_eq!(
            turn_endpoint_of("ws://127.0.0.1:8080"),
            Some("http://127.0.0.1:8080/turn".into())
        );
        assert_eq!(turn_endpoint_of("http://127.0.0.1:8080/turn"), None);
        assert_eq!(turn_endpoint_of("ws://"), None);
        assert_eq!(turn_endpoint_of(""), None);
    }

    #[test]
    fn parses_the_coturn_rest_shape() {
        let creds = parse_credentials(
            r#"{"username":"1799999999","password":"c29tZWhhbg==","ttl":3600,
                "uris":["turn:host:3478?transport=udp","turn:host:3478?transport=tcp"]}"#,
        )
        .unwrap();
        assert_eq!(creds.username, "1799999999");
        assert_eq!(creds.password, "c29tZWhhbg==");
        assert_eq!(creds.ttl, 3600);
        assert_eq!(creds.uris.len(), 2);
    }

    #[test]
    fn rejects_malformed_responses() {
        assert!(parse_credentials("not json").is_err());
        assert!(parse_credentials(r#"{"username":"u","ttl":1,"uris":["turn:h"]}"#).is_err());
        assert!(parse_credentials(r#"{"username":"u","password":"p","ttl":1}"#).is_err());
        assert!(parse_credentials(r#"{"username":"u","password":"p","ttl":1,"uris":[]}"#).is_err());
    }

    /// One-request-at-a-time canned HTTP server for the fetch tests.
    fn serve(response: String) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = hits.clone();
        std::thread::spawn(move || loop {
            let (mut sock, _) = match listener.accept() {
                Ok(s) => s,
                Err(_) => break,
            };
            hits2.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf);
            let _ = sock.write_all(response.as_bytes());
            let _ = sock.flush();
        });
        (format!("http://{addr}/turn"), hits)
    }

    /// A 200 response with a correct Content-Length for `body`.
    fn http_ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    fn ok_body() -> &'static str {
        "{\"username\":\"123\",\"password\":\"c2VjcmV0\",\"ttl\":7200,\"uris\":[\"turn:h:3478?transport=udp\"]}"
    }

    fn ok_cache() -> TurnCredentialCache {
        let (endpoint, _) = serve(http_ok(ok_body()));
        TurnCredentialCache::from_server_url(&format!(
            "ws://{}",
            endpoint
                .trim_start_matches("http://")
                .trim_end_matches("/turn")
        ))
    }

    const BODY_503: &str =
        "HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";

    #[test]
    fn fetches_over_real_http() {
        let (endpoint, _hits) = serve(http_ok(ok_body()));
        let creds = fetch_turn_credentials(&endpoint).unwrap();
        assert_eq!(creds.username, "123");
        assert_eq!(creds.ttl, 7200);
        assert_eq!(creds.uris, vec!["turn:h:3478?transport=udp".to_string()]);
    }

    #[test]
    fn http_503_maps_to_disabled() {
        let (endpoint, _hits) = serve(BODY_503.to_string());
        assert_eq!(
            fetch_turn_credentials(&endpoint),
            Err(TurnFetchError::Disabled)
        );
    }

    #[test]
    fn cache_reuses_within_ttl_and_disables_on_503() {
        // 200: repeated gets hit the server exactly once (TTL 7200 s).
        let cache = ok_cache();
        assert!(cache.enabled());
        let first = cache.get().expect("first fetch must succeed");
        for _ in 0..3 {
            assert_eq!(cache.get(), Some(first.clone()));
        }

        // 503: disabled after the first miss, no further requests.
        let (endpoint, hits) = serve(BODY_503.to_string());
        let cache = TurnCredentialCache::from_server_url(&format!(
            "ws://{}",
            endpoint
                .trim_start_matches("http://")
                .trim_end_matches("/turn")
        ));
        assert_eq!(cache.get(), None);
        assert_eq!(cache.get(), None);
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // Inert cache: no endpoint, no requests, no panics.
        let cache = TurnCredentialCache::disabled();
        assert!(!cache.enabled());
        assert_eq!(cache.get(), None);
    }
}
