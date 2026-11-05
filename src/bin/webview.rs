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
//! handshake without fetching the page again and risking another challenge. The
//! helper is deliberately interactive: it is the escape hatch for a gated
//! network, not an automated challenge solver. See `docs/CLOUDFLARE.md`.

use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

const DEFAULT_TIMEOUT_SECS: u64 = 300;
const POLL_INTERVAL: Duration = Duration::from_millis(250);

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
    let mut in_flight = false;
    let mut last_title = String::new();

    event_loop.run(move |event, _, control_flow| {
        // Keeps the platform window alive alongside the webview it hosts.
        let _window = &window;

        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
            return;
        }

        // The callback arrives as an ordinary event-loop wake-up; drain it
        // before scheduling the next poll.
        while let Ok(value) = rx.try_recv() {
            in_flight = false;
            match serde_json::from_str::<Snapshot>(&value) {
                Ok(snapshot) => {
                    if !snapshot.title.is_empty() && snapshot.title != last_title {
                        last_title.clone_from(&snapshot.title);
                        eprintln!("hotaudio-webview: {last_title}");
                    }
                    if let Some(state) = snapshot.state.as_deref().filter(|s| !s.is_empty()) {
                        match finish(&webview, &snapshot, state, &url) {
                            Ok(json) => {
                                println!("{json}");
                                std::process::exit(0);
                            }
                            Err(message) => {
                                eprintln!("error: {message}");
                                std::process::exit(1);
                            }
                        }
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

        if !in_flight && Instant::now() >= next_poll {
            next_poll = Instant::now() + POLL_INTERVAL;
            in_flight = true;
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

fn finish(
    webview: &wry::WebView,
    snapshot: &Snapshot,
    state: &str,
    requested_url: &str,
) -> Result<String, String> {
    let page_url = if snapshot.url.is_empty() {
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
        "url": page_url,
    })
    .to_string())
}
