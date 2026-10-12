//! Browser transport: the `fetch` API.
//!
//! `wasm32-unknown-unknown` has no sockets, no blocking call and no threads, so
//! the browser build borrows the platform's own HTTP stack. The retry policy,
//! timeouts, resume logic and response handling all live in the parent module;
//! this file only turns a [`Request`] into a fetch and a fetch back into bytes.
//!
//! Two things a browser will not let a page do, both of which the native build
//! depends on, are documented on [`Transport::new`]: setting `User-Agent`, and
//! cross-origin requests at all.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Function, Promise, Reflect, Uint8Array};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::crypto::{Error, Result};
use crate::http::{FullResponse, Head, Request};

/// Header names `fetch` silently drops because they are forbidden: the browser
/// sets them itself, and a page cannot override them.
const FORBIDDEN: [&str; 3] = ["user-agent", "origin", "referer"];

pub(super) struct Transport {
    timeout_ms: Option<u64>,
    fetch: Function,
}

impl Transport {
    pub(super) fn new(timeout_ms: Option<u64>) -> Self {
        let fetch = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("fetch"))
            .and_then(|f| f.dyn_into::<Function>())
            .expect("wasm build: no global fetch");
        Self { timeout_ms, fetch }
    }

    /// Swap in a caller-supplied `fetch`, for hosts whose HTTP does not go
    /// through the page — a Capacitor native bridge, or a proxy-aware shim. It
    /// must have `fetch`'s signature and return a `Response`.
    pub(super) fn with_fetch(mut self, fetch: JsValue) -> Self {
        self.fetch = fetch
            .dyn_into::<Function>()
            .expect("with_fetch: the replacement must be a function");
        self
    }

    /// One attempt, body read into memory.
    pub(super) async fn send(&self, req: &Request<'_>) -> Result<FullResponse> {
        let response = self.dispatch(req).await?;
        let status = response.status();
        let headers = headers(&response);
        let buffer = JsFuture::from(
            response
                .array_buffer()
                .map_err(|e| Error::Http(format!("cannot read response body: {e:?}")))?,
        )
        .await
        .map_err(|e| Error::Http(format!("cannot read response body: {e:?}")))?;
        Ok(FullResponse {
            status,
            headers,
            body: Uint8Array::new(&buffer).to_vec(),
        })
    }

    /// One attempt, body streamed.
    ///
    /// Reads `Response.body` as a stream rather than buffering it, so a 15 MB
    /// container costs one chunk of memory and a dropped transfer is visible
    /// mid-body — which is what makes the parent's resume policy work.
    pub(super) async fn send_streaming(&self, req: &Request<'_>) -> Result<(Head, OpenBody)> {
        let response = self.dispatch(req).await?;
        // `Headers::get` yields `Result<Option<String>, _>` here, unlike the ureq
        // backend's `http` header map. A missing or unreadable `Content-Length`
        // just means progress reports no total.
        let content_length = response
            .headers()
            .get("content-length")
            .ok()
            .flatten()
            .and_then(|v| v.parse::<u64>().ok());
        let head = Head {
            status: response.status(),
            content_length,
            headers: headers(&response),
        };
        let stream = response
            .body()
            .ok_or_else(|| Error::Http("response has no readable body".into()))?;
        let reader: web_sys::ReadableStreamDefaultReader = stream
            .get_reader()
            .dyn_into()
            .map_err(|_| Error::Http("unexpected body reader type".into()))?;
        Ok((head, OpenBody { reader }))
    }

    async fn dispatch(&self, req: &Request<'_>) -> Result<web_sys::Response> {
        let headers = web_sys::Headers::new()
            .map_err(|e| Error::Http(format!("cannot build headers: {e:?}")))?;
        for (k, v) in req.headers {
            if FORBIDDEN.contains(&k.to_ascii_lowercase().as_str()) {
                continue;
            }
            headers
                .append(k, v)
                .map_err(|e| Error::Http(format!("cannot set header {k}: {e:?}")))?;
        }

        let init = web_sys::RequestInit::new();
        init.set_method(req.method);
        init.set_headers(&headers);
        if let Some(body) = req.body {
            init.set_body(&JsValue::from(Uint8Array::from(body)));
        }

        // The per-attempt timeout, expressed the only way a page can: abort the
        // request. Bulk transfers get none, because a large container over a
        // slow link legitimately outlives any fixed budget.
        let controller = self
            .timeout_ms
            .map(|_| web_sys::AbortController::new().expect("AbortController is a browser basic"));
        if let Some(controller) = &controller {
            init.set_signal(Some(&controller.signal()));
        }

        let promise = self
            .fetch
            .call2(&JsValue::NULL, &JsValue::from_str(req.url), &init)
            .map_err(|e| Error::Http(format!("fetch failed: {e:?}")))?;
        let promise: Promise = promise
            .dyn_into::<Promise>()
            .map_err(|_| Error::Http("the injected fetch must return a Promise".into()))?;

        // The abort timer is cancelled if the request settles first, so a page
        // making many calls does not accumulate pending timeouts. The closure is
        // leaked because the timer may still fire after this frame.
        let mut abort_timer: Option<i32> = None;
        if let Some(c) = &controller {
            let ms = self.timeout_ms.unwrap_or_default().min(i32::MAX as u64) as i32;
            let settled = Closure::<dyn FnMut()>::once({
                let c = c.clone();
                move || c.abort()
            });
            let settled_fn: Function = settled
                .as_ref()
                .clone()
                .dyn_into()
                .expect("a Closure is a JS function");
            settled.forget();
            abort_timer = crate::clock::set_timer(&settled_fn, ms).ok();
        }

        let result = JsFuture::from(promise).await;

        if let Some(id) = abort_timer {
            crate::clock::clear_timer(id);
        }

        let value = result.map_err(|_| {
            if controller.is_some_and(|c| c.signal().aborted()) {
                Error::Http(format!(
                    "request aborted after {} ms",
                    self.timeout_ms.unwrap_or_default()
                ))
            } else {
                Error::Http("fetch failed".into())
            }
        })?;

        value
            .dyn_into::<web_sys::Response>()
            .map_err(|_| Error::Http("fetch resolved with something that is not a Response".into()))
    }
}

