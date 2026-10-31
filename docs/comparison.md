# OpacusR vs TeamViewer / AnyDesk — feature gap analysis

What the established products have that OpacusR does not, and what is
worth copying. Written 2026-10 against OpacusR v0.4.0 + current main
(zero-infra networking phases 1–2). Vendor feature sets are the publicly
documented ones (TeamViewer Remote / AnyDesk Free–Professional); quotas
and prices change, the structural gaps do not.

The short version: OpacusR already covers the *core* remote-control loop
(streaming, input, files, chat, terminal, unattended access, audio,
clipboard, multi-monitor, adaptive quality) and is **ahead** in openness
(AGPL, self-hosted, no account), security design (documented crypto model,
keychain storage, rate limiting + lockout, time-limited TURN credentials)
and cost (zero-infra mode with automatic endpoint selection). The gaps are
in polish, host-side ergonomics and enterprise niceties.

## Feature matrix

| Feature | TeamViewer | AnyDesk | OpacusR | Where / plan |
| --- | --- | --- | --- | --- |
| Screen streaming + remote input | ✅ | ✅ | ✅ | core (JPEG/H.264, adaptive quality) |
| Multi-monitor select | ✅ | ✅ | ✅ | shipped (v0.3.0) |
| File transfer | ✅ | ✅ | ✅ | shipped; drag-and-drop into the window missing |
| Chat during session | ✅ | ✅ | ✅ | shipped |
| Remote terminal/PTY | ✅ (PowerShell) | ➖ (TCP tunnel only) | ✅ | shipped (PTY) |
| Clipboard sync | ✅ | ✅ | ✅ | shipped (echo-suppressed) |
| Audio streaming | ✅ | ✅ | ✅ | shipped (WASAPI loopback / cpal, Opus) |
| Unattended access + password | ✅ | ✅ | ✅ | shipped (keychain-stored) |
| **Lock host's physical input** | ✅ | ✅ | ❌ | **gap — M1 candidate** (Windows `BlockInput`, X11 grabs; Wayland restricted) |
| Session recording | ✅ | ✅ | ❌ | **gap — M1/M3 candidate** (host-side, opt-in) |
| Two-factor auth (TOTP) for unattended | ✅ | ✅ | ❌ | **gap — M2 candidate** (next to the keychain password) |
| TCP tunneling / port forwarding | ✅ (VPN) | ✅ | ❌ | **gap — M2/M3 candidate** (reuse SecureSession framing) |
| Wake-on-LAN / reboot & auto-reconnect | ✅ | ✅ | ❌ | needs service mode (already M2) |
| Privacy mode (blank host's physical screen) | ✅ | ✅ | ❌ | hard — needs dummy display driver; M3, research item |
| Remote printing | ✅ | ✅ | ❌ | big per-OS spooler work; M3, likely "evaluate" |
| Address book / saved connections | ✅ | ✅ | ❌ | easy UX win — M1 candidate |
| Connection history / session log | ✅ | ✅ | ❌ | M1/M3 (agent already logs auth events) |
| Session stats panel (quality tier, loss) | ✅ | ✅ | ❌ | already roadmap M1 |
| Fullscreen/hotkeys, quality presets | ✅ | ✅ | partial | already roadmap M1 |
| Remote process/task control | ✅ (sysinfo) | ➖ | ❌ | M2 candidate (small: list + kill over PTY-style messages) |
| Auto-update (in-app install) | ✅ | ✅ | ❌ | check-only today; signed artifacts exist (M3) |
| Mobile client (view/control from phone) | ✅ | ✅ | ❌ | roadmap M4 |
| Multi-viewer observation | ✅ | paid tiers | ❌ (concurrent sessions already work) | roadmap M3 |
| Whiteboard / annotation | ✅ | ➖ | ❌ | M3, low priority |
| Voice chat / meetings | ✅ | ➖ | ❌ | likely non-goal (see below) |
| Custom branded clients | ✅ | ✅ | ❌ | likely non-goal (AGPL single binary) |
| LAN auto-discovery (mDNS/Bonjour) | ✅ | ✅ | planned | serverless phase 3 |
| **Self-hosted signaling + relay** | ➖ (on-prem = enterprise $$) | ✅ (on-prem paid) | ✅ free | shipped |
| **Zero-infra mode (no server at all)** | ❌ | ❌ | ✅ | shipped (embedded signaling + tunnels, docs/serverless.md) |
| **Open source, auditable** | ❌ | ❌ | ✅ AGPL | structural advantage |
| **Documented crypto/threat model** | ❌ | ❌ | ✅ | docs/security.md |

## What we will NOT copy (deliberately)

- Meetings/webinars/whiteboards-as-a-suite — out of scope for a screen
  control tool; the AGPL single-binary simplicity is worth more.
- MDM/monitoring/backup bundles — different product category.
- Account-gated ID routing as the only path — OpacusR's IDs are minted by
  whatever registry you point at (yours, or nobody's in LAN/zero-infra
  mode).

## Prioritized improvement list (input for the roadmap)

**P0 — small, high user-visible value**

1. Lock host input while a viewer is connected (toggle + policy).
2. Address book: saved hosts (name → endpoints + ID), recent connections.
3. Session stats panel + fullscreen hotkeys (already M1; ship them).
4. Drag-and-drop files onto the viewer window.

**P1 — differentiating security/ops**

5. TOTP second factor for unattended access (server-side-free: the agent
   verifies; pairs with the existing lockout).
6. Session recording (host-side, opt-in, clearly announced in-session).
7. TCP tunneling over the existing session channel (AnyDesk-style).
8. Reboot-and-reconnect (graceful agent restart; needs Windows service /
   launchd from M2).
9. Signaling over corporate proxies (HTTP CONNECT/SOCKS5 in the WS client).

**P2 — bigger bets, keep on the roadmap**

10. Privacy mode (Windows dummy display driver; research first).
11. Remote printing (evaluate effort per OS; possibly CUPS-only first).
12. Mobile viewer (M4) — the biggest remaining platform gap.
13. Auto-update channel with in-app install (artifacts are already signed
    and checksummed by the release pipeline).

Everything else from the matrix maps onto the existing milestones
(M1 media/UX, M2 trust, M3 scale/ops, M4 mobile) or the zero-infra
networking phases (mDNS, manual codes) in [roadmap.md](roadmap.md).
