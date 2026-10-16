//! The JavaScript-facing API, for `wasm32-unknown-unknown` only.
//!
//! Everything here is a thin adapter over the same pipeline the CLI runs: the
//! handshake, the signer, the key exchange and the decrypt are already in the
//! library and target-independent. What a browser cannot do natively — block,
//! open sockets, set `User-Agent` — is handled by [`crate::http`] and
//! [`crate::clock`], so there is nothing left for this module to reimplement.
//!
//! Results are plain JavaScript objects rather than `#[wasm_bindgen]` structs:
//! wasm-bindgen can only expose `Copy` fields as getters, so a struct of `String`
//! fields would be awkward to read from JS.
//!
//! # What the host has to provide
//!
//! `fetch` from a page is subject to CORS, and hotaudio.net sends no
//! `Access-Control-Allow-Origin` — verified against the live site, where the
//! track page, the listen endpoint and `cdn.hotaudio.net` all answer a
//! cross-origin request without it and refuse the preflight. A browser
//! deployment therefore needs its own origin in front; see "Running behind a
//! proxy" in `docs/ARCHITECTURE.md` for what that proxy must do, and
//! [`download_track_with`] for the URL rewrite it plugs into.
//!
//! Hosts that are not a browser page but still lack CORS — a Capacitor native
//! bridge, say — pass a replacement `fetch` to [`download_track_with`].
//!
//! # Typical use
//!
//! ```js
//! import init, { downloadTrack } from "./hotaudio.js";
//! await init();
//! const info = await downloadTrack(
//!   "https://hotaudio.net/u/user/track",
//!   (fragment) => sourceBuffer.appendBuffer(fragment),
//!   (phase, loaded, total) => console.log(phase, loaded + "/" + total),
//! );
//! ```

