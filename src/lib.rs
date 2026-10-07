//! hotaudio.net audio extractor.
//!
//! Standalone, no application dependencies: performs the track page handshake,
//! signs each listen request with a native port of the player's signature VM,
//! completes an X25519 key exchange, pages key branches for long tracks, and
//! decrypts the HAX0 container to playable `.m4a` bytes.
//!
//! The signer is the interesting part. Upstream, `X-Signature` comes from
//! evaluating a ~106 KB obfuscated JavaScript bundle inside a fake-browser
//! sandbox; the bundle's real payload turned out to be a register machine
//! running a 1314-byte program. This crate re-implements that machine directly
//! (see [`signer::vm`]), which is both exact — it is the same program, not a
//! re-derivation of the algorithm — and roughly a thousand times faster than
//! evaluating the bundle.
//!
//! Fast and lightweight, but it tracks site internals: when the upstream player
//! changes, [`signer::bytecode`] and [`signer::env`] must be re-captured
//! together or signatures will be rejected.

pub mod crypto;
pub mod download;
pub mod hax;
pub mod http;
pub mod listen;
pub mod range;
pub mod signer;

pub use crypto::{Error, Result};

/// Default User-Agent for page and API requests.
///
/// Cloudflare returns HTTP 403 (`cf-mitigated: challenge`) for Chrome
/// user-agents on the track page and the listen endpoint. The bare
/// `Mozilla/5.0` token is accepted, so it must not be "improved".
pub const HOTAUDIO_UA: &str = "Mozilla/5.0";

/// Base URL for the encrypted listen handshake.
pub const HOTAUDIO_API_BASE: &str = "https://hotaudio.net";

/// Match canonical hotaudio share links.
pub fn is_hotaudio_url(url: &str) -> bool {
    // https?://(www.)?hotaudio.net/u/<user>/<track> with an optional query.
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or("");
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let Some(path) = rest.strip_prefix("hotaudio.net/") else {
        return false;
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    // Expect exactly `u/<user>/<track>`.
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() != 3 || parts[0] != "u" {
        return false;
    }
    !parts[1].is_empty() && !parts[2].is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_detection() {
        assert!(is_hotaudio_url("https://hotaudio.net/u/a/b"));
        assert!(is_hotaudio_url("https://www.hotaudio.net/u/a/b?x=1"));
        assert!(!is_hotaudio_url("https://example.com/u/a/b"));
        assert!(!is_hotaudio_url("https://hotaudio.net/u/a"));
        assert!(!is_hotaudio_url("https://hotaudio.net/"));
    }
}
