//! `hotaudio-webview` — clear a Cloudflare challenge in a real browser window.
//!
//! The helper opens the track page in the platform webview, gives the user time
//! to pass the challenge, then prints the clearance the downloader needs:
//!
//! ```json
//! {"cookie":"cf_clearance=...","userAgent":"Mozilla/5.0 (...)","state":"...","url":"https://hotaudio.net/..."}
//! ```
//!
//! `state` is the page's `__ha_state` value, so the downloader can rebuild the
//! handshake without fetching the page again and risking another challenge. It
//! is `null` when the window is closed before the page exposes it; the
//! downloader then re-fetches the page with the cookie and User-Agent, which is
//! still a success. The helper is deliberately interactive: it is the escape
//! hatch for a gated network, not an automated challenge solver. See
//! `docs/CLOUDFLARE.md`.

use std::io::Write;
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

const DEFAULT_TIMEOUT_SECS: u64 = 300;
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// How long one script evaluation may stay outstanding before the next poll is
/// allowed anyway.
///
/// A navigation (the challenge redirecting to the track page) can drop the
/// callback for an evaluation that was already in flight. Without this bound
/// the `in_flight` flag would never clear and polling would stop for good.
const POLL_WATCHDOG: Duration = Duration::from_secs(2);

const USAGE: &str = "\
hotaudio-webview --url URL [--timeout SECS]

Opens URL in a webview so a Cloudflare challenge can be passed by hand, then
prints the browser clearance as JSON on stdout. Exit status is 0 on success and
1 on failure or timeout.";

/// The subset of page globals the downloader needs.
#[derive(serde::Deserialize)]
struct Snapshot {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    title: String,
    #[serde(default, rename = "userAgent")]
    user_agent: String,
    #[serde(default)]
    url: String,
}

/// Read the page globals. Returning an object (not a string) lets wry hand the
/// callback one JSON value instead of a doubly encoded one.
const SCRIPT: &str = r#"(() => {
    try {
        return {
            state: (typeof __ha_state === "string" ? __ha_state : null),
            title: document.title || "",
            userAgent: navigator.userAgent || "",
            url: location.href || ""
        };
    } catch (e) {
        return { state: null, title: String(e), userAgent: "", url: "" };
    }
})()"#;

