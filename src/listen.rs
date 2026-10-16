//! Page handshake and the encrypted listen API.
//!
//! Flow, mirroring the player:
//!
//! 1. Fetch the track page and decrypt the embedded `__ha_state` payload
//!    (ChaCha20-Poly1305, trailing 32-byte key, zero nonce).
//! 2. Establish an ephemeral X25519 session with the server's static key.
//! 3. Sign each listen payload, derive a fresh nonce from the signature,
//!    encrypt under the session secret, and exchange it for branch keys.

use std::collections::HashMap;

use serde::Deserialize;

use crate::crypto::{self, Error, Result};
use crate::http::{Client, FullResponse};
use crate::signer;

/// Vendor content type for encrypted listen request and response bodies.
pub const CRYPT_CONTENT_TYPE: &str = "application/vnd.hotaudio.crypt+json";

/// One track from the page state.
#[derive(Clone, Debug, Deserialize)]
pub struct Track {
    pub key: String,
    #[serde(default)]
    pub title: String,
}

/// The decrypted `__ha_state` payload.
#[derive(Clone, Debug, Deserialize)]
pub struct PageState {
    pub pid: String,
    pub tick: String,
    /// Server static X25519 public key, hex.
    pub key: String,
    pub tracks: HashMap<String, Track>,
    #[serde(default)]
    pub order: Vec<u64>,
}

/// A decrypted listen response.
#[derive(Clone, Debug, Deserialize)]
pub struct ListenResponse {
    /// The `.hax` container URL. Absent on paging responses.
    #[serde(default)]
    pub url: String,
    /// Branch keys: node index (as a decimal string) to hex key.
    pub keys: HashMap<String, String>,
}

/// A completed handshake, reusable across every listen request for one track.
pub struct Handshake {
    pub state: PageState,
    pub tid: String,
    pub track: Track,
    /// Ephemeral client public key, hex — sent as `X-Key`.
    pub client_pub_hex: String,
    /// SHA-256 of the X25519 shared secret.
    pub secret: [u8; 32],
    pub api_base: String,
    /// Page-URL `?key=` value, forwarded to listen calls (private tracks).
    pub listen_key: Option<String>,
}

