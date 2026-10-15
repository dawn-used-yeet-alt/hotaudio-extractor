//! HTTP transport with bounded retries.
//!
//! Mirrors the original's reliability policy: transient network errors and
//! retryable statuses get up to three attempts with exponential backoff and
//! jitter, honouring `Retry-After` on 429.
//!
//! Two clients are used. Small API calls (page, listen) go through
//! [`Client::api`], with a 30 s per-attempt timeout, so one stalled request —
//! observed live at 66 s — degrades into a bounded retry instead of an
//! unbounded stall. The bulk `.hax` transfer uses [`Client::bulk`], which has
//! no global timeout, because a large container over a slow link legitimately
//! takes minutes.
//!
//! The policy lives here and is shared by both targets; only the transport
//! underneath differs. Native uses blocking `ureq` (`ureq_backend`), the browser
//! uses `fetch` (`fetch_backend`). Neither target can block the other, so the
//! entry points are `async`, and a native future never actually suspends — see
//! `exec::block_on`.

use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
#[path = "ureq_backend.rs"]
mod backend;
#[cfg(target_arch = "wasm32")]
#[path = "fetch_backend.rs"]
mod backend;

use crate::clock;
use crate::crypto::{Error, Result};

/// Total attempts per request (initial attempt plus retries).
pub const ATTEMPTS: usize = 3;
/// Base backoff between retries; doubled per attempt, plus jitter.
pub const BASE_DELAY_MS: u64 = 500;
/// Upper bound for a single retry wait (also caps `Retry-After`).
pub const MAX_DELAY_MS: u64 = 10_000;
/// Default per-attempt timeout for small API calls.
pub const API_TIMEOUT_MS: u64 = 30_000;

