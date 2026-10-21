//! Minimal MP4 box inspection for the `--verify` check.
//!
//! This is not a demuxer and never touches the audio payload. It walks the box
//! sequence, requires an `ftyp` frame, a `moov` and some media, and sums the
//! fragment sample durations, so a truncated or wrongly keyed decrypt is caught
//! rather than merely looking plausible at offset 4. Like [`crate::hax`], it is
//! synchronous and identical on both targets.

use std::io::Read;

use crate::crypto::{Error, Result};

/// Cap on a box this module will buffer. Only `moov` and `moof` bodies are read
/// into memory (to read their headers); media is streamed past, so a real file
/// stays far below this and a corrupt size cannot force a huge allocation.
const MAX_BUFFERED_BOX: u64 = 64 << 20;

/// What [`verify`] found in an MP4 byte stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    /// Number of top-level boxes.
    pub boxes: u32,
    /// Whether the file carries `moof` fragments rather than one plain `mdat`.
    pub fragmented: bool,
    /// Total duration in milliseconds, when the headers carry a usable one.
    pub duration_ms: Option<u64>,
}

/// Walk an MP4 byte stream and report its shape.
///
/// Fails on any structural problem that indicates a truncated or corrupt file: a
/// bad box size, a box that overruns the stream, a first box that is not `ftyp`,
/// a missing `moov`, or no media at all. It does not require a seekable reader;
/// media boxes are skipped as they arrive.
///
/// # Errors
///
/// Returns [`Error::Protocol`] for a malformed, truncated or incomplete stream,
/// or [`Error::Io`] if the reader fails.
///
/// # Panics
///
/// If a fixed-width header field is not exactly its declared width; every slice
/// is produced by a checked length.
pub fn verify(mut reader: impl Read) -> Result<Info> {
    let mut boxes = 0u32;
    let mut fragmented = false;
    let mut saw_moov = false;
    let mut saw_media = false;
    let mut media_timescale = None;
    let mut movie_duration_ms = None;
    let mut fragment_ticks = 0u64;

    loop {
        let mut head = [0u8; 8];
        if !read_header_bytes(&mut reader, &mut head)? {
            break;
        }
        let size = u32::from_be_bytes(head[0..4].try_into().unwrap());
        let kind: [u8; 4] = head[4..8].try_into().unwrap();
        if boxes == 0 && &kind != b"ftyp" {
            return Err(Error::Protocol(format!(
                "mp4: first box is '{}', not 'ftyp'",
                label(kind)
            )));
        }
        boxes += 1;

        let payload_len = match size {
            0 => None, // A final box running to end-of-stream.
            1 => {
                let mut large = [0u8; 8];
                reader
                    .read_exact(&mut large)
                    .map_err(|_| Error::Protocol("mp4: truncated largesize".into()))?;
                Some(u64::from_be_bytes(large).checked_sub(16).ok_or_else(|| {
                    Error::Protocol("mp4: largesize smaller than its header".into())
                })?)
            }
            n if n < 8 => {
                return Err(Error::Protocol(format!(
                    "mp4: box '{}' has size {n}",
                    label(kind)
                )));
            }
            n => Some(u64::from(n - 8)),
        };

        match &kind {
            b"moov" => {
                saw_moov = true;
                let body = read_box_body(&mut reader, payload_len, kind)?;
                read_moov(&body, &mut media_timescale, &mut movie_duration_ms)?;
            }
            b"moof" => {
                fragmented = true;
                saw_media = true;
                let body = read_box_body(&mut reader, payload_len, kind)?;
                fragment_ticks = fragment_ticks.saturating_add(fragment_ticks_of(&body)?);
            }
            b"mdat" => {
                saw_media = true;
                skip(&mut reader, payload_len, kind)?;
            }
            _ => skip(&mut reader, payload_len, kind)?,
        }
    }

    if boxes == 0 {
        return Err(Error::Protocol("mp4: empty stream".into()));
    }
    if !saw_moov {
        return Err(Error::Protocol("mp4: no moov box".into()));
    }
    if !saw_media {
        return Err(Error::Protocol("mp4: no media (mdat or moof)".into()));
    }

    // A fragmented file's real duration is the summed sample durations in the
    // media timescale; the movie header usually carries zero. Only the media
    // timescale can convert those ticks, so without an `mdhd` the duration is
    // left unknown rather than guessed from the movie timescale (a different
    // unit). Fall back to a progressive `mvhd` duration when there is one.
    let duration_ms = if fragmented {
        media_timescale
            .and_then(|timescale| to_ms(fragment_ticks, timescale))
            .filter(|ms| *ms > 0)
            .or(movie_duration_ms)
    } else {
        movie_duration_ms
    };
    Ok(Info {
        boxes,
        fragmented,
        duration_ms,
    })
}

