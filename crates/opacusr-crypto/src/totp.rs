//! RFC 6238 time-based one-time passwords (TOTP) with RFC 4648 base32
//! secrets.
//!
//! Parameters follow the de-facto standard implemented by Google
//! Authenticator, Aegis, 1Password & co: HMAC-SHA1, 30-second step,
//! 6 decimal digits, base32 (unpadded) secret of 20 random bytes.
//! Verification accepts the previous/current/next window to tolerate
//! clock drift between host and viewer.

use hmac::{Hmac, Mac};
use sha1::Sha1;

use crate::random_bytes;

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// TOTP time step in seconds (RFC 6238 default).
pub const STEP: u64 = 30;
/// Number of decimal digits in a code (Google Authenticator default).
pub const DIGITS: u32 = 6;
/// How many windows before/after `now` are accepted during verification.
pub const CLOCK_SKEW_WINDOWS: u64 = 1;

/// Encode bytes as unpadded RFC 4648 base32.
pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    for chunk in data.chunks(5) {
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let bits = u64::from_be_bytes([0, buf[0], buf[1], buf[2], buf[3], buf[4], 0, 0]) >> 16; // 40 usable bits, left-aligned in a u64
        let n = chunk.len() * 8; // significant bits
        for i in 0..8 {
            if i * 5 < n {
                let idx = ((bits >> (35 - i * 5)) & 0x1f) as usize;
                out.push(ALPHABET[idx] as char);
            }
        }
    }
    out
}

/// Decode RFC 4648 base32 (padded or unpadded, case-insensitive on the
/// letters' canonical form — lowercase input is rejected like most
/// authenticator apps); `None` on invalid characters or trailing bits.
pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim().trim_end_matches('=');
    if s.is_empty() {
        return None;
    }
    let mut acc: u64 = 0;
    let mut bits: u32 = 0;
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    for ch in s.bytes() {
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'2'..=b'7' => ch - b'2' + 26,
            _ => return None,
        } as u64;
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    // Reject dangling bits that are not zero (a truncated encoding).
    if bits > 0 && (acc & ((1 << bits) - 1)) != 0 {
        return None;
    }
    Some(out)
}

/// Fresh random secret, base32-encoded (20 bytes = 160 bit, as RFC 4226
/// recommends). Usable both as an `otpauth://` secret and for manual
/// entry into authenticator apps.
pub fn generate_secret() -> String {
    base32_encode(&random_bytes::<20>())
}

/// HOTP (RFC 4226) for one counter value: HMAC-SHA1 → dynamic truncation
/// → 6 decimal digits.
fn hotp(secret: &[u8], counter: u64) -> String {
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(&counter.to_be_bytes());
    let d = mac.finalize().into_bytes();
    let off = (d[19] & 0x0f) as usize;
    let bin = (u32::from(d[off] & 0x7f) << 24)
        | (u32::from(d[off + 1]) << 16)
        | (u32::from(d[off + 2]) << 8)
        | u32::from(d[off + 3]);
    format!("{:0width$}", bin % 1_000_000, width = DIGITS as usize)
}

/// TOTP code (6 digits) for a base32 `secret_b32` at unix time
/// `time_secs`; `None` when the secret is not valid base32.
pub fn totp_at(secret_b32: &str, time_secs: u64) -> Option<String> {
    let secret = base32_decode(secret_b32)?;
    Some(hotp(&secret, time_secs / STEP))
}

/// Verify a user-typed code against a base32 secret, accepting codes from
/// the previous/current/next [`STEP`] windows (clock drift). Digit-string
/// shape is checked first, so garbage never reaches the comparison.
///
/// Note: the final match is an ordinary string comparison — the 6-digit
/// space is tiny, so constant-time tricks add nothing against an online
/// attacker; brute force is handled by the transport's auth lockout.
pub fn verify_totp(secret_b32: &str, code: &str, now_secs: u64) -> bool {
    let code = code.trim();
    if code.len() != DIGITS as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let Some(secret) = base32_decode(secret_b32) else {
        return false;
    };
    let t = now_secs / STEP;
    (t.saturating_sub(CLOCK_SKEW_WINDOWS)..=t.saturating_add(CLOCK_SKEW_WINDOWS))
        .any(|c| hotp(&secret, c) == code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4226 / 6238 test secret: ASCII "12345678901234567890".
    const RFC_SECRET: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    #[test]
    fn base32_roundtrip_and_vectors() {
        // The RFC 6238 reference secret decodes to the ASCII key.
        assert_eq!(base32_decode(RFC_SECRET).unwrap(), b"12345678901234567890");
        assert_eq!(base32_encode(b"12345678901234567890"), RFC_SECRET);
        assert_eq!(base32_encode(b""), "");
        assert_eq!(base32_encode(b"f"), "MY======".trim_end_matches('='));
        assert_eq!(
            base32_decode(&base32_encode(&[7u8; 20])).unwrap(),
            vec![7u8; 20]
        );
        assert!(base32_decode("ABC1!").is_none(), "invalid chars");
        assert!(base32_decode("").is_none());
    }

    #[test]
    fn rfc6238_vectors() {
        // RFC 6238 Appendix B (SHA1), truncated to 6 digits.
        assert_eq!(totp_at(RFC_SECRET, 59).unwrap(), "287082");
        assert_eq!(totp_at(RFC_SECRET, 1_111_111_109).unwrap(), "081804");
        assert_eq!(totp_at(RFC_SECRET, 1_234_567_890).unwrap(), "005924");
        assert_eq!(totp_at(RFC_SECRET, 2_000_000_000).unwrap(), "279037");
        assert!(totp_at("not base32!", 59).is_none());
    }

    #[test]
    fn verify_accepts_skew_and_rejects_garbage() {
        // Static check: the RFC vector is valid inside its own window.
        assert!(verify_totp(RFC_SECRET, "081804", 1_111_111_109));
        // Dynamic check: ±1 window around `now`.
        let now = 1_111_111_100;
        let cur = totp_at(RFC_SECRET, now).unwrap();
        assert_eq!(cur, "081804", "same counter as the RFC vector");
        let prev = totp_at(RFC_SECRET, now - STEP).unwrap();
        let next = totp_at(RFC_SECRET, now + STEP).unwrap();
        assert!(verify_totp(RFC_SECRET, &prev, now));
        assert!(verify_totp(RFC_SECRET, &next, now));
        assert!(verify_totp(
            RFC_SECRET,
            totp_at(RFC_SECRET, now + STEP).unwrap().as_str(),
            now
        ));
        // Whitespace around the code is tolerated.
        assert!(verify_totp(RFC_SECRET, " 081804 ", now));
        // Wrong code, wrong shape, wrong secret.
        assert!(!verify_totp(RFC_SECRET, "287083", now));
        assert!(!verify_totp(RFC_SECRET, "28708", now));
        assert!(!verify_totp(RFC_SECRET, "28708a", now));
        assert!(!verify_totp("", "287082", now));
    }

    #[test]
    fn generated_secrets_work_with_authenticator_shape() {
        let s = generate_secret();
        assert_eq!(s.len(), 32, "160 bit → 32 base32 chars");
        assert!(s.bytes().all(|b| ALPHABET.contains(&b)));
        assert!(verify_totp(
            &s,
            &totp_at(&s, 1_700_000_000).unwrap(),
            1_700_000_000
        ));
    }
}
