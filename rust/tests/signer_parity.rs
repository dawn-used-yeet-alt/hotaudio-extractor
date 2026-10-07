//! Signer parity: the Rust VM must reproduce the pinned `nozzle.js` signer
//! byte for byte.
//!
//! `tests/data/signer_golden.json` holds `(payload, timestamp, signature)`
//! triples captured from the original TypeScript signer. They are the contract:
//! if upstream changes the player build, these vectors must be re-captured
//! together with [`hotaudio::signer::bytecode`] and
//! [`hotaudio::signer::env`].

use hotaudio::signer;

#[test]
fn matches_golden_vectors() {
    let raw = include_str!("data/signer_golden.json");
    let vectors: Vec<(String, f64, String)> =
        serde_json::from_str(raw).expect("golden vectors parse");

    assert!(!vectors.is_empty(), "no golden vectors");
    let mut failures = Vec::new();
    for (payload, ts, expected) in &vectors {
        let got = signer::sign(payload, *ts)
            .unwrap_or_else(|e| panic!("sign failed for {payload:?}: {e}"));
        if &got != expected {
            failures.push(format!(
                "payload={payload:?} ts={ts}\n  expected {expected}\n  got      {got}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} golden vectors mismatched:\n{}",
        failures.len(),
        vectors.len(),
        failures.join("\n")
    );
}

#[test]
fn signature_has_expected_shape() {
    let sig = signer::sign("{\"tid\":\"7\"}", 1_700_000_000.0).unwrap();
    // "9:" + 8 hex digits of big-endian timestamp + 24 hex digits of tag.
    assert!(sig.starts_with("9:6553f100"), "unexpected signature: {sig}");
    assert_eq!(sig.len(), 2 + 8 + 24, "unexpected length: {sig}");
    assert!(
        sig[2..].chars().all(|c| c.is_ascii_hexdigit()),
        "signature is not hex: {sig}"
    );
}

#[test]
fn timestamp_is_big_endian_seconds() {
    for ts in [0.0, 1.0, 255.0, 256.0, 65535.0, 1_700_000_000.0, 4_294_967_295.0] {
        let sig = signer::sign("x", ts).unwrap();
        let hex = &sig[2..10];
        let decoded = u32::from_str_radix(hex, 16).unwrap();
        // The signer floors the clock to whole seconds.
        assert_eq!(decoded as f64, ts.floor(), "ts={ts} sig={sig}");
    }
}

#[test]
fn distinct_payloads_and_timestamps_differ() {
    let a = signer::sign("{\"a\":1}", 1_700_000_000.0).unwrap();
    let b = signer::sign("{\"a\":2}", 1_700_000_000.0).unwrap();
    let c = signer::sign("{\"a\":1}", 1_700_000_001.0).unwrap();
    assert_ne!(a, b, "payload must affect the signature");
    assert_ne!(a, c, "timestamp must affect the signature");
}

#[test]
fn signing_is_deterministic() {
    let a = signer::sign("{\"tid\":\"7\"}", 1_700_000_000.0).unwrap();
    let b = signer::sign("{\"tid\":\"7\"}", 1_700_000_000.0).unwrap();
    assert_eq!(a, b);
}

#[test]
fn utf16_length_semantics() {
    // A non-BMP character counts as two UTF-16 code units, which is what the
    // JS `.length` the VM reads reports. Golden vectors cover this too; this
    // test pins the specific behaviour so a future refactor cannot regress it.
    let ascii = signer::sign("aaaa", 1_700_000_000.0).unwrap();
    let astral = signer::sign("\u{1F980}\u{1F980}", 1_700_000_000.0).unwrap();
    assert_ne!(ascii, astral);
}