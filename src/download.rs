//! Full-track download: handshake, key paging, `.hax` fetch, decrypt.
//!
//! Key paging policy mirrors the measurements documented in the original: each
//! `first:<n>` listen response unlocks a small window starting at segment `n`,
//! and the server serialises per track, so on a cache miss we request exactly
//! the missing index. But the window *width* rotates per request (8/32/128), so
//! one draw for an index can miss the leaves that need it; on a miss the index
//! is drawn again, bounded by [`KEY_PAGE_ATTEMPTS`] (see [`derive_or_page`]).
//!
//! A sequential pass shrugs a miss off — a later segment pages again anyway — but
//! random access (seeking) lands on an arbitrary segment and has no later
//! iteration to absorb it, so the bounded retry lives here rather than in every
//! caller.

use std::collections::HashMap;

use crate::crypto::{self, Error, Result};
use crate::hax::{Hax0, KeyError, KeyTree};
use crate::http::Client;
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
    /// Keep the raw `.hax` container bytes on [`Downloaded::hax_bytes`] so the
    /// caller can archive them. Off by default: the container is 15–25 MB, and
    /// buffered mode already holds the decrypted audio.
    pub capture_hax: bool,
    /// Politeness pause before each listen/key-paging request, in milliseconds.
    /// `0` (the default) disables it. It only spaces load for bulk/batch use and
    /// never affects correctness.
    pub delay_ms: u64,
}

/// The decrypted track plus everything needed to reuse it.
pub struct Downloaded {
    /// Concatenated MP4 fragments — playable `.m4a` bytes.
    pub audio: Vec<u8>,
    pub title: Option<String>,
    /// Duration in seconds.
    pub duration: f64,
    pub mime: &'static str,
    /// Container codec string (e.g. `mp4a.40.2`).
    pub codec: String,
    /// Server-issued `.hax` URL.
    pub hax_url: String,
    /// Merged branch keys, for `--save-keys`.
    pub keys: crypto::KeyMap,
    pub segment_count: u32,
    /// How many key branches had to be paged.
    pub pages: u32,
    /// The raw container bytes, present only when [`DownloadOptions::capture_hax`]
    /// was set. Reusable with [`decrypt_offline`] and a saved-keys envelope.
    pub hax_bytes: Option<Vec<u8>>,
}

/// The two clients a full download uses.
///
/// The split is deliberate (`docs/ARCHITECTURE.md`): `api` carries the small,
/// latency-bound page, listen and key-paging calls under a timeout, while `bulk`
/// fetches the 15–25 MB container with no timeout, because that transfer
/// legitimately takes minutes on a slow link. This type is what lets the library
/// run the pipeline without the caller reimplementing it just to hold two
/// clients; pass [`Clients::single`] when the split does not matter.
#[derive(Clone, Copy)]
pub struct Clients<'a> {
    /// Page, listen and key-paging calls.
    pub api: &'a Client,
    /// The `.hax` container transfer.
    pub bulk: &'a Client,
}

impl<'a> Clients<'a> {
    /// Both roles on one client, for callers that do not need the split.
    #[must_use]
    pub fn single(client: &'a Client) -> Self {
        Self {
            api: client,
            bulk: client,
        }
    }

    /// Download a track from its page URL.
    pub async fn download_from_page(
        &self,
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
        let hs = listen::load_handshake(self.api, page_url, api_base, opts.track_id).await?;
        self.download_with_handshake(&hs, opts).await
    }

    /// Download using an existing handshake.
    pub async fn download_with_handshake(
        &self,
        hs: &Handshake,
        opts: &mut DownloadOptions<'_>,
    ) -> Result<Downloaded> {
        let initial = listen::listen(self.api, hs, -1, opts.delay_ms).await?;
        if initial.url.is_empty() {
            return Err(Error::Listen("listen API returned no .hax url".into()));
        }
        check_hax_url(hs, &initial.url);

        let hax_bytes = fetch_container(self.bulk, &initial.url, opts).await?;
        let mut downloaded = decrypt_container(self.api, hs, &hax_bytes, &initial, opts).await?;
        if opts.capture_hax {
            downloaded.hax_bytes = Some(hax_bytes);
        }
        Ok(downloaded)
    }
}