/// Fill `head` from `reader`. `false` is a clean EOF before any byte; a short
/// read is a truncated header.
fn read_header_bytes(reader: &mut impl Read, head: &mut [u8; 8]) -> Result<bool> {
    let mut filled = 0;
    while filled < head.len() {
        match reader.read(&mut head[filled..]).map_err(|e| io(&e))? {
            0 => break,
            n => filled += n,
        }
    }
    match filled {
        0 => Ok(false),
        n if n < head.len() => Err(Error::Protocol("mp4: truncated box header".into())),
        _ => Ok(true),
    }
}

/// Read a box body into memory; a box running to end-of-stream is refused.
fn read_box_body(reader: &mut impl Read, len: Option<u64>, kind: [u8; 4]) -> Result<Vec<u8>> {
    let len = len.ok_or_else(|| {
        Error::Protocol(format!("mp4: box '{}' runs to end of stream", label(kind)))
    })?;
    if len > MAX_BUFFERED_BOX {
        return Err(Error::Protocol(format!(
            "mp4: box '{}' is implausibly large ({len} bytes)",
            label(kind)
        )));
    }
    let mut body = vec![0u8; len as usize];
    reader
        .read_exact(&mut body)
        .map_err(|_| Error::Protocol(format!("mp4: box '{}' is truncated", label(kind))))?;
    Ok(body)
}

/// Skip `len` bytes, or drain to end-of-stream when `len` is `None`.
fn skip(reader: &mut impl Read, len: Option<u64>, kind: [u8; 4]) -> Result<()> {
    let copied = match len {
        Some(len) => std::io::copy(&mut reader.by_ref().take(len), &mut std::io::sink()),
        None => std::io::copy(reader.by_ref(), &mut std::io::sink()),
    }
    .map_err(|e| io(&e))?;
    if let Some(len) = len {
        if copied != len {
            return Err(Error::Protocol(format!(
                "mp4: box '{}' is truncated (wanted {len} bytes, got {copied})",
                label(kind)
            )));
        }
    }
    Ok(())
}

/// The byte range of the box starting at `pos` in a complete container body.
fn box_at(body: &[u8], pos: usize) -> Result<([u8; 4], std::ops::Range<usize>)> {
    let head = body
        .get(pos..pos + 8)
        .ok_or_else(|| Error::Protocol("mp4: truncated child box header".into()))?;
    let size = u32::from_be_bytes(head[0..4].try_into().unwrap());
    let kind: [u8; 4] = head[4..8].try_into().unwrap();
    let (size, header) = match size {
        0 => ((body.len() - pos) as u64, 8u64),
        1 => {
            let large = body
                .get(pos + 8..pos + 16)
                .ok_or_else(|| Error::Protocol("mp4: truncated largesize".into()))?;
            (u64::from_be_bytes(large.try_into().unwrap()), 16)
        }
        n if n < 8 => {
            return Err(Error::Protocol(format!(
                "mp4: box '{}' has size {n}",
                label(kind)
            )));
        }
        n => (u64::from(n), 8),
    };
    if size < header {
        return Err(Error::Protocol(format!(
            "mp4: box '{}' is smaller than its header",
            label(kind)
        )));
    }
    let start = pos + header as usize;
    let end = start
        .checked_add((size - header) as usize)
        .filter(|end| *end <= body.len())
        .ok_or_else(|| {
            Error::Protocol(format!("mp4: box '{}' overruns its container", label(kind)))
        })?;
    Ok((kind, start..end))
}

/// Every box directly inside a complete container body, in order.
fn children(body: &[u8]) -> Result<Vec<([u8; 4], &[u8])>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < body.len() {
        let (kind, range) = box_at(body, pos)?;
        pos = range.end;
        out.push((kind, &body[range]));
    }
    Ok(out)
}

