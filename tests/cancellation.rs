//! Cancellation through the wasm surface.
//!
//! `downloadTrackWith` takes a trailing `AbortSignal`; aborting it must stop the
//! download and reject with an `AbortError`, which is the name `signal`-aware JS
//! already treats as a deliberate cancel rather than a failure.
//!
//! ```bash
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
//!   cargo test --target wasm32-unknown-unknown --test cancellation
//! ```

#![cfg(target_arch = "wasm32")]

use js_sys::{Function, Reflect};
use wasm_bindgen::JsValue;

/// The token is checked before the handshake, so an already-aborted signal
/// rejects without a single request leaving the page.
#[wasm_bindgen_test::wasm_bindgen_test]
async fn an_aborted_signal_rejects_with_abort_error() {
    let controller = web_sys::AbortController::new().expect("AbortController");
    controller.abort();

    let err = hotaudio::wasm::download_track_with(
        "https://hotaudio.net/u/user/track",
        Function::new_no_args(""),
        None,
        None,
        None,
        None,
        Some(controller.signal()),
    )
    .await
    .expect_err("an aborted signal must reject");

    let name = Reflect::get(&err, &JsValue::from_str("name"))
        .ok()
        .and_then(|v| v.as_string());
    assert_eq!(name.as_deref(), Some("AbortError"));
}
