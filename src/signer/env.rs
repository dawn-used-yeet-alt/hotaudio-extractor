//! Static environment fingerprint for the pinned player build.
//!
//! The bundle's VM walks a small, fixed set of globals. `ENV_HASHES` is the
//! property-key set the VM tests against its environment object (the `op 118`
//! `in` sites); every value is `undefined`, so only the key set matters. This
//! table must be re-captured together with [`super::bytecode`].

/// Environment object keys, verbatim from the pinned build (144 entries).
pub const ENV_HASHES: [u32; 144] = [
    9472, 33069, 9084, 9085, 57851, 25490, 46091, 28291, 13145, 2878, 8555, 44083, 10887, 38236,
    7249, 61600, 61601, 21079, 65206, 28519, 57055, 18301, 29794, 30151, 14479, 39651, 14155,
    28841, 18059, 59124, 26231, 13572, 7712, 6080, 37657, 7209, 50231, 64739, 47332, 29853, 10603,
    33310, 49480, 22244, 64840, 53430, 48772, 23468, 45776, 41458, 24698, 35444, 29598, 40767,
    39109, 21105, 38039, 57910, 27319, 25094, 25095, 40753, 19740, 12481, 11193, 63331, 8136,
    18593, 65385, 59325, 20517, 47784, 21111, 21112, 3360, 40205, 12144, 39336, 36756, 3897, 22416,
    52312, 21932, 12602, 64563, 4482, 4483, 60981, 56984, 11461, 31421, 47071, 37700, 21413, 37485,
    14017, 2152, 8207, 49364, 64326, 63370, 36180, 36558, 8067, 55050, 53466, 20627, 64140, 54601,
    7552, 39085, 63748, 50674, 63769, 40586, 55676, 2580, 31582, 4568, 29827, 34347, 55828, 36058,
    28599, 50902, 1093, 14079, 29919, 16026, 53704, 13880, 52260, 58917, 36220, 11372, 14269,
    36711, 23013, 15463, 65015, 60357, 587, 61935, 2419,
];

/// Frozen `performance.now()` value the shim reports.
pub const PERF_NOW: f64 = 123456.789;

/// Player build the bytecode + environment table were captured from.
pub const PINNED_NOZZLE_VERSION: &str = "1J1Db0bF";

/// `__FAB`-synthesised V8 stack for the `Date.stack` TypeError probe.
/// The VM reads `Date.stack`, which is `undefined`, and converts the throw into
/// this exact V8-formatted stack before folding it into the digest.
pub fn nozzle_stack() -> String {
    format!(
        "TypeError: Cannot read properties of undefined (reading 'stack')\n    at https://hotaudio.net/nozzle.js?v={}:2:3472\n    at S (https://hotaudio.net/nozzle.js?v={}:1:37987)",
        PINNED_NOZZLE_VERSION, PINNED_NOZZLE_VERSION
    )
}
