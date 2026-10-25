//! Progressive, ranged container reading.
//!
//! The full-container download buffers the whole `.hax` in memory before any
//! audio is produced. That is fine for a 15 MB track, but it means a large
//! track costs its entire download before the first byte of audio exists, and
//! peak memory is the container size.
//!
//! This module reads the container incrementally:
//!
//! 1. `Range: bytes=0-15` to learn `headerLength` (16 bytes).
//! 2. `Range: bytes=0-<headerLength-1>` to parse the segment table.
//! 3. `Range` per segment, decrypting and writing each as it arrives.
//!
//! That is what the browser player does for progressive MSE playback, and it
//! is what makes `--stream-to` able to emit the first playable fragment after
//! one round trip instead of after the whole transfer.

use std::io::Write;

use crate::crypto::{self, Error, Result};
use crate::download::{KeyPager, derive_or_page};
use crate::hax::{Hax0, KeyTree};
use crate::http::Client;

/// How to fetch byte ranges.
///
/// `async` because a `RangeSource` may be backed by the browser's `fetch`,
/// which cannot be blocked on. The trait is deliberately not object-safe, so
/// take it by `impl` rather than `&dyn`.
///
/// `Send` is not required and is not offered: the browser transport holds JS
/// values, which are not `Send`, and requiring it would rule out both a future
/// that must not outlive its thread and the browser build itself.
#[allow(async_fn_in_trait)]
pub trait RangeSource {
    /// Fetch `start..=end`. Implementations must honour the range, or return
    /// the whole body with `from_slice` applied by the caller.
    async fn range(&self, start: u64, end: u64) -> Result<Vec<u8>>;
}

/// [`RangeSource`] over HTTP.
pub struct HttpRangeSource<'a> {
    client: &'a Client,
    url: String,
    /// Body length, filled in by the first range read.
    pub file_length: u64,
}

impl<'a> HttpRangeSource<'a> {
    #[must_use]
    pub fn new(client: &'a Client, url: &str) -> Self {
        Self {
            client,
            url: url.to_string(),
            file_length: 0,
        }
    }

    /// Record the body length once known, so callers can report progress
    /// against a total.
    pub fn set_file_length(&mut self, len: u64) {
        self.file_length = len;
    }
}

impl RangeSource for HttpRangeSource<'_> {
    async fn range(&self, start: u64, end: u64) -> Result<Vec<u8>> {
        let res = self
            .client
            .request(
                "GET",
                &self.url,
                &[
                    ("User-Agent", crate::HOTAUDIO_UA),
                    ("Range", &format!("bytes={start}-{end}")),
                ],
                None,
            )
            .await?;
        if let Some(detail) = res.challenge() {
            return Err(Error::Challenge(format!("range fetch: {detail}")));
        }
        match res.status {
            // Honour the range.
            206 => Ok(res.body),
            // Mirror ignored the range and sent everything: slice locally.
            200 => {
                let s = start as usize;
                let e = ((end + 1) as usize).min(res.body.len());
                if s >= res.body.len() {
                    return Err(Error::Protocol("range start beyond body".into()));
                }
                Ok(res.body[s..e].to_vec())
            }
            other => Err(Error::Http(format!("range fetch returned {other}"))),
        }
    }
}

/// Read the 16-byte container prefix and return `(file_length, header_length)`.
///
/// Validates the magic and that `header_length` is a plausible fraction of
/// `file_length`, so a truncated or wrong-object response fails loudly instead
/// of producing a bogus segment table.
///
/// # Errors
///
/// Returns [`Error::Protocol`] for a short prefix, wrong magic, or an
/// implausible header length, or a transport error from `source`.
///
/// # Panics
///
/// If the 16-byte prefix contains a field that is not four bytes; the length
/// check above makes that impossible.
pub async fn probe_header(source: &impl RangeSource) -> Result<(u64, u32)> {
    let head = source.range(0, 15).await?;
    if head.len() < 16 {
        return Err(Error::Protocol("short .hax prefix".into()));
    }
    if &head[..4] != b"HAX0" {
        return Err(Error::Protocol("not a HAX0 container".into()));
    }
    let file_length = u64::from(u32::from_le_bytes(head[4..8].try_into().unwrap()));
    let header_length = u32::from_le_bytes(head[8..12].try_into().unwrap());
    if header_length < 16 || u64::from(header_length) > file_length {
        return Err(Error::Protocol(format!(
            "implausible HAX0 headerLength {header_length} for fileLength {file_length}"
        )));
    }
    Ok((file_length, header_length))
}

