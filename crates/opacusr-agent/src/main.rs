//! `opacusr-agent` — host-side CLI: run the agent, set the unattended
//! password, show your ID.

use clap::Parser;
use opacusr_agent::config::AgentConfig;
use opacusr_agent::service::{AgentEvent, SessionDecision};
use opacusr_agent::AgentService;
use tokio::sync::{mpsc, watch};

#[derive(Debug, Parser)]
#[command(name = "opacusr-agent", version, about = "OpacusR host agent")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, clap::Subcommand)]
enum Cmd {
    /// Connect to the signaling server and accept sessions.
    Run {
        /// Override the signaling server URL from the config.
        #[arg(long)]
        server: Option<String>,
        /// Override the display name.
        #[arg(long)]
        name: Option<String>,
        /// Stream a synthetic test pattern instead of the real screen
        /// (useful in VMs/CI).
        #[arg(long)]
        demo: bool,
        /// Also accept direct TCP sessions (LAN mode).
        #[arg(long)]
        allow_lan: bool,
        /// Serve the embedded signaling registry + relay in this process
        /// (zero-infra mode; see docs/serverless.md).
        #[arg(long)]
        signal_listen: bool,
        /// Windows capture backend: auto | gdi | dxgi (overrides config).
        #[arg(long)]
        capture: Option<String>,
    },
    /// Set the unattended access password (OS keychain, file fallback).
    Passwd {
        /// Store the password in the config file instead of the OS keychain.
        #[arg(long)]
        file: bool,
    },
    /// Clear the unattended access password.
    PasswdClear,
    /// Register on the server and print the assigned ID.
    Id {
        #[arg(long)]
        server: Option<String>,
    },
    /// Print the effective config location and summary.
    Config,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    opacusr_transport::update::spawn_update_notice();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,opacusr=debug".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Run {
            server,
            name,
            demo,
            allow_lan,
            signal_listen,
            capture,
        } => run(server, name, demo, allow_lan, signal_listen, capture).await,
        Cmd::Passwd { file } => passwd(file),
        Cmd::PasswdClear => passwd_clear(),
        Cmd::Id { server } => id(server).await,
        Cmd::Config => config_info(),
    }
}

fn load_cfg(server: Option<String>, name: Option<String>) -> AgentConfig {
    let mut cfg = AgentConfig::load();
    if let Some(s) = server {
        cfg.server_url = s;
    }
    if let Some(n) = name {
        cfg.name = n;
    }
    cfg
}

async fn run(
    server: Option<String>,
    name: Option<String>,
    demo: bool,
    allow_lan: bool,
    signal_listen: bool,
    capture: Option<String>,
) -> anyhow::Result<()> {
    let mut cfg = load_cfg(server, name);
    if let Some(c) = capture {
        cfg.capture = c;
    }
    if signal_listen {
        cfg.signal_listen = true;
    }
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let unattended = cfg.is_unattended();
    if unattended {
        let where_ = if cfg.password_in_keychain {
            "OS keychain"
        } else {
            "config file"
        };
        eprintln!("Unattended mode: sessions are accepted after a password check ({where_}).");
    } else {
        eprintln!("Attended mode: every session needs your confirmation (set a password with `opacusr-agent passwd`).");
    }

    let service = AgentService {
        cfg: cfg.clone(),
        events: tx,
        demo,
        unattended,
    };

    // Console UI: event log.
    let ui = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::Registered { id, server } => {
                    eprintln!("┌─────────────────────────────────────────────┐");
                    eprintln!("│  OpacusR agent online                       │");
                    eprintln!("│  Your ID: {id}            ");
                    eprintln!("│  Server:  {server}  ");
                    eprintln!("└─────────────────────────────────────────────┘");
                }
                AgentEvent::IncomingRequest { name, platform, .. } => {
                    eprintln!("· session request from {name} ({})…", platform.as_str());
                }
                AgentEvent::SessionStarted { .. } => eprintln!("▶ session started"),
                AgentEvent::SessionEnded { reason } => eprintln!("■ session ended: {reason}"),
                AgentEvent::Notice(text) => eprintln!("· {text}"),
            }
        }
    });

    // Attended consent: a blocking stdin prompt on a dedicated thread,
    // bridged through a channel (the GUI replaces this with a dialog).
    let service_run = async {
        let is_unattended = unattended;
        let (req_tx, mut req_rx) = mpsc::unbounded_channel::<(opacusr_proto::PeerId, String)>();
        let answers: std::sync::Arc<std::sync::Mutex<Vec<SessionDecision>>> = Default::default();
        let prompt_answers = answers.clone();
        std::thread::spawn(move || {
            while let Some((id, name)) = req_rx.blocking_recv() {
                let d = prompt_consent(id, &name).unwrap_or(SessionDecision::Reject);
                prompt_answers.lock().unwrap().push(d);
            }
        });
        service
            .run(
                move |id, name, _| {
                    if is_unattended {
                        return SessionDecision::Full;
                    }
                    let _ = req_tx.send((id, name.to_string()));
                    // Wait for the console answer (the prompt thread sets
                    // it); 60 s timeout → decline.
                    for _ in 0..600 {
                        if let Some(v) = answers.lock().unwrap().pop() {
                            return v;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    SessionDecision::Reject
                },
                shutdown_rx.clone(),
            )
            .await
    };

    let lan_task = async {
        if allow_lan {
            let lan_service = AgentService {
                cfg: cfg.clone(),
                events: mpsc::unbounded_channel().0,
                demo,
                unattended: true, // LAN still requires the password
            };
            lan_service
                .serve_lan(cfg.lan_port, shutdown_rx.clone())
                .await
        } else {
            futures::future::pending::<anyhow::Result<()>>().await
        }
    };

    tokio::select! {
        r = service_run => { shutdown_tx.send(true).ok(); r?; }
        r = lan_task => { shutdown_tx.send(true).ok(); r?; }
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\nshutting down…");
            shutdown_tx.send(true).ok();
        }
    }
    ui.abort();
    Ok(())
}

