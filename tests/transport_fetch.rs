//! Transport tests for the browser build, driving `fetch_backend` directly.
//!
//! Compile-only checks prove the wasm transport type-checks; they say nothing
//! about whether it *works*. These run the real code path — header
//! construction, forbidden-header dropping, streaming body reads — against a
//! stub `fetch` injected through `Client::with_fetch`, which is the same seam a
//! Capacitor host or a proxy shim uses. No browser, no network.
//!
//! ```bash
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
//!   cargo test --target wasm32-unknown-unknown --test transport_fetch
//! ```

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;

use hotaudio::http::Client;
use js_sys::{Array, Function, JsString, Object, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};

/// Where the stub `fetch` finds the recorder, on the global scope.
const RECORDER_KEY: &str = "__hotaudio_test_recorder";

/// A JS object the stub `fetch` writes its observations into.
struct Recorder(Object);

impl Recorder {
    fn new() -> Self {
        Self(Object::new())
    }

    /// Set the reply the stub should produce.
    fn reply(&self, status: u16, body: Vec<u8>, headers: &[(&str, &str)]) {
        let set = |key: &str, value: JsValue| {
            Reflect::set(&self.0, &JsString::from(key), &value).expect("set field");
        };
        set("status", JsValue::from_f64(f64::from(status)));
        set("body", Uint8Array::from(body.as_slice()).into());
        let pairs: Array = headers
            .iter()
            .map(|(k, v)| {
                let pair = Array::new();
                pair.push(&JsString::from(*k));
                pair.push(&JsString::from(*v));
                pair
            })
            .collect();
        set("headerPairs", pairs.into());
    }

    /// A `fetch` that records into this recorder and replies with the configured
    /// status, body and headers. `stream` chooses whether the body arrives as a
    /// `ReadableStream`, which is the path `get_resumable` needs.
    fn fetch(&self, stream: bool) -> Function {
        Reflect::set(
            &self.0,
            &JsString::from("stream"),
            &JsValue::from_bool(stream),
        )
        .expect("set stream");
        // The recorder lives on the global, so the stub needs no bound arguments:
        // `bind` on an arrow function loses its parameter list in ways that are
        // easy to get wrong.
        Reflect::set(
            &js_sys::global(),
            &JsString::from(RECORDER_KEY),
            &self.0.clone().into(),
        )
        .expect("install recorder");

        // `new_with_args` names the parameters and takes the body separately.
        // `new_no_args` would build a *zero*-argument function, so wrapping the
        // stub in `return (…)` yields the inner function rather than its result.
        Function::new_with_args(
            "url, init",
            "\
              const rec = globalThis.__hotaudio_test_recorder;
              rec.method = init.method;
              rec.url = String(url);
              rec.rawBody = init.body ? Array.from(new Uint8Array(init.body)) : [];
              rec.headers = Array.from(init.headers);
              const payload = new Uint8Array(rec.body);
              const opts = { status: rec.status, headers: new Headers(rec.headerPairs) };
              const res = rec.stream
                ? new Response(new ReadableStream({ start(c) {
                    c.enqueue(payload);
                    c.close();
                  }}), opts)
                : new Response(payload, opts);
              return Promise.resolve(res);",
        )
    }

    fn str(&self, key: &str) -> String {
        Reflect::get(&self.0, &JsString::from(key))
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default()
    }

