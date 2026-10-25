//! HTTP transport with bounded retries.
//!
//! Mirrors the original's reliability policy: transient network errors and
//! retryable statuses get up to three attempts with exponential backoff and
//! jitter, honouring `Retry-After` on 429. The retry policy is [`Retry`], and a
//! client can replace it with [`Client::with_retries`].
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
//!
//! Proxies are the native transport's concern: `ureq` reads the standard
//! `ALL_PROXY`/`HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY` variables, and
//! [`Client::with_proxy`] overrides them. The browser uses its own settings.

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

/// Retry policy for a client: attempts and the backoff between them.
///
/// The defaults are the long-standing constants ([`ATTEMPTS`], [`BASE_DELAY_MS`],
/// [`MAX_DELAY_MS`]); override them per client with [`Client::with_retries`].
#[derive(Clone, Copy, Debug)]
pub struct Retry {
    /// Total attempts per request: the initial attempt plus retries. At least 1.
    pub attempts: usize,
    /// Base backoff between retries; doubled per attempt, plus jitter.
    pub base_delay_ms: u64,
    /// Upper bound for a single retry wait (also caps `Retry-After`).
    pub max_delay_ms: u64,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            attempts: ATTEMPTS,
            base_delay_ms: BASE_DELAY_MS,
            max_delay_ms: MAX_DELAY_MS,
        }
    }
}

impl Retry {
    /// Backoff for `attempt` (1-based), honouring `Retry-After` on 429.
    fn backoff(&self, attempt: usize, retry_after: Option<&str>) -> Duration {
        if let Some(raw) = retry_after {
            if let Ok(secs) = raw.trim().parse::<f64>() {
                let ms = (secs * 1000.0).max(0.0);
                if ms.is_finite() {
                    return Duration::from_millis((ms as u64).min(self.max_delay_ms));
                }
            }
        }
        let base = self
            .base_delay_ms
            .saturating_mul(1u64 << (attempt.saturating_sub(1)).min(16));
        Duration::from_millis((base + jitter()).min(self.max_delay_ms))
    }

    /// Cap a sleep so the per-attempt timeout still bounds the whole call.
    fn remaining(&self, started_ms: f64) -> Duration {
        let elapsed = (clock::monotonic_ms() - started_ms).max(0.0) as u64;
        Duration::from_millis(self.max_delay_ms.saturating_sub(elapsed))
    }
}

/// True for transient HTTP statuses worth retrying.
#[must_use]
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
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Case-insensitive header lookup.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// True for 2xx.
    #[must_use]
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Describe a Cloudflare challenge response, if this is one.
    ///
    /// The documented marker is `cf-mitigated: challenge`; the body check
    /// covers a proxy that strips it. A challenge is an environment condition,
    /// not a protocol failure: no HTTP client can answer an interactive gate,
    /// and retrying it only fetches the same page.
    #[must_use]
    pub fn challenge(&self) -> Option<String> {
        if !is_challenge_header(&self.headers) && !is_challenge_body(self.status, &self.body) {
            return None;
        }
        Some(challenge_detail(self.status, self.header("cf-ray")))
    }
}

/// True when Cloudflare marked this response as a challenge.
fn is_challenge_header(headers: &[(String, String)]) -> bool {
    headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("cf-mitigated") && v.trim().eq_ignore_ascii_case("challenge")
    })
}

/// True for the HTML body of a challenge page, when the header is unavailable.
///
/// Only 403/503 can be a challenge gate, and only a small prefix is scanned:
/// challenge pages are a few KiB, while origin error bodies are not.
fn is_challenge_body(status: u16, body: &[u8]) -> bool {
    if status != 403 && status != 503 {
        return false;
    }
    let prefix = &body[..body.len().min(8192)];
    let text = String::from_utf8_lossy(prefix);
    text.contains("Just a moment...")
        || text.contains("cf_chl_opt")
        || text.contains("Enable JavaScript and cookies to continue")
}

