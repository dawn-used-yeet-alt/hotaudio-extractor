//! `X-Signature` generation, ported from the pinned `nozzle.js` signer.
//!
//! The TypeScript original evaluates the obfuscated player bundle inside a fake
//! browser sandbox. This port replaces that whole sandbox with a native
//! interpreter for the VM the bundle was hiding — see [`vm`] for the recovered
//! machine and [`env`] for the pinned environment fingerprint.

pub mod bytecode;
pub mod env;
pub mod vm;

pub use bytecode::{PROGRAM, PROGRAM_LEN};
pub use env::{ENV_HASHES, PERF_NOW, PINNED_NOZZLE_VERSION};
pub use vm::{SignError, sign};

/// Current Unix time in seconds, floored — the clock the signer reads.
pub fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Compute the `X-Signature` header value for a listen-request payload,
/// stamped with the current time.
pub fn sign_payload(payload: &str) -> Result<String, SignError> {
    sign(payload, now_secs())
}

/// Convenience for callers that want to pin the clock (tests, replays).
pub fn sign_payload_at(payload: &str, timestamp_secs: f64) -> Result<String, SignError> {
    sign(payload, timestamp_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytecode_is_even_and_non_trivial() {
        // The program is 1314 entries: 657 two-byte instructions.
        assert_eq!(PROGRAM_LEN, 1314);
        assert_eq!(PROGRAM_LEN % 2, 0);
    }

    #[test]
    fn env_hashes_present_and_unique() {
        let set: std::collections::HashSet<_> = ENV_HASHES.iter().collect();
        assert_eq!(set.len(), ENV_HASHES.len(), "duplicate env hash");
        assert!(ENV_HASHES.contains(&2878));
    }
}