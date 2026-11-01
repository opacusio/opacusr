//! Host-side TCP tunneling (SSH-style local forwarding): the viewer asks
//! the host to open a TCP connection to a target and pumps bytes through
//! `TunnelOpen`/`TunnelData`/`TunnelClose` messages.
//!
//! Policy: [`TunnelPolicy::Loopback`] (the default) restricts targets to
//! the host's loopback interface, so a viewer can reach local services
//! (web UIs, databases, SSH) but cannot use the session to scan the
//! host's network. `off` disables the feature entirely, `any` allows
//! arbitrary targets. View-only sessions never get tunnels regardless of
//! the policy (enforced in the session loop).

use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use opacusr_proto::message::{TunnelData, TunnelOpened};
use opacusr_proto::Message;

/// Where the host may open TCP connections on a viewer's behalf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TunnelPolicy {
    /// Feature disabled: `TunnelOpen` is refused.
    Off,
    /// Targets must resolve to a loopback address (default — safe).
    #[default]
    Loopback,
    /// Any target the viewer asks for.
    Any,
}

impl TunnelPolicy {
    /// Parse the config value (`off` | `loopback` | `any`).
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "disabled" | "false" => Self::Off,
            "any" | "unrestricted" | "all" => Self::Any,
            _ => Self::Loopback,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TunnelPolicy::Off => "off",
            TunnelPolicy::Loopback => "loopback",
            TunnelPolicy::Any => "any",
        }
    }
}

/// Per-target connect timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Socket read buffer for the tunnel pump.
const PUMP_BUF: usize = 16 * 1024;

/// Validate a tunnel target against the policy and return the
/// `ip:port` (or `host:port`) string to connect to.
fn resolve_target(host: &str, port: u16, policy: TunnelPolicy) -> Result<String, String> {
    let host = host.trim();
    if host.is_empty() {
        return Err("empty tunnel target".into());
    }
    if port == 0 {
        return Err("tunnel port 0 is not allowed".into());
    }

    // Literal IP: check directly, no DNS involved.
    if let Ok(ip) = host.parse::<IpAddr>() {
        if policy == TunnelPolicy::Loopback && !ip.is_loopback() {
            return Err(format!(
                "target {ip} is not a loopback address (tunnel policy: loopback-only)"
            ));
        }
        return Ok(format!("{host}:{port}"));
    }

    // Hostname: under the Loopback policy it must resolve to loopback.
    if policy == TunnelPolicy::Loopback {
        let addrs: Vec<_> = (host, port)
            .to_socket_addrs()
            .map_err(|e| format!("resolve {host}: {e}"))?
            .collect();
        let first = addrs
            .first()
            .ok_or_else(|| format!("resolve {host}: no addresses"))?;
        if !first.ip().is_loopback() {
            return Err(format!(
                "target {host} resolves to {} which is not loopback (tunnel policy: loopback-only)",
                first.ip()
            ));
        }
        return Ok(first.to_string());
    }

    Ok(format!("{host}:{port}"))
}

/// A live host-side tunnel connection. The pump task owns the TCP socket:
/// socket → `TunnelData` → the session's outbound queue, and bytes pushed
/// through [`TunnelConn::send`] go into the socket. Dropping the conn
/// (removing it from the session's map) closes everything.
pub struct TunnelConn {
    pub id: u32,
    /// Bytes from the viewer into the remote TCP socket.
    writer: mpsc::UnboundedSender<Vec<u8>>,
}