/// Pull the media timescale and the progressive movie duration out of a `moov`.
fn read_moov(
    body: &[u8],
    media_timescale: &mut Option<u64>,
    movie_duration_ms: &mut Option<u64>,
) -> Result<()> {
    for (kind, child) in children(body)? {
        match &kind {
            b"mvhd" => {
                if let Some((timescale, duration)) = timescale_and_duration(child) {
                    if let Some(ms) = to_ms(duration, timescale).filter(|ms| *ms > 0) {
                        *movie_duration_ms = Some(ms);
                    }
                }
            }
            b"trak" => {
                for (mdia, mdia_body) in children(child)? {
                    if &mdia != b"mdia" {
                        continue;
                    }
                    for (mdhd, mdhd_body) in children(mdia_body)? {
                        if &mdhd == b"mdhd" {
                            if let Some((timescale, _)) = timescale_and_duration(mdhd_body) {
                                if timescale > 0 {
                                    *media_timescale = Some(timescale);
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// `(timescale, duration)` from a versioned `mvhd`/`mdhd` body.
///
/// Both are `FullBox`es with the same layout: `version(1)+flags(3)`, then
/// `creation` and `modification` (4 bytes each in version 0, 8 in version 1),
/// `timescale(4)` and `duration` (4 or 8).
fn timescale_and_duration(body: &[u8]) -> Option<(u64, u64)> {
    if *body.first()? == 0 {
        let timescale = u64::from(u32::from_be_bytes(body.get(12..16)?.try_into().ok()?));
        let duration = u64::from(u32::from_be_bytes(body.get(16..20)?.try_into().ok()?));
        Some((timescale, duration))
    } else {
        let timescale = u64::from(u32::from_be_bytes(body.get(20..24)?.try_into().ok()?));
        let duration = u64::from_be_bytes(body.get(24..32)?.try_into().ok()?);
        Some((timescale, duration))
    }
}

/// Convert `ticks` at `timescale` to milliseconds; `None` when the timescale is
/// zero, which would otherwise divide by zero.
fn to_ms(ticks: u64, timescale: u64) -> Option<u64> {
    (timescale > 0).then(|| ticks.saturating_mul(1000) / timescale)
}

/// Sum the sample durations of every `trun` in a `moof`, in media ticks.
fn fragment_ticks_of(moof: &[u8]) -> Result<u64> {
    let mut total = 0u64;
    for (kind, traf) in children(moof)? {
        if &kind != b"traf" {
            continue;
        }
        // The fragment header's default applies to any `trun` that does not
        // carry per-sample durations.
        let mut default_duration = None;
        for (kind, body) in children(traf)? {
            if &kind == b"tfhd" {
                default_duration = tfhd_default_duration(body);
            }
        }
        for (kind, body) in children(traf)? {
            if &kind == b"trun" {
                total = total.saturating_add(trun_ticks(body, default_duration)?);
            }
        }
    }
    Ok(total)
}

/// The 24-bit `flags` field of a `FullBox` body.
fn full_box_flags(body: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes([
        0,
        *body.get(1)?,
        *body.get(2)?,
        *body.get(3)?,
    ]))
}

/// `default_sample_duration` from a `tfhd`, when the flag carries it.
fn tfhd_default_duration(body: &[u8]) -> Option<u64> {
    let flags = full_box_flags(body)?;
    // FullBox header (4) + track_ID (4), then the optional fields in order.
    let mut offset = 8usize;
    if (flags & 0x00_0001) != 0 {
        offset += 8; // base_data_offset
    }
    if (flags & 0x00_0002) != 0 {
        offset += 4; // sample_description_index
    }
    if (flags & 0x00_0008) != 0 {
        let bytes: [u8; 4] = body.get(offset..offset + 4)?.try_into().ok()?;
        return Some(u64::from(u32::from_be_bytes(bytes)));
    }
    None
}

/// Total duration of a `trun`'s samples, in media ticks.
///
/// Per-sample durations (`0x00_0100`) win; otherwise every sample takes the track
/// fragment's default. A `trun` with neither contributes nothing.
fn trun_ticks(body: &[u8], default_duration: Option<u64>) -> Result<u64> {
    let flags =
        full_box_flags(body).ok_or_else(|| Error::Protocol("mp4: truncated trun".into()))?;
    let count = u64::from(u32::from_be_bytes(
        body.get(4..8)
            .ok_or_else(|| Error::Protocol("mp4: truncated trun".into()))?
            .try_into()
            .unwrap(),
    ));
    let mut offset = 8usize;
    if (flags & 0x00_0001) != 0 {
        offset += 4; // data_offset
    }
    if (flags & 0x00_0004) != 0 {
        offset += 4; // first_sample_flags
    }
    if (flags & 0x00_0100) == 0 {
        return Ok(count.saturating_mul(default_duration.unwrap_or(0)));
    }
    // Sample records are duration[, size][, flags][, composition offset], so the
    // stride is the sum of the fields that are present.
    let mut stride = 4usize;
    if (flags & 0x00_0200) != 0 {
        stride += 4;
    }
    if (flags & 0x00_0400) != 0 {
        stride += 4;
    }
    if (flags & 0x00_0800) != 0 {
        stride += 4;
    }
    let mut total = 0u64;
    for i in 0..count as usize {
        let at = offset + i * stride;
        let bytes: [u8; 4] = body
            .get(at..at + 4)
            .ok_or_else(|| Error::Protocol("mp4: truncated trun samples".into()))?
            .try_into()
            .unwrap();
        total = total.saturating_add(u64::from(u32::from_be_bytes(bytes)));
    }
    Ok(total)
}

/// A box type as text for diagnostics; non-printable bytes become `?`.
fn label(kind: [u8; 4]) -> String {
    kind.iter()
        .map(|b| {
            if b.is_ascii_graphic() {
                *b as char
            } else {
                '?'
            }
        })
        .collect()
}

/// An I/O failure while walking the stream, reported as a protocol error.
fn io(error: &std::io::Error) -> Error {
    Error::Protocol(format!("mp4: {error}"))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// One box: 32-bit size, type, body.
    fn bx(kind: [u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + body.len());
        out.extend_from_slice(&((8 + body.len()) as u32).to_be_bytes());
        out.extend_from_slice(&kind);
        out.extend_from_slice(body);
        out
    }

    /// Concatenate boxes into a stream.
    fn join(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.iter().flatten().copied().collect()
    }

    /// A `FullBox` body: `version`, 24-bit `flags`, then `payload`.
    fn full(version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![version];
        out.extend_from_slice(&flags.to_be_bytes()[1..4]);
        out.extend_from_slice(payload);
        out
    }

    /// `mvhd`/`mdhd` share the fields this module reads, so one builder serves
    /// both; `version` selects the 32- or 64-bit layout.
    fn timescale_box(kind: [u8; 4], version: u8, timescale: u32, duration: u64) -> Vec<u8> {
        let mut payload = Vec::new();
        if version == 1 {
            payload.extend_from_slice(&0u64.to_be_bytes()); // creation
            payload.extend_from_slice(&0u64.to_be_bytes()); // modification
            payload.extend_from_slice(&timescale.to_be_bytes());
            payload.extend_from_slice(&duration.to_be_bytes());
        } else {
            payload.extend_from_slice(&0u32.to_be_bytes()); // creation
            payload.extend_from_slice(&0u32.to_be_bytes()); // modification
            payload.extend_from_slice(&timescale.to_be_bytes());
            payload.extend_from_slice(&(duration as u32).to_be_bytes());
        }
        bx(kind, &full(version, 0, &payload))
    }

    /// A `moov` with a movie header and one audio track.
    fn moov(timescale: u32, duration: u64) -> Vec<u8> {
        let trak = bx(
            *b"trak",
            &bx(*b"mdia", &timescale_box(*b"mdhd", 0, timescale, duration)),
        );
        let body = join(&[timescale_box(*b"mvhd", 0, timescale, duration), trak]);
        bx(*b"moov", &body)
    }

    /// A `tfhd` with an optional default sample duration.
    fn tfhd(default_duration: Option<u32>) -> Vec<u8> {
        let mut payload = 1u32.to_be_bytes().to_vec(); // track_ID
        let flags = match default_duration {
            Some(d) => {
                payload.extend_from_slice(&d.to_be_bytes());
                0x00_0008
            }
            None => 0,
        };
        bx(*b"tfhd", &full(0, flags, &payload))
    }

    /// A `trun` whose samples all take the fragment default.
    fn trun_default(count: u32) -> Vec<u8> {
        bx(*b"trun", &full(0, 0, &count.to_be_bytes()))
    }

    /// A `trun` carrying a per-sample duration for each entry.
    fn trun_durations(durations: &[u32]) -> Vec<u8> {
        let mut payload = (durations.len() as u32).to_be_bytes().to_vec();
        for d in durations {
            payload.extend_from_slice(&d.to_be_bytes());
        }
        bx(*b"trun", &full(0, 0x00_0100, &payload))
    }

    /// A `moof` holding one track fragment with a single `trun`.
    fn moof(default_duration: Option<u32>, trun: Vec<u8>) -> Vec<u8> {
        bx(
            *b"moof",
            &bx(*b"traf", &join(&[tfhd(default_duration), trun])),
        )
    }

    /// A minimal `ftyp`.
    fn ftyp() -> Vec<u8> {
        bx(*b"ftyp", b"isom\x00\x00\x02\x00iso2")
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn sums_fragment_sample_durations() {
        // 48 kHz, two fragments of two 12000-tick samples: one second.
        let file = join(&[
            ftyp(),
            moov(48_000, 0),
            moof(None, trun_durations(&[12_000, 12_000])),
            bx(*b"mdat", &[0u8; 8]),
            moof(None, trun_durations(&[12_000, 12_000])),
            bx(*b"mdat", &[0u8; 8]),
        ]);
        let info = verify(Cursor::new(&file)).unwrap();
        assert_eq!(info.boxes, 6);
        assert!(info.fragmented);
        assert_eq!(info.duration_ms, Some(1000));
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn uses_the_tfhd_default_sample_duration() {
        // 100 samples of 480 ticks at 48 kHz: one second.
        let file = join(&[
            ftyp(),
            moov(48_000, 0),
            moof(Some(480), trun_default(100)),
            bx(*b"mdat", &[0u8; 4]),
        ]);
        assert_eq!(verify(Cursor::new(&file)).unwrap().duration_ms, Some(1000));
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn reads_the_progressive_movie_duration() {
        let file = join(&[ftyp(), moov(44_100, 44_100), bx(*b"mdat", &[0u8; 16])]);
        let info = verify(Cursor::new(&file)).unwrap();
        assert!(!info.fragmented);
        assert_eq!(info.duration_ms, Some(1000));
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn reads_a_version_1_duration() {
        let file = join(&[
            ftyp(),
            bx(*b"moov", &timescale_box(*b"mvhd", 1, 48_000, 96_000)),
            bx(*b"mdat", &[0u8; 4]),
        ]);
        assert_eq!(verify(Cursor::new(&file)).unwrap().duration_ms, Some(2000));
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn handles_a_64_bit_box_size() {
        let mut mdat = Vec::new();
        mdat.extend_from_slice(&1u32.to_be_bytes());
        mdat.extend_from_slice(b"mdat");
        mdat.extend_from_slice(&24u64.to_be_bytes()); // largesize: header + 8
        mdat.extend_from_slice(&[0u8; 8]);
        let file = join(&[ftyp(), moov(48_000, 48_000), mdat]);
        let info = verify(Cursor::new(&file)).unwrap();
        assert_eq!(info.boxes, 3);
        assert!(!info.fragmented);
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn rejects_a_non_ftyp_first_box() {
        let file = join(&[bx(*b"moov", &[]), bx(*b"mdat", &[0u8; 4])]);
        assert!(verify(Cursor::new(&file)).is_err());
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn rejects_a_missing_moov() {
        let file = join(&[ftyp(), bx(*b"mdat", &[0u8; 4])]);
        assert!(verify(Cursor::new(&file)).is_err());
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn rejects_a_missing_media_box() {
        let file = join(&[ftyp(), moov(48_000, 48_000)]);
        assert!(verify(Cursor::new(&file)).is_err());
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn rejects_a_truncated_mdat() {
        let mut file = join(&[ftyp(), moov(48_000, 48_000)]);
        file.extend_from_slice(&108u32.to_be_bytes()); // claims a 100-byte body
        file.extend_from_slice(b"mdat");
        file.extend_from_slice(&[0u8; 4]);
        assert!(verify(Cursor::new(&file)).is_err());
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn rejects_an_empty_stream() {
        assert!(verify(Cursor::new(Vec::new())).is_err());
    }

    /// A reader that hands out one byte per call, to exercise short reads.
    struct Trickle {
        data: Vec<u8>,
        at: usize,
    }

    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.at >= self.data.len() || buf.is_empty() {
                return Ok(0);
            }
            buf[0] = self.data[self.at];
            self.at += 1;
            Ok(1)
        }
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn tolerates_short_reads() {
        let file = join(&[
            ftyp(),
            moov(48_000, 0),
            moof(Some(480), trun_default(100)),
            bx(*b"mdat", &[0u8; 4]),
        ]);
        let info = verify(Trickle { data: file, at: 0 }).unwrap();
        assert_eq!(info.duration_ms, Some(1000));
    }
}
