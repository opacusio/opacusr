//! Hand-rolled Prometheus metrics (text exposition format 0.0.4).
//!
//! No metrics dependency on purpose — the server's surface is a dozen
//! counters and two gauges, and every crate pulled in is one more thing
//! a security-minded self-hosted binary carries. Counters are plain
//! atomics bumped inline at their events; [`Metrics::render`] formats
//! them with the `# HELP`/`# TYPE` preamble scrapers expect.
//!
//! The relay byte counter is shared between the two pipe directions of
//! a relay pair: each direction wraps its reader half in a
//! [`CountingReader`] that adds every `poll_read` into the same total.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncRead;

/// Server counters and gauges exposed on `GET /metrics`.
#[derive(Default)]
pub struct Metrics {
    /// WebSocket upgrades accepted on `/signal`.
    pub upgrades: AtomicU64,
    /// Successful `Register` handshakes.
    pub registrations: AtomicU64,
    /// Currently registered peers (gauge).
    pub peers: AtomicU64,
    /// Relay tickets allocated (lifetime).
    pub tickets: AtomicU64,
    /// Relay pairs completed (lifetime) — both roles arrived and piping
    /// started.
    pub paired: AtomicU64,
    /// Active relay pipes (gauge).
    pub pipes: AtomicU64,
    /// Bytes copied through the relay, both directions combined.
    pub relay_bytes: Arc<AtomicU64>,
    /// `/turn` credentials minted.
    pub turn_mints: AtomicU64,
    /// Requests refused by the per-IP limiter (all routes combined).
    pub rate_limited: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Render the exposition text. `version` labels `opacusr_build_info`.
    pub fn render(&self, version: &str) -> String {
        let c = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let mut out = String::with_capacity(1024);
        let counter = |name: &str, help: &str, value: u64, out: &mut String| {
            out.push_str(&format!(
                "# HELP {name} {help}\n# TYPE {name} counter\n{name} {value}\n"
            ));
        };
        counter(
            "opacusr_ws_upgrades_total",
            "Signaling WebSocket upgrades accepted.",
            c(&self.upgrades),
            &mut out,
        );
        counter(
            "opacusr_registrations_total",
            "Successful peer registrations.",
            c(&self.registrations),
            &mut out,
        );
        out.push_str(
            "# HELP opacusr_peers Currently registered peers.\n# TYPE opacusr_peers gauge\n",
        );
        out.push_str(&format!("opacusr_peers {}\n", c(&self.peers)));
        counter(
            "opacusr_relay_tickets_total",
            "Relay tickets allocated.",
            c(&self.tickets),
            &mut out,
        );
        counter(
            "opacusr_relay_paired_total",
            "Relay pairs completed.",
            c(&self.paired),
            &mut out,
        );
        out.push_str(
            "# HELP opacusr_relay_pipes Active relay pipes.\n# TYPE opacusr_relay_pipes gauge\n",
        );
        out.push_str(&format!("opacusr_relay_pipes {}\n", c(&self.pipes)));
        counter(
            "opacusr_relay_bytes_total",
            "Bytes relayed (both directions combined).",
            self.relay_bytes.load(Ordering::Relaxed),
            &mut out,
        );
        counter(
            "opacusr_turn_mints_total",
            "TURN credentials minted.",
            c(&self.turn_mints),
            &mut out,
        );
        counter(
            "opacusr_rate_limited_total",
            "Requests refused by the per-IP limiter.",
            c(&self.rate_limited),
            &mut out,
        );
        out.push_str(
            "# HELP opacusr_build_info Build metadata; the value is always 1.\n\
             # TYPE opacusr_build_info gauge\n",
        );
        // The version string is generated from trusted inputs (CARGO_PKG_VERSION).
        out.push_str(&format!(
            "opacusr_build_info{{version=\"{}\"}} 1\n",
            version.replace('\\', "\\\\").replace('"', "\\\"")
        ));
        out
    }
}

/// AsyncRead wrapper adding every byte read into a shared counter — the
/// read half of a relay pipe reports its direction's traffic this way.
pub struct CountingReader<R> {
    inner: R,
    total: Arc<AtomicU64>,
}

impl<R> CountingReader<R> {
    pub fn new(inner: R, total: Arc<AtomicU64>) -> Self {
        Self { inner, total }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for CountingReader<R> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let polled = std::pin::Pin::new(&mut this.inner).poll_read(cx, buf);
        if let std::task::Poll::Ready(Ok(())) = &polled {
            let read = buf.filled().len() - before;
            if read > 0 {
                this.total.fetch_add(read as u64, Ordering::Relaxed);
            }
        }
        polled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_contains_counters_and_build_info() {
        let m = Metrics::new();
        m.upgrades.fetch_add(3, Ordering::Relaxed);
        m.registrations.fetch_add(2, Ordering::Relaxed);
        m.peers.fetch_add(2, Ordering::Relaxed);
        m.pipes.fetch_add(1, Ordering::Relaxed);
        m.relay_bytes.fetch_add(1234, Ordering::Relaxed);
        let text = m.render("9.9.9-test");
        for expected in [
            "# TYPE opacusr_ws_upgrades_total counter",
            "opacusr_ws_upgrades_total 3",
            "opacusr_registrations_total 2",
            "opacusr_peers 2",
            "opacusr_relay_pipes 1",
            "opacusr_relay_bytes_total 1234",
            "opacusr_rate_limited_total 0",
            "opacusr_build_info{version=\"9.9.9-test\"} 1",
        ] {
            assert!(text.contains(expected), "missing {expected} in:\n{text}");
        }
        // The version label is escaped — a hostile version cannot break
        // the exposition format.
        let hostile = m.render("1.0\" x=\"2");
        assert!(hostile.contains("opacusr_build_info{version=\"1.0\\\" x=\\\"2\"} 1"));
    }

    #[tokio::test]
    async fn counting_reader_sums_every_byte() {
        let total = Arc::new(AtomicU64::new(0));
        let data = vec![7u8; 50_000];
        let mut reader = CountingReader::new(&data[..], total.clone());
        let mut buf = tokio::io::BufReader::new(&mut reader);
        let mut sink = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut buf, &mut sink)
            .await
            .unwrap();
        assert_eq!(sink.len(), 50_000);
        assert_eq!(total.load(Ordering::Relaxed), 50_000);
    }
}
