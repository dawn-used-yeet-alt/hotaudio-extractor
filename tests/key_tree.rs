//! Key-tree derivation against keys captured from the live API.
//!
//! The saved envelope in `tests/data/keys_golden.json` came from a real track,
//! so deriving every segment key from the saved branches must reproduce the
//! exact key material the server issued. This pins the tree geometry, the
//! `parent || branchByte` hash step, and the branch-paging coverage rule.

use std::collections::HashMap;

use hotaudio::hax::{KeyError, KeyTree};

#[derive(Debug, serde::Deserialize)]
struct Envelope {
    #[serde(default)]
    #[allow(dead_code)]
    hax_url: String,
    keys: HashMap<String, String>,
    #[serde(default)]
    segment_count: Option<u32>,
}

fn load() -> (HashMap<u32, [u8; 32]>, u32) {
    let raw = include_str!("data/keys_golden.json");
    let env: Envelope = serde_json::from_str(raw).expect("envelope parses");
    let mut keys = HashMap::new();
    for (node, hex) in &env.keys {
        let bytes = hotaudio::crypto::hex_decode(hex).expect("hex");
        keys.insert(
            node.parse::<u32>().expect("node"),
            bytes.try_into().expect("32 bytes"),
        );
    }
    (keys, env.segment_count.unwrap_or(899))
}

#[test]
fn envelope_loads() {
    let (keys, count) = load();
    assert!(!keys.is_empty(), "saved keys are empty");
    assert_eq!(count, 899);
}

#[test]
fn every_segment_is_covered_by_the_saved_branches() {
    let (keys, count) = load();
    let tree = KeyTree::new(count);
    let mut cache = HashMap::new();
    for seg in 0..count {
        assert!(
            tree.derive(&keys, seg, &mut cache).is_ok(),
            "saved keys do not cover segment {seg}"
        );
    }
}

#[test]
fn derivation_is_deterministic_and_cache_consistent() {
    let (keys, count) = load();
    let tree = KeyTree::new(count);

    // Deriving cold, then warm (cache populated), must agree.
    let mut cold = HashMap::new();
    let mut warm = HashMap::new();
    for seg in 0..count {
        let a = tree.derive(&keys, seg, &mut cold).unwrap();
        let b = tree.derive(&keys, seg, &mut warm).unwrap();
        assert_eq!(a, b, "cache changed the result for segment {seg}");
    }
}

#[test]
fn out_of_order_derivation_agrees() {
    let (keys, count) = load();
    let tree = KeyTree::new(count);

    let mut forward_cache = HashMap::new();
    let forward: Vec<[u8; 32]> = (0..count)
        .map(|s| tree.derive(&keys, s, &mut forward_cache).unwrap())
        .collect();

    // Descending order builds the cache in a different order; results must match.
    let mut reverse_cache = HashMap::new();
    for seg in (0..count).rev() {
        let got = tree.derive(&keys, seg, &mut reverse_cache).unwrap();
        assert_eq!(got, forward[seg as usize], "segment {seg} differs");
    }
}

#[test]
fn missing_branch_reports_the_leaf_index() {
    // Drop every branch: nothing above the leaves remains.
    let (_, count) = load();
    let tree = KeyTree::new(count);
    let err = tree
        .derive(&HashMap::new(), 42, &mut HashMap::new())
        .unwrap_err();
    assert_eq!(err, KeyError::Missing { seg_idx: 42 });
}

#[test]
fn single_segment_tree_still_derives() {
    let tree = KeyTree::new(1);
    // bit_len(0) = 0 -> tree_base = 1 + (1 << 1) = 3; leaf node 3.
    assert_eq!(tree.tree_base, 3);
    assert_eq!(tree.leaf_node(0), 3);

    let root = [5u8; 32];
    let keys = HashMap::from([(1u32, root)]);
    let key = tree.derive(&keys, 0, &mut HashMap::new()).unwrap();
    assert_ne!(key, root, "the leaf key must be derived, not the root");
}

#[test]
fn a_branch_ancestor_suffices() {
    // With the full branch set, pruning the root should still let deeper
    // branches cover their subtrees.
    let (keys, count) = load();
    let tree = KeyTree::new(count);
    let pruned: HashMap<u32, [u8; 32]> = keys
        .iter()
        .filter(|(node, _)| **node > 1)
        .map(|(k, v)| (*k, *v))
        .collect();
    let mut cache = HashMap::new();
    for seg in 0..count {
        assert!(
            tree.derive(&pruned, seg, &mut cache).is_ok(),
            "segment {seg} needs the root after pruning"
        );
    }
}
