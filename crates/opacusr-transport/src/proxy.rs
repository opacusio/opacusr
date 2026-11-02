//! Outbound proxy support for the internet-facing paths (signaling
//! WebSocket and the TCP relay): SOCKS5 (RFC 1928 + RFC 1929 auth) and
//! HTTP CONNECT.
//!
//! Scope (deliberate): only the two *TCP to a named server* paths are
//! proxied — a strict corporate firewall that only allows web traffic
//! can then carry the whole session via the relay fallback. LAN/direct
//! connections stay direct (a proxy between subnets is not a realistic
//! deployment), and WebRTC ICE/UDP cannot traverse a SOCKS5 TCP proxy —
//! behind such firewalls the transport ladder simply lands on the relay.
//!
//! Hand-rolled: the protocol subset is tiny (one CONNECT command, no
//! UDP ASSOCIATE, no GSSAPI), which keeps the dependency tree and the
//! audited surface small. Both proxies are unit-tested against in-process
//! fake servers on every platform.

use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Whole-handshake budget; a proxy that takes longer is dead.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyScheme {
    Socks5,
    HttpConnect,
}

/// A parsed proxy URL: `socks5://[user:pass@]host:port` (default port
/// 1080) or `http://[user:pass@]host:port` (explicit port required).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proxy {
    pub scheme: ProxyScheme,
    pub host: String,
    pub port: u16,
    pub auth: Option<(String, String)>,
}