/// True for transient HTTP statuses worth retrying.
pub fn is_retryable(status: u16) -> bool {
    matches!(status, 401 | 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

/// One HTTP response, with headers preserved.
pub struct FullResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl FullResponse {
    /// The body decoded as UTF-8, lossily.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// True for 2xx.
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Rewrites a request URL before it is sent. The seam for CDNs, mirrors and
/// proxies: the protocol's URLs are absolute and server-issued, so a deployment
/// that fronts the site with its own origin needs this rather than a fork.
#[cfg(not(target_arch = "wasm32"))]
type UrlRewrite = Box<dyn Fn(&str) -> String + Send + Sync + 'static>;
/// `wasm32` has one thread and JS values are not `Send`, so the bound is
/// dropped there. The signature is otherwise identical.
#[cfg(target_arch = "wasm32")]
type UrlRewrite = Box<dyn Fn(&str) -> String + 'static>;

/// An HTTP client: a transport plus the request policy applied to it.
///
/// Cheap to clone-free by reference and reusable across a whole download, the
/// way the old `ureq::Agent` was — connection pooling is the transport's job.
pub struct Client {
    backend: backend::Transport,
    rewrite: Option<UrlRewrite>,
}

impl Client {
    /// Client for small API calls: 30 s per-attempt timeout.
    #[must_use]
    pub fn api() -> Self {
        Self::with_timeout_ms(Some(API_TIMEOUT_MS))
    }

    /// Client for bulk `.hax` transfers: no global timeout.
    #[must_use]
    pub fn bulk() -> Self {
        Self::with_timeout_ms(None)
    }

    /// Client with an explicit per-attempt timeout, or `None` for none.
    #[must_use]
    pub fn with_timeout_ms(timeout_ms: Option<u64>) -> Self {
        Self {
            backend: backend::Transport::new(timeout_ms),
            rewrite: None,
        }
    }

    /// Send every request through `rewrite`, which maps a URL to the URL to
    /// actually fetch. Defaults to the identity.
    ///
    /// Split by target because a `Client` is `Send` natively and need not be on
    /// `wasm32`, where a rewrite usually captures a JS function.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn with_url_rewrite(
        mut self,
        rewrite: impl Fn(&str) -> String + Send + Sync + 'static,
    ) -> Self {
        self.rewrite = Some(Box::new(rewrite));
        self
    }

    /// Send every request through `rewrite`, which maps a URL to the URL to
    /// actually fetch. Defaults to the identity.
    #[cfg(target_arch = "wasm32")]
    #[must_use]
    pub fn with_url_rewrite(mut self, rewrite: impl Fn(&str) -> String + 'static) -> Self {
        self.rewrite = Some(Box::new(rewrite));
        self
    }

    /// Replace the browser's global `fetch`.
    ///
    /// For hosts whose HTTP does not go through the page — a Capacitor native
    /// bridge, a proxy-aware shim — without forking the crate. The replacement
    /// must take `(url, init)` and resolve to a `Response`.
    #[cfg(target_arch = "wasm32")]
    #[must_use]
    pub fn with_fetch(mut self, fetch: wasm_bindgen::JsValue) -> Self {
        self.backend = self.backend.with_fetch(fetch);
        self
    }

    fn url(&self, url: &str) -> String {
        match &self.rewrite {
            Some(rewrite) => rewrite(url),
            None => url.to_string(),
        }
    }

    /// Perform a request with retries, returning the response even on a
    /// retryable error status so callers can inspect the error body.
    pub async fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<FullResponse> {
        let req = Request {
            method,
            url: &self.url(url),
            headers,
            body,
        };
        let mut last: Option<Error> = None;

        for attempt in 1..=ATTEMPTS {
            let started = clock::monotonic_ms();
            match self.backend.send(&req).await {
                Ok(resp) => {
                    if !is_retryable(resp.status) {
                        return Ok(resp);
                    }
                    last = Some(Error::Http(format!("HTTP {}", resp.status)));
                    if attempt == ATTEMPTS {
                        return Ok(resp);
                    }
                    let wait = backoff(attempt, resp.header("retry-after")).min(remaining(started));
                    clock::sleep(wait).await;
                }
                Err(err) => {
                    last = Some(err);
                    if attempt == ATTEMPTS {
                        break;
                    }
                    clock::sleep(backoff(attempt, None).min(remaining(started))).await;
                }
            }
        }
        Err(last.unwrap_or_else(|| Error::Http("request failed".into())))
    }

    /// Fetch a large body, resuming with `Range` if the transfer drops
    /// mid-stream.
    ///
    /// The container is the only large transfer in the protocol and it is
    /// routinely 15–25 MB over a path that can be slow or intermittently lossy.
    /// Retrying a dropped body from byte zero re-spends everything already
    /// received, so on a flaky link a single download degrades into several
    /// full-length transfers.
    ///
    /// This keeps the bytes already read and asks only for the remainder. The
    /// CDN honours `Range` (it answers `206`); if a server ignores it and
    /// replies `200` with the whole body, the partial buffer is discarded and
    /// the request restarts, so correctness does not depend on range support.
    ///
    /// `on_progress` is called with `(received, total)` as data arrives, where
    /// `total` is `0` until a `Content-Length` is seen.
    pub async fn get_resumable(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        mut on_progress: impl FnMut(u64, u64),
    ) -> Result<FullResponse> {
        let url = self.url(url);
        let mut buf: Vec<u8> = Vec::new();
        let mut status: u16 = 0;
        let mut resp_headers: Vec<(String, String)> = Vec::new();
        let mut expected: Option<u64> = None;
        let mut last: Option<Error> = None;

        for attempt in 1..=ATTEMPTS {
            let started = clock::monotonic_ms();
            let resume_at = buf.len();
            let range = format!("bytes={resume_at}-");
            let mut request_headers: Vec<(&str, &str)> = headers.to_vec();
            if resume_at > 0 {
                request_headers.push(("Range", range.as_str()));
            }
            let req = Request {
                method: "GET",
                url: &url,
                headers: &request_headers,
                body: None,
            };

            let (head, mut body) = match self.backend.send_streaming(&req).await {
                Ok(pair) => pair,
                Err(err) => {
                    last = Some(err);
                    if attempt == ATTEMPTS {
                        break;
                    }
                    clock::sleep(backoff(attempt, None).min(remaining(started))).await;
                    continue;
                }
            };
            let code = head.status;

            // A ranged request that runs past the end yields 416; the body is
            // already complete, so accept it rather than discarding work.
            if resume_at > 0 && code == 416 {
                break;
            }
            // A server that ignores `Range` resends from the start; drop the
            // partial buffer so the body stays one contiguous copy of the
            // object.
            let ignored_range = resume_at > 0 && code == 200;
            if ignored_range {
                buf.clear();
            }
            if !(200..300).contains(&code) && !is_retryable(code) {
                return Err(Error::Http(format!(".hax fetch returned {code}")));
            }

            status = code;
            resp_headers = head.headers;
            // A 206 body is only the remainder, so its `Content-Length` is added
            // to where it resumes; a 200 (range ignored, buffer cleared) is the
            // whole object, so its offset is zero.
            let body_offset = if ignored_range { 0 } else { resume_at };
            let total = head
                .content_length
                .map_or(0, |cl| cl.saturating_add(body_offset as u64));
            if total > 0 {
                expected = Some(total);
            }

            match body
                .pump(&mut |chunk| {
                    buf.extend_from_slice(chunk);
                    on_progress(buf.len() as u64, total);
                })
                .await
            {
                // Clean end of body. A body that stops short of the announced
                // length is truncated, not complete: resume for the remainder
                // rather than accept a short object.
                Ok(()) => {
                    if expected.is_some_and(|e| (buf.len() as u64) < e) && attempt != ATTEMPTS {
                        clock::sleep(backoff(attempt, None).min(remaining(started))).await;
                        continue;
                    }
                    break;
                }
                Err(err) => {
                    last = Some(err);
                    if attempt == ATTEMPTS {
                        break;
                    }
                    clock::sleep(backoff(attempt, None).min(remaining(started))).await;
                }
            }
        }

        if status == 0 {
            return Err(last.unwrap_or_else(|| Error::Http("request failed".into())));
        }
        // Only a transfer that matches its announced length is complete. A body
        // cut short by a proxy, or a wrong `Content-Length`, would otherwise
        // decrypt into a corrupt MP4.
        if let Some(expected) = expected {
            if buf.len() as u64 != expected {
                return Err(Error::Http(format!(
                    ".hax transfer incomplete: got {} of {expected} bytes",
                    buf.len()
                )));
            }
        }
        on_progress(buf.len() as u64, buf.len() as u64);
        Ok(FullResponse {
            status,
            headers: resp_headers,
            body: buf,
        })
    }
}

/// Cap the sleep so the client's own timeout still bounds the whole call.
fn remaining(started_ms: f64) -> Duration {
    let elapsed = (clock::monotonic_ms() - started_ms).max(0.0) as u64;
    Duration::from_millis(MAX_DELAY_MS.saturating_sub(elapsed))
}

/// Backoff for `attempt` (1-based), honouring `Retry-After` on 429.
fn backoff(attempt: usize, retry_after: Option<&str>) -> Duration {
    if let Some(raw) = retry_after {
        if let Ok(secs) = raw.trim().parse::<f64>() {
            let ms = (secs * 1000.0).max(0.0);
            if ms.is_finite() {
                return Duration::from_millis((ms as u64).min(MAX_DELAY_MS));
            }
        }
    }
    let base = BASE_DELAY_MS.saturating_mul(1u64 << (attempt - 1).min(16));
    Duration::from_millis((base + jitter()).min(MAX_DELAY_MS))
}

/// Cheap 0..100 ms jitter, enough to de-synchronise retries without pulling in
/// an RNG dependency.
fn jitter() -> u64 {
    (clock::monotonic_ms().fract().abs() * 100.0) as u64 % 100
}

/// A request as the transports see it, with the URL already rewritten.
pub(crate) struct Request<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    pub body: Option<&'a [u8]>,
}

/// A response head: what arrived before the body finished streaming.
pub(crate) struct Head {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub content_length: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn backoff_grows_and_is_capped() {
        let first = backoff(1, None);
        let second = backoff(2, None);
        assert!(second > first, "backoff did not grow: {first:?} {second:?}");
        assert!(
            backoff(9, None) <= Duration::from_millis(MAX_DELAY_MS),
            "backoff escaped the cap"
        );
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn retry_after_overrides_backoff() {
        let d = backoff(1, Some("2"));
        assert_eq!(d, Duration::from_millis(2_000));
        // Absurd values are clamped rather than slept through.
        assert_eq!(
            backoff(1, Some("99999")),
            Duration::from_millis(MAX_DELAY_MS)
        );
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn retryable_statuses_are_the_transient_ones() {
        for status in [401, 408, 425, 429, 500, 502, 503, 504] {
            assert!(is_retryable(status), "{status} should be retryable");
        }
        for status in [200, 301, 400, 403, 404, 451] {
            assert!(!is_retryable(status), "{status} should not be retryable");
        }
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn url_rewrite_maps_requests() {
        let client = Client::api().with_url_rewrite(|u| u.replace("hotaudio.net", "proxy.test"));
        assert_eq!(
            client.url("https://hotaudio.net/u/a/b"),
            "https://proxy.test/u/a/b"
        );
        assert_eq!(Client::api().url("https://x/y"), "https://x/y");
    }
}