    fn bytes(&self, key: &str) -> Vec<u8> {
        Reflect::get(&self.0, &JsString::from(key))
            .ok()
            .and_then(|v| v.dyn_into::<Array>().ok())
            .map(|a| {
                a.iter()
                    .filter_map(|n| n.as_f64().map(|v| v as u8))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Request headers as `(name, value)`, lower-cased names.
    fn headers(&self) -> Vec<(String, String)> {
        Reflect::get(&self.0, &JsString::from("headers"))
            .ok()
            .and_then(|v| v.dyn_into::<Array>().ok())
            .map(|a| {
                a.iter()
                    .filter_map(|pair| {
                        let pair = pair.dyn_into::<Array>().ok()?;
                        let name = pair.get(0).as_string()?.to_ascii_lowercase();
                        let value = pair.get(1).as_string()?;
                        Some((name, value))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn client(rec: &Recorder, stream: bool) -> Client {
    Client::api().with_fetch(rec.fetch(stream).into())
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn carries_method_url_and_body() {
    let rec = Recorder::new();
    rec.reply(200, b"pong".to_vec(), &[]);

    let resp = client(&rec, false)
        .request(
            "POST",
            "https://example.test/thing",
            &[("X-Signature", "abc")],
            Some(b"ping"),
        )
        .await
        .expect("request");

    assert_eq!(resp.status, 200);
    assert_eq!(resp.text(), "pong");
    assert_eq!(rec.str("method"), "POST");
    assert_eq!(rec.str("url"), "https://example.test/thing");
    assert_eq!(rec.bytes("rawBody"), b"ping");
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn preserves_response_headers_for_lookup() {
    let rec = Recorder::new();
    rec.reply(
        200,
        vec![],
        &[("Content-Type", "application/vnd.hotaudio.crypt+json")],
    );

    let resp = client(&rec, false)
        .request("GET", "https://example.test/", &[], None)
        .await
        .unwrap();
    // Case-insensitive, matching the ureq transport.
    assert_eq!(
        resp.header("content-type"),
        Some("application/vnd.hotaudio.crypt+json")
    );
    assert!(resp.ok());
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn drops_forbidden_headers_but_keeps_real_ones() {
    let rec = Recorder::new();
    rec.reply(200, vec![], &[]);

    client(&rec, false)
        .request(
            "GET",
            "https://example.test/",
            &[
                ("User-Agent", "Mozilla/5.0"),
                ("Origin", "https://hotaudio.net"),
                ("Range", "bytes=0-15"),
            ],
            None,
        )
        .await
        .unwrap();

    let headers = rec.headers();
    let names: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
    // A browser sets these itself and rejects a page's attempt to; claiming to
    // send them would be a lie the transport tells.
    assert!(
        !names.contains(&"user-agent"),
        "sent User-Agent: {headers:?}"
    );
    assert!(!names.contains(&"origin"), "sent Origin: {headers:?}");
    assert!(names.contains(&"range"), "dropped Range: {headers:?}");
    assert_eq!(
        headers
            .iter()
            .find(|(k, _)| k == "range")
            .map(|(_, v)| v.as_str()),
        Some("bytes=0-15")
    );
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn streams_a_large_body_through_the_resumable_path() {
    let rec = Recorder::new();
    let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    rec.reply(206, body.clone(), &[("content-length", "300000")]);

    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    let resp = Client::bulk()
        .with_fetch(rec.fetch(true).into())
        .get_resumable("https://cdn.test/a.hax", &[], move |loaded, total| {
            sink.borrow_mut().push((loaded, total));
        })
        .await
        .expect("resumable fetch");

    assert_eq!(resp.status, 206);
    assert_eq!(resp.body, body, "streamed body must be byte-identical");

    let samples = seen.borrow();
    assert!(!samples.is_empty(), "no progress reported");
    assert!(
        samples.windows(2).all(|w| w[1].0 >= w[0].0),
        "progress went backwards: {samples:?}"
    );
    assert_eq!(samples.last().expect("samples").0, body.len() as u64);
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn a_non_2xx_status_is_returned_not_retried_forever() {
    let rec = Recorder::new();
    rec.reply(404, b"nope".to_vec(), &[]);

    let resp = client(&rec, false)
        .request("GET", "https://example.test/", &[], None)
        .await
        .unwrap();
    assert_eq!(resp.status, 404);
    assert!(!resp.ok());
    // The error body survives, which the native transport only manages because
    // `http_status_as_error(false)` is set.
    assert_eq!(resp.text(), "nope");
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn url_rewrite_maps_the_request() {
    let rec = Recorder::new();
    rec.reply(200, vec![], &[]);

    client(&rec, false)
        .with_url_rewrite(|url| url.replace("hotaudio.net", "proxy.test"))
        .request("GET", "https://hotaudio.net/u/a/b", &[], None)
        .await
        .unwrap();
    assert_eq!(rec.str("url"), "https://proxy.test/u/a/b");
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn a_range_read_sends_the_range_header() {
    use hotaudio::range::{HttpRangeSource, probe_header};

    // A minimal but genuinely valid HAX0 prefix: magic, file length, header
    // length, reserved.
    let prefix: Vec<u8> = [
        b"HAX0".to_vec(),
        16u32.to_le_bytes().to_vec(),
        16u32.to_le_bytes().to_vec(),
        0u32.to_le_bytes().to_vec(),
    ]
    .concat();

    let rec = Recorder::new();
    rec.reply(206, prefix.clone(), &[("content-length", "16")]);

    let client = client(&rec, false);
    let source = HttpRangeSource::new(&client, "https://cdn.test/a.hax");
    let (file_length, header_length) = probe_header(&source).await.expect("probe");

    assert_eq!(file_length, 16);
    assert_eq!(header_length, 16);
    let range = rec
        .headers()
        .into_iter()
        .find(|(k, _)| k == "range")
        .map(|(_, v)| v);
    assert_eq!(range.as_deref(), Some("bytes=0-15"));
}
