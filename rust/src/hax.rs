//! HAX0 container: bencoded header, segment table, and the segment key tree.
//!
//! Layout (all integers little-endian):
//!
//! ```text
//! offset 0:  magic "HAX0"
//! offset 4:  fileLength   u32
//! offset 8:  headerLength u32
//! offset 12: extraLength  u32
//! offset 16: bencoded metadata dict, through headerLength
//! ```
//!
//! Every segment slice (from its offset to the next, or `fileLength`) is an
//! independent ChaCha20-Poly1305 ciphertext under a zero nonce, keyed by a
//! value derived from the key tree.

use std::collections::HashMap;

use sha2::{Digest, Sha256};

use crate::crypto::{self, Result};

/// One entry of the segment table.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    /// Byte offset of the segment's ciphertext within the container.
    pub offset: u32,
    /// Presentation timestamp.
    pub pts: u32,
}

/// A parsed HAX0 header.
#[derive(Clone, Debug)]
pub struct Hax0 {
    pub file_length: u32,
    pub header_length: u32,
    pub extra_length: u32,
    pub base_key: Vec<u8>,
    pub codec: String,
    pub duration_ms: u64,
    pub segment_count: u32,
    pub segments: Vec<Segment>,
}

/// Minimal bencode reader for the HAX0 metadata dictionary.
pub struct Bencode<'a> {
    buf: &'a [u8],
    pos: usize,
}

