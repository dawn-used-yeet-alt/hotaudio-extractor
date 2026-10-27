//! Offline tests for the binding surface.
//!
//! These exercise the same functions the generated bindings call, without
//! crossing a foreign runtime. The golden vector is the first entry of the
//! core crate's `tests/data/signer_golden.json`; if the signer is recaptured,
//! this one must move with it.

use std::sync::Arc;

use hotaudio_ffi::{
    CancellationToken, DownloadOptions, FfiError, decrypt_hax, list_tracks, sign_payload_at,
    version,
};

const PAYLOAD: &str = r#"{"tid":"1","pid":"2","key":"k","tick":"t","first":-1}"#;
const SIGNATURE: &str = "10:000000006a389a76bb60f47800bb60f6";

#[test]
fn sign_payload_at_matches_the_core_vector() {
    assert_eq!(
        sign_payload_at(PAYLOAD.to_string(), 0.0).expect("sign"),
        SIGNATURE
    );
}

#[test]
fn list_tracks_tolerates_html_without_state() {
    assert!(list_tracks("<html><body>not a track page</body></html>".to_string()).is_empty());
}

#[test]
fn cancellation_token_is_shareable_and_resettable() {
    let token = CancellationToken::new();
    assert!(!token.is_cancelled());
    token.cancel();
    assert!(token.is_cancelled());
    token.reset();
    assert!(!token.is_cancelled());
}

#[test]
fn decrypt_hax_rejects_non_containers_as_a_core_error() {
    let keys = format!("{{\"0\":\"{}\"}}", "00".repeat(32));
    let error = decrypt_hax(b"not a hax0 container".to_vec(), keys).expect_err("must reject");
    match error {
        FfiError::Failed { code, .. } => assert_eq!(code, "protocol"),
        FfiError::Panic { .. } => panic!("a parse error must not become a panic"),
    }
}

#[test]
fn options_are_optional_and_defaultable() {
    let options = DownloadOptions::default();
    assert!(options.track_id.is_none());
    assert!(options.api_base.is_none());
    assert_eq!(options.delay_ms, 0);
    assert!(options.initial_keys_json.is_none());
}

#[test]
fn version_matches_the_crate() {
    assert_eq!(version(), env!("CARGO_PKG_VERSION"));
}

#[test]
fn a_token_can_be_shared_across_threads() {
    let token: Arc<CancellationToken> = CancellationToken::new();
    let worker = Arc::clone(&token);
    let handle = std::thread::spawn(move || {
        while !worker.is_cancelled() {
            std::thread::yield_now();
        }
    });
    token.cancel();
    handle.join().expect("worker joins");
}