/// Download a track from its page URL, on one client for both roles.
///
/// The whole pipeline runs on `client`, including the container transfer. Use
/// [`Clients`] to keep small API calls and the bulk transfer on separate
/// timeout policies.
pub async fn download_from_page(
    client: &Client,
    page_url: &str,
    opts: &mut DownloadOptions<'_>,
) -> Result<Downloaded> {
    let clients = Clients::single(client);
    clients.download_from_page(page_url, opts).await
}

/// Download using an existing handshake, on one client for both roles.
pub async fn download_with_handshake(
    client: &Client,
    hs: &Handshake,
    opts: &mut DownloadOptions<'_>,
) -> Result<Downloaded> {
    let clients = Clients::single(client);
    clients.download_with_handshake(hs, opts).await
}

/// Fetch the `.hax` container. `client` should be the untimed bulk client,
/// since this is the transfer that legitimately takes minutes.
pub async fn fetch_container(
    client: &Client,
    url: &str,
    opts: &mut DownloadOptions<'_>,
) -> Result<Vec<u8>> {
    // Resumable: a dropped transfer continues from the last byte rather than
    // restarting, which matters on slow or lossy paths.
    let resp = client
        .get_resumable(
            url,
            &[("User-Agent", crate::HOTAUDIO_UA)],
            |loaded, total| {
                if let Some(cb) = opts.on_progress.as_mut() {
                    cb(Progress {
                        phase: Phase::Fetching,
                        loaded,
                        total,
                    });
                }
            },
        )
        .await?;
    if !(200..300).contains(&resp.status) {
        return Err(Error::Http(format!(".hax fetch returned {}", resp.status)));
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

/// How many times a miss-driven key page is redrawn before giving up.
///
/// Branch width rotates per request, so a draw can miss (`docs/PROTOCOL.md`
/// §5). Four draws make a persistent miss vanishingly unlikely without letting a
/// genuinely uncovered segment spin; the previous single-draw policy is what
/// broke seeking.
pub const KEY_PAGE_ATTEMPTS: u32 = 4;

/// A source of key branches for miss-driven paging.
///
/// Paging the listen API is the only implementation that matters in production,
/// but isolating it lets [`derive_or_page`] — the retry policy callers would
/// otherwise reinvent — be tested without a network. `async` and
/// deliberately not object-safe, for the reasons given on
/// [`crate::range::RangeSource`].
#[allow(async_fn_in_trait)]
pub trait KeyPager {
    /// Page the branch window starting at segment `idx` and return its keys in
    /// the wire format (node index as a decimal string, key as hex).
    ///
    /// An empty or non-covering result is a miss, not an error: the caller
    /// draws again.
    async fn page(&mut self, idx: u32) -> Result<HashMap<String, String>>;
}

/// [`KeyPager`] over the encrypted listen API.
pub struct ListenPager<'a> {
    client: &'a Client,
    hs: &'a Handshake,
    delay_ms: u64,
}

impl<'a> ListenPager<'a> {
    /// Page `hs`'s track through `client`.
    pub fn new(client: &'a Client, hs: &'a Handshake) -> Self {
        Self {
            client,
            hs,
            delay_ms: 0,
        }
    }

    /// Pause `delay_ms` before each page request, for politeness on bulk jobs.
    #[must_use]
    pub fn with_delay_ms(mut self, delay_ms: u64) -> Self {
        self.delay_ms = delay_ms;
        self
    }
}

impl KeyPager for ListenPager<'_> {
    async fn page(&mut self, idx: u32) -> Result<HashMap<String, String>> {
        Ok(
            listen::listen(self.client, self.hs, idx as i64, self.delay_ms)
                .await?
                .keys,
        )
    }
}

