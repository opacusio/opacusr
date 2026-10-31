//! `opacusr-server` — self-hostable signaling + relay server binary.

use std::net::SocketAddr;
use std::sync::Arc;

use clap::Parser;
use opacusr_server::{AppState, Limits, TurnConfig};

/// OpacusR signaling & relay server.
#[derive(Debug, Parser)]
#[command(name = "opacusr-server", version, about)]
struct Cli {
    /// HTTP/WS listen host.
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    /// Signaling WebSocket port.
    #[arg(long, default_value_t = 8080)]
    port: u16,
    /// TCP relay port (fallback when direct WebRTC is blocked).
    #[arg(long, default_value_t = 8081)]
    relay_port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,opacusr=debug".into()),
        )
        .init();

    let cli = Cli::parse();
    let http_addr: SocketAddr = format!("{}:{}", cli.host, cli.port).parse()?;
    let relay_addr: SocketAddr = format!("{}:{}", cli.host, cli.relay_port).parse()?;

    let http = tokio::net::TcpListener::bind(http_addr).await?;
    let relay = tokio::net::TcpListener::bind(relay_addr).await?;

    eprintln!("┌──────────────────────────────────────────────┐");
    eprintln!(
        "│        OpacusR server v{}                 │",
        env!("CARGO_PKG_VERSION")
    );
    eprintln!("├──────────────────────────────────────────────┤");
    eprintln!("│ signaling (ws) : ws://{http_addr}/signal      ");
    eprintln!("│ health check   : http://{http_addr}/health    ");
    eprintln!("│ tcp relay      : {relay_addr}                 ");
    eprintln!("└──────────────────────────────────────────────┘");

    let limits = Limits::from_env();
    let turn = TurnConfig::from_env();
    match &turn {
        Some(t) => eprintln!(
            "turn credentials: minting enabled (ttl {}s, GET /turn)",
            t.ttl_secs
        ),
        None => {
            eprintln!("turn credentials: disabled (set OPACUSR_TURN_SECRET to enable GET /turn)")
        }
    }

    let state = Arc::new(AppState::with_options(cli.relay_port, limits, turn));
    opacusr_server::serve(http, relay, state).await?;
    Ok(())
}