impl Proxy {
    /// Parse a proxy URL. Accepts `socks5://` (and the hostname-resolving
    /// alias `socks5h://`) and `http://`; userinfo is `user:pass@`.
    pub fn parse(url: &str) -> Result<Proxy, String> {
        let url = url.trim();
        let (scheme, rest) = if let Some(r) = url
            .strip_prefix("socks5h://")
            .or_else(|| url.strip_prefix("socks5://"))
        {
            (ProxyScheme::Socks5, r)
        } else if let Some(r) = url.strip_prefix("http://") {
            (ProxyScheme::HttpConnect, r)
        } else {
            return Err(format!(
                "unsupported proxy url {url:?} (expected socks5://host:port or http://host:port)"
            ));
        };
        if rest.is_empty() {
            return Err("proxy url has no host".into());
        }

        // userinfo@host:port
        let (auth, hostport) = match rest.split_once('@') {
            Some((userinfo, hp)) => {
                let (user, pass) = userinfo
                    .split_once(':')
                    .ok_or("proxy userinfo must be user:pass")?;
                if user.is_empty() {
                    return Err("proxy username is empty".into());
                }
                (Some((user.to_string(), pass.to_string())), hp.to_string())
            }
            None => (None, rest.to_string()),
        };
        // Strip a path suffix if someone pasted one; proxies live at the root.
        let hostport = hostport.trim_end_matches('/');
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) => {
                let port: u16 = p.parse().map_err(|_| format!("bad proxy port {p:?}"))?;
                (h.to_string(), port)
            }
            None => match scheme {
                ProxyScheme::Socks5 => (hostport.to_string(), 1080),
                // No conventional default — refuse to guess silently.
                ProxyScheme::HttpConnect => {
                    return Err(format!("http proxy url needs an explicit port ({url})"))
                }
            },
        };
        if host.is_empty() {
            return Err("proxy url has no host".into());
        }
        Ok(Proxy {
            scheme,
            host,
            port,
            auth,
        })
    }

    pub fn host_port(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Establish a TCP connection to `target` (`host:port`, hostname
    /// allowed — domain names are resolved by a SOCKS5 proxy) through
    /// this proxy.
    pub async fn connect(&self, target: &str) -> std::io::Result<TcpStream> {
        let mut stream = TcpStream::connect(self.host_port()).await?;
        stream.set_nodelay(true).ok();
        let res = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            match self.scheme {
                ProxyScheme::Socks5 => self.socks5_connect(&mut stream, target).await,
                ProxyScheme::HttpConnect => self.http_connect(&mut stream, target).await,
            }
        })
        .await;
        match res {
            Ok(Ok(())) => Ok(stream),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("proxy handshake with {} timed out", self.host_port()),
            )),
        }
    }

    // -- SOCKS5 (RFC 1928; auth per RFC 1929) ----------------------------

    async fn socks5_connect(&self, stream: &mut TcpStream, target: &str) -> std::io::Result<()> {
        // Method negotiation: always offer no-auth, add user/pass when
        // credentials are configured.
        let mut greeting = vec![0x05u8, if self.auth.is_some() { 2 } else { 1 }, 0x00];
        if self.auth.is_some() {
            greeting.push(0x02);
        }
        stream.write_all(&greeting).await?;
        let mut method = [0u8; 2];
        stream.read_exact(&mut method).await?;
        if method[0] != 0x05 {
            return Err(bad("proxy is not SOCKS5"));
        }
        match (method[1], &self.auth) {
            (0x00, _) => {}
            (0x02, Some((user, pass))) => {
                let mut sub = vec![0x01u8, user.len() as u8];
                sub.extend_from_slice(user.as_bytes());
                sub.push(pass.len() as u8);
                sub.extend_from_slice(pass.as_bytes());
                stream.write_all(&sub).await?;
                let mut verdict = [0u8; 2];
                stream.read_exact(&mut verdict).await?;
                if verdict[1] != 0x00 {
                    return Err(bad("proxy rejected the user/password auth"));
                }
            }
            (0x02, None) => {
                return Err(bad(
                    "proxy requires user/password auth; add user:pass to the proxy url",
                ))
            }
            (0xFF, _) => return Err(bad("proxy accepts none of our auth methods")),
            (_, None) => return Err(bad("proxy picked an unsupported auth method")),
            (_, Some(_)) => return Err(bad("proxy picked an unsupported auth method")),
        }

        // CONNECT request with the address in proxy-resolvable form.
        let mut req = vec![0x05u8, 0x01, 0x00];
        encode_target(target, &mut req)?;
        stream.write_all(&req).await?;

        // Reply: VER REP RSV ATYP ADDR... PORT...
        let mut head = [0u8; 4];
        stream.read_exact(&mut head).await?;
        if head[1] != 0x00 {
            return Err(bad(match head[1] {
                0x01 => "proxy: general SOCKS failure",
                0x02 => "proxy: connection not allowed",
                0x03 => "proxy: target network unreachable",
                0x04 => "proxy: target host unreachable",
                0x05 => "proxy: target refused the connection",
                0x06 => "proxy: TTL expired",
                0x07 => "proxy: command not supported",
                0x08 => "proxy: address type not supported",
                _ => "proxy: unknown SOCKS5 error",
            }));
        }
        // Skip BND.ADDR + BND.PORT; sizes follow ATYP.
        let skip = match head[3] {
            0x01 => 4 + 2,
            0x03 => {
                let mut len = [0u8; 1];
                stream.read_exact(&mut len).await?;
                len[0] as usize + 2
            }
            0x04 => 16 + 2,
            other => {
                return Err(bad(
                    format!("proxy replied with address type {other}").as_str()
                ))
            }
        };
        let mut junk = vec![0u8; skip];
        stream.read_exact(&mut junk).await?;
        Ok(())
    }

    // -- HTTP CONNECT ----------------------------------------------------

    async fn http_connect(&self, stream: &mut TcpStream, target: &str) -> std::io::Result<()> {
        let mut req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
        if let Some((user, pass)) = &self.auth {
            let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
            req.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
        }
        req.push_str("User-Agent: opacusr\r\n\r\n");
        stream.write_all(req.as_bytes()).await?;

        // Read the response head; the body (if any) belongs to the tunnel.
        let mut head = Vec::with_capacity(256);
        let mut byte = [0u8; 1];
        loop {
            let n = stream.read(&mut byte).await?;
            if n == 0 {
                return Err(bad("proxy closed the connection during CONNECT"));
            }
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
                break;
            }
            if head.len() > 16 * 1024 {
                return Err(bad("proxy sent an oversized CONNECT response"));
            }
        }
        let head = String::from_utf8_lossy(&head);
        let status = head.lines().next().unwrap_or_default();
        // "HTTP/1.1 200 ..." — any 2xx is success.
        let ok = status
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .is_some_and(|code| (200..300).contains(&code));
        if !ok {
            return Err(bad(format!("proxy refused CONNECT: {status}").as_str()));
        }
        Ok(())
    }
}