/// Derive the key for `seg_idx`, paging branches until one covers it.
///
/// `keys` is the caller's merged-key map and `cache` its derivation memo; both
/// are updated in place. New keys invalidate the memo, since a new ancestor
/// changes every key beneath it. `pages` counts listen requests actually made,
/// misses included.
///
/// Fails only once [`KEY_PAGE_ATTEMPTS`] consecutive draws have all missed; a
/// transport error from `pager` is propagated immediately rather than retried,
/// so a real failure is not reported as an uncovered segment.
pub async fn derive_or_page(
    pager: &mut impl KeyPager,
    tree: &KeyTree,
    keys: &mut crypto::KeyMap,
    cache: &mut crypto::KeyMap,
    seg_idx: u32,
    pages: &mut u32,
) -> Result<[u8; 32]> {
    for _ in 0..KEY_PAGE_ATTEMPTS {
        let extra = pager.page(seg_idx).await?;
        *pages += 1;
        if listen::merge_keys(keys, &extra) == 0 {
            // The window missed this leaf; the next draw gets a new width.
            continue;
        }
        cache.clear();
        if let Ok(key) = tree.derive(keys, seg_idx, cache) {
            return Ok(key);
        }
        // Keys arrived, but none is an ancestor of this leaf. Draw again.
    }
    Err(Error::Protocol(format!(
        "no key branch covered segment {seg_idx} after {KEY_PAGE_ATTEMPTS} attempts"
    )))
}

/// The derived `.hax` URL when the server-issued one disagrees, else `None`.
///
/// [`listen::hax_url_for_key`] observes that the URL is fully determined by the
/// track key, so a disagreement means the CDN layout moved. This is a tripwire,
/// not a correction: the server-issued URL is still the one that gets fetched.
fn hax_url_disagreement(issued: &str, track_key: &str) -> Option<String> {
    let derived = listen::hax_url_for_key(track_key);
    (issued != derived).then_some(derived)
}

/// Check a server-issued `.hax` URL against the one derived from the handshake.
fn check_hax_url(hs: &Handshake, issued: &str) {
    if let Some(derived) = hax_url_disagreement(issued, &hs.track.key) {
        crate::warn(&format!(
            "issued .hax url '{issued}' does not match the derived '{derived}'; \
             the CDN layout may have changed"
        ));
    }
}

/// Cheap sanity check that decrypted bytes are MP4.
///
/// The first decrypted bytes are the head of the original file, so a valid MP4
/// has the `ftyp` box type at offset 4. This catches a truncated or wrongly
/// keyed decrypt that happened to authenticate before the bytes are written or
/// handed to a caller.
fn ensure_mp4(audio: &[u8]) -> Result<()> {
    if audio.len() < 8 || &audio[4..8] != b"ftyp" {
        return Err(Error::Protocol(
            "decrypted audio is not an MP4 (no ftyp box at offset 4)".into(),
        ));
    }
    Ok(())
}