/// Blocking console prompt (CLI-only). `y` = full control, `v` = view
/// only, anything else = reject.
fn prompt_consent(id: opacusr_proto::PeerId, name: &str) -> std::io::Result<SessionDecision> {
    use std::io::Write;
    eprint!("Session request from {name} (id {id}). Allow? [y=full / v=view-only / N=no] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let a = line.trim().to_ascii_lowercase();
    Ok(match a.as_str() {
        "y" | "yes" => SessionDecision::Full,
        "v" | "view" | "view-only" => SessionDecision::ViewOnly,
        _ => SessionDecision::Reject,
    })
}

/// Store the unattended password: OS keychain when available (default),
/// config-file fallback otherwise, or force the file with `force_file`.
fn passwd(force_file: bool) -> anyhow::Result<()> {
    let mut cfg = AgentConfig::load();
    let p1 = rpassword::prompt_password("Set unattended password: ")?;
    let p2 = rpassword::prompt_password("Repeat password: ")?;
    if p1 != p2 {
        anyhow::bail!("passwords do not match");
    }
    if p1.len() < 4 {
        anyhow::bail!("password too short (min 4 chars)");
    }
    if !force_file {
        match opacusr_agent::keychain::store_password(&p1) {
            Ok(()) => {
                cfg.password = None;
                cfg.password_in_keychain = true;
                let path = cfg.save()?;
                println!("Unattended password saved to the OS keychain (service 'opacusr').");
                println!("Config flag updated: {}", path.display());
                println!(
                    "The agent will now accept sessions that pass this password without prompting."
                );
                return Ok(());
            }
            Err(e) => {
                eprintln!("warning: {e}");
                eprintln!("falling back to the config file (mode 0600 on unix)");
            }
        }
    }
    // File fallback (also clears a stale keychain flag).
    let _ = opacusr_agent::keychain::delete_password();
    cfg.password = Some(p1);
    cfg.password_in_keychain = false;
    let path = cfg.save()?;
    println!("Unattended password saved to {}", path.display());
    println!("The agent will now accept sessions that pass this password without prompting.");
    Ok(())
}

fn passwd_clear() -> anyhow::Result<()> {
    let mut cfg = AgentConfig::load();
    // Remove from both stores; report keychain problems but stay successful.
    if let Err(e) = opacusr_agent::keychain::delete_password() {
        eprintln!("note: {e}");
    }
    cfg.password = None;
    cfg.password_in_keychain = false;
    cfg.save()?;
    println!("Unattended password cleared; attended mode (interactive consent) is active.");
    Ok(())
}

async fn id(server: Option<String>) -> anyhow::Result<()> {
    let cfg = load_cfg(server, None);
    // Fan-out: every configured endpoint assigns its own ID.
    for url in cfg.signaling_endpoints() {
        match register_once(&cfg, &url).await {
            Ok(id) => println!("{url} -> {id}"),
            Err(e) => eprintln!("{url} -> error: {e}"),
        }
    }
    Ok(())
}

async fn register_once(cfg: &AgentConfig, url: &str) -> anyhow::Result<opacusr_proto::PeerId> {
    let mut sig = opacusr_transport::signaling::SignalingClient::connect(url).await?;
    sig.send(&opacusr_proto::signal::SignalMessage::Register(
        opacusr_proto::signal::Register {
            platform: opacusr_proto::Platform::current(),
            display_name: cfg.name.clone(),
            is_agent: true,
            unattended: cfg.is_unattended(),
        },
    ))
    .await?;
    match sig.recv().await? {
        opacusr_proto::signal::SignalMessage::Welcome(w) => Ok(w.id),
        opacusr_proto::signal::SignalMessage::Error { message } => anyhow::bail!(message),
        other => anyhow::bail!("unexpected reply: {other:?}"),
    }
}

fn config_info() -> anyhow::Result<()> {
    let path = AgentConfig::path();
    let cfg = AgentConfig::load();
    println!("config file: {}", path.display());
    println!("server:      {}", cfg.server_url);
    println!(
        "signaling:   {} endpoint(s)",
        cfg.signaling_endpoints().len()
    );
    for u in cfg.signaling_endpoints() {
        println!("  - {u}");
    }
    println!(
        "embedded:    {} (port {}, relay +1)",
        if cfg.signal_listen { "on" } else { "off" },
        cfg.signal_listen_port
    );
    println!("name:        {}", cfg.name);
    println!("fps:         {}", cfg.fps);
    println!("quality:     {}", cfg.jpeg_quality);
    println!("scale:       {}%", cfg.scale_percent);
    println!("downloads:   {}", cfg.download_dir.display());
    println!("codec:       {}", cfg.codec);
    println!(
        "clipboard:   {}",
        if cfg.clipboard_sync { "synced" } else { "off" }
    );
    println!(
        "adaptive:    {}",
        if cfg.adaptive_quality { "on" } else { "off" }
    );
    println!(
        "acl:         default {}, {} rule(s)",
        cfg.default_permission,
        cfg.acl_rules.len()
    );
    println!("audio:       {}", if cfg.audio { "on" } else { "off" });
    let storage = if cfg.password.is_some() {
        "file"
    } else if cfg.password_in_keychain {
        "keychain"
    } else {
        "—"
    };
    println!("unattended:  {} ({storage})", cfg.is_unattended());
    Ok(())
}
