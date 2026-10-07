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
use crate::hax::{Hax0, KeyTree};
use crate::http;

/// How to fetch byte ranges.
pub trait RangeSource {
    /// Fetch `start..=end`. Implementations must honour the range, or return
    /// the whole body with `from_slice` applied by the caller.
    fn range(&self, start: u64, end: u64) -> Result<Vec<u8>>;
}

/// [`RangeSource`] over HTTP.
pub struct HttpRangeSource<'a> {
    agent: &'a ureq::Agent,
    url: String,
    /// Body length, filled in by the first range read.
    pub file_length: u64,
}

impl<'a> HttpRangeSource<'a> {
    pub fn new(agent: &'a ureq::Agent, url: &str) -> Self {
        Self {
            agent,
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
    fn range(&self, start: u64, end: u64) -> Result<Vec<u8>> {
        let res = http::request(
            self.agent,
            "GET",
            &self.url,
            &[
                ("User-Agent", crate::HOTAUDIO_UA),
                ("Range", &format!("bytes={start}-{end}")),
            ],
            None,
        )?;
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
pub fn probe_header(source: &dyn RangeSource) -> Result<(u64, u32)> {
    let head = source.range(0, 15)?;
    if head.len() < 16 {
        return Err(Error::Protocol("short .hax prefix".into()));
    }
    if &head[..4] != b"HAX0" {
        return Err(Error::Protocol("not a HAX0 container".into()));
    }
    let file_length = u32::from_le_bytes(head[4..8].try_into().unwrap()) as u64;
    let header_length = u32::from_le_bytes(head[8..12].try_into().unwrap());
    if header_length < 16 || header_length as u64 > file_length {
        return Err(Error::Protocol(format!(
            "implausible HAX0 headerLength {header_length} for fileLength {file_length}"
        )));
    }
    Ok((file_length, header_length))
}

/// Fetch and parse just the container header.
pub fn read_header(source: &dyn RangeSource) -> Result<Hax0> {
    let (_, header_length) = probe_header(source)?;
    let header = source.range(0, (header_length - 1) as u64)?;
    Hax0::parse(&header)
}

/// Decrypt segments one at a time into `sink`, in order.
///
/// `on_segment` is called with each decrypted fragment's bytes. Returning an
/// error aborts. Key paging is the caller's job: pass a `keys` map that already
/// covers every segment (see [`crate::download::decrypt_container`] for the
/// miss-driven paging policy).
pub fn decrypt_streaming(
    source: &dyn RangeSource,
    hax: &Hax0,
    keys: &crate::crypto::KeyMap,
    mut on_segment: impl FnMut(usize, &[u8]) -> Result<()>,
) -> Result<()> {
    let tree = KeyTree::new(hax.segment_count.max(1));
    let mut cache = std::collections::HashMap::new();

    for i in 0..hax.segment_count as usize {
        let key = tree
            .derive(keys, i as u32, &mut cache)
            .map_err(|_| Error::Protocol(format!("saved keys do not cover segment {i}")))?;

        let (start, end) = hax.slice_range(i);
        let slice = source.range(start as u64, (end - 1) as u64)?;
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
    Ok(())
}

/// Write decrypted audio straight to a file as segments arrive, avoiding the
/// full-container buffer.
pub fn write_to_file(
    source: &dyn RangeSource,
    hax: &Hax0,
    keys: &crate::crypto::KeyMap,
    out: &mut impl Write,
    mut on_progress: impl FnMut(u64, u64) -> Result<()>,
) -> Result<u64> {
    let mut written = 0u64;
    let tree = KeyTree::new(hax.segment_count.max(1));
    let mut cache = std::collections::HashMap::new();

    for i in 0..hax.segment_count as usize {
        let key = tree
            .derive(keys, i as u32, &mut cache)
            .map_err(|_| Error::Protocol(format!("saved keys do not cover segment {i}")))?;
        let (start, end) = hax.slice_range(i);
        let slice = source.range(start as u64, (end - 1) as u64)?;
        let plain = crypto::open_zero(&key, &slice)?;
        out.write_all(&plain).map_err(|e| Error::Http(e.to_string()))?;
        written += plain.len() as u64;
        on_progress((i + 1) as u64, hax.segment_count as u64)?;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
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

    impl RangeSource for MemoryRange {
        fn range(&self, start: u64, end: u64) -> Result<Vec<u8>> {
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

    impl RangeSource for IgnoresRange {
        fn range(&self, _start: u64, _end: u64) -> Result<Vec<u8>> {
            Ok(self.data.clone())
        }
    }

    #[test]
    fn reads_header_from_a_range_source() {
        let data = container(3);
        let src = MemoryRange { data: data.clone() };
        let (file_length, header_length) = probe_header(&src).unwrap();
        assert_eq!(file_length as usize, data.len());
        assert!(header_length >= 16 && (header_length as u64) <= file_length);

        let hax = read_header(&src).unwrap();
        assert_eq!(hax.segment_count, 3);
        assert_eq!(hax.codec, "mp4a.40.2");
        assert_eq!(hax.duration_ms, 1000);
        assert_eq!(hax.segments.len(), 3);
        assert_eq!(hax.segments[1].pts, 1000);
    }

    #[test]
    fn tolerates_a_mirror_that_ignores_range() {
        let src = IgnoresRange { data: container(3) };
        let hax = read_header(&src).unwrap();
        assert_eq!(hax.segment_count, 3);
    }

    #[test]
    fn probe_rejects_a_non_container() {
        let src = MemoryRange { data: b"NOTAHAX0___________".to_vec() };
        assert!(probe_header(&src).is_err());
    }

    #[test]
    fn probe_rejects_an_implausible_header_length() {
        let mut data = container(2);
        data[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        let src = MemoryRange { data };
        assert!(probe_header(&src).is_err());
    }

    #[test]
    fn streaming_reports_missing_keys_rather_than_guessing() {
        let data = container(2);
        let src = MemoryRange { data };
        let hax = read_header(&src).unwrap();
        let err =
            decrypt_streaming(&src, &hax, &crypto::KeyMap::new(), |_, _| Ok(())).unwrap_err();
        assert!(err.to_string().contains("segment 0"), "got {err}");
    }
}