fn main() -> ExitCode {
    let (url, timeout_secs) = match parse_args(std::env::args().skip(1)) {
        Ok(pair) => pair,
        Err(message) => {
            eprintln!("error: {message}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match run(&url, Duration::from_secs(timeout_secs)) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<(String, u64), String> {
    let mut url = None;
    let mut timeout_secs = DEFAULT_TIMEOUT_SECS;
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--url" => {
                url = Some(
                    args.next()
                        .ok_or_else(|| "--url needs a value".to_string())?,
                );
            }
            "--timeout" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--timeout needs a value".to_string())?;
                timeout_secs = value
                    .parse()
                    .map_err(|_| format!("--timeout must be a positive integer, got {value}"))?;
                if timeout_secs == 0 {
                    return Err("--timeout must be at least 1 second".into());
                }
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let url = url.ok_or_else(|| "--url is required".to_string())?;
    Ok((url, timeout_secs))
}

fn run(url: &str, timeout: Duration) -> Result<String, String> {
    // The event loop closure is `'static`, so own the URL before it is built.
    let url = url.to_string();
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("hotaudio clearance")
        .with_inner_size(tao::dpi::LogicalSize::new(1100.0, 800.0))
        .build(&event_loop)
        .map_err(|e| format!("cannot create the window: {e}"))?;
    let builder = WebViewBuilder::new().with_url(url.as_str());

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let webview = builder
        .build(&window)
        .map_err(|e| format!("cannot create the webview: {e}"))?;

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window
            .default_vbox()
            .ok_or_else(|| "the window has no container for the webview".to_string())?;
        builder
            .build_gtk(vbox)
            .map_err(|e| format!("cannot create the webview: {e}"))?
    };

    let (tx, rx) = mpsc::channel::<String>();
    let deadline = Instant::now() + timeout;
    let mut next_poll = Instant::now();
    let mut in_flight_since: Option<Instant> = None;
    let mut last_title = String::new();
    let mut last_snapshot: Option<Snapshot> = None;

    event_loop.run(move |event, _, control_flow| {
        // Keeps the platform window alive alongside the webview it hosts.
        let _window = &window;

        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            // tao exits the process when the loop ends, so this is the last
            // chance to report what the browser has. Closing the window after
            // passing the challenge is a normal way to finish: the cookie and
            // User-Agent are enough even if the page never exposed
            // `__ha_state`, because the downloader can re-fetch the page with
            // them. A snapshot is not required to be from the final document.
            *control_flow = ControlFlow::Exit;
            match last_snapshot.as_ref() {
                Some(snapshot) => {
                    let state = snapshot.state.as_deref().filter(|s| !s.is_empty());
                    finish_and_exit(&webview, snapshot, state, &url);
                }
                None => {
                    eprintln!("error: the window was closed before the page was read");
                    std::process::exit(1);
                }
            }
        }

        // The callback arrives as an ordinary event-loop wake-up; drain it
        // before scheduling the next poll.
        while let Ok(value) = rx.try_recv() {
            in_flight_since = None;
            match serde_json::from_str::<Snapshot>(&value) {
                Ok(snapshot) => {
                    if !snapshot.title.is_empty() && snapshot.title != last_title {
                        last_title.clone_from(&snapshot.title);
                        eprintln!("hotaudio-webview: {last_title}");
                    }
                    let state = snapshot
                        .state
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    last_snapshot = Some(snapshot);
                    if let (Some(state), Some(snapshot)) =
                        (state.as_deref(), last_snapshot.as_ref())
                    {
                        finish_and_exit(&webview, snapshot, Some(state), &url);
                    }
                }
                Err(e) => eprintln!("hotaudio-webview: unreadable page snapshot: {e}"),
            }
        }

        if Instant::now() >= deadline {
            eprintln!(
                "error: the challenge was not cleared within {} seconds",
                timeout.as_secs()
            );
            std::process::exit(1);
        }

        // A poll whose callback was dropped by a navigation must not freeze the
        // loop; after the watchdog expires, start another one.
        let stalled = in_flight_since.is_some_and(|since| since.elapsed() >= POLL_WATCHDOG);
        if (in_flight_since.is_none() || stalled) && Instant::now() >= next_poll {
            next_poll = Instant::now() + POLL_INTERVAL;
            in_flight_since = Some(Instant::now());
            let tx = tx.clone();
            // A script evaluated before the document exists can fail; the next
            // poll tries again, and the timeout is the real error path.
            let _ = webview.evaluate_script_with_callback(SCRIPT, move |value| {
                let _ = tx.send(value);
            });
        }

        *control_flow = ControlFlow::WaitUntil(next_poll);
    });
}

/// Print the payload and exit; used from the event-loop callback, where there
/// is no stack to unwind to.
fn finish_and_exit(
    webview: &wry::WebView,
    snapshot: &Snapshot,
    state: Option<&str>,
    requested_url: &str,
) -> ! {
    match finish(webview, snapshot, state, requested_url) {
        Ok(json) => {
            println!("{json}");
            // `process::exit` skips destructors, so flush the line explicitly.
            let _ = std::io::stdout().flush();
            std::process::exit(0);
        }
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
    }
}

fn finish(
    webview: &wry::WebView,
    snapshot: &Snapshot,
    state: Option<&str>,
    requested_url: &str,
) -> Result<String, String> {
    // Read cookies against the page the downloader will use, not whatever the
    // webview is showing right now: while the challenge is up that may be a
    // Cloudflare URL, whose jar does not contain the site's clearance. The
    // cookie lookup still matches by domain, so a canonical redirect is fine.
    let page_url = requested_url;
    let reported_url = if snapshot.url.is_empty() {
        requested_url
    } else {
        &snapshot.url
    };
    if snapshot.user_agent.is_empty() {
        return Err("the webview did not report a User-Agent".into());
    }
    let cookies = webview
        .cookies_for_url(page_url)
        .map_err(|e| format!("cannot read the webview cookies: {e}"))?;
    // The request header is `name=value` pairs, not `Cookie`'s display form
    // (which would also print attributes the request must not carry).
    let cookie = cookies
        .iter()
        .map(|c| format!("{}={}", c.name(), c.value()))
        .collect::<Vec<_>>()
        .join("; ");
    if cookie.is_empty() {
        return Err(format!("the webview has no cookies for {page_url}"));
    }

    Ok(serde_json::json!({
        "cookie": cookie,
        "userAgent": &snapshot.user_agent,
        "state": state,
        "url": reported_url,
    })
    .to_string())
}
