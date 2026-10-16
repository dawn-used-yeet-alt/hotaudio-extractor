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
//!
//! # Targets
//!
//! One implementation, two transports. The pipeline is `async` at the I/O
//! boundary so the same code drives blocking `ureq` natively and the browser's
//! `fetch` on `wasm32-unknown-unknown`; the CPU-bound half — [`crypto`],
//! [`hax`], [`signer`] — is synchronous and target-independent on both.
//!
//! ```no_run
//! # async fn run(url: &str) -> hotaudio::Result<()> {
//! use hotaudio::download::DownloadOptions;
//!
//! let client = hotaudio::http::Client::api();
//! let mut opts = DownloadOptions::default();
//! let track = hotaudio::download::download_from_page(&client, url, &mut opts).await?;
//! std::fs::write("track.m4a", track.audio)
//!     .map_err(|e| hotaudio::Error::Http(e.to_string()))?;
//! # Ok(()) }
//! ```
//!
//! On a native build, drive that with `block_on`; it does not exist on `wasm32`,
//! where the host event loop does it.

/// Declare a test with an `async` body that runs on either target.
///
/// Native gets a `#[test]` driving the body with `exec::block_on`; `wasm32`
/// gets a `#[wasm_bindgen_test]`, which awaits it on the host event loop.
/// Writing the body once is the point: a hand-duplicated wasm suite drifts from
/// the native one, and the drift is invisible until the browser behaves
/// differently from the CLI.
///
/// The two expansions are mutually exclusive `#[cfg]`s rather than one function
/// wearing two attributes, because only the `wasm_bindgen_test` form may be
/// `async`. Declared before the modules so it is in scope for their unit tests.
#[cfg(test)]
macro_rules! async_test {
    ($(#[$attr:meta])* $name:ident $body:block) => {
        $(#[$attr])*
        #[cfg(not(target_arch = "wasm32"))]
        #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
        #[cfg_attr(not(target_arch = "wasm32"), test)]
        fn $name() {
            crate::exec::block_on(async $body);
        }

        $(#[$attr])*
        #[cfg(target_arch = "wasm32")]
        #[::wasm_bindgen_test::wasm_bindgen_test]
        async fn $name() $body
    };
}

#[cfg(not(target_arch = "wasm32"))]
pub mod exec;

pub mod clock;
pub mod crypto;
pub mod download;
pub mod hax;
pub mod http;
pub mod listen;
pub mod mp4;
pub mod range;
pub mod signer;
#[cfg(target_arch = "wasm32")]
pub mod wasm;

pub use crypto::{Error, Result};

/// Drive an async pipeline on a native build.
///
/// Absent on `wasm32`, where the host event loop already does this and blocking
/// it would deadlock the page.
#[cfg(not(target_arch = "wasm32"))]
pub use exec::block_on;

/// Default User-Agent for page and API requests.
///
/// Cloudflare returns HTTP 403 (`cf-mitigated: challenge`) for Chrome
/// user-agents on the track page and the listen endpoint. The bare
/// `Mozilla/5.0` token is accepted, so it must not be "improved".
///
/// `fetch` forbids a page from setting `User-Agent` (or `Origin`, or
/// `Referer`), so on `wasm32` this header is not sent at all and the browser
/// sends its own — which the site answers with 403. That is one of the reasons
/// a browser deployment needs a proxy in front; see `docs/ARCHITECTURE.md`.
pub const HOTAUDIO_UA: &str = "Mozilla/5.0";

/// Base URL for the encrypted listen handshake.
pub const HOTAUDIO_API_BASE: &str = "https://hotaudio.net";

/// Emit a non-fatal diagnostic.
///
/// Native writes to stderr; `wasm32` has no stderr, so it goes to
/// `console.warn`. Centralised so both transports report the same way.
pub(crate) fn warn(msg: &str) {
    #[cfg(target_arch = "wasm32")]
    web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(msg));
    #[cfg(not(target_arch = "wasm32"))]
    eprintln!("warning: {msg}");
}

/// Match canonical hotaudio share links.
///
/// The crate fetches whatever URL it is given, so this is a heuristic for
/// catching a wrong link early, not an access-control check: mirrors, proxies and
/// short links may legitimately fail it.
pub fn is_hotaudio_url(url: &str) -> bool {
    // https?://(www.)?hotaudio.net/u/<user>/<track> with an optional query and
    // an optional trailing slash.
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or("");
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let Some(path) = rest.strip_prefix("hotaudio.net/") else {
        return false;
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    let path = path.trim_end_matches('/');
    // Expect exactly `u/<user>/<track>`.
    let mut parts = path.split('/');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("u"), Some(user), Some(track), None) => !user.is_empty() && !track.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn url_detection() {
        assert!(is_hotaudio_url("https://hotaudio.net/u/a/b"));
        assert!(is_hotaudio_url("https://www.hotaudio.net/u/a/b?x=1"));
        assert!(is_hotaudio_url("http://hotaudio.net/u/a/b/"));
        assert!(is_hotaudio_url("https://hotaudio.net/u/a/b/#frag"));
        assert!(!is_hotaudio_url("https://example.com/u/a/b"));
        assert!(!is_hotaudio_url("https://hotaudio.net/u/a"));
        assert!(!is_hotaudio_url("https://hotaudio.net/u/a/b/c"));
        assert!(!is_hotaudio_url("https://hotaudio.net/u//b"));
        assert!(!is_hotaudio_url("https://hotaudio.net/"));
    }
}
