//! An explicit track id must survive the online handshake path.
//!
//! The regression: `listen::load_handshake` built the handshake with no track
//! id, so `--track` (and `DownloadOptions::track_id`) was ignored for URL-based
//! downloads and the page's first track was always chosen. `load_handshake`
//! fetches the page, so the only offline way to drive it is the browser
//! transport's injectable `fetch`; the page state is sealed exactly as the site
//! seals it, so the handshake decrypts it for real.
//!
//! ```bash
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
//!   cargo test --target wasm32-unknown-unknown --test track_selection
//! ```

#![cfg(target_arch = "wasm32")]

use base64::Engine as _;
use hotaudio::crypto;
use hotaudio::http::Client;
use hotaudio::listen;
use js_sys::{Function, Reflect};
use wasm_bindgen::JsValue;

/// Page HTML whose `__ha_state` decrypts to two tracks, in order 9001, 9002.
fn multi_track_page() -> String {
    let state = concat!(
        r#"{"pid":"p","tick":"t","key":""#,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        r#"","tracks":{"9001":{"key":"k1","title":"First"},"#,
        r#""9002":{"key":"k2","title":"Second"}},"order":[9001,9002]}"#
    );
    let key = [7u8; 32];
    // `decrypt_state` reads the key from the trailing 32 bytes, so seal first and
    // append it, exactly as the site's payload is laid out.
    let mut raw = crypto::seal_zero(&key, state.as_bytes()).expect("seal state");
    raw.extend_from_slice(&key);
    let encoded = base64::engine::general_purpose::STANDARD.encode(&raw);
    format!(r#"<script>var __ha_state = "{encoded}";</script>"#)
}

/// A client whose `fetch` always answers with `html`.
fn client_serving(html: &str) -> Client {
    Reflect::set(
        &js_sys::global(),
        &JsValue::from_str("__hotaudio_test_page"),
        &JsValue::from_str(html),
    )
    .expect("install page");
    let fetch = Function::new_with_args(
        "url, init",
        "const page = globalThis.__hotaudio_test_page; \
         return Promise.resolve(new Response(page, { \
             status: 200, headers: { 'content-type': 'text/html' } }));",
    );
    Client::api().with_fetch(fetch.into())
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn load_handshake_selects_the_requested_track() {
    let client = client_serving(&multi_track_page());
    let hs = listen::load_handshake(
        &client,
        "https://hotaudio.net/u/a/b?key=secret",
        "https://hotaudio.net",
        Some("9002"),
    )
    .await
    .expect("handshake");

    assert_eq!(hs.tid, "9002");
    assert_eq!(hs.track.title, "Second");
    // The `?key=` is still taken from the page URL, not the state.
    assert_eq!(hs.listen_key.as_deref(), Some("secret"));
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn load_handshake_defaults_to_the_first_track() {
    let client = client_serving(&multi_track_page());
    let hs = listen::load_handshake(
        &client,
        "https://hotaudio.net/u/a/b",
        "https://hotaudio.net",
        None,
    )
    .await
    .expect("handshake");

    assert_eq!(hs.tid, "9001");
    assert_eq!(hs.track.title, "First");
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn load_handshake_rejects_an_unknown_track() {
    let client = client_serving(&multi_track_page());
    let result = listen::load_handshake(
        &client,
        "https://hotaudio.net/u/a/b",
        "https://hotaudio.net",
        Some("9999"),
    )
    .await;

    // `Handshake` is not `Debug`, so `expect_err` is unavailable here.
    let err = match result {
        Ok(_) => panic!("an unknown track must fail"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("9999"), "got {err}");
}
