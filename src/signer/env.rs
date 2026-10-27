//! Static environment fingerprint for the pinned player build.
//!
//! The bundle's VM walks a small, fixed set of globals. `ENV_HASHES` is the
//! subset of the environment object's keys that the program probes and that
//! the reference browser actually has (the `op 118` `in` sites); every value
//! is `undefined`, so only the key set matters. Probed keys the browser lacks
//! are omitted, which is what makes those checks false. This table must be
//! re-captured together with [`super::bytecode`].

/// Environment keys present in the reference browser (3 in this build).
pub const ENV_HASHES: [u32; 3] = [2878, 21112, 33069];

/// Frozen `performance.now()` value the shim reports.
pub const PERF_NOW: f64 = 123_456.789;

/// Player build the bytecode + environment table were captured from.
pub const PINNED_NOZZLE_VERSION: &str = "wGUeBtfc";

/// V8-formatted stack for the `Date.stack` `TypeError` probe.
/// The pinned program does not read `.stack`, so this is not folded into its
/// digest; it keeps the VM's error path bit-exact for builds that do.
#[must_use]
pub fn nozzle_stack() -> String {
    format!(
        "TypeError: Cannot read properties of undefined (reading 'stack')\n    at https://hotaudio.net/nozzle.js?v={PINNED_NOZZLE_VERSION}:2:3472\n    at S (https://hotaudio.net/nozzle.js?v={PINNED_NOZZLE_VERSION}:1:37987)"
    )
}