impl TunnelConn {
    /// Connect to the target (subject to the policy) and start the pump.
    /// Returns the wire reply and the live conn on success.
    pub async fn open(
        id: u32,
        host: &str,
        port: u16,
        policy: TunnelPolicy,
        out_tx: mpsc::UnboundedSender<Message>,
    ) -> (TunnelOpened, Option<TunnelConn>) {
        if policy == TunnelPolicy::Off {
            return (
                TunnelOpened {
                    id,
                    ok: false,
                    error: Some("tunneling is disabled on this host".into()),
                },
                None,
            );
        }

        // DNS under the loopback policy may block; keep it off the session
        // loop.
        let host = host.to_string();
        let resolved = tokio::task::spawn_blocking(move || resolve_target(&host, port, policy))
            .await
            .unwrap_or_else(|e| Err(format!("resolve task: {e}")));
        let target = match resolved {
            Ok(t) => t,
            Err(e) => {
                return (
                    TunnelOpened {
                        id,
                        ok: false,
                        error: Some(e),
                    },
                    None,
                )
            }
        };

        let sock = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&target)).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                return (
                    TunnelOpened {
                        id,
                        ok: false,
                        error: Some(format!("connect {target}: {e}")),
                    },
                    None,
                )
            }
            Err(_) => {
                return (
                    TunnelOpened {
                        id,
                        ok: false,
                        error: Some(format!("connect {target}: timed out")),
                    },
                    None,
                )
            }
        };

        let (writer, mut w_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (mut sock_r, mut sock_w) = sock.into_split();
        let out = out_tx.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; PUMP_BUF];
            loop {
                tokio::select! {
                    // Server → viewer. Stays active even after the viewer
                    // half-closes, so the EOF handshake completes.
                    read = sock_r.read(&mut buf) => {
                        match read {
                            Ok(0) | Err(_) => {
                                let _ = out.send(Message::TunnelClose(
                                    opacusr_proto::message::TunnelClose { id },
                                ));
                                break;
                            }
                            Ok(n) => {
                                let _ = out.send(Message::TunnelData(TunnelData {
                                    id,
                                    data: buf[..n].to_vec(),
                                }));
                            }
                        }
                    }
                    chunk = w_rx.recv() => {
                        match chunk {
                            // Session dropped the conn (bye/close/cleanup).
                            None => break,
                            // Viewer aborted: half-close; keep draining the
                            // read side until the server closes too.
                            Some(data) if data.is_empty() => {
                                let _ = sock_w.shutdown().await;
                            }
                            Some(data) => {
                                if sock_w.write_all(&data).await.is_err() {
                                    let _ = out.send(Message::TunnelClose(
                                        opacusr_proto::message::TunnelClose { id },
                                    ));
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            let _ = sock_w.shutdown().await;
        });

        (
            TunnelOpened {
                id,
                ok: true,
                error: None,
            },
            Some(TunnelConn { id, writer }),
        )
    }

    /// Push bytes from the viewer into the remote socket (never blocks).
    pub fn send(&self, data: &[u8]) {
        let _ = self.writer.send(data.to_vec());
    }

    /// Abort the connection: closes the remote socket; the session loop
    /// removes the conn from its map right after.
    pub fn abort(&self) {
        let _ = self.writer.send(Vec::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn policy_parse() {
        assert_eq!(TunnelPolicy::parse("off"), TunnelPolicy::Off);
        assert_eq!(TunnelPolicy::parse("OFF"), TunnelPolicy::Off);
        assert_eq!(TunnelPolicy::parse("loopback"), TunnelPolicy::Loopback);
        assert_eq!(TunnelPolicy::parse(" any "), TunnelPolicy::Any);
        assert_eq!(
            TunnelPolicy::parse("nonsense"),
            TunnelPolicy::default(),
            "unknown values fall back to the safe default"
        );
        assert_eq!(TunnelPolicy::default(), TunnelPolicy::Loopback);
    }

    #[test]
    fn resolve_rejects_bad_targets() {
        assert!(resolve_target("", 80, TunnelPolicy::Any).is_err());
        assert!(resolve_target("127.0.0.1", 0, TunnelPolicy::Any).is_err());
    }

    #[test]
    fn resolve_loopback_policy() {
        assert!(resolve_target("127.0.0.1", 80, TunnelPolicy::Loopback).is_ok());
        assert!(resolve_target("::1", 80, TunnelPolicy::Loopback).is_ok());
        assert!(resolve_target("localhost", 80, TunnelPolicy::Loopback).is_ok());
        assert!(resolve_target("10.0.0.5", 80, TunnelPolicy::Loopback).is_err());
        assert!(resolve_target("192.168.1.10", 22, TunnelPolicy::Loopback).is_err());
        // A hostname that resolves to a public address must be refused.
        assert!(resolve_target("example.com", 80, TunnelPolicy::Loopback).is_err());
    }

    #[test]
    fn resolve_any_policy_allows_remote() {
        assert!(resolve_target("10.0.0.5", 80, TunnelPolicy::Any).is_ok());
        assert!(resolve_target("192.168.1.10", 22, TunnelPolicy::Any).is_ok());
    }

    #[tokio::test]
    async fn open_refused_when_off() {
        let (out_tx, _out_rx) = mpsc::unbounded_channel();
        let (reply, conn) = TunnelConn::open(7, "127.0.0.1", 80, TunnelPolicy::Off, out_tx).await;
        assert!(!reply.ok);
        assert!(reply.error.is_some());
        assert!(conn.is_none());
    }

    #[tokio::test]
    async fn open_connect_failure_reports_error() {
        // Bind a listener, learn its port, drop it — connecting to that
        // port now reliably refuses.
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);

        let (out_tx, _out_rx) = mpsc::unbounded_channel();
        let (reply, conn) =
            TunnelConn::open(1, "127.0.0.1", port, TunnelPolicy::Loopback, out_tx).await;
        assert!(!reply.ok, "connecting to a dead port must fail");
        assert!(conn.is_none());
    }

    #[tokio::test]
    async fn echo_roundtrip_through_conn() {
        // Echo server: everything received is sent back.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    loop {
                        match sock.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => {
                                if sock.write_all(&buf[..n]).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                });
            }
        });

        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let (reply, conn) =
            TunnelConn::open(3, "127.0.0.1", port, TunnelPolicy::Loopback, out_tx).await;
        assert!(reply.ok, "connect to a live echo server must succeed");
        let conn = conn.unwrap();

        // Viewer → host → socket → echo → socket → viewer.
        conn.send(b"ping-tunnel");
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            match out_rx.recv().await.unwrap() {
                Message::TunnelData(d) => {
                    assert_eq!(d.id, 3);
                    d.data
                }
                other => panic!("unexpected message: {other:?}"),
            }
        })
        .await
        .expect("echo never came back");
        assert_eq!(got, b"ping-tunnel");

        // Viewer-side abort: dropping the conn shuts the pump down quietly.
        conn.abort();
        drop(conn);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            out_rx.try_recv().is_err(),
            "abort must not emit any session message"
        );
    }

    #[tokio::test]
    async fn socket_eof_emits_tunnel_close() {
        // Server accepts one connection and immediately drops it.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _sock = listener.accept().await;
            // Drop = close → the tunnel pump must see EOF.
        });

        let (out_tx, mut out_rx) = mpsc::unbounded_channel();
        let (reply, conn) =
            TunnelConn::open(9, "127.0.0.1", port, TunnelPolicy::Loopback, out_tx).await;
        assert!(reply.ok);
        assert!(conn.is_some());

        let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match out_rx.recv().await.unwrap() {
                    Message::TunnelClose(c) => return c.id,
                    Message::TunnelData(_) => continue,
                    other => panic!("unexpected message: {other:?}"),
                }
            }
        })
        .await
        .expect("EOF must surface as TunnelClose");
        assert_eq!(closed, 9);
    }
}