fn bad(msg: &str) -> std::io::Error {
    std::io::Error::other(msg.to_string())
}

/// Append ATYP + ADDR + PORT for `target` (`host:port`) to a SOCKS5
/// request. Domains are passed through unresolved (the proxy resolves
/// them); IPv4/IPv6 are sent in binary form.
fn encode_target(target: &str, req: &mut Vec<u8>) -> std::io::Result<()> {
    use std::net::IpAddr;
    let (host, port) = target
        .rsplit_once(':')
        .ok_or_else(|| bad("target must be host:port"))?;
    let port: u16 = port.parse().map_err(|_| bad("bad target port"))?;
    // IPv6 literals arrive bracketed; rsplit_once above already kept the
    // whole bracketed literal in `host`.
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let addr: Option<IpAddr> = host.parse().ok();
    match addr {
        Some(IpAddr::V4(v4)) => {
            req.push(0x01);
            req.extend_from_slice(&v4.octets());
        }
        Some(IpAddr::V6(v6)) => {
            req.push(0x04);
            req.extend_from_slice(&v6.octets());
        }
        None => {
            let bytes = host.as_bytes();
            if bytes.is_empty() || bytes.len() > 255 {
                return Err(bad("target hostname is empty or too long"));
            }
            req.push(0x03);
            req.push(bytes.len() as u8);
            req.extend_from_slice(bytes);
        }
    }
    req.extend_from_slice(&port.to_be_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_socks5_with_defaults_and_auth() {
        let p = Proxy::parse("socks5://10.0.0.1").unwrap();
        assert_eq!(p.scheme, ProxyScheme::Socks5);
        assert_eq!(p.host, "10.0.0.1");
        assert_eq!(p.port, 1080);
        assert!(p.auth.is_none());

        let p = Proxy::parse("socks5://alice:secret@corp.example:9050/").unwrap();
        assert_eq!(p.auth, Some(("alice".to_string(), "secret".to_string())));
        assert_eq!(p.port, 9050);
    }

    #[test]
    fn parses_http_proxy() {
        let p = Proxy::parse("http://proxy.corp:3128").unwrap();
        assert_eq!(p.scheme, ProxyScheme::HttpConnect);
        assert_eq!(p.port, 3128);
    }

    #[test]
    fn rejects_bad_urls() {
        for url in [
            "https://x:1",
            "ftp://x:1",
            "socks5://",
            "http://no-port",
            "socks5://u@x:1", // userinfo without password
            "socks5://x:notaport",
            "socks5://:pass@x:1",
        ] {
            assert!(Proxy::parse(url).is_err(), "expected reject: {url}");
        }
    }

    #[tokio::test]
    async fn socks5_roundtrip_through_fake_proxy() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            // Greeting: offer no-auth only.
            let mut buf = [0u8; 3];
            s.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, &[0x05, 0x01, 0x00]);
            s.write_all(&[0x05, 0x00]).await.unwrap();
            // Request: CONNECT 198.51.100.7:443 (binary IPv4 ATYP).
            let mut head = [0u8; 10];
            s.read_exact(&mut head).await.unwrap();
            assert_eq!(head[..3], [0x05, 0x01, 0x00]);
            assert_eq!(head[3], 0x01);
            assert_eq!(&head[4..8], &[198, 51, 100, 7]);
            assert_eq!(&head[8..10], &443u16.to_be_bytes());
            s.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            // Tunnel: echo everything back once.
            let mut data = [0u8; 4];
            s.read_exact(&mut data).await.unwrap();
            s.write_all(&data).await.unwrap();
        });

        let proxy = Proxy::parse(&format!("socks5://{addr}")).unwrap();
        let mut stream = proxy.connect("198.51.100.7:443").await.unwrap();
        stream.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }

    #[tokio::test]
    async fn socks5_auth_and_domain_target() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4];
            s.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, &[0x05, 0x02, 0x00, 0x02]);
            s.write_all(&[0x05, 0x02]).await.unwrap();
            // RFC 1929 subnegotiation: VER ULEN UNAME PLEN PASSWD.
            let mut ver = [0u8; 1];
            s.read_exact(&mut ver).await.unwrap();
            assert_eq!(ver[0], 0x01);
            let mut ulen = [0u8; 1];
            s.read_exact(&mut ulen).await.unwrap();
            let mut user = vec![0u8; ulen[0] as usize];
            s.read_exact(&mut user).await.unwrap();
            let mut plen = [0u8; 1];
            s.read_exact(&mut plen).await.unwrap();
            let mut pass = vec![0u8; plen[0] as usize];
            s.read_exact(&mut pass).await.unwrap();
            assert_eq!(user, b"u");
            assert_eq!(pass, b"p");
            s.write_all(&[0x01, 0x00]).await.unwrap();
            // Domain target: VER CMD RSV ATYP LEN | domain | port.
            let mut head = [0u8; 5];
            s.read_exact(&mut head).await.unwrap();
            assert_eq!(head[..4], [0x05, 0x01, 0x00, 0x03]);
            let dlen = head[4] as usize;
            let mut rest = vec![0u8; dlen + 2];
            s.read_exact(&mut rest).await.unwrap();
            assert_eq!(&rest[..dlen], b"host.internal");
            s.write_all(&[0x05, 0x00, 0x00, 0x03, 1, b'x', 0, 0])
                .await
                .unwrap();
            let mut data = [0u8; 2];
            s.read_exact(&mut data).await.unwrap();
            s.write_all(&data).await.unwrap();
        });

        let proxy = Proxy::parse(&format!("socks5://u:p@{addr}")).unwrap();
        let mut stream = proxy.connect("host.internal:8080").await.unwrap();
        stream.write_all(b"ok").await.unwrap();
        let mut buf = [0u8; 2];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ok");
    }

    #[tokio::test]
    async fn socks5_error_is_surfaced() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 3];
            s.read_exact(&mut buf).await.unwrap();
            s.write_all(&[0x05, 0x00]).await.unwrap();
            let mut head = [0u8; 10];
            s.read_exact(&mut head).await.unwrap();
            // 0x04 = host unreachable.
            s.write_all(&[0x05, 0x04, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
        });
        let proxy = Proxy::parse(&format!("socks5://{addr}")).unwrap();
        let err = proxy.connect("203.0.113.9:80").await.unwrap_err();
        assert!(err.to_string().contains("host unreachable"), "{err}");
    }

    #[tokio::test]
    async fn http_connect_roundtrip() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                s.read_exact(&mut byte).await.unwrap();
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let head = String::from_utf8(head).unwrap();
            assert!(head.starts_with("CONNECT backend.internal:99 HTTP/1.1\r\n"));
            assert!(head.contains("Host: backend.internal:99\r\n"));
            assert!(head.contains(&format!(
                "Proxy-Authorization: Basic {}",
                base64::engine::general_purpose::STANDARD.encode("bob:hunter2")
            )));
            s.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .unwrap();
            let mut data = [0u8; 3];
            s.read_exact(&mut data).await.unwrap();
            s.write_all(&data).await.unwrap();
        });

        let proxy = Proxy::parse(&format!("http://bob:hunter2@{addr}")).unwrap();
        let mut stream = proxy.connect("backend.internal:99").await.unwrap();
        stream.write_all(b"hi!").await.unwrap();
        let mut buf = [0u8; 3];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hi!");
    }

    #[tokio::test]
    async fn http_connect_failure_is_surfaced() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                s.read_exact(&mut byte).await.unwrap();
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            s.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                .await
                .unwrap();
        });
        let proxy = Proxy::parse(&format!("http://{addr}")).unwrap();
        let err = proxy.connect("backend:80").await.unwrap_err();
        assert!(err.to_string().contains("407"), "{err}");
    }
}