/// Extract the raw `__ha_state` value from page HTML.
pub fn extract_ha_state(html: &str) -> Option<String> {
    let marker = "var __ha_state = \"";
    let start = html.find(marker)? + marker.len();
    let rest = &html[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Decrypt the base64 `__ha_state` payload.
pub fn decrypt_state(state_b64: &str) -> Result<PageState> {
    let raw = crypto::base64_decode(state_b64)?;
    if raw.len() < 32 + 16 {
        return Err(Error::State("payload too short".into()));
    }
    let (ct, key) = raw.split_at(raw.len() - 32);
    let key: [u8; 32] = key.try_into().unwrap();
    let plain = crypto::open_zero(&key, ct)?;
    serde_json::from_slice(&plain).map_err(|e| Error::State(e.to_string()))
}

/// Build a handshake from already-fetched page HTML.
pub fn handshake_from_html(
    html: &str,
    api_base: &str,
    track_id: Option<&str>,
) -> Result<Option<Handshake>> {
    let Some(state_b64) = extract_ha_state(html) else {
        return Ok(None);
    };
    let state = decrypt_state(&state_b64).map_err(|e| Error::State(e.to_string()))?;

    let tid = match track_id {
        Some(id) => id.to_string(),
        None => state
            .order
            .first()
            .map(|n| n.to_string())
            .filter(|id| state.tracks.contains_key(id))
            .or_else(|| state.tracks.keys().next().cloned())
            .unwrap_or_default(),
    };
    let Some(track) = state.tracks.get(&tid).cloned() else {
        return Ok(None);
    };

    let session = crypto::key_exchange(&state.key)?;
    Ok(Some(Handshake {
        state,
        tid,
        track,
        client_pub_hex: session.client_pub_hex,
        secret: session.secret,
        api_base: api_base.to_string(),
        listen_key: None,
    }))
}

/// Extract the `?key=` query parameter the player forwards to listen calls.
pub fn extract_listen_key(page_url: &str) -> Option<String> {
    let query = page_url.split_once('?')?.1;
    for pair in query.split('&') {
        if let Some(v) = pair.strip_prefix("key=") {
            return Some(percent_decode(v));
        }
    }
    None
}

/// Minimal percent-decoding (`+` and `%XX`).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Minimal percent-encoding for query values.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Fetch the track page and build a handshake, selecting `track_id` when given.
///
/// `track_id` is the same id `--track` accepts; `None` keeps the page's default
/// (its first track). A requested id that the page does not carry is a protocol
/// error rather than a silent fall-back to the default.
pub async fn load_handshake(
    client: &Client,
    page_url: &str,
    api_base: &str,
    track_id: Option<&str>,
) -> Result<Handshake> {
    let resp = client
        .request("GET", page_url, &[("User-Agent", crate::HOTAUDIO_UA)], None)
        .await?;
    if !(200..300).contains(&resp.status) {
        return Err(Error::Http(format!("track page returned {}", resp.status)));
    }
    let html = String::from_utf8_lossy(&resp.body).into_owned();
    let mut handshake = handshake_from_html(&html, api_base, track_id)?.ok_or_else(|| {
        // `handshake_from_html` returns `None` both for an unreadable page and
        // for a missing track; only the former is a state problem.
        match track_id {
            Some(id) if extract_ha_state(&html).is_some() => {
                Error::Protocol(format!("track {id} not on page"))
            }
            _ => Error::State("page has no decryptable state".into()),
        }
    })?;
    handshake.listen_key = extract_listen_key(page_url);
    Ok(handshake)
}

/// List every track on a page, in page order.
pub fn list_tracks(html: &str) -> Option<Vec<(String, String, String)>> {
    let state = decrypt_state(&extract_ha_state(html)?).ok()?;
    let mut ids: Vec<String> = state
        .order
        .iter()
        .map(|n| n.to_string())
        .filter(|id| state.tracks.contains_key(id))
        .collect();
    for id in state.tracks.keys() {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    Some(
        ids.into_iter()
            .filter_map(|id| {
                state
                    .tracks
                    .get(&id)
                    .map(|t| (id.clone(), t.key.clone(), t.title.clone()))
            })
            .collect(),
    )
}

/// Derive the `.hax` URL from a track key.
///
/// Observed and stable across tracks: `https://cdn.hotaudio.net/a/<key>.hax`.
/// The downloader still uses the server-issued URL.
pub fn hax_url_for_key(track_key: &str) -> String {
    format!("https://cdn.hotaudio.net/a/{track_key}.hax")
}

/// The pause to take before a listen request: `Some` for a non-zero politeness
/// delay, `None` when it is disabled.
fn politeness_pause(delay_ms: u64) -> Option<std::time::Duration> {
    if delay_ms == 0 {
        None
    } else {
        Some(std::time::Duration::from_millis(delay_ms))
    }
}

/// Perform one encrypted listen request.
///
/// `first` is `-1` for the initial call (returns the `.hax` URL), or a segment
/// index to page additional key branches for long tracks.
///
/// `delay_ms` pauses before the request when non-zero. It is politeness only —
/// batch and bulk callers use it to space listen/key-paging load — and never
/// affects correctness (see `docs/PROTOCOL.md` §7).
pub async fn listen(
    client: &Client,
    hs: &Handshake,
    first: i64,
    delay_ms: u64,
) -> Result<ListenResponse> {
    if let Some(pause) = politeness_pause(delay_ms) {
        crate::clock::sleep(pause).await;
    }
    // Field order matters: the server validates the signature over exactly
    // this string, so it must match the player's `JSON.stringify` order.
    let payload = format!(
        r#"{{"tid":{},"pid":{},"key":{},"tick":{},"first":{first}}}"#,
        json_string(&hs.tid),
        json_string(&hs.state.pid),
        json_string(&hs.track.key),
        json_string(&hs.state.tick),
    );

    let signature = signer::sign_payload(&payload).map_err(|e| Error::Sign(e.to_string()))?;
    let sig_bytes = signature.as_bytes();
    let nonce = crypto::sha256(sig_bytes);
    let mut req_nonce = [0u8; 12];
    req_nonce.copy_from_slice(&nonce[..12]);

    let body = crypto::seal_with_nonce(&hs.secret, &req_nonce, payload.as_bytes())?;

    let mut url = format!("{}/api/v1/audio/listen", hs.api_base);
    if let Some(key) = &hs.listen_key {
        url.push_str(&format!("?key={}", percent_encode(key)));
    }

    let headers = [
        ("X-Signature", signature.as_str()),
        ("X-Key", hs.client_pub_hex.as_str()),
        ("Content-Type", CRYPT_CONTENT_TYPE),
        ("User-Agent", crate::HOTAUDIO_UA),
        ("Origin", "https://hotaudio.net"),
        ("Referer", "https://hotaudio.net/"),
    ];
    let resp: FullResponse = client.request("POST", &url, &headers, Some(&body)).await?;

    if !(200..300).contains(&resp.status) {
        let snippet = String::from_utf8_lossy(&resp.body);
        let snippet: String = snippet.chars().take(300).collect();
        return Err(Error::Listen(format!(
            "HTTP {} for first={first}: {snippet}",
            resp.status
        )));
    }

    // Success bodies are encrypted; a plaintext body is an error the caller
    // should see rather than a decrypt failure.
    let content_type = resp.header("content-type").unwrap_or("");
    if !content_type.is_empty() && !content_type.contains("hotaudio.crypt") {
        let snippet: String = String::from_utf8_lossy(&resp.body)
            .chars()
            .take(300)
            .collect();
        return Err(Error::Listen(format!(
            "non-crypt body for first={first}: {snippet}"
        )));
    }

    // The response reuses the session secret with the first nonce byte bumped,
    // separating the request and response nonce domains.
    let mut resp_nonce = req_nonce;
    resp_nonce[0] = resp_nonce[0].wrapping_add(1);
    let plain = crypto::open_with_nonce(&hs.secret, &resp_nonce, &resp.body)?;
    serde_json::from_slice(&plain).map_err(|e| Error::Listen(e.to_string()))
}

/// JSON-encode a string, matching `JSON.stringify` escaping.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Merge branch keys into a key map. Returns the number of *new* keys.
pub fn merge_keys(map: &mut crypto::KeyMap, keys: &HashMap<String, String>) -> usize {
    let mut added = 0;
    for (k, v) in keys {
        let Ok(node) = k.parse::<u32>() else { continue };
        let Ok(bytes) = crypto::hex_decode(v) else {
            continue;
        };
        let Ok(key) = <[u8; 32]>::try_from(bytes.as_slice()) else {
            continue;
        };
        if map.insert(node, key).is_none() {
            added += 1;
        }
    }
    added
}

/// Convert a branch-key map into the typed key map, skipping malformed entries
/// so paging can refetch them.
pub fn keys_to_map(keys: &HashMap<String, String>) -> crypto::KeyMap {
    let mut map = HashMap::new();
    merge_keys(&mut map, keys);
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn extracts_ha_state() {
        let html = r#"<script>var __ha_state = "abc123";</script>"#;
        assert_eq!(extract_ha_state(html).as_deref(), Some("abc123"));
        assert_eq!(extract_ha_state("<html></html>"), None);
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn extracts_listen_key() {
        assert_eq!(
            extract_listen_key("https://hotaudio.net/u/a/b?key=secret123").as_deref(),
            Some("secret123")
        );
        assert_eq!(extract_listen_key("https://hotaudio.net/u/a/b"), None);
        assert_eq!(
            extract_listen_key("https://x/u/a/b?a=1&key=a%20b").as_deref(),
            Some("a b")
        );
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn politeness_delay_is_off_at_zero() {
        assert_eq!(politeness_pause(0), None);
        assert_eq!(
            politeness_pause(500),
            Some(std::time::Duration::from_millis(500))
        );
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn json_string_escapes_like_json_stringify() {
        assert_eq!(json_string("abc"), "\"abc\"");
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_string("a\\b"), "\"a\\\\b\"");
        assert_eq!(json_string("a\nb"), "\"a\\nb\"");
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn hax_url_is_cdn_derived() {
        assert_eq!(
            hax_url_for_key("abc123"),
            "https://cdn.hotaudio.net/a/abc123.hax"
        );
    }

    /// Page HTML whose `__ha_state` decrypts to two tracks, in order 9001, 9002.
    fn multi_track_page() -> String {
        use base64::Engine as _;

        let state = concat!(
            r#"{"pid":"p","tick":"t","key":""#,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            r#"","tracks":{"9001":{"key":"k1","title":"First"},"#,
            r#""9002":{"key":"k2","title":"Second"}},"order":[9001,9002]}"#
        );
        let key = [7u8; 32];
        // `decrypt_state` reads the key from the trailing 32 bytes, so seal first
        // and append it, exactly as the site's payload is laid out.
        let mut raw = crypto::seal_zero(&key, state.as_bytes()).expect("seal state");
        raw.extend_from_slice(&key);
        let encoded = base64::engine::general_purpose::STANDARD.encode(&raw);
        format!(r#"<script>var __ha_state = "{encoded}";</script>"#)
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn handshake_from_html_selects_the_requested_track() {
        let html = multi_track_page();

        let requested = handshake_from_html(&html, "https://hotaudio.net", Some("9002"))
            .expect("state decrypts")
            .expect("track 9002 exists");
        assert_eq!(requested.tid, "9002");
        assert_eq!(requested.track.title, "Second");

        // No id keeps the page default: the first track in `order`.
        let default = handshake_from_html(&html, "https://hotaudio.net", None)
            .expect("state decrypts")
            .expect("default track exists");
        assert_eq!(default.tid, "9001");
        assert_eq!(default.track.title, "First");
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn handshake_from_html_returns_none_for_an_unknown_track() {
        let html = multi_track_page();
        assert!(
            handshake_from_html(&html, "https://hotaudio.net", Some("9999"))
                .expect("state decrypts")
                .is_none()
        );
    }
}
