//! Native transport: blocking `ureq` over rustls.
//!
//! Every entry point here completes before it returns, which is the whole point
//! of the native build: the pipeline is latency-bound, not concurrency-bound, so
//! one connection doing one request at a time is as fast as anything more
//! elaborate and far easier to reason about (see `docs/ARCHITECTURE.md`).

use ureq::{Agent, Proxy, ProxyProtocol};

use crate::crypto::{Error, Result};
use crate::http::{FullResponse, Head, Request};

pub(super) struct Transport {
    agent: Agent,
    timeout_ms: Option<u64>,
}

// The shared client awaits these; this blocking backend has nothing to await,
// and the same method is genuinely async on the browser target.
#[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
impl Transport {
    pub(super) fn new(timeout_ms: Option<u64>) -> Self {
        Self {
            agent: build_agent(timeout_ms, Proxy::try_from_env()),
            timeout_ms,
        }
    }

    /// Rebuild the agent around an explicit proxy, overriding any that came from
    /// the environment.
    pub(super) fn with_proxy(self, proxy: &str) -> Result<Self> {
        let parsed = Proxy::new(proxy)
            .map_err(|e| Error::Protocol(format!("invalid proxy '{proxy}': {e}")))?;
        // `ureq` accepts a SOCKS URL even when the feature is off, then panics on
        // the first request. Reject it here so `--proxy` fails cleanly instead.
        if matches!(
            parsed.protocol(),
            ProxyProtocol::Socks4
                | ProxyProtocol::Socks4A
                | ProxyProtocol::Socks5
                | ProxyProtocol::Socks5h
        ) {
            return Err(Error::Protocol(format!(
                "SOCKS proxy '{proxy}' needs ureq's socks-proxy feature, which this build keeps off; \
                 use an http(s) proxy"
            )));
        }
        Ok(Self {
            agent: build_agent(self.timeout_ms, Some(parsed)),
            timeout_ms: self.timeout_ms,
        })
    }

    /// One attempt, body read into memory.
    pub(super) async fn send(&self, req: &Request<'_>) -> Result<FullResponse> {
        let response = self.dispatch(req)?;
        let status = response.status().as_u16();
        let headers = collect_headers(&response);
        // `read_to_vec` caps at 10 MiB to guard against memory exhaustion, which
        // a `.hax` container routinely exceeds; raise the limit explicitly.
        let bytes = response
            .into_body()
            .with_config()
            .limit(u64::MAX)
            .read_to_vec()
            .map_err(|e| Error::Http(e.to_string()))?;
        Ok(FullResponse {
            status,
            headers,
            body: bytes,
        })
    }

    /// One attempt, body left open so it can stream.
    pub(super) async fn send_streaming(&self, req: &Request<'_>) -> Result<(Head, OpenBody)> {
        let response = self.dispatch(req)?;
        let head = Head {
            status: response.status().as_u16(),
            content_length: response
                .headers()
                .get("content-length")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok()),
            headers: collect_headers(&response),
        };
        Ok((
            head,
            OpenBody {
                body: response.into_body(),
            },
        ))
    }

    fn dispatch(&self, req: &Request<'_>) -> Result<ureq::http::Response<ureq::Body>> {
        // ureq 3 is typed: GET uses the no-body builder, anything else the body
        // one.
        let raw = if req.method.eq_ignore_ascii_case("GET") {
            let mut b = self.agent.get(req.url);
            for (k, v) in req.headers {
                b = b.header(*k, *v);
            }
            b.call()
        } else {
            let mut b = self.agent.post(req.url);
            for (k, v) in req.headers {
                b = b.header(*k, *v);
            }
            b.send(req.body.unwrap_or_default().to_vec())
        };
        // `http_status_as_error(false)` means a status is never this error; the
        // arm keeps a config change degrading into a message, not a panic.
        raw.map_err(|e| Error::Http(e.to_string()))
    }
}

/// A response body that is still open, so a dropped transfer can be resumed.
pub(super) struct OpenBody {
    body: ureq::Body,
}

// Awaited by the shared streaming loop; reading a blocking body never suspends.
#[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
impl OpenBody {
    pub(super) async fn pump(
        &mut self,
        on_chunk: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<()> {
        use std::io::Read;
        // `into_reader` is unlimited; the explicit limit keeps that true if the
        // default ever changes, and states the intent.
        let mut reader = self.body.with_config().limit(u64::MAX).reader();
        let mut chunk = vec![0u8; 256 * 1024];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => return Ok(()),
                Ok(n) => on_chunk(&chunk[..n])?,
                Err(e) => return Err(Error::Http(e.to_string())),
            }
        }
    }
}

/// Build an agent with the shared policy and, when given, an explicit proxy.
///
/// `proxy` is `Some` for `--proxy` and for the environment variables read at
/// construction; `None` means a direct connection.
fn build_agent(timeout_ms: Option<u64>, proxy: Option<Proxy>) -> Agent {
    let config = Agent::config_builder()
        .user_agent(crate::HOTAUDIO_UA)
        // Non-2xx is a normal outcome here: callers inspect statuses and read
        // the error body, which the default error path discards.
        .http_status_as_error(false)
        .proxy(proxy);
    let config = match timeout_ms {
        Some(ms) => config.timeout_global(Some(std::time::Duration::from_millis(ms))),
        None => config,
    };
    config.build().into()
}

fn collect_headers(response: &ureq::http::Response<ureq::Body>) -> Vec<(String, String)> {
    response
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                v.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}
