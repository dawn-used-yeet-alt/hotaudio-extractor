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
) -> std::result::Result<JsValue, JsValue> {
    warn_if_not_canonical(&page_url);
    download_track_with(&page_url, on_fragment, on_progress, None, None).await
}

/// As `downloadTrack`, with an explicit `fetch` and/or URL rewrite.
///
/// `fetch_impl` replaces the global `fetch`: it must take `(url, init)` and
/// resolve to a `Response`. `rewrite` maps a request URL to the URL to actually
/// fetch, which is how a proxy deployment points the client at its own origin
/// without touching the caller's code. It must return a string; returning
/// anything else leaves the URL unchanged.
#[wasm_bindgen(js_name = downloadTrackWith)]
pub async fn download_track_with(
    page_url: &str,
    on_fragment: Function,
    on_progress: Option<Function>,
    fetch_impl: Option<Function>,
    rewrite: Option<Function>,
) -> std::result::Result<JsValue, JsValue> {
    let client = match fetch_impl {
        Some(f) => Client::api().with_fetch(f.into()),
        None => Client::api(),
    };
    let client = match rewrite {
        Some(f) => client.with_url_rewrite(move |url| call_string(&f, url, url)),
        None => client,
    };
    stream(&client, page_url, &on_fragment, on_progress)
        .await
        .map_err(js_error)
}

/// The handshake, the first key branch and the `.hax` URL, without any audio.
///
/// Cheaper than `downloadTrack` when the point is to save keys for a later
/// offline fetch. Resolves with `{ title, haxUrl, keysJson }`.
#[wasm_bindgen(js_name = trackKeys)]
pub async fn track_keys(page_url: String) -> std::result::Result<JsValue, JsValue> {
    warn_if_not_canonical(&page_url);
    let bundle = download::fetch_keys_only(&Client::api(), &page_url, None, None)
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
) -> Result<JsValue> {
    let hs = listen::load_handshake(client, page_url, crate::HOTAUDIO_API_BASE, None).await?;
    let title = Some(hs.track.title.clone()).filter(|t| !t.is_empty());

    let sink = on_fragment.clone();
    let mut report = |p: Progress| {
        if let Some(cb) = &on_progress {
            let phase = match p.phase {
                Phase::Resolving => "resolving",
                Phase::Fetching => "fetching",
                Phase::Decrypting => "decrypting",
            };
            let _ = cb.call3(
                &JsValue::NULL,
                &JsValue::from_str(phase),
                &JsValue::from_f64(p.loaded as f64),
                &JsValue::from_f64(p.total as f64),
            );
        }
    };

    let streamed = download::download_streaming(
        client,
        &hs,
        None,
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