/// A decoded bencode value.
#[derive(Debug)]
pub enum BValue<'a> {
    Int(i64),
    Bytes(&'a [u8]),
    List(Vec<BValue<'a>>),
    Dict(HashMap<String, BValue<'a>>),
}

impl<'a> Bencode<'a> {
    /// Wrap a buffer, positioned at its first value.
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Decode one value, leaving the reader just past it.
    pub fn next_value(&mut self) -> Result<BValue<'a>> {
        let byte = *self
            .buf
            .get(self.pos)
            .ok_or(crypto::Error::Decode("truncated bencode"))?;
        match byte {
            b'i' => {
                let end = find(self.buf, self.pos + 1, b'e')?;
                let text = std::str::from_utf8(&self.buf[self.pos + 1..end])
                    .map_err(|_| crypto::Error::Decode("bencode integer"))?;
                let n: i64 = text
                    .parse()
                    .map_err(|_| crypto::Error::Decode("bencode integer"))?;
                self.pos = end + 1;
                Ok(BValue::Int(n))
            }
            b'l' => {
                self.pos += 1;
                let mut items = Vec::new();
                while self.buf.get(self.pos) != Some(&b'e') {
                    if self.pos >= self.buf.len() {
                        return Err(crypto::Error::Decode("truncated bencode list"));
                    }
                    items.push(self.next_value()?);
                }
                self.pos += 1;
                Ok(BValue::List(items))
            }
            b'd' => {
                self.pos += 1;
                let mut map = HashMap::new();
                while self.buf.get(self.pos) != Some(&b'e') {
                    if self.pos >= self.buf.len() {
                        return Err(crypto::Error::Decode("truncated bencode dict"));
                    }
                    let key = match self.next_value()? {
                        BValue::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
                        _ => return Err(crypto::Error::Decode("bencode dict key")),
                    };
                    let value = self.next_value()?;
                    map.insert(key, value);
                }
                self.pos += 1;
                Ok(BValue::Dict(map))
            }
            _ => {
                // Byte string: `<len>:<data>`.
                let colon = find(self.buf, self.pos, b':')?;
                let len: usize = std::str::from_utf8(&self.buf[self.pos..colon])
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .ok_or(crypto::Error::Decode("bencode length"))?;
                let start = colon + 1;
                let end = start
                    .checked_add(len)
                    .filter(|e| *e <= self.buf.len())
                    .ok_or(crypto::Error::Decode("bencode bytes"))?;
                self.pos = end;
                Ok(BValue::Bytes(&self.buf[start..end]))
            }
        }
    }
}

/// Index of `needle` at or after `from`, or an error.
fn find(buf: &[u8], from: usize, needle: u8) -> Result<usize> {
    buf[from..]
        .iter()
        .position(|b| *b == needle)
        .map(|i| i + from)
        .ok_or(crypto::Error::Decode("unterminated bencode token"))
}

impl Hax0 {
    /// Parse a HAX0 header from the start of a container.
    pub fn parse(buf: &[u8]) -> Result<Self> {
        if buf.len() < 16 || &buf[..4] != b"HAX0" {
            return Err(crypto::Error::Protocol("not a HAX0 container".into()));
        }
        let file_length = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        let header_length = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        let extra_length = u32::from_le_bytes(buf[12..16].try_into().unwrap());

        let header_end = (header_length as usize).min(buf.len());
        let meta_bytes = buf.get(16..header_end).unwrap_or(&[]);
        let meta = match Bencode::new(meta_bytes).next_value()? {
            BValue::Dict(d) => d,
            _ => return Err(crypto::Error::Protocol("HAX0 metadata is not a dict".into())),
        };

        let codec = match meta.get("codec") {
            Some(BValue::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
            _ => return Err(crypto::Error::Protocol("HAX0 metadata missing codec".into())),
        };
        let duration_ms = match meta.get("durationMs") {
            Some(BValue::Int(n)) => *n as u64,
            _ => return Err(crypto::Error::Protocol("HAX0 metadata missing durationMs".into())),
        };
        let segment_count = match meta.get("segmentCount") {
            Some(BValue::Int(n)) => *n as u32,
            _ => return Err(crypto::Error::Protocol("HAX0 metadata missing segmentCount".into())),
        };
        let base_key = match meta.get("baseKey") {
            Some(BValue::Bytes(b)) => b.to_vec(),
            _ => return Err(crypto::Error::Protocol("HAX0 metadata missing baseKey".into())),
        };
        let table = match meta.get("segments") {
            Some(BValue::Bytes(b)) => b,
            _ => return Err(crypto::Error::Protocol("HAX0 metadata missing segments".into())),
        };

        if (table.len() as u64) < segment_count as u64 * 8 {
            return Err(crypto::Error::Protocol("HAX0 segment table is short".into()));
        }
        let mut segments = Vec::with_capacity(segment_count as usize);
        for i in 0..segment_count as usize {
            let o = i * 8;
            segments.push(Segment {
                offset: u32::from_le_bytes(table[o..o + 4].try_into().unwrap()),
                pts: u32::from_le_bytes(table[o + 4..o + 8].try_into().unwrap()),
            });
        }

        Ok(Self {
            file_length,
            header_length,
            extra_length,
            base_key,
            codec,
            duration_ms,
            segment_count,
            segments,
        })
    }

    /// Byte range of segment `i`'s ciphertext, from its offset to the next
    /// segment's offset (or `file_length` for the last one).
    pub fn slice_range(&self, i: usize) -> (usize, usize) {
        let start = self.segments[i].offset as usize;
        let end = if i + 1 < self.segments.len() {
            self.segments[i + 1].offset as usize
        } else {
            self.file_length as usize
        };
        (start, end)
    }
}

/// Where a key-tree lookup got its seed.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyError {
    /// No ancestor of the requested leaf is present in the key map.
    Missing { seg_idx: u32 },
}

/// Tree geometry for a given segment count.
///
/// Keys form a complete binary tree above the leaves: with `n` segments the
/// leaf indices start at `tree_base`. Given a leaf, the path to the root is
/// read off the node index, and each step hashes `parent || branchByte` where
/// `branchByte` is the low byte of the node index at that level.
pub struct KeyTree {
    /// Lowest node index that is a leaf.
    pub tree_base: u32,
    /// Number of significant bits in a leaf index.
    pub depth: u32,
}

impl KeyTree {
    /// Build the tree geometry for `segment_count` segments.
    pub fn new(segment_count: u32) -> Self {
        let bit_len = bit_len(segment_count.saturating_sub(1));
        Self {
            tree_base: 1 + (1 << (bit_len + 1)),
            depth: bit_len + 1,
        }
    }

    /// Node index of leaf `seg_idx`.
    #[inline]
    pub fn leaf_node(&self, seg_idx: u32) -> u32 {
        self.tree_base + seg_idx
    }

    /// Node index at tree level `level` on the path to leaf `seg_idx`.
    ///
    /// `level` counts down from the root (`depth`) to the leaf.
    #[inline]
    pub fn node_at(&self, leaf: u32, level: u32) -> u32 {
        leaf >> (self.depth - level)
    }

    /// Derive the key for segment `seg_idx`.
    ///
    /// Starts at the nearest known ancestor in `keys` and hashes down. `cache`
    /// memoises intermediate node keys and must be cleared whenever new branch
    /// keys are merged into `keys`.
    pub fn derive(
        &self,
        keys: &crypto::KeyMap,
        seg_idx: u32,
        cache: &mut crypto::KeyMap,
    ) -> std::result::Result<[u8; 32], KeyError> {
        let leaf = self.leaf_node(seg_idx);

        // Find the deepest ancestor we actually have a key for.
        let mut start_level = None;
        let mut current = [0u8; 32];
        for level in 0..=self.depth {
            let node = self.node_at(leaf, level);
            if let Some(k) = keys.get(&node) {
                start_level = Some(level);
                current = *k;
                break;
            }
        }
        let mut level = match start_level {
            Some(l) => l,
            None => return Err(KeyError::Missing { seg_idx }),
        };

        while level < self.depth {
            level += 1;
            let node = self.node_at(leaf, level);
            if let Some(hit) = cache.get(&node) {
                current = *hit;
                continue;
            }
            // child = SHA-256(parent || low byte of this level's node index)
            let mut hasher = Sha256::new();
            hasher.update(current);
            hasher.update([(node & 0xff) as u8]);
            let derived: [u8; 32] = hasher.finalize().into();
            cache.insert(node, derived);
            current = derived;
        }
        Ok(current)
    }
}

/// Number of significant bits in `n` (`0` for `0`).
#[inline]
fn bit_len(n: u32) -> u32 {
    32 - n.leading_zeros()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_lengths() {
        assert_eq!(bit_len(0), 0);
        assert_eq!(bit_len(1), 1);
        assert_eq!(bit_len(255), 8);
        assert_eq!(bit_len(256), 9);
    }

    #[test]
    fn tree_geometry_matches_reference() {
        // 10 segments -> leaf nodes 33..42, chain 1 -> 2 -> 4 -> ... -> leaf.
        let t = KeyTree::new(10);
        assert_eq!(t.tree_base, 33);
        assert_eq!(t.leaf_node(0), 33);
        assert_eq!(t.leaf_node(9), 42);
    }

    #[test]
    fn derives_deterministically_and_matches_manual_chain() {
        let root = [3u8; 32];
        let keys = HashMap::from([(1u32, root)]);
        let tree = KeyTree::new(10);
        let mut cache = HashMap::new();
        let k0 = tree.derive(&keys, 0, &mut cache).unwrap();

        // Manually walk root -> 2 -> 4 -> 8 -> 16 -> 33.
        let step = |p: [u8; 32], b: u8| -> [u8; 32] {
            let mut h = Sha256::new();
            h.update(p);
            h.update([b]);
            h.finalize().into()
        };
        let mut expect = root;
        for b in [2u8, 4, 8, 16, 33] {
            expect = step(expect, b);
        }
        assert_eq!(k0, expect);
    }

    #[test]
    fn missing_root_is_reported() {
        let tree = KeyTree::new(10);
        let mut cache = HashMap::new();
        let err = tree.derive(&HashMap::new(), 0, &mut cache).unwrap_err();
        assert!(matches!(err, KeyError::Missing { seg_idx: 0 }));
    }
}