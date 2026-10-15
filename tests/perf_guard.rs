//! Guards on the CPU-bound half of the crate.
//!
//! The `wasm` transport work made the pipeline `async` at the I/O boundary. That
//! is free at runtime *provided* the decrypt path stays synchronous and
//! allocation-free per segment, which is where the crate's performance claims
//! come from. Nothing in the type system prevents a future change from pushing a
//! state machine, a copy or an extra allocation into that loop, and it would show
//! up only as "somehow slower now".
//!
//! Two tests, because they answer different questions and have different
//! preconditions:
//!
//! - `decrypts_every_segment_in_place` is a correctness check. It is valid in
//!   every build profile and on every target, so it always runs — including on
//!   `wasm32`, which makes it evidence that the assembly loop is genuinely the
//!   same code on both targets rather than merely compiled for both.
//! - `bulk_decrypt_stays_fast` is a throughput check, and only exists in
//!   optimised builds. The same work takes ~0.5 s with `--release` and ~30 s
//!   unoptimised, so no single floor serves both profiles. `#[cfg]` rather than
//!   `#[ignore]`: a debug build is not a case where this test "should be
//!   skipped by hand", it is a build where the assertion does not apply.

use std::collections::HashMap;
#[cfg(not(debug_assertions))]
use std::time::Instant;

use hotaudio::crypto::KeyMap;
use hotaudio::download::decrypt_offline;
use hotaudio::hax::KeyTree;

/// Floor in MB/s, measured at ~100 MB/s on a 14.7 MB container. Deliberately
/// ~5x below that, so it survives slower CI hardware while still failing on an
/// order-of-magnitude regression. Raise it only with a measurement behind it.
#[cfg(not(debug_assertions))]
const MIN_MB_PER_SEC: f64 = 20.0;

#[cfg(not(debug_assertions))]
const SEGMENTS: u32 = 300;
#[cfg(not(debug_assertions))]
const SEGMENT_BYTES: usize = 48 * 1024;

/// Small enough to run in every profile; large enough that a per-segment copy or
/// an interleaved reorder is visible.
const CORRECTNESS_SEGMENTS: u32 = 24;
const CORRECTNESS_SEGMENT_BYTES: usize = 4096;

/// Segments decrypt in order, in place, into one contiguous output.
///
/// A mis-sliced segment, a wrong key or an out-of-order assembly all fail here
/// regardless of speed.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn decrypts_every_segment_in_place() {
    let tree = KeyTree::new(CORRECTNESS_SEGMENTS);
    let keys = leaf_keys(&tree, CORRECTNESS_SEGMENTS);
    let container = build_container(
        &tree,
        &keys,
        CORRECTNESS_SEGMENTS,
        CORRECTNESS_SEGMENT_BYTES,
    );

    let (audio, count, duration) = decrypt_offline(&container, &keys, None).expect("decrypt");
    assert_eq!(count, CORRECTNESS_SEGMENTS);
    assert_eq!(duration, 1.0);
    assert_eq!(
        audio.len(),
        CORRECTNESS_SEGMENTS as usize * CORRECTNESS_SEGMENT_BYTES
    );

    // Each segment's first byte is its own index, so a slice that is off by one
    // or a segment appended out of order shows up as a mismatch.
    for (i, chunk) in audio.chunks_exact(CORRECTNESS_SEGMENT_BYTES).enumerate() {
        assert_eq!(
            chunk[0],
            (i % 251) as u8,
            "segment {i} did not decrypt in place"
        );
    }
}

