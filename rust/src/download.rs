//! Full-track download: handshake, key paging, `.hax` fetch, decrypt.
//!
//! Key paging policy mirrors the measurements documented in the original: each
//! `first:<n>` listen response unlocks a small window starting at segment `n`,
//! and the server serialises per track, so on a cache miss we request exactly
//! the missing index. Every request is then provably needed, which minimises
//! both wall time and server load.

use std::collections::HashMap;
use std::io::Write;

use crate::crypto::{self, Error, Result};
use crate::hax::{Hax0, KeyError, KeyTree};
use crate::http;
use crate::listen::{self, Handshake, ListenResponse};
use crate::range::{self, HttpRangeSource, RangeSource};

/// Progress phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Resolving,
    Fetching,
    Decrypting,
}

/// A progress report.
#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub phase: Phase,
    pub loaded: u64,
    pub total: u64,
}

/// Callback for progress reports.
pub type ProgressFn<'a> = &'a mut dyn FnMut(Progress);

/// A progress reporter that can be threaded through the pipeline.
pub type ProgressSink<'a> = Option<ProgressFn<'a>>;

/// Options for a download.
#[derive(Default)]
pub struct DownloadOptions<'a> {
    /// Extra branch keys to seed the key map (resume / offline reuse).
    pub initial_keys: Option<crypto::KeyMap>,
    pub on_progress: ProgressSink<'a>,
    pub api_base: Option<&'a str>,
    pub track_id: Option<&'a str>,
}

/// The decrypted track plus everything needed to reuse it.
pub struct Downloaded {
    /// Concatenated MP4 fragments — playable `.m4a` bytes.
    pub audio: Vec<u8>,
    pub title: Option<String>,
    /// Duration in seconds.
    pub duration: f64,
    pub mime: &'static str,
    /// Server-issued `.hax` URL.
    pub hax_url: String,
    /// Merged branch keys, for `--save-keys`.
    pub keys: crypto::KeyMap,
    pub segment_count: u32,
}

/// Download a track from its page URL.
pub fn download_from_page(
    agent: &ureq::Agent,
    page_url: &str,
    opts: &mut DownloadOptions<'_>,
) -> Result<Downloaded> {
    if let Some(cb) = opts.on_progress.as_mut() {
        cb(Progress {
            phase: Phase::Resolving,
            loaded: 0,
            total: 0,
        });
    }
    let api_base = opts.api_base.unwrap_or(crate::HOTAUDIO_API_BASE);
    let hs = listen::load_handshake(agent, page_url, api_base)?;
    if let Some(id) = opts.track_id {
        // Re-derive the handshake for the requested track.
        if hs.tid != id {
            return Err(Error::Protocol(format!("track {id} not on page")));
        }
    }
    download_with_handshake(agent, &hs, opts)
}

/// Download using an existing handshake.
pub fn download_with_handshake(
    agent: &ureq::Agent,
    hs: &Handshake,
    opts: &mut DownloadOptions<'_>,
) -> Result<Downloaded> {
    let initial = listen::listen(agent, hs, -1)?;
    if initial.url.is_empty() {
        return Err(Error::Listen("listen API returned no .hax url".into()));
    }

    let hax_bytes = fetch_container(agent, &initial.url, opts)?;
    decrypt_container(agent, hs, &hax_bytes, &initial, opts)
}

/// Fetch the `.hax` container. `agent` should be the untimed bulk agent,
/// since this is the transfer that legitimately takes minutes.
pub fn fetch_container(
    agent: &ureq::Agent,
    url: &str,
    opts: &mut DownloadOptions<'_>,
) -> Result<Vec<u8>> {
    let resp = http::request(
        agent,
        "GET",
        url,
        &[("User-Agent", crate::HOTAUDIO_UA)],
        None
    )?;
    if !(200..300).contains(&resp.status) {
        return Err(Error::Http(format!(
            ".hax fetch returned {}",
            resp.status
        )));
    }
    if let Some(cb) = opts.on_progress.as_mut() {
        let n = resp.body.len() as u64;
        cb(Progress {
            phase: Phase::Fetching,
            loaded: n,
            total: n,
        });
    }
    Ok(resp.body)
}

