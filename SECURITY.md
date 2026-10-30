# Security Policy

## Supported versions

| Version | Supported |
| --- | --- |
| 0.3.x | yes |
| < 0.3 | no |

## Reporting a vulnerability

OpacusR is remote-control software - treat any security issue with priority.

**Do not open a public issue for a security report.**

Use GitHub private vulnerability reporting:
https://github.com/opacusio/opacusr/security/advisories/new

Please include: affected version or commit, platform, component (viewer / agent / server / transport / crypto), reproduction steps or PoC, and impact assessment.

You will get an acknowledgment within 72 hours and a status update at least every 7 days until the issue is resolved. Fixes land as a patch release; reporters are credited in the release notes unless they prefer otherwise.

## Scope notes

- All transports are expected to be E2E-encrypted (ChaCha20-Poly1305); findings that break transport confidentiality or integrity are critical.
- The signaling/relay server is designed for self-hosting; reports about metadata leakage to third parties are in scope.
- The unattended-access password flow (challenge-response, keychain storage) is in scope.