/// Fetch and parse just the container header.
///
/// # Errors
///
/// Propagates [`probe_header`]'s errors and [`Hax0::parse`]'s, or a transport
/// error from `source`.
pub async fn read_header(source: &impl RangeSource) -> Result<Hax0> {
    let (_, header_length) = probe_header(source).await?;
    let header = source.range(0, u64::from(header_length - 1)).await?;
    Hax0::parse(&header)
}

/// Decrypt segments one at a time from `source`, paging key branches on demand.
///
/// `on_segment` is called with each decrypted fragment's bytes; returning an
/// error aborts. `initial_keys` seeds the key map (resume / offline reuse) and
/// `pager` supplies further branches when a segment is not covered yet, through
/// the same [`derive_or_page`] policy the buffered path uses.
///
/// Returns the merged key map (paged branches included, for `--save-keys`) and
/// the number of page requests made.
///
/// # Errors
///
/// Propagates key paging, range-read, decryption and `on_segment` errors; also
/// returns [`Error::Protocol`] if a range read is shorter than its segment.
pub async fn decrypt_streaming(
    source: &impl RangeSource,
    hax: &Hax0,
    initial_keys: &crypto::KeyMap,
    pager: &mut impl KeyPager,
    mut on_segment: impl FnMut(usize, &[u8]) -> Result<()>,
) -> Result<(crypto::KeyMap, u32)> {
    let tree = KeyTree::new(hax.segment_count.max(1));
    let mut keys = initial_keys.clone();
    let mut cache = crypto::KeyMap::new();
    let mut page_count = 0u32;

    for i in 0..hax.segment_count as usize {
        let key = derive_or_page(
            pager,
            &tree,
            &mut keys,
            &mut cache,
            i as u32,
            &mut page_count,
        )
        .await?;

        let (start, end) = hax.slice_range(i);
        let slice = source.range(start as u64, (end - 1) as u64).await?;
        if slice.len() != end - start {
            return Err(Error::Protocol(format!(
                "segment {i}: expected {} bytes, got {}",
                end - start,
                slice.len()
            )));
        }
        let plain = crypto::open_zero(&key, &slice)?;
        on_segment(i, &plain)?;
    }
    Ok((keys, page_count))
}