/// `"Cloudflare challenge (HTTP 403, cf-ray ...)"`, with the ray when present.
fn challenge_detail(status: u16, ray: Option<&str>) -> String {
    match ray {
        Some(ray) if !ray.is_empty() => {
            format!("Cloudflare challenge (HTTP {status}, cf-ray {ray})")
        }
        _ => format!("Cloudflare challenge (HTTP {status})"),
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
    retry: Retry,
    rewrite: Option<UrlRewrite>,
    /// Overrides the per-request `User-Agent` when set. Needed when a
    /// `cf_clearance` cookie was issued to a browser: Cloudflare binds the
    /// clearance to the user agent that earned it.
    user_agent: Option<String>,
    /// A raw `Cookie` header sent with every request. This is how a caller
    /// reuses a browser's challenge clearance; the crate never obtains one
    /// itself.
    cookie: Option<String>,
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
            retry: Retry::default(),
            rewrite: None,
            user_agent: None,
            cookie: None,
        }
    }

    /// Send `ua` as the `User-Agent` on every request, overriding any
    /// per-request value.
    ///
    /// Set this to the browser's user agent when supplying that browser's
    /// `cf_clearance` cookie with [`Client::with_cookie`]: Cloudflare binds a
    /// clearance to the user agent that earned it.
    #[must_use]
    pub fn with_user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = Some(ua.into());
        self
    }

    /// Send `cookie` as the raw `Cookie` header on every request.
    ///
    /// The crate never solves a challenge itself; a caller that passed one in a
    /// browser can hand the resulting cookie (typically `cf_clearance=...`) to
    /// the client. Cookies may carry credentials, so they are never logged.
    ///
    /// On `wasm32` the browser forbids setting this header, so the transport
    /// drops it; cookie handling belongs to the host's `fetch` there.
    #[must_use]
    pub fn with_cookie(mut self, cookie: impl Into<String>) -> Self {
        self.cookie = Some(cookie.into());
        self
    }

    /// Per-request headers after applying the client-wide user agent and
    /// cookie. A client-wide value replaces a same-named per-request header;
    /// everything else is passed through.
    fn headers<'a>(
        &'a self,
        request: &[(&'a str, &'a str)],
        extra: &[(&'a str, &'a str)],
    ) -> Vec<(&'a str, &'a str)> {
        let mut headers: Vec<(&str, &str)> = Vec::with_capacity(request.len() + extra.len() + 2);
        let replace_ua = self.user_agent.is_some();
        let replace_cookie = self.cookie.is_some();
        headers.extend(request.iter().copied().filter(|(name, _)| {
            (!replace_ua || !name.eq_ignore_ascii_case("user-agent"))
                && (!replace_cookie || !name.eq_ignore_ascii_case("cookie"))
        }));
        if let Some(ua) = self.user_agent.as_deref() {
            headers.push(("User-Agent", ua));
        }
        headers.extend(extra.iter().copied());
        if let Some(cookie) = self.cookie.as_deref() {
            headers.push(("Cookie", cookie));
        }
        headers
    }

    /// Replace the retry policy (attempts and backoff).
    #[must_use]
    pub fn with_retries(mut self, retry: Retry) -> Self {
        self.retry = retry;
        self
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

    /// Route every request through `proxy`, an HTTP(S) CONNECT proxy URL
    /// (`http://host:port`, optionally with credentials).
    ///
    /// Overrides the proxy taken from the environment, which the transport
    /// applies by default (`ALL_PROXY`/`HTTPS_PROXY`/`HTTP_PROXY`, plus
    /// `NO_PROXY`). Returns an error for a malformed URL or an unsupported
    /// scheme (SOCKS needs ureq's `socks-proxy` feature, which is off).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Http`] if the proxy URL is malformed or its scheme is
    /// unsupported.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_proxy(mut self, proxy: &str) -> Result<Self> {
        self.backend = self.backend.with_proxy(proxy)?;
        Ok(self)
    }

    /// `wasm32` has no proxy configuration of its own: the browser's `fetch`
    /// already uses the embedder's settings, and [`Client::with_fetch`] is the
    /// seam for a host that needs one. This returns the client unchanged.
    ///
    /// # Errors
    ///
    /// Never; the `Result` matches the native builder.
    #[cfg(target_arch = "wasm32")]
    pub fn with_proxy(self, _proxy: &str) -> Result<Self> {
        Ok(self)
    }

    fn url(&self, url: &str) -> String {
        match &self.rewrite {
            Some(rewrite) => rewrite(url),
            None => url.to_string(),
        }
    }

    /// Perform a request with retries, returning the response even on a
    /// retryable error status so callers can inspect the error body.
    ///
    /// # Errors
    ///
    /// Returns the last transport error once the retry budget is exhausted.
    pub async fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<FullResponse> {
        let url = self.url(url);
        let merged = self.headers(headers, &[]);
        let req = Request {
            method,
            url: &url,
            headers: &merged,
            body,
        };
        let mut last: Option<Error> = None;
        let attempts = self.retry.attempts.max(1);

        for attempt in 1..=attempts {
            let started = clock::monotonic_ms();
            match self.backend.send(&req).await {
                Ok(resp) => {
                    if !is_retryable(resp.status) {
                        return Ok(resp);
                    }
                    last = Some(Error::Http(format!("HTTP {}", resp.status)));
                    if attempt == attempts {
                        return Ok(resp);
                    }
                    let wait = self
                        .retry
                        .backoff(attempt, resp.header("retry-after"))
                        .min(self.retry.remaining(started));
                    clock::sleep(wait).await;
                }
                Err(err) => {
                    last = Some(err);
                    if attempt == attempts {
                        break;
                    }
                    clock::sleep(
                        self.retry
                            .backoff(attempt, None)
                            .min(self.retry.remaining(started)),
                    )
                    .await;
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
    /// `total` is `0` until a `Content-Length` is seen. Returning an error from
    /// it aborts the transfer: that error is propagated as-is and never retried,
    /// since it is the caller's decision (a cancellation, say) rather than a
    /// transport failure.
    ///
    /// # Errors
    ///
    /// Returns the last transport error, an [`Error::Http`] if the transfer
    /// stops short of its announced length, or the error returned by
    /// `on_progress`, which is propagated without retrying.
    // One retry loop whose state (partial buffer, expected length, last error)
    // only makes sense together; splitting it would hide the state machine.
    #[allow(clippy::too_many_lines)]
    pub async fn get_resumable(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        mut on_progress: impl FnMut(u64, u64) -> Result<()>,
    ) -> Result<FullResponse> {
        let url = self.url(url);
        let mut buf: Vec<u8> = Vec::new();
        let mut status: u16 = 0;
        let mut resp_headers: Vec<(String, String)> = Vec::new();
        let mut expected: Option<u64> = None;
        let mut last: Option<Error> = None;
        let attempts = self.retry.attempts.max(1);

        for attempt in 1..=attempts {
            let started = clock::monotonic_ms();
            let resume_at = buf.len();
            let range = format!("bytes={resume_at}-");
            let extra: Vec<(&str, &str)> = if resume_at > 0 {
                vec![("Range", range.as_str())]
            } else {
                Vec::new()
            };
            let merged = self.headers(headers, &extra);
            let req = Request {
                method: "GET",
                url: &url,
                headers: &merged,
                body: None,
            };

            let (head, mut body) = match self.backend.send_streaming(&req).await {
                Ok(pair) => pair,
                Err(err) => {
                    last = Some(err);
                    if attempt == attempts {
                        break;
                    }
                    clock::sleep(
                        self.retry
                            .backoff(attempt, None)
                            .min(self.retry.remaining(started)),
                    )
                    .await;
                    continue;
                }
            };
            let code = head.status;

            // A challenge gate is not a container: returning it as a short
            // transfer would retry (and refetch the gate). Surface it directly.
            if let Some(detail) = head.challenge() {
                return Err(Error::Challenge(detail));
            }

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
                    on_progress(buf.len() as u64, total)
                })
                .await
            {
                // Clean end of body. A body that stops short of the announced
                // length is truncated, not complete: resume for the remainder
                // rather than accept a short object.
                Ok(()) => {
                    if expected.is_some_and(|e| (buf.len() as u64) < e) && attempt != attempts {
                        clock::sleep(
                            self.retry
                                .backoff(attempt, None)
                                .min(self.retry.remaining(started)),
                        )
                        .await;
                        continue;
                    }
                    break;
                }
                Err(err) => {
                    // `pump` fails two ways: a transport error, always `Http`,
                    // which is retryable; or an error handed back by
                    // `on_progress`, which is the caller's own decision and must
                    // not be retried — a cancellation would otherwise re-run the
                    // whole transfer. Propagate the latter as-is.
                    if !matches!(err, Error::Http(_)) {
                        return Err(err);
                    }
                    last = Some(err);
                    if attempt == attempts {
                        break;
                    }
                    clock::sleep(
                        self.retry
                            .backoff(attempt, None)
                            .min(self.retry.remaining(started)),
                    )
                    .await;
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
        on_progress(buf.len() as u64, buf.len() as u64)?;
        Ok(FullResponse {
            status,
            headers: resp_headers,
            body: buf,
        })
    }
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

impl Head {
    /// The header-only challenge check: a streaming caller has not read the
    /// body yet, and dropping it is the right response anyway.
    fn challenge(&self) -> Option<String> {
        is_challenge_header(&self.headers).then(|| {
            let ray = self
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("cf-ray"))
                .map(|(_, v)| v.as_str());
            challenge_detail(self.status, ray)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn backoff_grows_and_is_capped() {
        let retry = Retry::default();
        let first = retry.backoff(1, None);
        let second = retry.backoff(2, None);
        assert!(second > first, "backoff did not grow: {first:?} {second:?}");
        assert!(
            retry.backoff(9, None) <= Duration::from_millis(MAX_DELAY_MS),
            "backoff escaped the cap"
        );
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn retry_after_overrides_backoff() {
        let retry = Retry::default();
        let d = retry.backoff(1, Some("2"));
        assert_eq!(d, Duration::from_millis(2_000));
        // Absurd values are clamped rather than slept through.
        assert_eq!(
            retry.backoff(1, Some("99999")),
            Duration::from_millis(MAX_DELAY_MS)
        );
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn retry_defaults_match_the_constants() {
        let retry = Retry::default();
        assert_eq!(retry.attempts, ATTEMPTS);
        assert_eq!(retry.base_delay_ms, BASE_DELAY_MS);
        assert_eq!(retry.max_delay_ms, MAX_DELAY_MS);
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
    fn challenge_detection_uses_the_header_then_the_body() {
        let header = FullResponse {
            status: 403,
            headers: vec![
                ("Cf-Mitigated".into(), "challenge".into()),
                ("cf-ray".into(), "abc".into()),
            ],
            body: Vec::new(),
        };
        assert_eq!(
            header.challenge().as_deref(),
            Some("Cloudflare challenge (HTTP 403, cf-ray abc)")
        );

        let body = FullResponse {
            status: 503,
            headers: Vec::new(),
            body: b"<title>Just a moment...</title>".to_vec(),
        };
        assert!(body.challenge().is_some());

        // A plain origin 403 must stay a normal HTTP failure.
        let origin = FullResponse {
            status: 403,
            headers: Vec::new(),
            body: b"forbidden".to_vec(),
        };
        assert!(origin.challenge().is_none());
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn client_headers_pair_the_cookie_with_the_user_agent() {
        let client = Client::api()
            .with_user_agent("Brave/156")
            .with_cookie("cf_clearance=abc");
        let merged = client.headers(
            &[("User-Agent", "Mozilla/5.0"), ("Accept", "*/*")],
            &[("Range", "bytes=0-")],
        );
        assert!(merged.contains(&("User-Agent", "Brave/156")));
        assert!(!merged.contains(&("User-Agent", "Mozilla/5.0")));
        assert!(merged.contains(&("Cookie", "cf_clearance=abc")));
        assert!(merged.contains(&("Accept", "*/*")));
        assert!(merged.contains(&("Range", "bytes=0-")));
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn client_without_overrides_keeps_the_callers_user_agent() {
        let client = Client::api();
        let merged = client.headers(&[("User-Agent", "Mozilla/5.0")], &[]);
        assert!(merged.contains(&("User-Agent", "Mozilla/5.0")));
        assert!(
            !merged
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("cookie"))
        );
    }

    /// Proxies are the native transport's concern: on `wasm32`
    /// [`Client::with_proxy`] is a no-op, so parsing only means anything here.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn proxy_urls_are_validated() {
        assert!(Client::api().with_proxy("http://127.0.0.1:8080").is_ok());
        // SOCKS needs ureq's `socks-proxy` feature, which this build keeps off.
        assert!(Client::api().with_proxy("socks5://127.0.0.1:1080").is_err());
        assert!(Client::api().with_proxy("not a url").is_err());
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
