# Zero-infra networking: connecting without renting servers

How to use OpacusR without paying for a VPS, and how the clients should
automatically pick the first working path. Status: **design** — phases 1–4
are tracked in [roadmap.md](roadmap.md). Everything described here builds on
pieces that already ship (LAN mode, WebRTC ICE, the TCP relay fallback,
`GET /turn`); the new work is glue, not new transports.

## 1. Why there is nothing to rent

The signaling server is the only component that needs a *public* address
today, and it does three small jobs: a peer registry (name → online),
relaying offer/answer SDP, and minting TURN credentials. For one agent
(and realistically up to a handful) the registry holds one entry. That job
fits comfortably inside:

1. the agent binary itself (it already embeds the whole transport stack),
2. a free tunnel in front of it, or
3. a mesh VPN address that needs no public exposure at all.

Media never depends on the signaling host in the happy path: WebRTC tries
direct P2P first and only falls back to relay. The relay itself can be the
same free-tunneled host over TCP when TURN is unavailable.

## 2. The connection ladder (automatic path selection)

The viewer probes paths top-down and uses the first one that completes the
session handshake. The active mode is shown in the UI ("LAN direct",
"P2P (STUN)", "Relay (TURN)"), so the user always knows what is carrying
the session.

| # | Path | Needs | Monthly cost | When it wins |
| --- | --- | --- | --- | --- |
| L0 | LAN direct TCP (`ip:port`, mDNS name) | nothing | 0 | viewer and agent on one network |
| L1 | WebRTC P2P, host + server-reflexive candidates | public STUN (free) | 0 | at least one side has a "good" NAT (no symmetric×symmetric) |
| L2 | WebRTC via TURN | a reachable TURN server | 0¹ | both sides symmetric NAT / UDP blocked |
| L3 | TCP relay through the signaling host | the tunneled agent (below) | 0 | UDP fully blocked, L2 unreachable |

¹ Free options, see §4: coturn on the agent's own router (port-forwarding)
or free-tier managed TURN (verify current quotas before relying on them).

Auto-switch rules:

- **At connect.** `connect = "auto"` (viewer) runs the ladder with short
  per-step budgets (mDNS 2 s, direct TCP 3 s, ICE gathering 8 s) instead of
  today's either/or choice between a 9-digit ID and `ip:port`.
- **Mid-session.** WebRTC already re-negotiates paths with ICE restarts —
  a laptop moving from Ethernet to mobile keeps the session without user
  action. The TCP transport gets a reconnect ladder that retries each
  configured signaling URL in order.
- **On the agent side.** `signaling_urls` becomes a *list*; the agent
  registers on every URL simultaneously, so a flaky free tunnel never takes
  connectivity down — viewers just use whichever endpoint answers.

## 3. Signaling without a rented server

Three ways to give the viewer an address, cheapest first.

### A. Embedded signaling + free tunnel (public, still free)

Run the registry inside the agent (`opacusr-agent serve --signal-listen`,
phase 2) and expose it with a free tunnel:

- **cloudflared** (Cloudflare Tunnel): free, gives TLS (`wss://`) for free,
  which closes the plaintext-signaling caveat from [security.md](security.md)
  §8.5; no port-forwarding on the router. Note: tunnels proxy TCP/WebSocket
  only — fine for signaling, not for TURN media.
- **Tailscale Funnel**: same idea, TCP 443 in; also free on the personal
  plan.

The viewer URL looks like `wss://agent.example.com/signal`; the agent is
the only machine that must be on.

### B. Mesh VPN, nothing public at all (Tailscale / NetBird / self-hosted headscale)

Both machines join the same tailnet (free personal plans cover individual
use). The viewer connects with the LAN mode over the VPN address
(`100.x.y.z:8081`, i.e. plain `ip:port` — L0 again, zero new code), or the
embedded signaling runs on the tailnet address for the full ID-based flow.
No ports are opened anywhere; the tunnel is WireGuard. This is the most
robust option for one person's machines and needs no router changes.

### C. Manual signaling (fully serverless fallback)

The ultimate no-infra mode, phase 4: the agent renders its WebRTC offer as
a short base64 code; the viewer pastes it, answers with its own code back.
No registry, no tunnel, no DNS — "meeting code" style. Clunkier UX, but it
always works from a clean machine and is the reference implementation that
keeps `SignalingClient` swappable behind a trait (already true — the app
talks to sessions, not to the registry).

## 4. TURN without paying

Only the symmetric×symmetric case needs TURN, and L3 (TCP relay) already
covers connectivity-wise what TURN covers, just over TCP through the
tunneled host. Still, for UDP throughput the options are:

1. **coturn on the agent's own router/NAS** — the repo already ships
   `docker/turnserver.conf`; port-forward 3478 + the relay range. Free,
   uses hardware you own. Set `OPACUSR_TURN_SECRET`, clients fetch
   time-limited credentials from `GET /turn` (shipped in v0.4.0).
2. **Managed free-tier TURN** (Cloudflare Calls TURN, Metered, etc.) —
   quotas change; check them before depending on it. Plug the URLs into
   `ice_urls`; no code change.
3. **A one-off cheap VPS as TURN-only** (~$1–2/mo range) is the last resort
   for people whose routers cannot forward ports; signaling stays free.

## 5. Implementation phases

1. **Ladder + multi-URL signaling** — `signaling_urls: Vec<String>` in the
   agent, viewer `connect = "auto"` with the L0→L1 probing order and a mode
   indicator. Uses only existing crates; the largest change is refactoring
   `SessionClient::connect` behind the path-selection loop.
2. **Embedded signaling in the agent** — lift `opacusr-server`'s registry
   into a library the agent can serve on `signal_listen_port`; fan-out
   registration across all `signaling_urls`; tunnel cookbook (cloudflared
   and Tailscale Funnel configs ready to copy).
3. **mDNS discovery** — `_opacusr._tcp.local.` via `mdns-sd`; the viewer's
   connect box accepts `hostname.local` and offers a picker of discovered
   agents; removes the need to know IPs on LAN.
4. **Manual signaling codes** — offer/answer paste flow as the L4
   last-resort path; also unblocks debugging when tunnels/registries are
   broken.

## 6. Security notes

- Free tunnels terminate TLS for us: signaling rides `wss://` everywhere,
  which fixes today's "plain ws:// crosses the network unencrypted" caveat
  for internet-facing use.
- Embedded signaling shrinks the trust surface: no third-party host sees
  peer metadata, and the registry has exactly one peer.
- The password handshake, lockout and rate limits apply unchanged on every
  path (LAN and signaling are separate accept paths — see
  [security.md](security.md) §8.3); TURN credentials stay time-limited
  regardless of where coturn runs.
- Manual codes (phase 4) must be shown with the same fingerprint warnings
  as SSH host keys on first use (TOFU is a roadmap item in
  [security.md](security.md) §10).