/// Decrypt a container, paging key branches as needed.
///
/// `api` is used for key paging (small, latency-bound calls); the container
/// itself is supplied as bytes by the caller.
pub fn decrypt_container(
    agent: &ureq::Agent,
    hs: &Handshake,
    hax_bytes: &[u8],
    initial: &ListenResponse,
    opts: &mut DownloadOptions<'_>,
) -> Result<Downloaded> {
    let hax = Hax0::parse(hax_bytes)?;
    let tree = KeyTree::new(hax.segment_count.max(1));

    let mut keys: crypto::KeyMap = listen::keys_to_map(&initial.keys);
    if let Some(seed) = &opts.initial_keys {
        for (node, key) in seed {
            keys.entry(*node).or_insert(*key);
        }
    }
    // Node keys are memoised across segments; cleared whenever new branch keys
    // arrive because a new ancestor changes the whole subtree.
    let mut cache: crypto::KeyMap = HashMap::new();

    let mut audio: Vec<u8> = Vec::with_capacity(
        (hax.file_length.saturating_sub(hax.header_length)) as usize + 64,
    );

    for i in 0..hax.segment_count as usize {
        let key = match tree.derive(&keys, i as u32, &mut cache) {
            Ok(k) => k,
            Err(KeyError::Missing { seg_idx }) => {
                // Page exactly the missing index.
                let extra = listen::listen(agent, hs, seg_idx as i64)?;
                if listen::merge_keys(&mut keys, &extra.keys) == 0 {
                    return Err(Error::Protocol(format!(
                        "no key branch covered segment {seg_idx}"
                    )));
                }
                cache.clear();
                tree.derive(&keys, seg_idx, &mut cache).map_err(|_| {
                    Error::Protocol(format!("key branch for segment {seg_idx} still missing"))
                })?
            }
        };

        let (start, end) = hax.slice_range(i);
        let slice = hax_bytes
            .get(start..end)
            .ok_or_else(|| Error::Protocol("segment slice out of range".into()))?;
        crypto::open_zero_into(&key, slice, &mut audio)?;

        if let Some(cb) = opts.on_progress.as_mut() {
            // Report every 16 segments, and always on the last one.
            if (i & 15) == 15 || i + 1 == hax.segment_count as usize {
                cb(Progress {
                    phase: Phase::Decrypting,
                    loaded: (i + 1) as u64,
                    total: hax.segment_count as u64,
                });
            }
        }
    }

    Ok(Downloaded {
        audio,
        title: Some(hs.track.title.clone()).filter(|t| !t.is_empty()),
        duration: hax.duration_ms as f64 / 1000.0,
        mime: "audio/mp4",
        hax_url: initial.url.clone(),
        keys,
        segment_count: hax.segment_count,
    })
}