use js_sys::{Function, Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;

use crate::download::{self, Phase, Progress};
use crate::http::Client;
use crate::listen;
use crate::{Error, Result};

/// Download and decrypt a track, handing each fragment to `on_fragment`.
///
/// `page_url` is the share URL, or any URL serving the track page.
/// `on_fragment` receives a `Uint8Array` per segment, in order: append each to a
/// `SourceBuffer` for MediaSource playback, or collect them into a Blob. The
/// first call is the earliest point at which the bytes are a playable (if
/// short) MP4.
///
/// `on_progress`, if given, is called with `(phase, loaded, total)` where phase
/// is `"resolving"`, `"fetching"` or `"decrypting"`.
///
/// `track_id`, if given, selects a track on a multi-track page, the same id the
/// CLI's `--track` accepts and `listTracks` reports; omit it for the page
/// default. A page that does not carry the id is rejected.
///
/// Resolves with `{ title, durationSecs, codec, mime, bytes, segmentCount,
/// keyPages, haxUrl, keysJson }`, or rejects with the pipeline's message.
///
/// See `downloadTrackWith` to point the crate at a proxy or at a host whose HTTP
/// is not the page's `fetch`.
#[wasm_bindgen(js_name = downloadTrack)]
pub async fn download_track(
    page_url: String,
    on_fragment: Function,
    on_progress: Option<Function>,
    track_id: Option<String>,
) -> std::result::Result<JsValue, JsValue> {
    warn_if_not_canonical(&page_url);
    download_track_with(&page_url, on_fragment, on_progress, None, None, track_id).await
}

/// As `downloadTrack`, with an explicit `fetch` and/or URL rewrite.
///
/// `fetch_impl` replaces the global `fetch`: it must take `(url, init)` and
/// resolve to a `Response`. `rewrite` maps a request URL to the URL to actually
/// fetch, which is how a proxy deployment points the client at its own origin
/// without touching the caller's code. It must return a string; returning
/// anything else leaves the URL unchanged.
///
/// `track_id` is the same optional selection `downloadTrack` takes.
#[wasm_bindgen(js_name = downloadTrackWith)]
pub async fn download_track_with(
    page_url: &str,
    on_fragment: Function,
    on_progress: Option<Function>,
    fetch_impl: Option<Function>,
    rewrite: Option<Function>,
    track_id: Option<String>,
) -> std::result::Result<JsValue, JsValue> {
    let client = client_with(fetch_impl, rewrite);
    stream(
        &client,
        page_url,
        &on_fragment,
        on_progress,
        track_id.as_deref(),
    )
    .await
    .map_err(js_error)
}

/// The handshake, the first key branch and the `.hax` URL, without any audio.
///
/// Cheaper than `downloadTrack` when the point is to save keys for a later
/// offline fetch. `track_id` selects a track on a multi-track page, as in
/// `downloadTrack`. Resolves with `{ title, haxUrl, keysJson }`.
#[wasm_bindgen(js_name = trackKeys)]
pub async fn track_keys(
    page_url: String,
    track_id: Option<String>,
) -> std::result::Result<JsValue, JsValue> {
    warn_if_not_canonical(&page_url);
    track_keys_with(page_url, None, None, track_id).await
}

/// As `trackKeys`, with an explicit `fetch` and/or URL rewrite.
///
/// The same integration seam as `downloadTrackWith`: a key-only save can go
/// through a proxy origin or a Capacitor-style native bridge. `track_id` selects
/// a track on a multi-track page.
#[wasm_bindgen(js_name = trackKeysWith)]
pub async fn track_keys_with(
    page_url: String,
    fetch_impl: Option<Function>,
    rewrite: Option<Function>,
    track_id: Option<String>,
) -> std::result::Result<JsValue, JsValue> {
    let client = client_with(fetch_impl, rewrite);
    let bundle = download::fetch_keys_only(&client, &page_url, None, track_id.as_deref(), 0)
        .await
        .map_err(js_error)?;
    let title = bundle.title.clone();
    let value = Object::new();
    set(&value, "title", title.clone());
    set(&value, "haxUrl", bundle.hax_url.clone());
    set(
        &value,
        "keysJson",
        download::keys_to_json(&bundle.keys, &bundle.hax_url, title.as_deref()),
    );
    Ok(value.into())
}

/// Decrypt a `.hax` container you already hold, with a saved-keys envelope.
///
/// `keys_json` is the envelope `trackKeys` / `downloadTrack` produce, or a bare
/// `{ index: hex }` map. Returns the decrypted MP4 as a `Uint8Array`. Pure and
/// offline: cache the container and this replays it without the page, the
/// listen API or any network.
#[wasm_bindgen(js_name = decryptHax)]
pub fn decrypt_hax(hax_bytes: &[u8], keys_json: &str) -> std::result::Result<Uint8Array, JsValue> {
    let saved = download::parse_saved_keys(keys_json).map_err(js_error)?;
    let (audio, _, _) =
        download::decrypt_offline(hax_bytes, &saved.keys, None).map_err(js_error)?;
    Ok(Uint8Array::from(audio.as_slice()))
}

/// Fetch a `.hax` container by URL and decrypt it with saved keys — no page
/// fetch and no listen calls, for replaying a track from a saved envelope.
///
/// The offline decrypt works on the whole container in memory, so unlike
/// `downloadTrack` there is nothing to stream incrementally: `on_fragment` is
/// called exactly once, with the complete MP4. `on_progress` reports
/// `(phase, loaded, total)`. Resolves with the same fields as `downloadTrack`;
/// `title` comes from the envelope when it carries one.
#[wasm_bindgen(js_name = downloadCached)]
pub async fn download_cached(
    hax_url: String,
    keys_json: String,
    on_fragment: Function,
    on_progress: Option<Function>,
) -> std::result::Result<JsValue, JsValue> {
    let saved = download::parse_saved_keys(&keys_json).map_err(js_error)?;
    let title = saved.title.clone();

    let mut report = |p: Progress| report_progress(&on_progress, &p);
    let downloaded = download::download_cached(
        &Client::bulk(),
        &hax_url,
        &saved.keys,
        Some(&mut report),
        false,
    )
    .await
    .map_err(js_error)?;

    on_fragment
        .call1(
            &JsValue::NULL,
            &Uint8Array::from(downloaded.audio.as_slice()),
        )
        .map_err(|e| js_error(Error::Protocol(format!("on_fragment threw: {e:?}"))))?;

    let info = Object::new();
    set(&info, "title", title.clone());
    set(&info, "durationSecs", downloaded.duration);
    set(&info, "codec", downloaded.codec);
    set(&info, "mime", downloaded.mime);
    set(&info, "bytes", downloaded.audio.len() as f64);
    set(&info, "segmentCount", downloaded.segment_count as f64);
    set(&info, "keyPages", downloaded.pages as f64);
    set(&info, "haxUrl", downloaded.hax_url.clone());
    set(
        &info,
        "keysJson",
        download::keys_to_json(&downloaded.keys, &downloaded.hax_url, title.as_deref()),
    );
    Ok(info.into())
}

/// The `X-Signature` this crate would send for `payload` right now.
///
/// Exposed so a host can compare against the reference signer in a browser,
/// which is the parity check `tests/signer_parity.rs` runs natively.
#[wasm_bindgen(js_name = signPayload)]
pub fn sign_payload(payload: &str) -> std::result::Result<String, JsValue> {
    crate::signer::sign_payload(payload).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// Tracks listed on a page, without any network call beyond the page itself.
///
/// Takes page HTML as readily as a URL, so a host that already fetched the page
/// does not have to fetch it twice. Resolves to `[{ id, key, title }, …]`, or an
/// empty array when the page has no decryptable state.
#[wasm_bindgen(js_name = listTracks)]
pub fn list_tracks(html: &str) -> JsValue {
    let tracks = listen::list_tracks(html).unwrap_or_default();
    let array = js_sys::Array::new();
    for (id, key, title) in tracks {
        let entry = Object::new();
        set(&entry, "id", id);
        set(&entry, "key", key);
        set(&entry, "title", title);
        array.push(&entry);
    }
    array.into()
}

async fn stream(
    client: &Client,
    page_url: &str,
    on_fragment: &Function,
    on_progress: Option<Function>,
    track_id: Option<&str>,
) -> Result<JsValue> {
    let hs = listen::load_handshake(client, page_url, crate::HOTAUDIO_API_BASE, track_id).await?;
    let title = Some(hs.track.title.clone()).filter(|t| !t.is_empty());

    let sink = on_fragment.clone();
    let mut report = |p: Progress| report_progress(&on_progress, &p);

    let streamed = download::download_streaming(
        client,
        &hs,
        None,
        0,
        |fragment| {
            sink.call1(&JsValue::NULL, &Uint8Array::from(fragment))
                .map(|_| ())
                .map_err(|e| Error::Protocol(format!("on_fragment threw: {e:?}")))
        },
        Some(&mut report),
    )
    .await?;

    let info = Object::new();
    set(&info, "title", title.clone());
    set(&info, "durationSecs", streamed.duration);
    set(&info, "codec", streamed.codec.clone());
    set(&info, "mime", "audio/mp4");
    set(&info, "bytes", streamed.bytes as f64);
    set(&info, "segmentCount", streamed.segment_count as f64);
    set(&info, "keyPages", streamed.pages as f64);
    set(&info, "haxUrl", streamed.hax_url.clone());
    set(
        &info,
        "keysJson",
        download::keys_to_json(&streamed.keys, &streamed.hax_url, title.as_deref()),
    );
    Ok(info.into())
}

/// Forward one progress report to the JS callback, if there is one.
fn report_progress(on_progress: &Option<Function>, p: &Progress) {
    if let Some(cb) = on_progress {
        let _ = cb.call3(
            &JsValue::NULL,
            &JsValue::from_str(phase_name(&p.phase)),
            &JsValue::from_f64(p.loaded as f64),
            &JsValue::from_f64(p.total as f64),
        );
    }
}

/// The phase name a progress report carries to JS.
fn phase_name(phase: &Phase) -> &'static str {
    match phase {
        Phase::Resolving => "resolving",
        Phase::Fetching => "fetching",
        Phase::Decrypting => "decrypting",
    }
}

/// Warn through `console.warn` when a page URL is not a canonical share link.
///
/// The crate fetches whatever URL it is given — a proxy or mirror is a
/// supported deployment — so this is a hint for a likely typo, not a rejection.
fn warn_if_not_canonical(page_url: &str) {
    if crate::is_hotaudio_url(page_url) {
        return;
    }
    crate::warn(&format!(
        "'{page_url}' is not a canonical share link (https://hotaudio.net/u/<user>/<track>)"
    ));
}

/// Build the client for the proxy/bridge seam shared by `downloadTrackWith` and
/// `trackKeysWith`: a replacement `fetch`, a URL rewrite, or both.
fn client_with(fetch_impl: Option<Function>, rewrite: Option<Function>) -> Client {
    let client = match fetch_impl {
        Some(f) => Client::api().with_fetch(f.into()),
        None => Client::api(),
    };
    match rewrite {
        Some(f) => client.with_url_rewrite(move |url| call_string(&f, url, url)),
        None => client,
    }
}

/// Call a JS function that must return a string, falling back to `fallback` if
/// it throws or returns anything else.
fn call_string(f: &Function, arg: &str, fallback: &str) -> String {
    f.call1(&JsValue::NULL, &JsValue::from_str(arg))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| fallback.to_string())
}

fn set(object: &Object, key: &str, value: impl Into<JsValue>) {
    let _ = Reflect::set(object, &JsValue::from_str(key), &value.into());
}

fn js_error(e: Error) -> JsValue {
    JsValue::from_str(&e.to_string())
}
