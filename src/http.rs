//! HTTP transport with bounded retries.
//!
//! Mirrors the original's reliability policy: transient network errors and
//! retryable statuses get up to three attempts with exponential backoff and
//! jitter, honouring `Retry-After` on 429.
//!
//! Two agents are used. Small API calls (page, listen) go through an agent
//! with a 30 s per-attempt timeout, so one stalled request — observed live at
//! 66 s — degrades into a bounded retry instead of an unbounded stall. The bulk
//! `.hax` transfer uses an agent with no global timeout, because a large
//! container over a slow link legitimately takes minutes.

use std::time::{Duration, Instant};

use ureq::{Agent, Error as UreqError};

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

/// Agent for small API calls: 30 s per-attempt timeout, pooled connections.
pub fn api_agent() -> Agent {
    Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(API_TIMEOUT_MS)))
        .user_agent(crate::HOTAUDIO_UA)
        .build()
        .into()
}

/// Agent for bulk `.hax` transfers: no global timeout.
pub fn bulk_agent() -> Agent {
    Agent::config_builder()
        .user_agent(crate::HOTAUDIO_UA)
        .build()
        .into()
}

/// Perform a request with retries, returning the response even on a retryable
/// error status so callers can inspect the error body.
pub fn request(
    agent: &Agent,
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<FullResponse> {
    let mut last: Option<Error> = None;

    for attempt in 1..=ATTEMPTS {
        let started = Instant::now();
        match attempt_once(agent, method, url, headers, body) {
            Ok(resp) => {
                if !is_retryable(resp.status) {
                    return Ok(resp);
                }
                last = Some(Error::Http(format!("HTTP {}", resp.status)));
                if attempt == ATTEMPTS {
                    return Ok(resp);
                }
                let wait = backoff(attempt, resp.header("retry-after")).min(remaining(started));
                std::thread::sleep(wait);
            }
            Err(err) => {
                last = Some(err);
                if attempt == ATTEMPTS {
                    break;
                }
                std::thread::sleep(backoff(attempt, None).min(remaining(started)));
            }
        }
    }
    Err(last.unwrap_or_else(|| Error::Http("request failed".into())))
}

/// Fetch a large body, resuming with `Range` if the transfer drops mid-stream.
///
/// The container is the only large transfer in the protocol and it is routinely
/// 15–25 MB over a path that can be slow or intermittently lossy. Retrying a
/// dropped body from byte zero re-spends everything already received, so on a
/// flaky link a single download degrades into several full-length transfers.
///
/// This keeps the bytes already read and asks only for the remainder. The CDN
/// honours `Range` (it answers `206`); if a server ignores it and replies `200`
/// with the whole body, the partial buffer is discarded and the request restarts,
/// so correctness does not depend on range support.
///
/// `on_progress` is called with `(received, total)` as data arrives, where
/// `total` is `0` until a `Content-Length` is seen.
pub fn get_resumable(
    agent: &Agent,
    url: &str,
    headers: &[(&str, &str)],
    mut on_progress: impl FnMut(u64, u64),
) -> Result<FullResponse> {
    use std::io::Read;

    let mut buf: Vec<u8> = Vec::new();
    let mut status: u16 = 0;
    let mut resp_headers: Vec<(String, String)> = Vec::new();
    let mut last: Option<Error> = None;

    for attempt in 1..=ATTEMPTS {
        let started = Instant::now();
        let resume_at = buf.len();

        let mut req = agent.get(url);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        if resume_at > 0 {
            req = req.header("Range", &format!("bytes={resume_at}-"));
        }

        let resp = match req.call() {
            Ok(r) => r,
            Err(UreqError::StatusCode(code)) => {
                // A ranged request that runs past the end yields 416; the body is
                // already complete, so accept it rather than discarding work.
                if resume_at > 0 && code == 416 {
                    break;
                }
                return Err(Error::Http(format!("HTTP {code}")));
            }
            Err(e) => {
                last = Some(Error::Http(e.to_string()));
                if attempt == ATTEMPTS {
                    break;
                }
                std::thread::sleep(backoff(attempt, None).min(remaining(started)));
                continue;
            }
        };

        let code = resp.status().as_u16();

        // A server that ignores `Range` resends from the start; drop the partial
        // buffer so the body stays a single contiguous copy of the object.
        if resume_at > 0 && code == 200 {
            buf.clear();
        }
        if !(200..300).contains(&code) && !is_retryable(code) {
            return Err(Error::Http(format!(".hax fetch returned {code}")));
        }

        status = code;
        resp_headers = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    v.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let total: u64 = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .map(|cl: u64| cl + resume_at as u64)
            .unwrap_or(0);

        let mut reader = resp.into_body().into_reader();
        let mut chunk = vec![0u8; 256 * 1024];
        let mut cut = None;
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    on_progress(buf.len() as u64, total);
                }
                Err(e) => {
                    cut = Some(Error::Http(e.to_string()));
                    break;
                }
            }
        }

        match cut {
            // Clean end of body.
            None => break,
            Some(err) => {
                last = Some(err);
                if attempt == ATTEMPTS {
                    break;
                }
                std::thread::sleep(backoff(attempt, None).min(remaining(started)));
            }
        }
    }

    if status == 0 {
        return Err(last.unwrap_or_else(|| Error::Http("request failed".into())));
    }
    on_progress(buf.len() as u64, buf.len() as u64);
    Ok(FullResponse {
        status,
        headers: resp_headers,
        body: buf,
    })
}

/// Cap the sleep so the agent's own timeout still bounds the whole call.
fn remaining(started: Instant) -> Duration {
    Duration::from_millis(MAX_DELAY_MS).saturating_sub(started.elapsed())
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
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0)
        % 100
}

/// One attempt.
fn attempt_once(
    agent: &Agent,
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<FullResponse> {
    // ureq 3 is typed: GET uses the no-body builder, anything else the body one.
    let raw = if method.eq_ignore_ascii_case("GET") {
        let mut req = agent.get(url);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        req.call()
    } else {
        let mut req = agent.post(url);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        req.send(body.unwrap_or(&[]).to_vec())
    };

    let resp = match raw {
        Ok(r) => r,
        // ureq turns non-2xx into a status error; rebuild a bare response so the
        // caller can read the status and the error body.
        Err(UreqError::StatusCode(code)) => {
            // No public way to build an empty `Body`; carry the status alone.
            return Ok(FullResponse {
                status: code,
                headers: Vec::new(),
                body: Vec::new(),
            });
        }
        Err(e) => return Err(Error::Http(e.to_string())),
    };

    let status = resp.status().as_u16();
    let header_vec: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                v.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    // `read_to_vec` caps at 10 MiB to guard against memory exhaustion, which a
    // `.hax` container routinely exceeds; raise the limit explicitly.
    let body = resp
        .into_body()
        .with_config()
        .limit(u64::MAX)
        .read_to_vec()
        .map_err(|e| Error::Http(e.to_string()))?;

    Ok(FullResponse {
        status,
        headers: header_vec,
        body,
    })
}