/// Streamed download: read the container header over `Range`, then fetch and
/// decrypt one segment at a time straight into `out`.
///
/// Peak memory is one segment rather than the whole container, and the first
/// playable fragment exists after a single round trip instead of after the full
/// transfer. Key branches are paged on demand exactly as in
/// [`decrypt_container`].
///
/// `on_first_fragment` is invoked once the first decrypted segment is written,
/// which is the earliest point at which the output is a valid (if short) MP4.
pub fn download_streaming(
    api_agent: &ureq::Agent,
    hs: &Handshake,
    out: &mut dyn Write,
    initial_keys: Option<&crypto::KeyMap>,
    mut on_progress: Option<ProgressFn<'_>>,
    mut on_first_fragment: Option<&mut dyn FnMut()>,
) -> Result<Streamed> {
    let initial = listen::listen(api_agent, hs, -1)?;
    if initial.url.is_empty() {
        return Err(Error::Listen("listen API returned no .hax url".into()));
    }

    // Small ranged reads belong on the timeout-bound agent.
    let mut source = HttpRangeSource::new(api_agent, &initial.url);
    let (file_length, _) = range::probe_header(&source)?;
    let hax = range::read_header(&source)?;
    source.set_file_length(file_length);

    let tree = KeyTree::new(hax.segment_count.max(1));
    let mut keys: crypto::KeyMap = listen::keys_to_map(&initial.keys);
    if let Some(seed) = initial_keys {
        for (node, key) in seed {
            keys.entry(*node).or_insert(*key);
        }
    }
    let mut cache: HashMap<u32, [u8; 32]> = HashMap::new();

    let mut written = 0u64;
    let mut paged = 0u32;

    for i in 0..hax.segment_count as usize {
        let key = match tree.derive(&keys, i as u32, &mut cache) {
            Ok(k) => k,
            Err(KeyError::Missing { seg_idx }) => {
                let extra = listen::listen(api_agent, hs, seg_idx as i64)?;
                if listen::merge_keys(&mut keys, &extra.keys) == 0 {
                    return Err(Error::Protocol(format!(
                        "no key branch covered segment {seg_idx}"
                    )));
                }
                cache.clear();
                paged += 1;
                tree.derive(&keys, seg_idx, &mut cache).map_err(|_| {
                    Error::Protocol(format!("key branch for segment {seg_idx} still missing"))
                })?
            }
        };

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
        out.write_all(&plain)
            .map_err(|e| Error::Http(e.to_string()))?;
        written += plain.len() as u64;
        if i == 0 {
            if let Some(cb) = on_first_fragment.as_mut() {
                cb();
            }
        }

        if let Some(cb) = on_progress.as_mut() {
            if (i & 15) == 15 || i + 1 == hax.segment_count as usize {
                cb(Progress {
                    phase: Phase::Decrypting,
                    loaded: (i + 1) as u64,
                    total: hax.segment_count as u64,
                });
            }
        }
    }

    Ok(Streamed {
        bytes: written,
        segment_count: hax.segment_count,
        duration: hax.duration_ms as f64 / 1000.0,
        codec: hax.codec.clone(),
        hax_url: initial.url,
        keys,
        pages: paged,
    })
}

/// Summary of a streamed download.
#[derive(Debug)]
pub struct Streamed {
    /// Plaintext audio bytes written.
    pub bytes: u64,
    pub segment_count: u32,
    pub duration: f64,
    pub codec: String,
    pub hax_url: String,
    /// Merged branch keys, for `--save-keys`.
    pub keys: crypto::KeyMap,
    /// How many key branches had to be paged.
    pub pages: u32,
}

/// Decrypt a container offline with previously saved branch keys.
pub fn decrypt_offline(
    hax_bytes: &[u8],
    all_keys: &crypto::KeyMap,
    mut on_progress: Option<ProgressFn<'_>>,
) -> Result<(Vec<u8>, u32, f64)> {
    let hax = Hax0::parse(hax_bytes)?;
    let tree = KeyTree::new(hax.segment_count.max(1));
    let mut cache = HashMap::new();
    let mut audio = Vec::with_capacity(
        hax.file_length.saturating_sub(hax.header_length) as usize + 64,
    );

    for i in 0..hax.segment_count as usize {
        let key = tree
            .derive(all_keys, i as u32, &mut cache)
            .map_err(|_| Error::Protocol(format!("saved keys do not cover segment {i}")))?;
        let (start, end) = hax.slice_range(i);
        let slice = hax_bytes
            .get(start..end)
            .ok_or_else(|| Error::Protocol("segment slice out of range".into()))?;
        crypto::open_zero_into(&key, slice, &mut audio)?;

        if let Some(cb) = on_progress.as_mut() {
            if (i & 15) == 15 || i + 1 == hax.segment_count as usize {
                cb(Progress {
                    phase: Phase::Decrypting,
                    loaded: (i + 1) as u64,
                    total: hax.segment_count as u64,
                });
            }
        }
    }
    Ok((audio, hax.segment_count, hax.duration_ms as f64 / 1000.0))
}

