//! `X-Signature` generation, ported from the pinned `nozzle.js` signer.
//!
//! The TypeScript original evaluates the obfuscated player bundle inside a fake
//! browser sandbox. This port replaces that whole sandbox with a native
//! interpreter for the VM the bundle was hiding — see [`vm`] for the recovered
//! machine and [`mod@env`] for the pinned environment fingerprint.

pub mod bytecode;
pub mod env;
pub mod vm;

pub use bytecode::{PROGRAM, PROGRAM_LEN};
pub use env::{ENV_HASHES, PERF_NOW, PINNED_NOZZLE_VERSION};
pub use vm::{Rendered, SignError, sign, trace};

/// Current Unix time in seconds, floored — the clock the signer reads.
///
/// Routed through [`crate::clock`] because `SystemTime::now` panics on
/// `wasm32-unknown-unknown`, and the browser build must produce the same
/// signature the native one does.
#[must_use]
pub fn now_secs() -> f64 {
    crate::clock::unix_secs()
}

/// Compute the `X-Signature` header value for a listen-request payload,
/// stamped with the current time.
///
/// # Errors
///
/// Returns [`SignError`] if the payload cannot be rendered or the VM stops.
pub fn sign_payload(payload: &str) -> Result<String, SignError> {
    sign(payload, now_secs())
}

/// Convenience for callers that want to pin the clock (tests, replays).
///
/// # Errors
///
/// Returns [`SignError`] if the payload cannot be rendered or the VM stops.
pub fn sign_payload_at(payload: &str, timestamp_secs: f64) -> Result<String, SignError> {
    sign(payload, timestamp_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn bytecode_is_even_and_non_trivial() {
        // The program is 1314 entries: 657 two-byte instructions.
        assert_eq!(PROGRAM_LEN, 1314);
        assert!(PROGRAM_LEN.is_multiple_of(2));
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn env_hashes_present_and_unique() {
        let set: std::collections::HashSet<_> = ENV_HASHES.iter().collect();
        assert_eq!(set.len(), ENV_HASHES.len(), "duplicate env hash");
        assert!(ENV_HASHES.contains(&2878));
    }
}