/// A response body that is still open, so a dropped transfer can be resumed.
pub(super) struct OpenBody {
    reader: web_sys::ReadableStreamDefaultReader,
}

impl OpenBody {
    pub(super) async fn pump(&mut self, on_chunk: &mut dyn FnMut(&[u8])) -> Result<()> {
        // `read` resolves with `{ value, done }`; a rejected promise is a dropped
        // transfer, which is the case the parent's resume policy exists for.
        loop {
            let result = JsFuture::from(self.reader.read())
                .await
                .map_err(|e| Error::Http(format!("body read failed: {e:?}")))?;
            let done = Reflect::get(&result, &JsValue::from_str("done"))
                .map(|v| v.is_truthy())
                .unwrap_or(false);
            if done {
                return Ok(());
            }
            let value = Reflect::get(&result, &JsValue::from_str("value"))
                .map_err(|e| Error::Http(format!("body read failed: {e:?}")))?;
            let chunk: Uint8Array = value
                .dyn_into()
                .map_err(|_| Error::Http("unexpected body chunk type".into()))?;
            if chunk.length() == 0 {
                continue;
            }
            on_chunk(&chunk.to_vec());
        }
    }
}

fn headers(response: &web_sys::Response) -> Vec<(String, String)> {
    let out: Rc<RefCell<Vec<(String, String)>>> = RefCell::new(Vec::new()).into();
    let sink = out.clone();
    // `for_each` is synchronous and does not retain the callback, so the closure
    // is borrowed rather than leaked.
    let collect = Closure::wrap(Box::new(move |value: JsValue, key: JsValue| {
        if let (Some(k), Some(v)) = (key.as_string(), value.as_string()) {
            sink.borrow_mut().push((k, v));
        }
    }) as Box<dyn FnMut(JsValue, JsValue)>);
    let _ = response
        .headers()
        .for_each(collect.as_ref().unchecked_ref::<Function>());
    out.borrow().clone()
}