/// Fetch a `.hax` container by URL and decrypt it with saved keys (no page
/// fetch, no listen calls).
pub fn download_cached(
    agent: &ureq::Agent,
    hax_url: &str,
    all_keys: &crypto::KeyMap,
    on_progress: Option<ProgressFn<'_>>,
) -> Result<(Vec<u8>, u32, f64)> {
    let mut opts = DownloadOptions::default();
    let bytes = fetch_container(agent, hax_url, &mut opts)?;
    decrypt_offline(&bytes, all_keys, on_progress)
}

/// The first key branch plus the `.hax` URL, without downloading audio.
#[derive(Debug)]
pub struct KeyBundle {
    /// The first key branch.
    pub keys: crypto::KeyMap,
    /// The server-issued `.hax` URL.
    pub hax_url: String,
    /// Track title, when the page provided one.
    pub title: Option<String>,
}

/// Fetch only the first key branch and the `.hax` URL, without audio bytes.
pub fn fetch_keys_only(
    agent: &ureq::Agent,
    page_url: &str,
    api_base: Option<&str>,
) -> Result<KeyBundle> {
    let api_base = api_base.unwrap_or(crate::HOTAUDIO_API_BASE);
    let hs = listen::load_handshake(agent, page_url, api_base)?;
    let initial = listen::listen(agent, &hs, -1)?;
    if initial.url.is_empty() {
        return Err(Error::Listen("listen API returned no .hax url".into()));
    }
    let title = Some(hs.track.title.clone()).filter(|t| !t.is_empty());
    Ok(KeyBundle {
        keys: listen::keys_to_map(&initial.keys),
        hax_url: initial.url,
        title,
    })
}

/// Serialise branch keys into the saved-keys envelope.
pub fn keys_to_json(keys: &crypto::KeyMap, hax_url: &str, title: Option<&str>) -> String {
    use std::collections::BTreeMap;
    let mut map = BTreeMap::new();
    for (node, key) in keys {
        map.insert(node.to_string(), crypto::hex_encode(key));
    }
    let envelope = serde_json::json!({
        "version": 1,
        "haxUrl": hax_url,
        "title": title,
        "savedAt": "",
        "keys": map,
    });
    serde_json::to_string_pretty(&envelope).unwrap_or_default()
}

/// A parsed saved-keys file.
#[derive(Debug)]
pub struct SavedKeys {
    /// Branch keys, keyed by tree node index.
    pub keys: crypto::KeyMap,
    /// The `.hax` URL from the envelope, when present.
    pub hax_url: Option<String>,
    /// Track title from the envelope, when present.
    pub title: Option<String>,
}

/// Parse a saved-keys envelope (or a bare `{ index: hex }` map).
pub fn parse_saved_keys(text: &str) -> Result<SavedKeys> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| Error::Protocol(e.to_string()))?;

    let keys_obj = match value.get("keys") {
        Some(k) => k.clone(),
        None => value.clone(),
    };
    let Some(obj) = keys_obj.as_object() else {
        return Err(Error::Protocol("keys must be a { index: hex } map".into()));
    };
    if obj.is_empty() {
        return Err(Error::Protocol("keys map is empty".into()));
    }

    let mut map = HashMap::new();
    for (k, v) in obj {
        let Ok(node) = k.parse::<u32>() else {
            return Err(Error::Protocol(format!("key {k:?} is not an index")));
        };
        let Some(hex) = v.as_str() else {
            return Err(Error::Protocol(format!("key {node} is not a hex string")));
        };
        let bytes = crypto::hex_decode(hex)?;
        let key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| Error::Protocol(format!("key {node} is not 32 bytes")))?;
        map.insert(node, key);
    }
    let hax_url = value
        .get("haxUrl")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let title = value
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok(SavedKeys {
        keys: map,
        hax_url,
        title,
    })
}