/// Decrypt a container, paging key branches as needed.
///
/// `client` is used for key paging (small, latency-bound calls); the container
/// itself is supplied as bytes by the caller.
pub async fn decrypt_container(
    client: &Client,
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

    let mut audio: Vec<u8> =
        Vec::with_capacity((hax.file_length.saturating_sub(hax.header_length)) as usize + 64);

    let mut pager = ListenPager::new(client, hs).with_delay_ms(opts.delay_ms);
    let mut pages = 0u32;
    for i in 0..hax.segment_count as usize {
        let key = match tree.derive(&keys, i as u32, &mut cache) {
            Ok(k) => k,
            Err(KeyError::Missing { seg_idx }) => {
                derive_or_page(
                    &mut pager, &tree, &mut keys, &mut cache, seg_idx, &mut pages,
                )
                .await?
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

    ensure_mp4(&audio)?;
    Ok(Downloaded {
        audio,
        title: Some(hs.track.title.clone()).filter(|t| !t.is_empty()),
        duration: hax.duration_ms as f64 / 1000.0,
        mime: "audio/mp4",
        hax_url: initial.url.clone(),
        keys,
        segment_count: hax.segment_count,
        pages,
        codec: hax.codec,
        hax_bytes: None,
    })
}

/// Streamed download: read the container header over `Range`, then fetch and
/// decrypt one segment at a time, handing each decrypted fragment to
/// `on_fragment` as it arrives.
///
/// Peak memory is one segment rather than the whole container, and the first
/// playable fragment exists after a single round trip instead of after the full
/// transfer. Key branches are paged on demand exactly as in
/// [`decrypt_container`].
///
/// `on_fragment` is called once per segment, in order, which is what makes the
/// browser build able to append straight to a `MediaSource`: the first call is
/// the earliest point at which the output is a valid (if short) MP4. Returning
/// an error aborts the download.
///
/// `delay_ms` pauses before each listen/key-paging request, as in
/// [`DownloadOptions::delay_ms`].
pub async fn download_streaming(
    api_client: &Client,
    hs: &Handshake,
    initial_keys: Option<&crypto::KeyMap>,
    delay_ms: u64,
    mut on_fragment: impl FnMut(&[u8]) -> Result<()>,
    mut on_progress: Option<ProgressFn<'_>>,
) -> Result<Streamed> {
    let initial = listen::listen(api_client, hs, -1, delay_ms).await?;
    if initial.url.is_empty() {
        return Err(Error::Listen("listen API returned no .hax url".into()));
    }
    check_hax_url(hs, &initial.url);

    // Small ranged reads belong on the timeout-bound client.
    let mut source = HttpRangeSource::new(api_client, &initial.url);
    let (file_length, _) = range::probe_header(&source).await?;
    let hax = range::read_header(&source).await?;
    source.set_file_length(file_length);

    let tree = KeyTree::new(hax.segment_count.max(1));
    let mut keys: crypto::KeyMap = listen::keys_to_map(&initial.keys);
    if let Some(seed) = initial_keys {
        for (node, key) in seed {
            keys.entry(*node).or_insert(*key);
        }
    }
    let mut cache: crypto::KeyMap = HashMap::new();

    let mut written = 0u64;
    let mut paged = 0u32;
    let mut pager = ListenPager::new(api_client, hs).with_delay_ms(delay_ms);

    for i in 0..hax.segment_count as usize {
        let key = match tree.derive(&keys, i as u32, &mut cache) {
            Ok(k) => k,
            Err(KeyError::Missing { seg_idx }) => {
                derive_or_page(
                    &mut pager, &tree, &mut keys, &mut cache, seg_idx, &mut paged,
                )
                .await?
            }
        };

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
        if i == 0 {
            // The first fragment is the earliest point a caller has MP4 bytes;
            // reject a non-MP4 first segment before handing it over.
            ensure_mp4(&plain)?;
        }
        on_fragment(&plain)?;
        written += plain.len() as u64;

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
    let mut audio =
        Vec::with_capacity(hax.file_length.saturating_sub(hax.header_length) as usize + 64);

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
    ensure_mp4(&audio)?;
    Ok((audio, hax.segment_count, hax.duration_ms as f64 / 1000.0))
}

/// Fetch a `.hax` container by URL and decrypt it with saved keys (no page
/// fetch, no listen calls).
///
/// Returns the same [`Downloaded`] shape as the online entry points, so callers
/// can report one result format regardless of how the keys arrived. `pages` is
/// zero: the offline decrypt never pages key branches. When `capture_hax` is
/// set, the raw container is kept on [`Downloaded::hax_bytes`] for archiving.
pub async fn download_cached(
    client: &Client,
    hax_url: &str,
    all_keys: &crypto::KeyMap,
    on_progress: Option<ProgressFn<'_>>,
    capture_hax: bool,
) -> Result<Downloaded> {
    let mut opts = DownloadOptions::default();
    let hax_bytes = fetch_container(client, hax_url, &mut opts).await?;
    let hax = Hax0::parse(&hax_bytes)?;
    let (audio, segment_count, duration) = decrypt_offline(&hax_bytes, all_keys, on_progress)?;
    Ok(Downloaded {
        audio,
        title: None,
        duration,
        mime: "audio/mp4",
        codec: hax.codec,
        hax_url: hax_url.to_string(),
        keys: all_keys.clone(),
        segment_count,
        pages: 0,
        hax_bytes: capture_hax.then_some(hax_bytes),
    })
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
///
/// `track_id` selects a track on a multi-track page; `None` takes the page
/// default, the same rule as [`download_with_handshake`]. `delay_ms` pauses
/// before the listen request, as in [`DownloadOptions::delay_ms`].
pub async fn fetch_keys_only(
    client: &Client,
    page_url: &str,
    api_base: Option<&str>,
    track_id: Option<&str>,
    delay_ms: u64,
) -> Result<KeyBundle> {
    let api_base = api_base.unwrap_or(crate::HOTAUDIO_API_BASE);
    let hs = listen::load_handshake(client, page_url, api_base, track_id).await?;
    let initial = listen::listen(client, &hs, -1, delay_ms).await?;
    if initial.url.is_empty() {
        return Err(Error::Listen("listen API returned no .hax url".into()));
    }
    check_hax_url(&hs, &initial.url);
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
        "savedAt": crate::clock::iso8601_utc(),
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
///
/// An envelope `version` must be `1`; any other value is rejected rather than
/// misread. A bare map has no version and stays accepted for files written
/// before the envelope.
pub fn parse_saved_keys(text: &str) -> Result<SavedKeys> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| Error::Protocol(e.to_string()))?;

    // A bare `{ index: hex }` map has no `version`; an envelope that does must be
    // one this parser understands, so a future format fails loudly instead of
    // being misread as v1.
    if let Some(version) = value.get("version") {
        let version = version
            .as_u64()
            .ok_or_else(|| Error::Protocol("envelope version must be an integer".into()))?;
        if version != 1 {
            return Err(Error::Protocol(format!(
                "unsupported saved-keys version {version}; expected 1"
            )));
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A pager that replays a fixed script of windows, one per draw.
    ///
    /// `Err` entries fail the draw, so a script can mix misses, non-covering
    /// hits and transport errors. Records the indices it was asked for.
    struct ScriptedPager {
        windows: Vec<std::result::Result<HashMap<String, String>, Error>>,
        seen: Vec<u32>,
    }

    impl ScriptedPager {
        fn new(windows: Vec<HashMap<String, String>>) -> Self {
            Self {
                windows: windows.into_iter().map(Ok).collect(),
                seen: Vec::new(),
            }
        }

        fn with_error(err: Error) -> Self {
            Self {
                windows: vec![Err(err)],
                seen: Vec::new(),
            }
        }
    }

    impl KeyPager for ScriptedPager {
        async fn page(&mut self, idx: u32) -> Result<HashMap<String, String>> {
            self.seen.push(idx);
            let at = self.seen.len() - 1;
            match self.windows.get(at) {
                Some(Ok(w)) => Ok(w.clone()),
                Some(Err(e)) => Err(Error::Protocol(e.to_string())),
                None => Ok(HashMap::new()),
            }
        }
    }

    /// A one-key wire window at `node`.
    fn window(node: u32, byte: u8) -> HashMap<String, String> {
        HashMap::from([(node.to_string(), crypto::hex_encode(&[byte; 32]))])
    }

    async_test!(a_draw_that_covers_the_leaf_takes_one_page {
        let tree = KeyTree::new(899);
        let mut keys = crypto::KeyMap::new();
        let mut cache = crypto::KeyMap::new();
        let mut pages = 0;
        let mut pager = ScriptedPager::new(vec![window(1, 7)]);

        let key = derive_or_page(&mut pager, &tree, &mut keys, &mut cache, 42, &mut pages)
            .await
            .unwrap();

        assert_eq!(pages, 1);
        assert_eq!(pager.seen, vec![42]);
        let expect = tree
            .derive(&crypto::KeyMap::from([(1u32, [7u8; 32])]), 42, &mut HashMap::new())
            .unwrap();
        assert_eq!(key, expect);
    });

    async_test!(a_missed_window_is_redrawn_for_the_same_index {
        // Window misses, then a non-ancestor arrives (keys merge but the leaf
        // stays uncovered), then the root covers it. This is the seeking case
        // the single-draw policy failed: the first draw is not the last word.
        let tree = KeyTree::new(899);
        let mut keys = crypto::KeyMap::new();
        let mut cache = crypto::KeyMap::new();
        let mut pages = 0;
        let non_ancestor = tree.tree_base + 500;
        let mut pager = ScriptedPager::new(vec![
            HashMap::new(),
            window(non_ancestor, 9),
            window(1, 7),
        ]);

        let key = derive_or_page(&mut pager, &tree, &mut keys, &mut cache, 42, &mut pages)
            .await
            .unwrap();

        assert_eq!(pager.seen, vec![42, 42, 42], "must redraw the same index");
        assert_eq!(pages, 3);
        let expect = tree
            .derive(&crypto::KeyMap::from([(1u32, [7u8; 32])]), 42, &mut HashMap::new())
            .unwrap();
        assert_eq!(key, expect);
    });

    async_test!(paging_gives_up_after_the_bounded_attempts {
        let tree = KeyTree::new(899);
        let mut keys = crypto::KeyMap::new();
        let mut cache = crypto::KeyMap::new();
        let mut pages = 0;
        let mut pager = ScriptedPager::new(vec![]);

        let err = derive_or_page(&mut pager, &tree, &mut keys, &mut cache, 42, &mut pages)
            .await
            .unwrap_err();

        assert_eq!(pages, KEY_PAGE_ATTEMPTS);
        assert_eq!(pager.seen.len(), KEY_PAGE_ATTEMPTS as usize);
        let msg = err.to_string();
        assert!(msg.contains("segment 42"), "got {msg}");
        assert!(msg.contains("4 attempts"), "got {msg}");
    });

    async_test!(a_transport_error_is_not_mistaken_for_a_miss {
        let tree = KeyTree::new(899);
        let mut keys = crypto::KeyMap::new();
        let mut cache = crypto::KeyMap::new();
        let mut pages = 0;
        let mut pager = ScriptedPager::with_error(Error::Protocol("listen exploded".into()));

        let err = derive_or_page(&mut pager, &tree, &mut keys, &mut cache, 42, &mut pages)
            .await
            .unwrap_err();

        assert!(err.to_string().contains("listen exploded"), "got {err}");
        assert_eq!(pager.seen, vec![42], "a failure must not be retried");
        assert_eq!(pages, 0);
    });

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn saved_keys_envelope_round_trips() {
        let keys: crypto::KeyMap = [(1u32, [7u8; 32]), (4u32, [9u8; 32])].into_iter().collect();
        let json = keys_to_json(&keys, "https://cdn.hotaudio.net/a/x.hax", Some("A Title"));

        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["version"].as_u64(), Some(1));
        let saved_at = value["savedAt"].as_str().expect("savedAt is a string");
        assert_eq!(saved_at.len(), 20, "savedAt is not ISO-8601: {saved_at}");
        assert!(saved_at.ends_with('Z'), "savedAt is not UTC: {saved_at}");

        let parsed = parse_saved_keys(&json).expect("round-trips");
        assert_eq!(parsed.keys, keys);
        assert_eq!(
            parsed.hax_url.as_deref(),
            Some("https://cdn.hotaudio.net/a/x.hax")
        );
        assert_eq!(parsed.title.as_deref(), Some("A Title"));
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn saved_keys_accepts_v1_and_bare_maps() {
        let key_hex = crypto::hex_encode(&[7u8; 32]);
        let envelope = format!(r#"{{"version":1,"keys":{{"1":"{key_hex}"}}}}"#);
        assert!(parse_saved_keys(&envelope).is_ok());

        // No `version`: a bare `{ index: hex }` map, still accepted.
        let bare = format!(r#"{{"1":"{key_hex}"}}"#);
        assert!(parse_saved_keys(&bare).is_ok());
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn saved_keys_rejects_unknown_version() {
        let key_hex = crypto::hex_encode(&[7u8; 32]);
        let envelope = format!(r#"{{"version":2,"keys":{{"1":"{key_hex}"}}}}"#);
        let err = parse_saved_keys(&envelope).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("version 2"), "got {msg}");
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn saved_keys_rejects_a_non_integer_version() {
        let key_hex = crypto::hex_encode(&[7u8; 32]);
        let envelope = format!(r#"{{"version":"1","keys":{{"1":"{key_hex}"}}}}"#);
        assert!(parse_saved_keys(&envelope).is_err());
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn hax_url_tripwire_flags_only_a_derived_mismatch() {
        assert!(hax_url_disagreement("https://cdn.hotaudio.net/a/abc.hax", "abc").is_none());
        assert_eq!(
            hax_url_disagreement("https://cdn.example.net/a/abc.hax", "abc").as_deref(),
            Some("https://cdn.hotaudio.net/a/abc.hax")
        );
    }
}