/// Write decrypted audio straight to a file as segments arrive, avoiding the
/// full-container buffer.
///
/// # Errors
///
/// Propagates [`decrypt_streaming`]'s errors, a write error as [`Error::Io`],
/// and any error returned by `on_progress`.
pub async fn write_to_file(
    source: &impl RangeSource,
    hax: &Hax0,
    initial_keys: &crypto::KeyMap,
    pager: &mut impl KeyPager,
    out: &mut impl Write,
    mut on_progress: impl FnMut(u64, u64) -> Result<()>,
) -> Result<u64> {
    let mut written = 0u64;
    decrypt_streaming(source, hax, initial_keys, pager, |i, plain| {
        out.write_all(plain).map_err(|e| Error::Io(e.to_string()))?;
        written += plain.len() as u64;
        on_progress((i + 1) as u64, u64::from(hax.segment_count))
    })
    .await?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn bstr(bytes: &[u8]) -> Vec<u8> {
        let mut out = format!("{}:", bytes.len()).into_bytes();
        out.extend_from_slice(bytes);
        out
    }

    /// Bencode the HAX0 metadata dict with a (possibly empty) segment table.
    fn meta_with_table(n: u32, table: &[u8]) -> Vec<u8> {
        let mut meta = b"d".to_vec();
        meta.extend(bstr(b"baseKey"));
        meta.extend(bstr(&[0u8; 32]));
        meta.extend(bstr(b"codec"));
        meta.extend(bstr(b"mp4a.40.2"));
        meta.extend(bstr(b"durationMs"));
        meta.extend(b"i1000e");
        meta.extend(bstr(b"segmentCount"));
        meta.extend(format!("i{n}e").into_bytes());
        meta.extend(bstr(b"segments"));
        meta.extend(bstr(table));
        meta.push(b'e');
        meta
    }

    /// A structurally valid HAX0 container with `n` segments, each a fixed
    /// 48-byte slice (so offsets are known without encrypting anything).
    fn container(n: u32) -> Vec<u8> {
        let table_len = n as usize * 8;
        // The table's encoded length does not depend on its contents, so the
        // header length is known before offsets are filled in.
        let header_length = 16 + meta_with_table(n, &vec![0u8; table_len]).len() as u32;

        let mut table = vec![0u8; table_len];
        let mut cursor = header_length;
        for i in 0..n as usize {
            table[i * 8..i * 8 + 4].copy_from_slice(&cursor.to_le_bytes());
            table[i * 8 + 4..i * 8 + 8].copy_from_slice(&((i as u32) * 1000).to_le_bytes());
            cursor += 48;
        }

        let mut out = Vec::with_capacity(cursor as usize);
        out.extend(b"HAX0");
        out.extend(cursor.to_le_bytes());
        out.extend(header_length.to_le_bytes());
        out.extend(0u32.to_le_bytes());
        out.extend(meta_with_table(n, &table));
        out.resize(cursor as usize, 0);
        out
    }

    /// In-memory `RangeSource` that honours ranges.
    struct MemoryRange {
        data: Vec<u8>,
    }

    #[allow(clippy::unused_async_trait_impl)]
    impl RangeSource for MemoryRange {
        async fn range(&self, start: u64, end: u64) -> Result<Vec<u8>> {
            let s = start as usize;
            let e = ((end + 1) as usize).min(self.data.len());
            Ok(self.data[s..e].to_vec())
        }
    }

    /// A source that ignores `Range` and always returns the whole body, to
    /// exercise the local-slicing fallback.
    struct IgnoresRange {
        data: Vec<u8>,
    }

    #[allow(clippy::unused_async_trait_impl)]
    impl RangeSource for IgnoresRange {
        async fn range(&self, _start: u64, _end: u64) -> Result<Vec<u8>> {
            Ok(self.data.clone())
        }
    }

    /// A container whose segments are real `seal_zero` ciphertext under the leaf
    /// keys derived from `root` (node 1), so a pager that returns
    /// `{ "1": hex(root) }` can unlock them.
    fn encrypted_container(root: [u8; 32], plaintexts: &[&[u8]]) -> Vec<u8> {
        let n = plaintexts.len() as u32;
        let tree = KeyTree::new(n);
        let seeded = crypto::KeyMap::from([(1u32, root)]);
        let segments: Vec<Vec<u8>> = plaintexts
            .iter()
            .copied()
            .enumerate()
            .map(|(i, pt)| {
                let leaf = tree
                    .derive(&seeded, i as u32, &mut HashMap::new())
                    .expect("root covers every leaf");
                crypto::seal_zero(&leaf, pt).expect("seal")
            })
            .collect();

        // The table's encoded length does not depend on its contents, so the
        // header length is known before offsets are filled in.
        let table_len = n as usize * 8;
        let header_length = 16 + meta_with_table(n, &vec![0u8; table_len]).len() as u32;

        let mut table = vec![0u8; table_len];
        let mut cursor = header_length;
        for (i, seg) in segments.iter().enumerate() {
            table[i * 8..i * 8 + 4].copy_from_slice(&cursor.to_le_bytes());
            table[i * 8 + 4..i * 8 + 8].copy_from_slice(&((i as u32) * 1000).to_le_bytes());
            cursor += seg.len() as u32;
        }

        let mut out = Vec::with_capacity(cursor as usize);
        out.extend(b"HAX0");
        out.extend(cursor.to_le_bytes());
        out.extend(header_length.to_le_bytes());
        out.extend(0u32.to_le_bytes());
        out.extend(meta_with_table(n, &table));
        for seg in &segments {
            out.extend_from_slice(seg);
        }
        out
    }

    /// A pager with one fixed wire window, recording the indices asked for.
    struct OneWindow {
        window: HashMap<String, String>,
        seen: Vec<u32>,
    }

    /// A pager whose every window is empty, so no branch is ever covered.
    struct EmptyPager {
        seen: Vec<u32>,
    }

    #[allow(clippy::unused_async_trait_impl)]
    impl KeyPager for OneWindow {
        async fn page(&mut self, idx: u32) -> Result<HashMap<String, String>> {
            self.seen.push(idx);
            Ok(self.window.clone())
        }
    }

    #[allow(clippy::unused_async_trait_impl)]
    impl KeyPager for EmptyPager {
        async fn page(&mut self, idx: u32) -> Result<HashMap<String, String>> {
            self.seen.push(idx);
            Ok(HashMap::new())
        }
    }

    async_test!(reads_header_from_a_range_source {
        let data = container(3);
        let src = MemoryRange { data: data.clone() };
        let (file_length, header_length) = probe_header(&src).await.unwrap();
        assert_eq!(file_length as usize, data.len());
        assert!(header_length >= 16 && u64::from(header_length) <= file_length);

        let hax = read_header(&src).await.unwrap();
        assert_eq!(hax.segment_count, 3);
        assert_eq!(hax.codec, "mp4a.40.2");
        assert_eq!(hax.duration_ms, 1000);
        assert_eq!(hax.segments.len(), 3);
        assert_eq!(hax.segments[1].pts, 1000);
    });

    async_test!(tolerates_a_mirror_that_ignores_range {
        let src = IgnoresRange { data: container(3) };
        let hax = read_header(&src).await.unwrap();
        assert_eq!(hax.segment_count, 3);
    });

    async_test!(probe_rejects_a_non_container {
        let src = MemoryRange {
            data: b"NOTAHAX0___________".to_vec(),
        };
        assert!(probe_header(&src).await.is_err());
    });

    async_test!(probe_rejects_an_implausible_header_length {
        let mut data = container(2);
        data[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        let src = MemoryRange { data };
        assert!(probe_header(&src).await.is_err());
    });

    async_test!(streaming_pages_the_missing_branch_and_decrypts {
        let root = [7u8; 32];
        let data = encrypted_container(root, &[b"first-segment!", b"second-segment"]);
        let src = MemoryRange { data };
        let hax = read_header(&src).await.unwrap();
        assert_eq!(hax.segment_count, 2);

        // Nothing is seeded, so segment 0 must be paged before it can decrypt.
        let mut pager = OneWindow {
            window: HashMap::from([("1".to_string(), crypto::hex_encode(&root))]),
            seen: Vec::new(),
        };
        let mut got = Vec::new();
        let (keys, page_count) = decrypt_streaming(
            &src,
            &hax,
            &crypto::KeyMap::new(),
            &mut pager,
            |_, plain| {
                got.extend_from_slice(plain);
                Ok(())
            },
        )
        .await
        .unwrap();

        assert_eq!(got, b"first-segment!second-segment".to_vec());
        // Only segment 0 pages: the root covers the rest via the memo.
        assert_eq!(pager.seen, vec![0]);
        assert_eq!(page_count, 1);
        assert!(keys.contains_key(&1), "paged branch is kept for --save-keys");
    });

    async_test!(streaming_gives_up_when_no_branch_covers_a_segment {
        let data = container(2);
        let src = MemoryRange { data };
        let hax = read_header(&src).await.unwrap();
        let mut pager = EmptyPager { seen: Vec::new() };
        let err = decrypt_streaming(
            &src,
            &hax,
            &crypto::KeyMap::new(),
            &mut pager,
            |_, _| Ok(()),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("segment 0"), "got {err}");
        assert_eq!(
            pager.seen.len(),
            crate::download::KEY_PAGE_ATTEMPTS as usize
        );
    });
}
