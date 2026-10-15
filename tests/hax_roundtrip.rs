//! Offline protocol round-trip against a real HAX0 container.
//!
//! Builds a synthetic container the way the server does — bencoded metadata,
//! a segment table, each slice an independent ChaCha20-Poly1305 ciphertext
//! under a zero nonce with a key taken from the tree — then parses it,
//! derives the same keys, and decrypts it back.

use std::collections::HashMap;

use hotaudio::crypto::{self, open_zero_into, seal_zero};
use hotaudio::download::decrypt_offline;
use hotaudio::hax::{BValue, Bencode, Hax0, KeyTree};

/// bencode a byte string: `<len>:<bytes>`.
fn bstr(bytes: &[u8]) -> Vec<u8> {
    let mut out = format!("{}:", bytes.len()).into_bytes();
    out.extend_from_slice(bytes);
    out
}

/// bencode an integer: `i<n>e`.
fn bint(n: u64) -> Vec<u8> {
    format!("i{n}e").into_bytes()
}

/// Build a valid HAX0 container whose segments encrypt `plaintexts` under keys
/// derived from `root`, plus an optional extra metadata region, and return the
/// container bytes.
fn build_container(
    plaintexts: &[Vec<u8>],
    codec: &str,
    duration_ms: u64,
    extra: Option<&[u8]>,
) -> Vec<u8> {
    let root: [u8; 32] = [7u8; 32];
    let segment_count = plaintexts.len() as u32;
    let tree = KeyTree::new(segment_count);
    let keys = HashMap::from([(1u32, root)]);
    let mut cache = HashMap::new();

    // Encrypt each slice.
    let mut ciphers = Vec::with_capacity(plaintexts.len());
    for i in 0..segment_count {
        let key = tree.derive(&keys, i, &mut cache).expect("root present");
        ciphers.push(seal_zero(&key, &plaintexts[i as usize]).expect("seal"));
    }

    // The extra blob is keyed at the node below the first leaf ("segment" -1).
    let extra_cipher = match extra {
        Some(plain) => {
            let key = tree
                .derive_node(&keys, tree.extra_node(), &mut cache)
                .expect("root present");
            seal_zero(&key, plain).expect("seal extra")
        }
        None => Vec::new(),
    };

    // Segment table: offset/pts pairs, filled once offsets are known.
    let mut table = vec![0u8; segment_count as usize * 8];

    // Bencode the metadata.
    let mut meta = vec![b'd'];
    meta.extend(bstr(b"baseKey"));
    meta.extend(bstr(&[0u8; 32]));
    meta.extend(bstr(b"codec"));
    meta.extend(bstr(codec.as_bytes()));
    meta.extend(bstr(b"durationMs"));
    meta.extend(bint(duration_ms));
    meta.extend(bstr(b"segmentCount"));
    meta.extend(bint(segment_count as u64));
    meta.extend(bstr(b"segments"));
    // Table length is fixed, so it can be encoded before offsets are filled.
    meta.extend(bstr(&table));
    meta.push(b'e');

    let header_length = 16 + meta.len() as u32;
    let mut cursor = header_length + extra_cipher.len() as u32;
    for (i, c) in ciphers.iter().enumerate() {
        table[i * 8..i * 8 + 4].copy_from_slice(&cursor.to_le_bytes());
        table[(i * 8 + 4)..(i * 8 + 8)].copy_from_slice(&((i as u32) * 1000).to_le_bytes());
        cursor += c.len() as u32;
    }
    // Re-encode the metadata now that offsets are final (same length).
    let mut meta = vec![b'd'];
    meta.extend(bstr(b"baseKey"));
    meta.extend(bstr(&[0u8; 32]));
    meta.extend(bstr(b"codec"));
    meta.extend(bstr(codec.as_bytes()));
    meta.extend(bstr(b"durationMs"));
    meta.extend(bint(duration_ms));
    meta.extend(bstr(b"segmentCount"));
    meta.extend(bint(segment_count as u64));
    meta.extend(bstr(b"segments"));
    meta.extend(bstr(&table));
    meta.push(b'e');
    assert_eq!(meta.len() as u32, header_length - 16);

    let mut out = Vec::with_capacity(cursor as usize);
    out.extend(b"HAX0");
    out.extend(cursor.to_le_bytes());
    out.extend(header_length.to_le_bytes());
    out.extend((extra_cipher.len() as u32).to_le_bytes());
    out.extend(&meta);
    out.extend(&extra_cipher);
    for c in &ciphers {
        out.extend(c);
    }
    assert_eq!(out.len() as u32, cursor);
    out
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn round_trips_a_synthetic_container() {
    let plaintexts: Vec<Vec<u8>> = (0..17)
        .map(|i| {
            if i == 0 {
                let mut p = vec![0, 0, 0, 0x20];
                p.extend(b"ftypM4A ");
                p.extend(vec![0xab; 20]);
                p
            } else {
                let mut p = vec![0xcd; 64];
                let marker = format!("segment-{i}");
                p[..marker.len()].copy_from_slice(marker.as_bytes());
                p
            }
        })
        .collect();

    let container = build_container(&plaintexts, "mp4a.40.2", 90_000, None);

    // Header parses with the expected metadata.
    let hax = Hax0::parse(&container).expect("parse");
    assert_eq!(hax.segment_count, 17);
    assert_eq!(hax.codec, "mp4a.40.2");
    assert_eq!(hax.duration_ms, 90_000);
    assert_eq!(hax.base_key.len(), 32);
    assert_eq!(hax.extra_length, 0);
    assert_eq!(hax.segments.len(), 17);
    assert_eq!(hax.segments[0].pts, 0);
    assert_eq!(hax.segments[3].pts, 3000);

    // Decrypt with the root key and confirm we recover the plaintexts.
    let keys = HashMap::from([(1u32, [7u8; 32])]);
    let (audio, count, duration) = decrypt_offline(&container, &keys, None).expect("decrypt");
    assert_eq!(count, 17);
    assert!((duration - 90.0).abs() < 1e-9);

    let expected: Vec<u8> = plaintexts.concat();
    assert_eq!(audio, expected);
    assert_eq!(&audio[..4], &[0, 0, 0, 0x20]);
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn surfaces_and_keys_the_extra_region() {
    let plaintexts: Vec<Vec<u8>> = (0..4).map(|i| vec![i as u8; 40]).collect();
    // A tiny, valid bencode dict, the shape the player decodes into `ext`.
    let extra_plain = b"d1:ai42ee".to_vec();
    let container = build_container(&plaintexts, "mp4a.40.2", 1000, Some(&extra_plain));

    let hax = Hax0::parse(&container).expect("parse");
    // The extra region is the ciphertext between the header and the first segment.
    assert_eq!(hax.extra_length as usize, extra_plain.len() + 16);
    assert_eq!(hax.segments[0].offset, hax.header_length + hax.extra_length);
    assert_eq!(hax.extra.len(), extra_plain.len() + 16);
    assert_ne!(
        hax.extra, extra_plain,
        "the extra region must stay encrypted"
    );

    // It decrypts under the tree's "segment -1" node, and the segments still
    // decrypt unaffected.
    let keys = HashMap::from([(1u32, [7u8; 32])]);
    let tree = KeyTree::new(hax.segment_count);
    let mut cache = HashMap::new();
    let key = tree
        .derive_node(&keys, tree.extra_node(), &mut cache)
        .expect("root present");
    let decrypted = crypto::open_zero(&key, &hax.extra).expect("decrypt extra");
    assert_eq!(decrypted, extra_plain);

    let (audio, _, _) = decrypt_offline(&container, &keys, None).expect("decrypt");
    assert_eq!(audio, plaintexts.concat());
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn rejects_bad_magic() {
    let mut container = build_container(&[vec![1, 2, 3]], "mp4a.40.2", 1000, None);
    container[..4].copy_from_slice(b"XXXX");
    let err = Hax0::parse(&container).unwrap_err();
    assert!(err.to_string().contains("HAX0"), "got {err}");
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn wrong_keys_fail_closed() {
    let plaintexts: Vec<Vec<u8>> = (0..4).map(|i| vec![i as u8; 32]).collect();
    let container = build_container(&plaintexts, "mp4a.40.2", 1000, None);

    // A wrong root must fail authentication, not silently produce garbage.
    let wrong = HashMap::from([(1u32, [9u8; 32])]);
    assert!(decrypt_offline(&container, &wrong, None).is_err());

    // Missing keys report a protocol error naming the segment.
    let err = decrypt_offline(&container, &HashMap::new(), None).unwrap_err();
    assert!(err.to_string().contains("segment 0"), "got {err}");
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn detects_a_corrupted_slice() {
    let plaintexts: Vec<Vec<u8>> = (0..4).map(|i| vec![i as u8; 48]).collect();
    let mut container = build_container(&plaintexts, "mp4a.40.2", 1000, None);
    // Flip a byte inside the first segment's ciphertext.
    let last = container.len() - 1;
    container[last] ^= 0xff;
    let keys = HashMap::from([(1u32, [7u8; 32])]);
    assert!(decrypt_offline(&container, &keys, None).is_err());
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn in_place_decrypt_matches_one_shot() {
    let key = [3u8; 32];
    let plain = b"the quick brown fox jumps over the lazy dog".to_vec();
    let ct = seal_zero(&key, &plain).unwrap();

    let one_shot = crypto::open_zero(&key, &ct).unwrap();
    let mut appended = b"prefix".to_vec();
    open_zero_into(&key, &ct, &mut appended).unwrap();

    assert_eq!(one_shot, plain);
    assert_eq!(&appended[..6], b"prefix");
    assert_eq!(&appended[6..], &plain[..]);
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn bencode_round_trips() {
    let encoded = b"i42e";
    match Bencode::new(encoded).next_value().unwrap() {
        BValue::Int(n) => assert_eq!(n, 42),
        other => panic!("expected int, got {other:?}"),
    }

    let encoded = b"3:abc";
    match Bencode::new(encoded).next_value().unwrap() {
        BValue::Bytes(b) => assert_eq!(b, b"abc"),
        other => panic!("expected bytes, got {other:?}"),
    }

    let encoded = b"d1:ai1ee";
    match Bencode::new(encoded).next_value().unwrap() {
        BValue::Dict(d) => {
            assert!(matches!(d.get("a"), Some(BValue::Int(1))));
        }
        other => panic!("expected dict, got {other:?}"),
    }

    assert!(Bencode::new(b"l").next_value().is_err());
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn reports_progress() {
    let plaintexts: Vec<Vec<u8>> = (0..32).map(|_| vec![1u8; 16]).collect();
    let container = build_container(&plaintexts, "mp4a.40.2", 1000, None);
    let keys = HashMap::from([(1u32, [7u8; 32])]);

    let mut seen: Vec<(u64, u64)> = Vec::new();
    {
        let mut cb = |p: hotaudio::download::Progress| seen.push((p.loaded, p.total));
        decrypt_offline(&container, &keys, Some(&mut cb)).unwrap();
    }
    assert!(!seen.is_empty(), "no progress reported");
    assert_eq!(seen.last().unwrap(), &(32, 32), "must finish at 32/32");
}