/// Optimised builds only: a structural regression on the decrypt path costs
/// throughput, not correctness.
///
/// ```bash
/// cargo test --release --test perf_guard
/// ```
#[cfg(not(debug_assertions))]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn bulk_decrypt_stays_fast() {
    let tree = KeyTree::new(SEGMENTS);
    let keys = leaf_keys(&tree, SEGMENTS);
    let container = build_container(&tree, &keys, SEGMENTS, SEGMENT_BYTES);

    // Warm the allocator and the key cache so the timed pass is not paying for
    // the first-touch cost of a 14.7 MB buffer.
    let _ = decrypt_offline(&container, &keys, None).expect("decrypt");

    let started = Instant::now();
    let (audio, _, _) = decrypt_offline(&container, &keys, None).expect("decrypt");
    let elapsed = started.elapsed();

    assert_eq!(audio.len(), SEGMENTS as usize * SEGMENT_BYTES);
    let mb_per_sec = audio.len() as f64 / 1e6 / elapsed.as_secs_f64();
    assert!(
        mb_per_sec >= MIN_MB_PER_SEC,
        "decrypt throughput regressed to {mb_per_sec:.1} MB/s (floor {MIN_MB_PER_SEC} MB/s): \
         something is now on the per-segment path"
    );
}

/// One key per leaf.
///
/// The server issues keys at some tree level and the rest are derived; a
/// leaf-key map exercises the same derive and decrypt work while keeping these
/// tests to the property under test.
fn leaf_keys(tree: &KeyTree, segments: u32) -> KeyMap {
    (0..segments)
        .map(|i| (tree.leaf_node(i), [i as u8 | 1; 32]))
        .collect()
}

/// A HAX0 container whose segments are encrypted, with the header and segment
/// table laid out exactly as `hax.rs` expects.
fn build_container(tree: &KeyTree, keys: &KeyMap, segments: u32, segment_bytes: usize) -> Vec<u8> {
    let mut cache: HashMap<u32, [u8; 32]> = HashMap::new();
    let ciphertexts: Vec<Vec<u8>> = (0..segments)
        .map(|i| {
            let key = tree.derive(keys, i, &mut cache).expect("derive");
            let mut plain: Vec<u8> = (0..segment_bytes)
                .map(|b| {
                    if b == 0 {
                        (i % 251) as u8
                    } else {
                        (b % 251) as u8
                    }
                })
                .collect();
            if i == 0 {
                // A valid MP4 head, so `decrypt_offline`'s `ftyp` check passes.
                // `plain[0]` stays 0, so the ordering assertion still holds.
                plain[..8].copy_from_slice(b"\0\0\0\x20ftyp");
            }
            hotaudio::crypto::seal_zero(&key, &plain).expect("seal")
        })
        .collect();

    let table_len = segments as usize * 8;
    let mut table = vec![0u8; table_len];
    let mut cursor = 16 + bencode(&table, segments).len() as u32;
    for (i, ct) in ciphertexts.iter().enumerate() {
        let start = cursor;
        table[i * 8..i * 8 + 4].copy_from_slice(&start.to_le_bytes());
        table[i * 8 + 4..i * 8 + 8].copy_from_slice(&(ct.len() as u32).to_le_bytes());
        cursor += ct.len() as u32;
    }

    let meta = bencode(&table, segments);
    let header_length = 16 + meta.len() as u32;
    let mut out = Vec::with_capacity(cursor as usize);
    out.extend(b"HAX0");
    out.extend(cursor.to_le_bytes());
    out.extend(header_length.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(meta);
    for ct in &ciphertexts {
        out.extend(ct);
    }
    out
}

/// The bencoded `meta` dict, matching the field set `hax.rs` requires.
fn bencode(table: &[u8], segments: u32) -> Vec<u8> {
    let mut meta = b"d".to_vec();
    meta.extend(bstr(b"baseKey"));
    meta.extend(bstr(&[0u8; 32]));
    meta.extend(bstr(b"codec"));
    meta.extend(bstr(b"mp4a.40.2"));
    meta.extend(bstr(b"durationMs"));
    meta.extend(b"i1000e");
    meta.extend(bstr(b"segmentCount"));
    meta.extend(format!("i{segments}e").into_bytes());
    meta.extend(bstr(b"segments"));
    meta.extend(bstr(table));
    meta.push(b'e');
    meta
}

fn bstr(bytes: &[u8]) -> Vec<u8> {
    let mut out = format!("{}:", bytes.len()).into_bytes();
    out.extend_from_slice(bytes);
    out
}
