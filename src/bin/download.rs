//! `hotaudio-download` — download and decrypt a hotaudio track.
//!
//! Online mode runs the full pipeline and writes playable `.m4a`. Cached mode
//! reuses a saved-keys envelope (no page fetch, no listen calls). Offline mode
//! decrypts a local `.hax` with saved keys (no network at all).
//!
//! Run with `--help` for the full flag list.
//!
//! The binary compiles for every target so that a plain `cargo build` never
//! breaks on one, but only `main` survives on `wasm32`: argv, the filesystem and
//! a blocking transport do not exist there.

// Everything below `main` is native-only, and `wasm32` builds keep only the
// stub, so the flags, parsers and helpers are unreachable there.
#![cfg_attr(target_arch = "wasm32", allow(dead_code, unused_imports))]

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::process::ExitCode;

use hotaudio::crypto::{Error, KeyMap};
use hotaudio::download::{self, DownloadOptions, Phase, Progress};
use hotaudio::http::{Clearance, Client, Retry};
use hotaudio::listen;

#[cfg(not(target_arch = "wasm32"))]
use clap::{CommandFactory, Parser};

/// Program name, printed by `--version`.
const BIN_NAME: &str = "hotaudio-download";

/// Text after the generated option list: what clap cannot derive from an
/// argument's own documentation.
#[cfg(not(target_arch = "wasm32"))]
const AFTER_HELP: &str = "\
The default User-Agent is Mozilla/5.0. When hotaudio.net answers with a
Cloudflare challenge, open the track page in a browser on this machine, pass the
challenge, then copy the browser's `cf_clearance` cookie and its exact
User-Agent and retry:
  --cookie 'cf_clearance=...' --user-agent 'Mozilla/5.0 (...)'
Cloudflare binds the clearance to both the network and the exact User-Agent.
`HOTAUDIO_COOKIE` and `HOTAUDIO_USER_AGENT` set the same two values; flags win.

Or let the downloaded helper do both steps:
  --webview
It opens the track page in a real window, waits for the challenge, and reuses
that browser's cookie, User-Agent and page state. The run also re-handshakes
automatically when key paging hits the server's per-tick window limit, even
without --webview.

Without --webview, a challenge opens the helper automatically once, unless
--no-webview is given. The clearance it earns is cached and reused by later
runs, so the window normally appears only when the site stops trusting it.

Guide: https://github.com/dawn-used-yeet-alt/hotaudio-extractor/blob/main/docs/CLOUDFLARE.md

Exit status:
  0 success; 1 unclassified failure; 2 bad input or unknown flag; 3 network,
  transport or site-challenge failure; 4 protocol, listen, decryption or signer
  failure; 5 local filesystem failure; 130 cancelled (Ctrl-C).

Config file:
  A flat TOML file of 'key = value' lines. Recognised keys: output_dir,
  filename_template, api_base, proxy, cookie, user_agent, ip_family, retries,
  timeout (seconds), delay (milliseconds). Command-line flags override the file;
  an absent default file is not an error.";

/// Printed after a [`Error::Challenge`] so the next attempt can actually pass
/// the gate: the cookie and the user agent are a pair, not independent options.
const CHALLENGE_HINT: &str = "\
hint: hotaudio.net is asking this network to pass a browser challenge.
      Open the track page in a browser on this machine, pass the challenge,
      then copy its `cf_clearance` cookie and exact User-Agent and retry:
        --cookie 'cf_clearance=...' --user-agent 'Mozilla/5.0 (...)'
      The clearance is bound to both the network and the exact User-Agent.
      The bundled helper does this for you: retry with `--webview`.
      Without --webview, a challenge opens it automatically unless
      --no-webview is set; the clearance is cached for later runs.
      Step-by-step guide:
      https://github.com/dawn-used-yeet-alt/hotaudio-extractor/blob/main/docs/CLOUDFLARE.md";

/// The command line, as clap sees it.
///
/// Native-only, like clap itself (see `Cargo.toml`): there is no argv on
/// `wasm32`, and upstream supports that target only best-effort, which a
/// required-green build cannot rest on. The pure helpers below (`sanitize`, the
/// config parser, name derivation) stay target-agnostic and keep their wasm
/// tests.
#[cfg(not(target_arch = "wasm32"))]
// Each flag is independent: `--quiet`, `--json`, `--verify` and the rest do not
// form a state machine, so pairing them into enums would only move the
// conversion to every use site.
#[allow(clippy::struct_excessive_bools)]
#[derive(Parser)]
#[command(
    name = BIN_NAME,
    version,
    about = "Download and decrypt a hotaudio.net track to a playable .m4a",
    long_about = "Download and decrypt a hotaudio.net track to a playable .m4a. Online mode runs \
                  the full pipeline (fetch page, listen, download); `--keys` re-downloads from \
                  saved keys without a page fetch; `--hax --keys` decrypts a local container with \
                  no network at all. Several sources may be given and are processed in order; the \
                  run exits non-zero if any fails.",
    after_help = AFTER_HELP,
    arg_required_else_help = true,
)]
struct Cli {
    /// Page URL or saved HTML to process (several are handled in order)
    #[arg(value_name = "URL|HTML")]
    sources: Vec<String>,

    /// Write the decrypted audio here, or '-' for stdout (default: the track title, sanitised)
    #[arg(long, value_name = "PATH")]
    out: Option<String>,

    /// Name title-derived outputs from PATTERN: {title}, {index} (1-based), {ext}; an explicit --out or --stream-to wins
    #[arg(long, value_name = "PATTERN")]
    filename_template: Option<String>,

    /// Write title-derived outputs under DIR (default: the current directory); an explicit --out or --stream-to wins
    #[arg(long, value_name = "DIR")]
    output_dir: Option<String>,

    /// Read defaults from PATH instead of the default config file
    #[arg(long, value_name = "PATH")]
    config: Option<String>,

    /// Read one source per line from FILE, or '-' for stdin; blank lines are skipped
    #[arg(long, value_name = "FILE")]
    input: Option<String>,

    /// After each source from --input succeeds, remove it from that file
    /// (tracks progress across runs; not usable with stdin)
    #[arg(long, visible_alias = "prune-input", requires = "input")]
    remove_downloaded: bool,

    /// Stream segments to PATH instead of buffering: one segment in memory, first playable fragment after one round trip
    #[arg(long, value_name = "PATH")]
    stream_to: Option<String>,

    /// Write the saved-keys envelope here for later reuse
    #[arg(long, value_name = "PATH")]
    save_keys: Option<String>,

    /// Save the first key branch (needs --save-keys) and exit, without the container or a decrypt
    #[arg(long, requires = "save_keys", conflicts_with_all = ["hax", "keys", "stream_to", "verify"])]
    keys_only: bool,

    /// Write the raw .hax container here for a later offline decrypt (buffered modes only)
    #[arg(long, value_name = "PATH", conflicts_with = "stream_to")]
    save_hax: Option<String>,

    /// After decrypting, walk the MP4 boxes and check the output against the container; fails loudly on a mismatch
    #[arg(long)]
    verify: bool,

    /// Fail instead of replacing an existing output file (audio, keys or .hax)
    #[arg(long, conflicts_with = "force")]
    no_clobber: bool,

    /// Replace existing output files (the default)
    #[arg(long, conflicts_with = "no_clobber")]
    force: bool,

    /// Saved-keys envelope to resume from: a file path or inline JSON; online it seeds the key map
    #[arg(long, value_name = "PATH|JSON")]
    keys: Option<String>,

    /// Decrypt this local .hax container (fully offline)
    #[arg(long, value_name = "PATH")]
    hax: Option<String>,

    /// Container URL for cached mode, when the envelope has none
    #[arg(long, value_name = "URL")]
    hax_url: Option<String>,

    /// Select a track by id on a multi-track page
    #[arg(long, value_name = "ID")]
    track: Option<String>,

    /// Download every track on the page, sequentially; drop --track
    #[arg(long, conflicts_with = "track")]
    all_tracks: bool,

    /// Override the listen API base (default: hotaudio.net)
    #[arg(long, value_name = "URL")]
    api_base: Option<String>,

    /// Send requests through an HTTP(S) proxy; `ALL_PROXY`/`HTTPS_PROXY`/
    /// `HTTP_PROXY` and `NO_PROXY` are honored too
    #[arg(long, value_name = "URL")]
    proxy: Option<String>,

    /// Send this Cookie header with every request; after clearing a Cloudflare
    /// challenge in a browser, pass its `cf_clearance` cookie here
    #[arg(long, value_name = "COOKIE")]
    cookie: Option<String>,

    /// User-Agent to send (default Mozilla/5.0); match the browser that earned
    /// the `--cookie` clearance
    #[arg(long, value_name = "UA")]
    user_agent: Option<String>,

    /// IP family to use for network connections ('any', 'ipv4', or 'ipv6')
    #[arg(long, value_name = "FAMILY", value_parser = ["any", "ipv4", "ipv6"])]
    ip_family: Option<String>,

    /// Open the track page in a browser window to clear the Cloudflare
    /// challenge, then use that browser's cookie, User-Agent and page state
    #[arg(long, conflicts_with = "no_webview")]
    webview: bool,

    /// Never open the browser helper automatically; fail with the challenge
    /// instead (a cached clearance is still used)
    #[arg(long)]
    no_webview: bool,

    /// How long the browser helper waits for the challenge, in seconds
    /// (default: 0, no timeout)
    #[arg(long, value_name = "SECS", default_value_t = 0)]
    webview_timeout: u64,

    /// Retry a failed request up to N times (default 2)
    #[arg(long, value_name = "N")]
    retries: Option<usize>,

    /// Per-attempt timeout for API calls, in seconds (default 30); the bulk container transfer keeps no timeout by design
    #[arg(long, value_name = "SECS", value_parser = clap::value_parser!(u64).range(1..))]
    timeout: Option<u64>,

    /// Pause this many milliseconds before each listen/key-paging request (default 0); politeness for bulk runs
    #[arg(long, value_name = "MS")]
    delay: Option<u64>,

    /// Suppress the progress reporter
    #[arg(short, long, conflicts_with = "verbose")]
    quiet: bool,

    /// Report key-page counts and timing
    #[arg(short, long, conflicts_with = "quiet")]
    verbose: bool,

    /// Print a JSON result object on stdout (a track array for --list-tracks)
    #[arg(long)]
    json: bool,

    /// Progress on stderr: 'text' (default) or 'json' (NDJSON)
    #[arg(long, value_name = "FORMAT", value_parser = ["text", "json"])]
    progress: Option<String>,

    /// List the tracks on the page and exit
    #[arg(long)]
    list_tracks: bool,

    /// Print shell completions for SHELL to stdout and exit (used by packaging)
    #[arg(long, value_name = "SHELL", hide = true)]
    completions: Option<clap_complete::Shell>,

    /// Print the man page to stdout and exit (used by packaging)
    #[arg(long, hide = true)]
    man_page: bool,
}

/// The generated `--help`, for the few runtime errors that are not parse
/// failures but still leave the user needing the usage.
#[cfg(not(target_arch = "wasm32"))]
fn usage() -> String {
    Cli::command().render_help().to_string()
}

/// Render the man page (roff) for the binary, as `--man-page` prints it.
#[cfg(not(target_arch = "wasm32"))]
fn write_man(out: &mut impl Write) -> std::io::Result<()> {
    clap_mangen::Man::new(Cli::command()).render(out)
}

/// Process exit codes, so a script can tell failure classes apart without
/// parsing stderr. Documented in `--help`; `0` is `ExitCode::SUCCESS` and `1` is
/// `ExitCode::FAILURE`, so only the classes below need constants.
mod code {
    /// Bad input: an unknown flag, a bad value, or an unusable config file.
    pub const USAGE: u8 = 2;
    /// A network or transport failure.
    pub const NETWORK: u8 = 3;
    /// A protocol, listen, decryption or signer failure.
    pub const PROTOCOL: u8 = 4;
    /// A local filesystem failure.
    pub const IO: u8 = 5;
    /// The run was cancelled (Ctrl-C). `128 + SIGINT`, the shell convention.
    pub const CANCELLED: u8 = 130;
}

/// A clearance saved from a browser helper, so later runs do not need to pop a
/// window while Cloudflare still trusts the cookie.
///
/// The cookie, User-Agent and IP family are one unit: `cf_clearance` is rejected
/// unless all three match the browser that earned it.
#[derive(serde::Serialize, serde::Deserialize)]
struct CachedClearance {
    cookie: String,
    #[serde(rename = "userAgent")]
    user_agent: String,
    #[serde(default, rename = "ipFamily")]
    ip_family: Option<String>,
    /// Unix seconds when the cache was written; advisory only, because the
    /// server decides when a clearance actually expires.
    saved_at: f64,
}

/// The cache location: `$HOTAUDIO_CLEARANCE`, else
/// `$XDG_CACHE_HOME/hotaudio/clearance.json` (an absolute `XDG_CACHE_HOME`
/// only), else `$HOME/.cache/hotaudio/clearance.json`.
#[cfg(not(target_arch = "wasm32"))]
fn default_clearance_path() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("HOTAUDIO_CLEARANCE") {
        return Some(std::path::PathBuf::from(path));
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".cache"))
        })?;
    Some(base.join("hotaudio").join("clearance.json"))
}

/// Read the cached clearance, ignoring a missing or unreadable file: this is a
/// convenience, never a source of failure.
#[cfg(not(target_arch = "wasm32"))]
fn load_cached_clearance() -> Option<CachedClearance> {
    let path = default_clearance_path()?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Persist a helper's clearance for the next run. Best-effort: a read-only cache
/// directory only costs an extra browser pass later.
#[cfg(not(target_arch = "wasm32"))]
fn save_cached_clearance(payload: &ClearancePayload) {
    let Some(path) = default_clearance_path() else {
        return;
    };
    let entry = CachedClearance {
        cookie: payload.cookie.clone(),
        user_agent: payload.user_agent.clone(),
        ip_family: payload.ip_family.clone(),
        saved_at: hotaudio::clock::unix_secs(),
    };
    let Ok(json) = serde_json::to_vec(&entry) else {
        return;
    };
    if let Err(e) = ensure_cache_dir(&path) {
        eprintln!(
            "warning: cannot create the clearance cache directory for {}: {e}",
            path.display()
        );
        return;
    }
    if let Err(e) = write_private(&path, &json) {
        eprintln!(
            "warning: cannot cache the browser clearance at {}: {e}",
            path.display()
        );
    }
}

/// Create the cache's parent directory, if it has one.
#[cfg(not(target_arch = "wasm32"))]
fn ensure_cache_dir(path: &std::path::Path) -> std::io::Result<()> {
    path.parent()
        .map_or(Ok(()), |dir| std::fs::create_dir_all(dir))
}

/// Move `tmp` onto `path`, replacing an existing file.
///
/// Unix `rename` already replaces; Windows refuses to, so the destination is
/// removed first. Both files live in the same directory, so this is not a
/// cross-device move.
fn replace_file(tmp: &std::path::Path, path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::rename(tmp, path) {
        Ok(()) => Ok(()),
        Err(_) if path.exists() => {
            std::fs::remove_file(path)?;
            std::fs::rename(tmp, path)
        }
        Err(e) => Err(e),
    }
}

/// Write `data` to `path` with owner-only permissions on Unix, replacing any
/// previous file atomically.
#[cfg(not(target_arch = "wasm32"))]
fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(data)?;
    }
    replace_file(&tmp, path)
}

/// Apply a helper's detected IP family unless the user pinned one explicitly.
///
/// A family loaded from the cache is only a hint; when Cloudflare challenges it,
/// the helper may have earned the new clearance on the other stack, so its
/// answer must win. `user_pinned` records whether `--ip-family`, the environment
/// or the config file chose the family.
fn apply_ip_family(payload: &ClearancePayload, user_pinned: bool) {
    if let Some(family) = payload.ip_family.as_ref().filter(|_| !user_pinned) {
        hotaudio::http::set_global_ip_family(family);
    }
}

/// Resolve the browser-clearance triple: flags, then environment, then config,
/// then the cache written by an earlier browser pass.
///
/// A cookie without its matching User-Agent is almost always a failed attempt,
/// so warn while still letting the run proceed (the user may know better).
/// Returns `(cookie, user_agent, cached_ip_family)`; the family is only a hint
/// and is ignored when a flag, environment variable or config sets one.
#[cfg(not(target_arch = "wasm32"))]
fn identity(cli: &Cli, config: &Config) -> (Option<String>, Option<String>, Option<String>) {
    let cookie = cli
        .cookie
        .clone()
        .or_else(|| env_value("HOTAUDIO_COOKIE"))
        .or_else(|| config.cookie.clone());
    let user_agent = cli
        .user_agent
        .clone()
        .or_else(|| env_value("HOTAUDIO_USER_AGENT"))
        .or_else(|| config.user_agent.clone());

    let cached = if cookie.is_none() {
        load_cached_clearance()
    } else {
        None
    };
    if let Some(cached) = cached {
        match &user_agent {
            Some(ua) if ua != &cached.user_agent => {
                if cli.verbose {
                    eprintln!(
                        "verbose: ignoring the cached clearance; its User-Agent does not \
                         match --user-agent"
                    );
                }
            }
            _ => {
                if cli.verbose {
                    eprintln!(
                        "verbose: using a cached browser clearance (saved {}s ago)",
                        (hotaudio::clock::unix_secs() - cached.saved_at).max(0.0) as u64
                    );
                }
                return (
                    Some(cached.cookie),
                    Some(cached.user_agent),
                    cached.ip_family,
                );
            }
        }
    }

    if cookie.is_some() && user_agent.is_none() {
        eprintln!(
            "warning: a cookie is set without a matching User-Agent; a cf_clearance \
             cookie is rejected unless the browser's exact User-Agent is sent too"
        );
    }
    (cookie, user_agent, None)
}

/// The exit code for a pipeline failure, by [`Error`] class.
fn exit_code(e: &Error) -> u8 {
    match e {
        Error::Http(_) | Error::Challenge(_) => code::NETWORK,
        Error::Io(_) => code::IO,
        Error::Cancelled => code::CANCELLED,
        Error::Auth
        | Error::Decode(_)
        | Error::State(_)
        | Error::Listen(_)
        | Error::Protocol(_)
        | Error::Sign(_) => code::PROTOCOL,
    }
}

/// Set by the Ctrl-C handler and polled by the pipeline between segments.
#[cfg(not(target_arch = "wasm32"))]
static CTRL_C: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Install the Ctrl-C handler and return the token the pipeline polls.
///
/// Best-effort: where signals are unavailable the handler fails to install and
/// the run is simply not cancellable, which is better than refusing to start.
#[cfg(not(target_arch = "wasm32"))]
fn install_ctrlc() -> download::Cancel {
    if let Err(e) = ctrlc::set_handler(|| {
        // The first press asks the pipeline to stop at the next segment or key
        // page. A second press — or one during a transfer that stalls before any
        // boundary — exits immediately, so the user is never stuck; it can leave
        // the `.part` temp file behind, which is the price of a hard quit.
        if CTRL_C.swap(true, std::sync::atomic::Ordering::SeqCst) {
            std::process::exit(130);
        }
    }) {
        eprintln!(
            "warning: cannot install the Ctrl-C handler ({e}); \
             the download will not be cancellable"
        );
    }
    download::Cancel::new(|| CTRL_C.load(std::sync::atomic::Ordering::SeqCst))
}

fn main() -> ExitCode {
    #[cfg(target_arch = "wasm32")]
    {
        // The binary itself builds everywhere so that a bare
        // `cargo build --target wasm32-unknown-unknown` succeeds; it has no
        // filesystem, no argv and no blocking transport, so it can only say so.
        eprintln!(
            "hotaudio-download is a native binary. On wasm, use the library:\n\
             see the `wasm` module and the README section on running in a browser."
        );
        ExitCode::FAILURE
    }
    #[cfg(not(target_arch = "wasm32"))]
    run()
}

/// Whether the browser helper may run for this invocation.
///
/// The helper needs an online page URL and is pointless for a local `.hax`.
/// Without `--webview` it stays lazy: it is used only if a request is actually
/// challenged. `--no-webview` keeps the cache but never opens a window.
#[cfg(not(target_arch = "wasm32"))]
fn webview_usable(cli: &Cli, sources: &[String]) -> bool {
    cfg!(feature = "webview")
        && !cli.no_webview
        && cli.hax.is_none()
        && sources.iter().any(|source| is_url(source))
}

/// Clear Cloudflare up front when `--webview` is set.
///
/// Returns whether the helper stays enabled for challenge renewal and for the
/// optional probe in [`run`]. Usage mistakes are exit codes; a helper that
/// cannot start only warns, so a host without a webview toolkit, display or the
/// helper binary still downloads.
#[cfg(not(target_arch = "wasm32"))]
fn proactive_webview(
    cli: &Cli,
    sources: &[String],
    user_pinned_family: bool,
    cookie: &mut Option<String>,
    user_agent: &mut Option<String>,
) -> Result<bool, ExitCode> {
    if !cli.webview {
        return Ok(webview_usable(cli, sources));
    }
    if cli.hax.is_some() {
        eprintln!("error: --webview is for online pages; it cannot be used with --hax\n");
        return Err(ExitCode::from(code::USAGE));
    }
    let Some(url) = sources.iter().find(|s| is_url(s)) else {
        eprintln!("error: --webview needs a page URL; a local HTML file cannot be cleared\n");
        return Err(ExitCode::from(code::USAGE));
    };
    match run_webview(url, cli.webview_timeout) {
        Ok(payload) => {
            *cookie = Some(payload.cookie.clone());
            *user_agent = Some(payload.user_agent.clone());
            apply_ip_family(&payload, user_pinned_family);
            save_cached_clearance(&payload);
            Ok(true)
        }
        Err(e) => {
            eprintln!("warning: {e}");
            eprintln!(
                "warning: continuing without --webview; pass --cookie and --user-agent \
                 if the site asks for a challenge"
            );
            Ok(false)
        }
    }
}

/// Probe `url` once and clear a challenge in the browser helper if it appears.
///
/// This covers the first page fetch, which happens before the pipeline can renew
/// a handshake. A cached clearance is validated here too, so a stale cookie is
/// replaced before the download starts instead of failing at segment 0. A
/// helper failure keeps the original challenge: the real request then reports it
/// with the usual hint.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn probe_clearance(
    url: &str,
    proxy: Option<&str>,
    api_timeout_ms: Option<u64>,
    retry: Retry,
    webview_timeout: u64,
    user_pinned_family: bool,
    cookie: &mut Option<String>,
    user_agent: &mut Option<String>,
) {
    let clearance = Clearance::new();
    clearance.set(cookie.clone(), user_agent.clone());
    let net = Net {
        proxy,
        clearance: &clearance,
        api_timeout_ms,
        retry,
        delay_ms: 0,
    };
    let client = match api_client(net) {
        Ok(client) => client,
        Err(_) => return,
    };
    let challenged = matches!(
        hotaudio::block_on(client.request("GET", url, &[], None)),
        Ok(response) if response.challenge().is_some()
    );
    if !challenged {
        return;
    }
    eprintln!(
        "warning: the site is challenging this network; opening a browser window to clear it \
         (pass --no-webview to disable)"
    );
    match run_webview(url, webview_timeout) {
        Ok(payload) => {
            *cookie = Some(payload.cookie.clone());
            *user_agent = Some(payload.user_agent.clone());
            apply_ip_family(&payload, user_pinned_family);
            save_cached_clearance(&payload);
        }
        Err(e) => {
            eprintln!("warning: automatic browser clearance failed: {e}");
        }
    }
}

/// Handle the hidden packaging flags (`--completions`, `--man-page`).
///
/// Returns `Some(exit)` when one was requested; `None` continues to a download.
#[cfg(not(target_arch = "wasm32"))]
fn packaging_exit(cli: &Cli) -> Option<ExitCode> {
    if let Some(shell) = cli.completions {
        clap_complete::generate(shell, &mut Cli::command(), BIN_NAME, &mut std::io::stdout());
        return Some(ExitCode::SUCCESS);
    }
    if cli.man_page {
        return Some(match write_man(&mut std::io::stdout()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: cannot write the man page: {e}");
                ExitCode::from(code::IO)
            }
        });
    }
    None
}

/// Positional sources, rejecting combinations that cannot name one output.
#[cfg(not(target_arch = "wasm32"))]
fn validated_sources(cli: &Cli) -> Result<Vec<String>, ExitCode> {
    let sources = match collect_sources(cli) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}\n");
            return Err(ExitCode::from(code::IO));
        }
    };
    if cli.all_tracks && sources.len() != 1 {
        eprintln!("error: --all-tracks needs exactly one page URL or HTML file\n");
        return Err(ExitCode::from(code::USAGE));
    }
    if let Some(name) = multi_output_conflict(sources.len(), cli.all_tracks, cli) {
        eprintln!(
            "error: {name} names one path; it cannot be used with several sources or --all-tracks\n"
        );
        return Err(ExitCode::from(code::USAGE));
    }
    Ok(sources)
}

/// Turn one pipeline result into the process exit code, printing diagnostics.
#[cfg(not(target_arch = "wasm32"))]
fn finish(result: Result<(), Error>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if matches!(e, Error::Cancelled) {
                eprintln!("cancelled");
            } else {
                eprintln!("error: {e}");
                if matches!(&e, Error::Challenge(_)) {
                    eprintln!("{CHALLENGE_HINT}");
                }
            }
            ExitCode::from(exit_code(&e))
        }
    }
}

/// Parse arguments and run. The pipeline is `async` so one implementation serves
/// both transports; natively it never suspends, so driving it is a single poll.
#[cfg(not(target_arch = "wasm32"))]
fn run() -> ExitCode {
    let cli = Cli::parse();

    // Packaging hooks, hidden from the help: they print machine-readable output
    // and exit without touching the network.
    if let Some(exit) = packaging_exit(&cli) {
        return exit;
    }

    if let Err(e) = validate_mode(&cli) {
        eprintln!("error: {e}\n");
        eprintln!("{}", usage());
        return ExitCode::from(code::USAGE);
    }

    if cli.remove_downloaded && cli.input.as_deref() == Some("-") {
        eprintln!(
            "error: --remove-downloaded rewrites a file, so --input must name one, not '-'\n"
        );
        return ExitCode::from(code::USAGE);
    }

    // Defaults from the config file, then the CLI overrides them: every
    // `cli.field.or(config.…)` below is "flag wins, else file, else built-in".
    let config = match Config::load(cli.config.as_deref()) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("error: {message}\n");
            return ExitCode::from(code::USAGE);
        }
    };

    let api_timeout_ms = match timeout_ms(cli.timeout, config.timeout_secs) {
        Ok(timeout) => timeout,
        Err(message) => {
            eprintln!("error: {message}\n");
            return ExitCode::from(code::USAGE);
        }
    };
    let clobber = if cli.no_clobber {
        Clobber::Refuse
    } else {
        Clobber::Replace
    };
    let progress_json = cli.progress.as_deref() == Some("json");

    let (mut cookie, mut user_agent, cached_family) = identity(&cli, &config);

    // `--ip-family`, the environment and the config file are an explicit
    // choice: a later browser pass must not override them. The cached family is
    // only a hint, so the helper's answer wins when the cookie ages out.
    let user_family = cli
        .ip_family
        .clone()
        .or_else(|| env_value("HOTAUDIO_IP_FAMILY"))
        .or_else(|| config.ip_family.clone());
    let user_pinned_family = user_family.is_some();
    let ip_family = user_family.or(cached_family);
    if let Some(family) = &ip_family {
        hotaudio::http::set_global_ip_family(family);
    }

    let sources = match validated_sources(&cli) {
        Ok(sources) => sources,
        Err(code) => return code,
    };

    // Proactive clearance: pass the challenge once, then hand the browser's
    // cookie and User-Agent to every client in the run. Renewal repeats this
    // against the same page when the server's key-window budget resets.
    let webview_active = match proactive_webview(
        &cli,
        &sources,
        user_pinned_family,
        &mut cookie,
        &mut user_agent,
    ) {
        Ok(active) => active,
        Err(code) => return code,
    };

    // A cached clearance can age out, and the first page fetch has no renewal
    // callback of its own. Probe it once so the helper can replace it before the
    // download starts; an explicit --webview has already just run.
    let probe_url = if webview_active && !cli.webview {
        sources.iter().find(|source| is_url(source))
    } else {
        None
    };
    if let Some(url) = probe_url {
        probe_clearance(
            url,
            cli.proxy.as_deref().or(config.proxy.as_deref()),
            api_timeout_ms,
            retry_for(cli.retries, config.retries),
            cli.webview_timeout,
            user_pinned_family,
            &mut cookie,
            &mut user_agent,
        );
    }

    let clearance = Clearance::new();
    clearance.set(cookie, user_agent);

    // `--remove-downloaded` rewrites the input file as each source completes, so
    // an interrupted run can be restarted without re-downloading what is done.
    // `--list-tracks` only inspects the file, so it must not consume it.
    let remove_downloaded = cli.remove_downloaded && !cli.list_tracks;
    let input_path = cli.input.clone().filter(|path| path != "-");
    let prune_sources = sources.clone();

    let result = hotaudio::block_on(execute(Run {
        sources,
        out: cli.out,
        filename_template: cli.filename_template.or(config.filename_template),
        output_dir: cli.output_dir.or(config.output_dir),
        stream_to: cli.stream_to,
        save_keys: cli.save_keys,
        save_hax: cli.save_hax,
        hax_path: cli.hax,
        hax_url: cli.hax_url,
        keys_path: cli.keys,
        track_id: cli.track,
        api_base: cli.api_base.or(config.api_base),
        proxy: cli.proxy.or(config.proxy),
        clearance,
        retry: retry_for(cli.retries, config.retries),
        api_timeout_ms,
        delay_ms: delay_ms(cli.delay, config.delay_ms),
        list_tracks: cli.list_tracks,
        keys_only: cli.keys_only,
        all_tracks: cli.all_tracks,
        clobber,
        quiet: cli.quiet,
        verbose: cli.verbose,
        json: cli.json,
        progress_json,
        verify: cli.verify,
        webview: webview_active,
        webview_timeout: cli.webview_timeout,
        user_pinned_family,
        remove_downloaded,
        input_path: input_path.clone(),
        cancel: install_ctrlc(),
    }));

    // Batch mode prunes each source as it completes; a single source (or a
    // successful `--all-tracks` page) is pruned here. Removing a line twice is a
    // no-op, so the two paths can overlap without coordination.
    let prune_path = if result.is_ok() && remove_downloaded {
        input_path.as_deref()
    } else {
        None
    };
    if let Some(path) = prune_path {
        for source in &prune_sources {
            if let Err(e) = prune_input_source(path, source) {
                eprintln!("warning: cannot update {path}: {e}");
            }
        }
    }
    finish(result)
}

// A flat option bag built once from the parsed flags; each bool is an
// independent switch, so there is no state machine to encode.
#[allow(clippy::struct_excessive_bools)]
struct Run {
    sources: Vec<String>,
    out: Option<String>,
    filename_template: Option<String>,
    /// Directory for title-derived outputs (`--output-dir` or config
    /// `output_dir`); an explicit `--out`/`--stream-to` bypasses it.
    output_dir: Option<String>,
    stream_to: Option<String>,
    save_keys: Option<String>,
    save_hax: Option<String>,
    hax_path: Option<String>,
    hax_url: Option<String>,
    keys_path: Option<String>,
    track_id: Option<String>,
    api_base: Option<String>,
    proxy: Option<String>,
    /// Shared by every client in the run; a `--webview` refresh updates it in
    /// place, so key paging and the bulk transfer both see the new cookie.
    clearance: Clearance,
    /// Retry policy for every client the run builds.
    retry: Retry,
    /// Per-attempt API timeout override; `None` keeps the 30 s default.
    api_timeout_ms: Option<u64>,
    /// Politeness pause before each listen/key-paging request; `0` disables it.
    delay_ms: u64,
    list_tracks: bool,
    keys_only: bool,
    all_tracks: bool,
    clobber: Clobber,
    quiet: bool,
    verbose: bool,
    json: bool,
    progress_json: bool,
    /// `--verify`: walk the decrypted MP4 and check its duration.
    verify: bool,
    /// `--webview`: refresh the clearance through the helper after a challenge.
    webview: bool,
    /// `--webview-timeout`: seconds the helper waits for the challenge.
    webview_timeout: u64,
    /// Whether `--ip-family`, the environment or the config file pinned the
    /// family; if so, a browser pass must not override it.
    user_pinned_family: bool,
    /// `--remove-downloaded`: rewrite the input file after each success.
    remove_downloaded: bool,
    /// The `--input` file to rewrite; `None` for positional sources or stdin.
    input_path: Option<String>,
    /// Set when the user interrupts the run, so the pipeline can stop between
    /// segments and the temp-file sink can discard its partial output.
    cancel: download::Cancel,
}

// One setup path shared by every mode, ending in the same error type. Splitting
// it would thread `Run` through more functions without clarifying any step.
#[allow(clippy::too_many_lines)]
async fn execute(r: Run) -> Result<(), Error> {
    // A URL that is not a canonical share link is usually a typo, and the
    // failure would otherwise only surface deep in the pipeline. This is a
    // warning, not a rejection: proxies and mirrors are legitimate.
    for source in &r.sources {
        if is_url(source) && !hotaudio::is_hotaudio_url(source) {
            eprintln!(
                "warning: '{source}' is not a canonical hotaudio share link \
                 (https://hotaudio.net/u/<user>/<track>); trying it anyway"
            );
        }
    }

    let started_ms = hotaudio::clock::monotonic_ms();

    // One progress closure for every mode. It captures only `quiet` and the
    // format, so it does not borrow `r`; `--quiet` wins over `--progress=json`.
    let quiet = r.quiet;
    let progress_json = r.progress_json;
    let clobber = r.clobber;
    let mut report = move |p: Progress| {
        if quiet {
            return;
        }
        if progress_json {
            eprintln!("{}", progress_event(p));
        } else {
            progress(p);
        }
    };

    // ---- Batch: several sources, each to its own title-derived file ----
    // Single-source flags are rejected for several sources in `run`, so only
    // the plain download path is reachable here.
    if r.sources.len() >= 2 {
        return run_batch(&r, &mut report).await;
    }

    // ---- Every track on one page, each to its own title-derived file ----
    if r.all_tracks {
        return run_all_tracks(&r, &mut report).await;
    }

    // ---- Offline: local .hax + saved keys, no network ----
    if let Some(hax_path) = &r.hax_path {
        let saved = load_keys(r.keys_path.as_deref())?;
        let keys = saved.keys;
        let title = saved.title;
        let bytes = std::fs::read(hax_path)
            .map_err(|e| Error::Io(format!("cannot read {hax_path}: {e}")))?;
        let hax = hotaudio::hax::Hax0::parse(&bytes)?;
        if let Some(path) = &r.save_hax {
            write_file(path, &bytes, clobber)?;
        }
        let (audio, segments, _) = download::decrypt_offline(&bytes, &keys, Some(&mut report))?;
        if r.verify {
            verify_or_report(std::io::Cursor::new(&audio), hax.duration_ms)?;
        }
        if r.verbose {
            eprintln!(
                "verbose: {segments} segments in {:.1}s",
                secs_since(started_ms)
            );
        }
        write_saved_keys(r.save_keys.as_deref(), "", title.as_deref(), &keys, clobber)?;
        let title = title.unwrap_or_else(|| stem(hax_path));
        let out = r.out.clone().unwrap_or_else(|| {
            default_output(
                r.filename_template.as_deref(),
                r.output_dir.as_deref(),
                &title,
                1,
            )
        });
        write_file(&out, &audio, clobber)?;
        let line = if r.json {
            let keys_json = download::keys_to_json(&keys, "", Some(&title));
            DownloadInfo {
                title: Some(&title),
                duration: hax.duration_ms as f64 / 1000.0,
                codec: &hax.codec,
                mime: "audio/mp4",
                bytes: audio.len() as u64,
                segment_count: hax.segment_count,
                key_pages: 0,
                hax_url: "",
                keys_json: &keys_json,
                saved_to: &out,
            }
            .to_json()
        } else {
            format!("Saved {}", std::path::Path::new(&out).display())
        };
        print_result(&out, &line);
        return Ok(());
    }

    // ---- Track listing ----
    if r.list_tracks {
        let Some(source) = r.sources.first() else {
            return Err(Error::Protocol(
                "--list-tracks needs a URL or HTML file; see --help".into(),
            ));
        };
        let tracks = if is_url(source) {
            let client = api_client(net_of(&r))?;
            let resp = client.request("GET", source, &[], None).await?;
            if let Some(detail) = resp.challenge() {
                return Err(Error::Challenge(format!("track page: {detail}")));
            }
            if !resp.ok() {
                return Err(Error::Http(format!("page returned {}", resp.status)));
            }
            listen::list_tracks(&resp.text())
        } else {
            let html = std::fs::read_to_string(source)
                .map_err(|e| Error::Io(format!("cannot read {source}: {e}")))?;
            listen::list_tracks(&html)
        };
        let Some(tracks) = tracks.filter(|t| !t.is_empty()) else {
            return Err(Error::State(
                "no tracks found (missing or undecryptable page state)".into(),
            ));
        };
        if r.json {
            let items: Vec<String> = tracks
                .iter()
                .map(|(id, key, title)| {
                    format!(
                        "{{\"id\":{},\"key\":{},\"title\":{}}}",
                        json_string(id),
                        json_string(key),
                        json_string(title)
                    )
                })
                .collect();
            println!("[{}]", items.join(","));
        } else {
            for (id, _key, title) in &tracks {
                println!("{id}\t{title}");
            }
        }
        return Ok(());
    }

    // ---- Keys only: handshake + first listen, no container ----
    if r.keys_only {
        let Some(source) = r.sources.first() else {
            return Err(Error::Protocol("--keys-only needs a page URL".into()));
        };
        if !is_url(source) {
            return Err(Error::Protocol(
                "--keys-only needs a page URL, not a local file".into(),
            ));
        }
        let client = api_client(net_of(&r))?;
        let bundle = download::fetch_keys_only(
            &client,
            source,
            r.api_base.as_deref(),
            r.track_id.as_deref(),
            r.delay_ms,
        )
        .await?;
        write_saved_keys(
            r.save_keys.as_deref(),
            &bundle.hax_url,
            bundle.title.as_deref(),
            &bundle.keys,
            clobber,
        )?;
        if r.json {
            let keys_json =
                download::keys_to_json(&bundle.keys, &bundle.hax_url, bundle.title.as_deref());
            let title = bundle
                .title
                .as_deref()
                .map_or_else(|| "null".to_string(), json_string);
            println!(
                "{{\"title\":{title},\"haxUrl\":{},\"keysJson\":{keys_json}}}",
                json_string(&bundle.hax_url)
            );
        }
        return Ok(());
    }

    // ---- Cached mode: saved keys (+ envelope hax URL), no page/listen ----
    if r.keys_path.is_some() && r.sources.is_empty() {
        let saved = load_keys(r.keys_path.as_deref())?;
        let (keys, envelope_url, title) = (saved.keys, saved.hax_url, saved.title);
        let Some(url) = r.hax_url.clone().or(envelope_url) else {
            return Err(Error::Protocol(
                "keys file has no .hax URL — pass --hax-url or use online mode".into(),
            ));
        };
        let client = bulk_client(net_of(&r))?;
        let res = download::download_cached(
            &client,
            &url,
            &keys,
            Some(&mut report),
            r.save_hax.is_some(),
            Some(r.cancel.clone()),
        )
        .await?;
        if r.verify {
            verify_or_report(std::io::Cursor::new(&res.audio), res.duration_ms)?;
        }
        if let Some(path) = &r.save_hax {
            if let Some(bytes) = &res.hax_bytes {
                write_file(path, bytes, clobber)?;
            }
        }
        if r.verbose {
            eprintln!(
                "verbose: {} segments in {:.1}s",
                res.segment_count,
                secs_since(started_ms)
            );
        }
        write_saved_keys(
            r.save_keys.as_deref(),
            &url,
            title.as_deref(),
            &keys,
            clobber,
        )?;
        let title = title.unwrap_or_else(|| "hotaudio-track".into());
        let out = r.out.clone().unwrap_or_else(|| {
            default_output(
                r.filename_template.as_deref(),
                r.output_dir.as_deref(),
                &title,
                1,
            )
        });
        write_file(&out, &res.audio, clobber)?;
        let line = if r.json {
            let keys_json = download::keys_to_json(&keys, &url, Some(&title));
            DownloadInfo {
                title: Some(&title),
                duration: res.duration,
                codec: &res.codec,
                mime: res.mime,
                bytes: res.audio.len() as u64,
                segment_count: res.segment_count,
                key_pages: res.pages,
                hax_url: &url,
                keys_json: &keys_json,
                saved_to: &out,
            }
            .to_json()
        } else {
            format!("Saved {}", std::path::Path::new(&out).display())
        };
        print_result(&out, &line);
        return Ok(());
    }

    // ---- Online mode ----
    let Some(source) = r.sources.first().cloned() else {
        return Err(Error::Protocol("no input given; see --help".into()));
    };

    let seed = match &r.keys_path {
        Some(p) => Some(load_keys(Some(p.as_str()))?.keys),
        None => None,
    };

    // ---- Streamed mode: ranged reads, one segment in memory at a time ----
    if let Some(dest) = &r.stream_to {
        let api = api_client(net_of(&r))?;
        let hs = if is_url(&source) {
            let api_base = r.api_base.as_deref().unwrap_or(hotaudio::HOTAUDIO_API_BASE);
            listen::load_handshake(&api, &source, api_base, r.track_id.as_deref()).await?
        } else {
            let html = std::fs::read_to_string(&source)
                .map_err(|e| Error::Io(format!("cannot read {source}: {e}")))?;
            let api_base = r.api_base.as_deref().unwrap_or(hotaudio::HOTAUDIO_API_BASE);
            let mut hs = listen::handshake_from_html(&html, api_base, r.track_id.as_deref())?
                .ok_or_else(|| {
                    Error::State("could not decrypt __ha_state from HTML file".into())
                })?;
            hs.listen_key = listen::extract_listen_key(&source);
            hs
        };

        // Stream into a sibling temp file and promote it only on success, so a
        // failed run never leaves a corrupt `.m4a` at `dest` (see [`StreamSink`]).
        let (sink, out) = StreamSink::open(dest, clobber)?;
        let mut writer = std::io::BufWriter::with_capacity(1 << 20, out);

        let renewal = r.renewal_for(&source, r.track_id.as_deref());
        let res = download::download_streaming_renewing(
            &api,
            &hs,
            seed.as_ref(),
            r.delay_ms,
            renewal,
            |fragment| {
                writer
                    .write_all(fragment)
                    .map_err(|e| Error::Io(e.to_string()))
            },
            Some(&mut report),
            Some(r.cancel.clone()),
        )
        .await?;
        writer.flush().map_err(|e| Error::Io(e.to_string()))?;
        // Close the file before promoting it: Windows will not move an open file.
        drop(writer);
        if r.verify {
            let pending = sink.pending_path().ok_or_else(|| {
                Error::Protocol("--verify cannot check a stream written to stdout".into())
            })?;
            let file = std::fs::File::open(pending)
                .map_err(|e| Error::Io(format!("cannot read {pending}: {e}")))?;
            verify_or_report(file, res.duration_ms)?;
        }
        sink.commit()?;

        let title = Some(hs.track.title.clone()).filter(|t| !t.is_empty());
        write_saved_keys(
            r.save_keys.as_deref(),
            &res.hax_url,
            title.as_deref(),
            &res.keys,
            clobber,
        )?;
        eprintln!(
            "streamed {} bytes across {} segments ({} page(s)) to {}",
            res.bytes, res.segment_count, res.pages, dest
        );
        if r.verbose {
            eprintln!(
                "verbose: {} segments, {} key page(s) in {:.1}s",
                res.segment_count,
                res.pages,
                secs_since(started_ms)
            );
        }
        if r.json {
            let keys_json = download::keys_to_json(&res.keys, &res.hax_url, title.as_deref());
            let line = DownloadInfo {
                title: title.as_deref(),
                duration: res.duration,
                codec: &res.codec,
                mime: "audio/mp4",
                bytes: res.bytes,
                segment_count: res.segment_count,
                key_pages: res.pages,
                hax_url: &res.hax_url,
                keys_json: &keys_json,
                saved_to: dest.as_str(),
            }
            .to_json();
            print_result(dest, &line);
        }
        return Ok(());
    }

    let res = fetch_online(
        &r,
        &source,
        seed,
        &mut report,
        r.track_id.as_deref(),
        r.save_hax.is_some(),
    )
    .await?;

    if r.verify {
        verify_or_report(std::io::Cursor::new(&res.audio), res.duration_ms)?;
    }
    if let Some(path) = &r.save_hax {
        if let Some(bytes) = &res.hax_bytes {
            write_file(path, bytes, clobber)?;
        }
    }
    let title = res.title.clone().unwrap_or_else(|| "hotaudio-track".into());
    write_saved_keys(
        r.save_keys.as_deref(),
        &res.hax_url,
        Some(&title),
        &res.keys,
        clobber,
    )?;
    let out = r.out.clone().unwrap_or_else(|| {
        default_output(
            r.filename_template.as_deref(),
            r.output_dir.as_deref(),
            &title,
            1,
        )
    });
    write_file(&out, &res.audio, clobber)?;
    if r.verbose {
        eprintln!(
            "verbose: {} segments, {} key page(s) in {:.1}s",
            res.segment_count,
            res.pages,
            secs_since(started_ms)
        );
    }
    let line = if r.json {
        let keys_json = download::keys_to_json(&res.keys, &res.hax_url, Some(&title));
        DownloadInfo {
            title: Some(&title),
            duration: res.duration,
            codec: &res.codec,
            mime: res.mime,
            bytes: res.audio.len() as u64,
            segment_count: res.segment_count,
            key_pages: res.pages,
            hax_url: &res.hax_url,
            keys_json: &keys_json,
            saved_to: &out,
        }
        .to_json()
    } else {
        format!("Saved {}", std::path::Path::new(&out).display())
    };
    print_result(&out, &line);
    Ok(())
}

/// Download one online source — a page URL or a local HTML file — and decrypt it.
///
/// Shared by single-source and batch modes. `r` supplies the API base, delay and
/// cancellation; `initial_keys` seeds a resume, the HTML branch recovers the
/// handshake from a saved page, and `track_id` narrows a multi-track page.
async fn fetch_online(
    r: &Run,
    source: &str,
    initial_keys: Option<KeyMap>,
    report: download::ProgressFn<'_>,
    track_id: Option<&str>,
    capture_hax: bool,
) -> Result<download::Downloaded, Error> {
    let net = net_of(r);
    let api = api_client(net)?;
    let bulk = bulk_client(net)?;
    let clients = download::Clients {
        api: &api,
        bulk: &bulk,
    };
    let renew = r.renewal_for(source, track_id);
    let mut opts = DownloadOptions {
        initial_keys,
        on_progress: Some(report),
        api_base: r.api_base.as_deref(),
        track_id,
        capture_hax,
        delay_ms: net.delay_ms,
        cancel: Some(r.cancel.clone()),
        renew,
    };
    if is_url(source) {
        clients.download_from_page(source, &mut opts).await
    } else {
        let html = std::fs::read_to_string(source)
            .map_err(|e| Error::Io(format!("cannot read {source}: {e}")))?;
        let base = r.api_base.as_deref().unwrap_or(hotaudio::HOTAUDIO_API_BASE);
        let mut hs = listen::handshake_from_html(&html, base, track_id)?
            .ok_or_else(|| Error::State("could not decrypt __ha_state from HTML file".into()))?;
        hs.listen_key = listen::extract_listen_key(source);
        clients.download_with_handshake(&hs, &mut opts).await
    }
}

/// The per-run HTTP and politeness settings shared by the client helpers and the
/// download pipeline.
#[derive(Clone, Copy)]
struct Net<'a> {
    proxy: Option<&'a str>,
    /// Browser clearance shared by every client in the run.
    clearance: &'a Clearance,
    /// Per-attempt API timeout; `None` keeps [`Client::api`]'s 30 s default.
    api_timeout_ms: Option<u64>,
    retry: Retry,
    /// Politeness pause before each listen/key-paging request; `0` disables it.
    delay_ms: u64,
}

/// The network settings for `r`.
fn net_of(r: &Run) -> Net<'_> {
    Net {
        proxy: r.proxy.as_deref(),
        clearance: &r.clearance,
        api_timeout_ms: r.api_timeout_ms,
        retry: r.retry,
        delay_ms: r.delay_ms,
    }
}

/// The retry policy: `--retries N` (`N` retries after the first attempt), else
/// the config's `retries`, else the built-in default.
#[cfg(not(target_arch = "wasm32"))]
fn retry_for(flag: Option<usize>, fallback: Option<usize>) -> Retry {
    match flag.or(fallback) {
        Some(n) => Retry {
            attempts: n.saturating_add(1),
            ..Retry::default()
        },
        None => Retry::default(),
    }
}

/// The per-attempt API timeout in milliseconds: `--timeout SECS`, else the
/// config's `timeout`. Zero is rejected either way.
#[cfg(not(target_arch = "wasm32"))]
fn timeout_ms(flag: Option<u64>, fallback: Option<u64>) -> Result<Option<u64>, String> {
    match flag.or(fallback) {
        None => Ok(None),
        Some(0) => Err("--timeout must be at least 1 second".into()),
        Some(secs) => Ok(Some(secs.saturating_mul(1000))),
    }
}

/// The politeness pause in milliseconds: `--delay MS`, else the config's
/// `delay`, else none.
#[cfg(not(target_arch = "wasm32"))]
fn delay_ms(flag: Option<u64>, fallback: Option<u64>) -> u64 {
    flag.or(fallback).unwrap_or(0)
}

/// A small-call client for `net`: `--timeout` and `--retries` applied.
fn api_client(net: Net<'_>) -> Result<Client, Error> {
    let client = match net.api_timeout_ms {
        Some(ms) => Client::with_timeout_ms(Some(ms)),
        None => Client::api(),
    };
    let client = apply_identity(apply_proxy(client, net.proxy)?, net);
    Ok(client.with_retries(net.retry))
}

/// The bulk-transfer client for `net`: `--retries` and `--proxy` applied, but
/// never `--timeout` (the container keeps its deliberate no-timeout).
fn bulk_client(net: Net<'_>) -> Result<Client, Error> {
    let client = apply_identity(apply_proxy(Client::bulk(), net.proxy)?, net);
    Ok(client.with_retries(net.retry))
}

/// Attach the run's shared browser clearance.
///
/// Every client is built from the same handle, so a `--webview` refresh updates
/// the API and bulk clients together. Cloudflare binds `cf_clearance` to the
/// requesting user agent, which is why [`Clearance`] stores the pair as one
/// value rather than two independent options.
fn apply_identity(client: Client, net: Net<'_>) -> Client {
    client.with_clearance(net.clearance.clone())
}

/// Apply `--proxy` to `client`; without it the transport honors the standard
/// proxy environment variables on its own.
fn apply_proxy(client: Client, proxy: Option<&str>) -> Result<Client, Error> {
    match proxy {
        Some(url) => client.with_proxy(url),
        None => Ok(client),
    }
}

impl Run {
    /// A challenge-refresh callback for `source`.
    ///
    /// The pipeline calls this after a challenge and expects a handshake for the
    /// same track. It always re-fetches the page first, which is what resets the
    /// server's per-tick key-window budget. When the page fetch is itself
    /// challenged, the browser helper runs (unless `--no-webview`); a helper
    /// snapshot without `__ha_state` is completed by fetching the page with the
    /// new clearance. Local HTML has no page to refresh, so it gets no callback.
    #[cfg(not(target_arch = "wasm32"))]
    fn renewal_for(&self, source: &str, track_id: Option<&str>) -> Option<download::Renewal> {
        if !is_url(source) {
            return None;
        }
        let source = source.to_string();
        let clearance = self.clearance.clone();
        let api_base = self.api_base.clone();
        let track_id = track_id.map(str::to_string);
        let proxy = self.proxy.clone();
        let retry = self.retry;
        let api_timeout_ms = self.api_timeout_ms;
        let webview = self.webview;
        let webview_timeout = self.webview_timeout;
        let user_pinned_family = self.user_pinned_family;

        Some(download::Renewal::new(move || {
            let base = api_base.as_deref().unwrap_or(hotaudio::HOTAUDIO_API_BASE);
            let refresh = || -> Result<listen::Handshake, Error> {
                let net = Net {
                    proxy: proxy.as_deref(),
                    clearance: &clearance,
                    api_timeout_ms,
                    retry,
                    delay_ms: 0,
                };
                let client = api_client(net)?;
                hotaudio::block_on(listen::load_handshake(
                    &client,
                    &source,
                    base,
                    track_id.as_deref(),
                ))
            };

            // The clearance from the last browser pass normally outlives the
            // server's key-window tick, so re-reading the page is enough and
            // does not pop another window. Only Cloudflare itself needs the
            // browser again.
            match refresh() {
                Ok(hs) => Ok(hs),
                Err(challenge @ Error::Challenge(_)) if webview => {
                    let payload = match run_webview(&source, webview_timeout) {
                        Ok(payload) => payload,
                        Err(e) => {
                            // Keep the site's own challenge: it is what tells the
                            // user the network is blocked, and that the helper
                            // could not replace the clearance either.
                            eprintln!("warning: automatic browser clearance failed: {e}");
                            return Err(challenge);
                        }
                    };
                    clearance.set(
                        Some(payload.cookie.clone()),
                        Some(payload.user_agent.clone()),
                    );
                    apply_ip_family(&payload, user_pinned_family);
                    save_cached_clearance(&payload);
                    if payload.state.is_some() {
                        handshake_from_payload(&payload, base, track_id.as_deref(), &source)
                    } else {
                        // The window was closed before the snapshot carried
                        // `__ha_state`; the new cookie is still fresh.
                        refresh()
                    }
                }
                Err(e) => Err(e),
            }
        }))
    }
}

/// The browser build has no page fetch and no helper: its host owns clearance.
#[cfg(target_arch = "wasm32")]
impl Run {
    // The native version is a method; keep call sites uniform on both targets
    // even though the browser has nothing to refresh.
    #[allow(clippy::unused_self)]
    fn renewal_for(&self, _source: &str, _track_id: Option<&str>) -> Option<download::Renewal> {
        None
    }
}

/// What `hotaudio-webview` prints after the challenge is cleared.
#[derive(serde::Deserialize)]
struct ClearancePayload {
    /// A ready `Cookie` header (`name=value; ...`).
    cookie: String,
    /// The webview's exact `User-Agent`.
    #[serde(rename = "userAgent")]
    user_agent: String,
    /// The page's raw `__ha_state` value, when the helper caught it.
    ///
    /// `None` is not a failure: a window closed right after the challenge
    /// clears still yields the cookie and User-Agent, and the page is then
    /// re-fetched with them.
    state: Option<String>,
    /// The final page URL, after any challenge redirect.
    #[allow(dead_code)]
    url: String,
    /// The IP family used by the webview ("ipv4" or "ipv6"), if detected.
    #[serde(default, rename = "ipFamily")]
    ip_family: Option<String>,
}

/// Rebuild a handshake from a webview snapshot.
fn handshake_from_payload(
    payload: &ClearancePayload,
    api_base: &str,
    track_id: Option<&str>,
    page_url: &str,
) -> Result<listen::Handshake, Error> {
    let state = payload.state.as_deref().ok_or_else(|| {
        Error::State("the webview snapshot has no page state; re-fetch the page".into())
    })?;
    let mut hs = listen::handshake_from_state_b64(state, api_base, track_id)?.ok_or_else(|| {
        Error::State(
            "the refreshed page does not carry the requested track; \
             clear the challenge on the track page itself"
                .into(),
        )
    })?;
    hs.listen_key = listen::extract_listen_key(page_url);
    Ok(hs)
}

/// Run the clearance helper for `url` and parse its JSON.
#[cfg(feature = "webview")]
fn run_webview(url: &str, timeout_secs: u64) -> Result<ClearancePayload, Error> {
    let exe = webview_exe()?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("--url").arg(url);
    if timeout_secs > 0 {
        cmd.arg("--timeout").arg(timeout_secs.to_string());
    }
    let output = cmd
        .output()
        .map_err(|e| Error::Io(format!("cannot run {}: {e}", exe.display())))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("the helper exited without a message");
        return Err(Error::Challenge(format!(
            "webview clearance failed: {detail}"
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|e| Error::Protocol(format!("unreadable webview output: {e}")))
}

/// This build has no `webview` feature: keep the flag and fail with a clear fix.
#[cfg(not(feature = "webview"))]
fn run_webview(_url: &str, _timeout_secs: u64) -> Result<ClearancePayload, Error> {
    Err(Error::Protocol(
        "--webview needs a build with the `webview` feature; the release archives include it"
            .into(),
    ))
}

/// Locate `hotaudio-webview`, next to this executable unless overridden.
#[cfg(feature = "webview")]
fn webview_exe() -> Result<std::path::PathBuf, Error> {
    if let Some(path) = env_value("HOTAUDIO_WEBVIEW") {
        return Ok(path.into());
    }
    let name = if cfg!(windows) {
        "hotaudio-webview.exe"
    } else {
        "hotaudio-webview"
    };
    let exe = std::env::current_exe()
        .map_err(|e| Error::Io(format!("cannot locate this executable: {e}")))?;
    let path = exe
        .parent()
        .map_or_else(|| name.into(), |dir| dir.join(name));
    if path.is_file() {
        Ok(path)
    } else {
        Err(Error::Protocol(format!(
            "cannot find {}; install the webview-enabled build or set HOTAUDIO_WEBVIEW",
            path.display()
        )))
    }
}

/// Download several sources in order, continuing past failures.
///
/// Every failure is printed as it happens; the summary error makes the process
/// exit non-zero if any occurred.
async fn run_batch(r: &Run, report: download::ProgressFn<'_>) -> Result<(), Error> {
    let mut written: HashSet<String> = HashSet::new();
    let mut failures = 0usize;
    for (i, source) in r.sources.iter().enumerate() {
        match batch_one(r, source, &mut *report, &mut written, None, i + 1).await {
            Ok(()) => {
                let path = r.input_path.as_deref().filter(|_| r.remove_downloaded);
                if let Some(path) = path {
                    if let Err(e) = prune_input_source(path, source) {
                        eprintln!("warning: cannot update {path}: {e}");
                    }
                }
            }
            Err(e) => {
                // A cancel applies to the whole run, not one item: stop instead
                // of working down the remaining sources.
                if matches!(e, Error::Cancelled) {
                    return Err(e);
                }
                eprintln!("error: {source}: {e}");
                failures += 1;
            }
        }
    }
    if failures > 0 {
        return Err(Error::Http(format!(
            "{failures} of {} tracks failed",
            r.sources.len()
        )));
    }
    Ok(())
}

/// Download every track on one page, in page order, continuing past failures.
///
/// Track ids come from `listen::list_tracks`; each is fetched with its own
/// handshake. Downloads stay sequential, since the server serialises per track
/// (`docs/PROTOCOL.md` §7).
async fn run_all_tracks(r: &Run, report: download::ProgressFn<'_>) -> Result<(), Error> {
    let Some(source) = r.sources.first() else {
        return Err(Error::Protocol(
            "--all-tracks needs a page URL or HTML file".into(),
        ));
    };
    let html = if is_url(source) {
        let client = api_client(net_of(r))?;
        let resp = client.request("GET", source, &[], None).await?;
        if let Some(detail) = resp.challenge() {
            return Err(Error::Challenge(format!("track page: {detail}")));
        }
        if !resp.ok() {
            return Err(Error::Http(format!("page returned {}", resp.status)));
        }
        resp.text()
    } else {
        std::fs::read_to_string(source)
            .map_err(|e| Error::Io(format!("cannot read {source}: {e}")))?
    };
    let Some(tracks) = listen::list_tracks(&html).filter(|t| !t.is_empty()) else {
        return Err(Error::State(
            "no tracks found (missing or undecryptable page state)".into(),
        ));
    };
    let ids: Vec<String> = tracks.iter().map(|(id, _, _)| id.clone()).collect();

    let mut written: HashSet<String> = HashSet::new();
    let mut failures = 0usize;
    for (i, id) in ids.iter().enumerate() {
        if let Err(e) = batch_one(
            r,
            source,
            &mut *report,
            &mut written,
            Some(id.as_str()),
            i + 1,
        )
        .await
        {
            // As in [`run_batch`]: a cancel ends the run, it is not a per-track
            // failure to continue past.
            if matches!(e, Error::Cancelled) {
                return Err(e);
            }
            eprintln!("error: {source} track {id}: {e}");
            failures += 1;
        }
    }
    if failures > 0 {
        return Err(Error::Http(format!(
            "{failures} of {} tracks failed",
            ids.len()
        )));
    }
    Ok(())
}

/// Download one item of a batch — a source, optionally narrowed to a track — and
/// write its title-derived file.
///
/// Tracking written paths turns a title collision into a clear per-item error
/// instead of a silent overwrite.
async fn batch_one(
    r: &Run,
    source: &str,
    report: download::ProgressFn<'_>,
    written: &mut HashSet<String>,
    track_id: Option<&str>,
    index: usize,
) -> Result<(), Error> {
    let started = hotaudio::clock::monotonic_ms();
    let res = fetch_online(r, source, None, report, track_id, false).await?;
    if r.verify {
        verify_or_report(std::io::Cursor::new(&res.audio), res.duration_ms)?;
    }
    let title = res.title.clone().unwrap_or_else(|| "hotaudio-track".into());
    let out = default_output(
        r.filename_template.as_deref(),
        r.output_dir.as_deref(),
        &title,
        index,
    );
    if !written.insert(out.clone()) {
        return Err(Error::State(format!(
            "output {out} was already written by an earlier source; rename it and retry"
        )));
    }
    write_file(&out, &res.audio, r.clobber)?;
    if r.verbose {
        eprintln!(
            "verbose: {} segments, {} key page(s) in {:.1}s",
            res.segment_count,
            res.pages,
            secs_since(started)
        );
    }
    let line = if r.json {
        let keys_json = download::keys_to_json(&res.keys, &res.hax_url, Some(&title));
        DownloadInfo {
            title: Some(&title),
            duration: res.duration,
            codec: &res.codec,
            mime: res.mime,
            bytes: res.audio.len() as u64,
            segment_count: res.segment_count,
            key_pages: res.pages,
            hax_url: &res.hax_url,
            keys_json: &keys_json,
            saved_to: &out,
        }
        .to_json()
    } else {
        format!("Saved {}", std::path::Path::new(&out).display())
    };
    println!("{line}");
    Ok(())
}

/// Seconds elapsed since a `clock::monotonic_ms` reading.
fn secs_since(started_ms: f64) -> f64 {
    (hotaudio::clock::monotonic_ms() - started_ms) / 1000.0
}

/// Escape a string as a JSON string literal, quotes included.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The `--json` result object.
///
/// Mirrors the wasm `downloadTrack` shape (`title`, `durationSecs`, `codec`,
/// `mime`, `bytes`, `segmentCount`, `keyPages`, `haxUrl`, `keysJson`) plus
/// `savedTo`, the path the CLI wrote. `keysJson` is already JSON, so it is
/// embedded verbatim; `title` is `null` when unknown.
struct DownloadInfo<'a> {
    title: Option<&'a str>,
    duration: f64,
    codec: &'a str,
    mime: &'a str,
    bytes: u64,
    segment_count: u32,
    key_pages: u32,
    hax_url: &'a str,
    keys_json: &'a str,
    saved_to: &'a str,
}

impl DownloadInfo<'_> {
    fn to_json(&self) -> String {
        let title = self.title.map_or_else(|| "null".to_string(), json_string);
        format!(
            "{{\"title\":{title},\"durationSecs\":{:.3},\"codec\":{},\"mime\":{},\"bytes\":{},\"segmentCount\":{},\"keyPages\":{},\"haxUrl\":{},\"keysJson\":{},\"savedTo\":{}}}",
            self.duration,
            json_string(self.codec),
            json_string(self.mime),
            self.bytes,
            self.segment_count,
            self.key_pages,
            json_string(self.hax_url),
            self.keys_json,
            json_string(self.saved_to),
        )
    }
}

/// One NDJSON progress event for `--progress=json`.
fn progress_event(p: Progress) -> String {
    let phase = match p.phase {
        Phase::Resolving => "resolving",
        Phase::Fetching => "fetching",
        Phase::Decrypting => "decrypting",
    };
    format!(
        "{{\"phase\":{},\"loaded\":{},\"total\":{}}}",
        json_string(phase),
        p.loaded,
        p.total
    )
}

/// Progress reporting on stderr.
fn progress(p: Progress) {
    if p.total == 0 {
        return;
    }
    match p.phase {
        Phase::Fetching => {
            eprint!(
                "\rfetching: {:.1}%",
                (p.loaded as f64 / p.total as f64) * 100.0
            );
        }
        Phase::Decrypting => {
            eprint!("\rdecrypting: {}/{} segments", p.loaded, p.total);
        }
        Phase::Resolving => {}
    }
    if p.loaded == p.total {
        eprintln!();
    }
    let _ = std::io::stderr().flush();
}

/// The first single-path flag present, so a multi-source run can name the one
/// that rules it out. Order decides which is named when several are given.
#[cfg(not(target_arch = "wasm32"))]
fn single_path_flag(cli: &Cli) -> Option<&'static str> {
    if cli.out.is_some() {
        return Some("--out");
    }
    if cli.stream_to.is_some() {
        return Some("--stream-to");
    }
    if cli.save_keys.is_some() {
        return Some("--save-keys");
    }
    if cli.save_hax.is_some() {
        return Some("--save-hax");
    }
    if cli.keys.is_some() {
        return Some("--keys");
    }
    if cli.hax.is_some() {
        return Some("--hax");
    }
    if cli.keys_only {
        return Some("--keys-only");
    }
    if cli.list_tracks {
        return Some("--list-tracks");
    }
    None
}

/// The single-path flag that rules out a multi-output run, if any.
///
/// A run is multi-output when it has several sources or is `--all-tracks`.
#[cfg(not(target_arch = "wasm32"))]
fn multi_output_conflict(sources: usize, all_tracks: bool, cli: &Cli) -> Option<&'static str> {
    if sources < 2 && !all_tracks {
        return None;
    }
    single_path_flag(cli)
}

/// The checks clap cannot express: a bad template, and a rule that depends on a
/// *value* rather than mere presence (`--verify` cannot check a stream written
/// to stdout).
#[cfg(not(target_arch = "wasm32"))]
fn validate_mode(cli: &Cli) -> Result<(), String> {
    if cli.verify && cli.stream_to.as_deref() == Some("-") {
        return Err("--verify cannot check a stream written to stdout; write to a file".into());
    }
    let template = cli.filename_template.as_deref().unwrap_or_default();
    if let Some(message) = template_error(template) {
        return Err(message);
    }
    Ok(())
}

/// Positional sources plus one per line of `--input` (`-` means stdin).
///
/// Blank lines are skipped. Positionals come first, then the file, in the order
/// given.
#[cfg(not(target_arch = "wasm32"))]
fn collect_sources(cli: &Cli) -> Result<Vec<String>, String> {
    let mut sources = cli.sources.clone();
    let Some(path) = cli.input.as_deref() else {
        return Ok(sources);
    };
    let text = if path == "-" {
        let mut s = String::new();
        std::io::stdin()
            .lock()
            .read_to_string(&mut s)
            .map_err(|e| format!("cannot read stdin: {e}"))?;
        s
    } else {
        std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?
    };
    for line in text.lines() {
        let line = line.trim();
        if !line.is_empty() {
            sources.push(line.to_string());
        }
    }
    Ok(sources)
}

/// Remove the first line of `path` equal to `source`, preserving every other
/// line (including blanks) so hand annotations survive.
///
/// The rewrite goes through a sibling temp file and a rename, so an interrupted
/// run leaves the original list intact. Returns `Ok(false)` when the line was
/// already gone, which makes a repeated prune harmless.
fn prune_input_source(path: &str, source: &str) -> Result<bool, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let mut removed = false;
    let mut remaining = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if !removed && line.trim() == source {
            removed = true;
        } else {
            remaining.push_str(line);
        }
    }
    if !removed {
        return Ok(false);
    }
    let tmp = format!("{path}.hotaudio-tmp");
    std::fs::write(&tmp, remaining).map_err(|e| format!("cannot write {tmp}: {e}"))?;
    // Keep the list's existing permissions; a fresh temp file would otherwise
    // replace, say, a group-writable shared queue with the process default.
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    replace_file(std::path::Path::new(&tmp), std::path::Path::new(path))
        .map_err(|e| format!("cannot replace {path}: {e}"))?;
    Ok(true)
}

/// Defaults from the config file. Every field is `None` when the file is absent
/// or does not set that key, so an unset key falls back to the built-in default
/// rather than an empty value.
#[derive(Default)]
struct Config {
    output_dir: Option<String>,
    filename_template: Option<String>,
    api_base: Option<String>,
    proxy: Option<String>,
    cookie: Option<String>,
    user_agent: Option<String>,
    ip_family: Option<String>,
    retries: Option<usize>,
    timeout_secs: Option<u64>,
    delay_ms: Option<u64>,
}

impl Config {
    /// Read the config file: `--config PATH` when given (a missing file is an
    /// error), else `$HOTAUDIO_CONFIG`, else `<config dir>/hotaudio/config.toml`
    /// (a missing default file is not an error).
    fn load(path_flag: Option<&str>) -> Result<Self, String> {
        let (path, required) = match path_flag {
            Some(path) => (std::path::PathBuf::from(path), true),
            None => match default_config_path() {
                Some(path) => (path, false),
                None => return Ok(Self::default()),
            },
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(e) => return Err(format!("cannot read config {}: {e}", path.display())),
        };
        Self::parse(&text).map_err(|e| format!("config {}: {e}", path.display()))
    }

    /// Parse the flat TOML subset documented in `--help`.
    ///
    /// Only `key = value` lines are accepted, with `#` comments and blank lines
    /// skipped; values are double-quoted strings or integers. Tables, arrays and
    /// bare strings are rejected rather than guessed at, so a file a full TOML
    /// parser would read differently is never silently half-applied.
    fn parse(text: &str) -> Result<Self, String> {
        let mut config = Self::default();
        for (n, raw) in text.lines().enumerate() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let line_no = n + 1;
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!(
                    "line {line_no}: expected 'key = value', got '{line}'"
                ));
            };
            let key = key.trim();
            let value = value.trim();
            match key {
                "output_dir" => config.output_dir = Some(parse_string(value, key, line_no)?),
                "filename_template" => {
                    let template = parse_string(value, key, line_no)?;
                    if let Some(message) = template_error(&template) {
                        return Err(format!("line {line_no}: {message}"));
                    }
                    config.filename_template = Some(template);
                }
                "api_base" => config.api_base = Some(parse_string(value, key, line_no)?),
                "proxy" => config.proxy = Some(parse_string(value, key, line_no)?),
                "cookie" => config.cookie = Some(parse_string(value, key, line_no)?),
                "user_agent" => config.user_agent = Some(parse_string(value, key, line_no)?),
                "ip_family" => {
                    let family = parse_string(value, key, line_no)?;
                    if !matches!(
                        family.to_ascii_lowercase().as_str(),
                        "any" | "ipv4" | "ipv6"
                    ) {
                        return Err(format!(
                            "line {line_no}: ip_family must be 'any', 'ipv4' or 'ipv6', got '{family}'"
                        ));
                    }
                    config.ip_family = Some(family);
                }
                "retries" => {
                    let retries = parse_uint(value, key, line_no)?;
                    config.retries = Some(usize::try_from(retries).map_err(|_| {
                        format!("line {line_no}: {key} is too large for this platform")
                    })?);
                }
                "timeout" => config.timeout_secs = Some(parse_uint(value, key, line_no)?),
                "delay" => config.delay_ms = Some(parse_uint(value, key, line_no)?),
                other => {
                    return Err(format!(
                        "line {line_no}: unknown key '{other}' (expected output_dir, \
                         filename_template, api_base, proxy, cookie, user_agent, ip_family, \
                         retries, timeout or delay)"
                    ));
                }
            }
        }
        Ok(config)
    }
}

/// An environment variable, treating an empty value as unset — CI passes an
/// unset secret through as the empty string.
fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// The default config location: `$HOTAUDIO_CONFIG`, else `config.toml` under
/// `$XDG_CONFIG_HOME/hotaudio` (an absolute `XDG_CONFIG_HOME` only, per the
/// spec), else `$HOME/.config/hotaudio`.
fn default_config_path() -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("HOTAUDIO_CONFIG") {
        return Some(std::path::PathBuf::from(path));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config"))
        })?;
    Some(base.join("hotaudio").join("config.toml"))
}

/// Drop a trailing `#` comment, ignoring a `#` inside a double-quoted string.
fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    for (i, byte) in line.bytes().enumerate() {
        if escaped {
            escaped = false;
        } else if byte == b'\\' && in_string {
            escaped = true;
        } else if byte == b'"' {
            in_string = !in_string;
        } else if byte == b'#' && !in_string {
            return &line[..i];
        }
    }
    line
}

/// A TOML basic string: `"..."`, with `\n`, `\r`, `\t`, `\"` and `\\` escapes.
fn parse_string(value: &str, key: &str, line_no: usize) -> Result<String, String> {
    let not_a_string =
        || format!("line {line_no}: {key} needs a double-quoted string, got '{value}'");
    let mut chars = value.chars();
    if chars.next() != Some('"') {
        return Err(not_a_string());
    }
    let mut out = String::new();
    loop {
        match chars.next() {
            Some('"') => break,
            Some('\\') => out.push(match chars.next() {
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some('"') => '"',
                Some('\\') => '\\',
                _ => return Err(not_a_string()),
            }),
            Some(c) => out.push(c),
            None => return Err(not_a_string()),
        }
    }
    if chars.as_str().trim().is_empty() {
        Ok(out)
    } else {
        Err(not_a_string())
    }
}

/// A non-negative integer, allowing `_` digit separators as TOML does.
fn parse_uint(value: &str, key: &str, line_no: usize) -> Result<u64, String> {
    let digits: String = value.chars().filter(|c| *c != '_').collect();
    digits
        .parse()
        .map_err(|_| format!("line {line_no}: {key} needs a whole number, got '{value}'"))
}

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Load saved keys from a path or inline JSON.
fn load_keys(path: Option<&str>) -> Result<download::SavedKeys, Error> {
    let Some(path) = path else {
        return Err(Error::Protocol("--keys <file | JSON> is required".into()));
    };
    let text = if path.trim_start().starts_with('{') {
        path.to_string()
    } else {
        std::fs::read_to_string(path).map_err(|e| Error::Io(format!("cannot read {path}: {e}")))?
    };
    download::parse_saved_keys(&text)
}

/// Write the saved-keys envelope, if requested.
fn write_saved_keys(
    path: Option<&str>,
    hax_url: &str,
    title: Option<&str>,
    keys: &HashMap<u32, [u8; 32]>,
    clobber: Clobber,
) -> Result<(), Error> {
    let Some(path) = path else { return Ok(()) };
    check_clobber(path, clobber)?;
    ensure_parent(path)?;
    let json = download::keys_to_json(keys, hax_url, title);
    std::fs::write(path, json).map_err(|e| Error::Io(format!("cannot write {path}: {e}")))?;
    eprintln!("Keys saved to {path}");
    Ok(())
}

/// Whether an existing output file may be replaced.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Clobber {
    /// Replace it (the default).
    Replace,
    /// Fail rather than touch it (`--no-clobber`).
    Refuse,
}

/// Fail under `--no-clobber` when `path` already exists.
///
/// `-` is stdout, which is never an existing file to protect.
fn check_clobber(path: &str, clobber: Clobber) -> Result<(), Error> {
    if clobber == Clobber::Refuse && path != "-" && std::path::Path::new(path).exists() {
        return Err(Error::State(format!(
            "{path} already exists; refusing to overwrite (--no-clobber)"
        )));
    }
    Ok(())
}

/// Create `path`'s parent directory if it is missing, so `--out a/b/x.m4a`
/// works even when `a/b` does not exist.
fn ensure_parent(path: &str) -> Result<(), Error> {
    let Some(parent) = std::path::Path::new(path).parent() else {
        return Ok(());
    };
    // A bare filename (`track.m4a`) and stdout (`-`) have an empty parent.
    if !parent.as_os_str().is_empty() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Io(format!("cannot create {}: {e}", parent.display())))?;
    }
    Ok(())
}

/// Write bytes to `path`, or to stdout when `path` is `-`.
fn write_file(path: &str, bytes: &[u8], clobber: Clobber) -> Result<(), Error> {
    let mut out = open_output(path, clobber)?;
    out.write_all(bytes).map_err(|e| Error::Io(e.to_string()))?;
    out.flush().map_err(|e| Error::Io(e.to_string()))
}

/// Run `--verify` over decrypted audio and report the result on stderr.
///
/// Takes a reader rather than bytes so `--stream-to` can check the file it just
/// wrote without loading it back into memory.
fn verify_or_report(reader: impl std::io::Read, expected_ms: u64) -> Result<(), Error> {
    let info = download::verify_output(reader, expected_ms)?;
    match info.duration_ms {
        Some(ms) => eprintln!(
            "verified: {} boxes, {ms} ms (container {expected_ms} ms)",
            info.boxes
        ),
        None => eprintln!(
            "verified: {} boxes, duration not carried in the headers",
            info.boxes
        ),
    }
    Ok(())
}

/// A writer for `path`, or stdout when `path` is `-`.
///
/// `-` lets the audio be piped (`--out - | ffplay -`). Progress and the result
/// line stay on stderr in that case (see [`print_result`]) so the stream is
/// never corrupted. Missing parent directories are created.
fn open_output(path: &str, clobber: Clobber) -> Result<Box<dyn Write>, Error> {
    if path == "-" {
        return Ok(Box::new(std::io::stdout()));
    }
    check_clobber(path, clobber)?;
    ensure_parent(path)?;
    let file =
        std::fs::File::create(path).map_err(|e| Error::Io(format!("cannot create {path}: {e}")))?;
    Ok(Box::new(file))
}

/// The `--stream-to` destination, written through a sibling temp file.
///
/// A streaming download can fail midway, and a truncated fragmented MP4 is not
/// playable. Rather than risk that, the bytes go to `<dest>.part` and
/// [`StreamSink::commit`] renames them onto `dest` only once the whole download
/// succeeded. [`Drop`] removes the temp file otherwise, so a failed run leaves
/// neither a corrupt `.m4a` nor a stray temp file. `-` streams straight to
/// stdout, which cannot be made atomic.
struct StreamSink {
    /// Temp file to promote, or `None` when the destination is stdout.
    temp: Option<String>,
    dest: String,
    clobber: Clobber,
    committed: bool,
}

impl StreamSink {
    /// Prepare `dest` and return its writer. Fails early under `--no-clobber`
    /// if `dest` already exists.
    fn open(dest: &str, clobber: Clobber) -> Result<(Self, Box<dyn Write>), Error> {
        if dest == "-" {
            let sink = Self {
                temp: None,
                dest: dest.to_string(),
                clobber,
                committed: false,
            };
            return Ok((sink, Box::new(std::io::stdout())));
        }
        check_clobber(dest, clobber)?;
        ensure_parent(dest)?;
        let temp = format!("{dest}.part");
        let file = std::fs::File::create(&temp)
            .map_err(|e| Error::Io(format!("cannot create {temp}: {e}")))?;
        let sink = Self {
            temp: Some(temp),
            dest: dest.to_string(),
            clobber,
            committed: false,
        };
        Ok((sink, Box::new(file)))
    }

    /// Promote the temp file onto the destination.
    ///
    /// Re-checks `--no-clobber`: the destination could have appeared since
    /// [`StreamSink::open`]. Otherwise the rename replaces it; on Windows
    /// `rename` cannot replace, so an existing destination is removed first.
    fn commit(mut self) -> Result<(), Error> {
        if let Some(temp) = self.temp.clone() {
            check_clobber(&self.dest, self.clobber)?;
            #[cfg(windows)]
            if std::path::Path::new(&self.dest).exists() {
                std::fs::remove_file(&self.dest)
                    .map_err(|e| Error::Io(format!("cannot replace {}: {e}", self.dest)))?;
            }
            std::fs::rename(&temp, &self.dest)
                .map_err(|e| Error::Io(format!("cannot move {temp} to {}: {e}", self.dest)))?;
        }
        self.committed = true;
        Ok(())
    }

    /// The temp file's path before [`StreamSink::commit`], so `--verify` can
    /// inspect the output that is about to be promoted; `None` for stdout.
    fn pending_path(&self) -> Option<&str> {
        self.temp.as_deref()
    }
}

impl Drop for StreamSink {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Some(temp) = &self.temp {
            let _ = std::fs::remove_file(temp);
        }
    }
}

/// Print a result line to stdout, or to stderr when `dest` is stdout (`-`),
/// where stdout carries the audio instead.
fn print_result(dest: &str, line: &str) {
    if dest == "-" {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
}

/// Windows device names that are unusable as file names, extension or not.
const RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Longest stem [`sanitize`] emits, before an extension. Leaves room under the
/// common 255-byte filename limit for `.m4a` and the truncation suffix.
const MAX_STEM: usize = 200;

/// Placeholders accepted by `--filename-template`.
const TEMPLATE_FIELDS: &[&str] = &["title", "index", "ext"];

/// Turn a title into a safe, single path component.
///
/// Removes characters that are illegal on Windows, macOS or Linux and all
/// control characters; drops leading/trailing dots and spaces (which Windows
/// forbids and which would hide the file on Unix); escapes Windows reserved
/// device names; and caps the length with a deterministic `~<hash>` suffix so
/// two long titles do not collapse to one name.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(c, '\\' | '/' | '*' | '?' | '"' | '<' | '>' | '|' | ':')
        })
        .collect();
    let trimmed = cleaned.trim_matches(['.', ' ']);
    let base = if trimmed.is_empty() {
        "hotaudio-track"
    } else {
        trimmed
    };
    // A reserved name is unusable even with an extension (`CON.m4a`), so prefix.
    let base = if is_reserved(base) {
        format!("_{base}")
    } else {
        base.to_string()
    };
    cap_length(&base)
}

/// Whether `name` is a Windows reserved device name, ignoring any extension.
fn is_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_lowercase();
    RESERVED_NAMES.contains(&stem.as_str())
}

/// Cap `stem` to [`MAX_STEM`] bytes, appending `~<8 hex>` of the original when
/// it has to be cut. The suffix keeps the mapping deterministic and collision-
/// resistant instead of silently truncating.
fn cap_length(stem: &str) -> String {
    if stem.len() <= MAX_STEM {
        return stem.to_string();
    }
    // "~" + 8 hex chars of headroom.
    let keep = MAX_STEM - 9;
    let hash = hotaudio::crypto::hex_encode(&hotaudio::crypto::sha256(stem.as_bytes()));
    format!("{}~{}", truncate_bytes(stem, keep), &hash[..8])
}

/// The longest prefix of `s` within `max` bytes, cut on a char boundary.
fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Reject a `--filename-template` with an unknown or unclosed placeholder, so a
/// typo fails before any network work.
fn template_error(template: &str) -> Option<String> {
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            return Some(format!("unclosed '{{' in --filename-template: {template}"));
        };
        let name = &after[..close];
        if !TEMPLATE_FIELDS.contains(&name) {
            return Some(format!(
                "unknown placeholder {{{name}}} in --filename-template \
                 (known: {{title}}, {{index}}, {{ext}})"
            ));
        }
        rest = &after[close + 1..];
    }
    None
}

/// Expand a template validated by [`template_error`]. `{title}` is sanitised
/// here, before the result is split into path components, so a title can never
/// introduce a directory. Unknown placeholders cannot reach here and are kept
/// verbatim if they somehow do.
fn expand_template(template: &str, title: &str, index: usize) -> String {
    let mut out = String::with_capacity(template.len() + title.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(after);
            return out;
        };
        match &after[..close] {
            "title" => out.push_str(&sanitize(title)),
            "index" => out.push_str(&index.to_string()),
            "ext" => out.push_str("m4a"),
            other => {
                out.push('{');
                out.push_str(other);
                out.push('}');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// The output path for a title-derived download.
///
/// `--out` is handled by the caller; this only names the file when no path was
/// given. With a template, each `/`-separated component is sanitised (so a
/// title cannot inject a path) and the last gets the `.m4a` extension; without
/// one, the sanitised title is used. `index` is 1-based.
fn derived_name(template: Option<&str>, title: &str, index: usize) -> String {
    let Some(template) = template else {
        return format!("{}.m4a", sanitize(title));
    };
    let expanded = expand_template(template, title, index);
    let mut parts: Vec<String> = expanded.split('/').map(sanitize).collect();
    // `split` always yields at least one item, so `parts` is non-empty.
    let last = parts.len() - 1;
    let has_m4a = std::path::Path::new(&parts[last])
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("m4a"));
    if !has_m4a {
        parts[last].push_str(".m4a");
    }
    parts.join("/")
}

/// The title-derived output path for one track, honouring `--output-dir` (or the
/// config's `output_dir`). An explicit `--out` or `--stream-to` is a whole path
/// and never goes through here.
fn default_output(
    template: Option<&str>,
    output_dir: Option<&str>,
    title: &str,
    index: usize,
) -> String {
    let name = derived_name(template, title, index);
    match output_dir {
        Some(dir) => std::path::Path::new(dir)
            .join(name)
            .to_string_lossy()
            .into_owned(),
        None => name,
    }
}

/// Filename stem from a path, without its extension.
fn stem(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    match base.rsplit_once('.') {
        Some((head, _)) if !head.is_empty() => head.to_string(),
        _ => base.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse `args` the way the binary would, with the program name prepended.
    #[cfg(not(target_arch = "wasm32"))]
    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        let mut cli_args = vec!["hotaudio-download"];
        cli_args.extend_from_slice(args);
        Cli::try_parse_from(cli_args)
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn rejects_unknown_flags() {
        assert!(parse(&["--nope", "x"]).is_err());
        assert!(parse(&["-x"]).is_err());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn accepts_known_flag_forms() {
        assert!(parse(&["--out", "a.m4a"]).is_ok());
        assert!(parse(&["--out=a.m4a"]).is_ok());
        assert!(parse(&["--list-tracks"]).is_ok());
        assert!(parse(&["--all-tracks"]).is_ok());
        assert!(parse(&["--no-clobber"]).is_ok());
        assert!(parse(&["--force"]).is_ok());
        assert!(parse(&["--filename-template", "{index}-{title}"]).is_ok());
        assert!(parse(&["--proxy", "http://127.0.0.1:8080"]).is_ok());
        assert!(parse(&["--retries", "3"]).is_ok());
        assert!(parse(&["--timeout", "60"]).is_ok());
        assert!(parse(&["--delay", "500"]).is_ok());
        assert!(parse(&["--quiet"]).is_ok());
        assert!(parse(&["-q"]).is_ok());
        assert!(parse(&["--verbose"]).is_ok());
        assert!(parse(&["-v"]).is_ok());
        assert!(parse(&["--json"]).is_ok());
        assert!(parse(&["--progress", "json"]).is_ok());
        assert!(parse(&["--ip-family", "ipv4"]).is_ok());
        assert!(parse(&["--ip-family", "ipv6"]).is_ok());
        assert!(parse(&["--ip-family", "any"]).is_ok());
        assert!(parse(&["--ip-family", "ipv5"]).is_err());
        assert!(parse(&["--webview"]).is_ok());
        assert!(parse(&["--no-webview"]).is_ok());
        assert!(parse(&["--webview", "--no-webview"]).is_err());
        // The helper timeout also bounds an automatic pass.
        assert!(parse(&["--webview-timeout", "30"]).is_ok());
        assert!(parse(&["--input", "urls.txt", "--remove-downloaded"]).is_ok());
        // Rewriting a file needs a file; stdin cannot be pruned.
        assert!(parse(&["--remove-downloaded"]).is_err());
        // Inline JSON is a value, not a flag.
        assert!(parse(&["--keys", "{\"a\":1}"]).is_ok());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn retry_timeout_and_delay_flags_parse() {
        // Absent means the library defaults.
        assert_eq!(retry_for(None, None).attempts, Retry::default().attempts);
        // `--retries N` is N retries after the first attempt.
        assert_eq!(retry_for(Some(0), None).attempts, 1);
        assert_eq!(retry_for(Some(5), None).attempts, 6);
        assert!(parse(&["--retries", "x"]).is_err());

        assert_eq!(timeout_ms(None, None).unwrap(), None);
        assert_eq!(timeout_ms(Some(60), None).unwrap(), Some(60_000));
        // Zero is rejected, whether it arrives from clap or from a config file.
        assert!(timeout_ms(Some(0), None).is_err());
        assert!(parse(&["--timeout", "soon"]).is_err());
        assert!(parse(&["--timeout", "0"]).is_err());

        assert_eq!(delay_ms(None, None), 0);
        assert_eq!(delay_ms(Some(250), None), 250);
        assert!(parse(&["--delay", "later"]).is_err());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn quiet_and_verbose_conflict() {
        assert!(parse(&["--quiet", "--verbose"]).is_err());
        assert!(parse(&["-q", "-v"]).is_err());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn no_clobber_and_force_conflict() {
        assert!(parse(&["--no-clobber", "--force"]).is_err());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn save_hax_conflicts_with_stream_to() {
        assert!(parse(&["--save-hax", "c.hax", "--stream-to", "a.m4a"]).is_err());
        assert!(parse(&["--save-hax=c.hax", "--stream-to=a.m4a"]).is_err());
        assert!(parse(&["--save-hax", "c.hax"]).is_ok());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn keys_only_requires_save_keys() {
        assert!(parse(&["--keys-only"]).is_err());
        assert!(parse(&["--keys-only", "--save-keys", "k.json"]).is_ok());
        assert!(parse(&["--keys-only", "--save-keys=k.json"]).is_ok());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn keys_only_conflicts_with_other_modes() {
        for extra in [
            ["--hax", "in.hax"],
            ["--stream-to", "a.m4a"],
            ["--keys", "k2.json"],
        ] {
            let mut args = vec!["--keys-only", "--save-keys", "k.json"];
            args.extend(extra);
            assert!(parse(&args).is_err(), "{extra:?}");
        }
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn all_tracks_conflicts_with_track() {
        assert!(parse(&["--all-tracks", "--track", "t1"]).is_err());
        assert!(parse(&["--all-tracks", "--track=t1"]).is_err());
        assert!(parse(&["--all-tracks", "--json"]).is_ok());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn verify_conflicts_with_keys_only_and_a_stdout_stream() {
        assert!(parse(&["--verify", "--keys-only", "--save-keys", "k.json"]).is_err());
        // The stdout rule depends on the value, so it is a post-parse check.
        let to_stdout = parse(&["--verify", "--stream-to", "-"]).unwrap();
        assert!(validate_mode(&to_stdout).is_err());
        assert!(validate_mode(&parse(&["--verify", "--stream-to", "a.m4a"]).unwrap()).is_ok());
        assert!(validate_mode(&parse(&["--verify"]).unwrap()).is_ok());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn filename_template_rejects_unknown_placeholders() {
        let err = validate_mode(&parse(&["--filename-template", "{nope}"]).unwrap()).unwrap_err();
        assert!(err.contains("nope"), "got {err}");
        assert!(validate_mode(&parse(&["--filename-template", "{title}"]).unwrap()).is_ok());
    }

    #[test]
    fn json_string_escapes_control_characters() {
        assert_eq!(json_string("plain"), "\"plain\"");
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_string("a\\b"), "\"a\\\\b\"");
        assert_eq!(json_string("a\nb\tc"), "\"a\\nb\\tc\"");
        assert_eq!(json_string("\u{1}"), "\"\\u0001\"");
    }

    #[test]
    fn download_info_is_a_json_object() {
        let info = DownloadInfo {
            title: Some("Track"),
            duration: 12.5,
            codec: "mp4a.40.2",
            mime: "audio/mp4",
            bytes: 1024,
            segment_count: 4,
            key_pages: 2,
            hax_url: "https://cdn.hotaudio.net/a/x.hax",
            keys_json: "{\"v\":1}",
            saved_to: "out.m4a",
        };
        assert_eq!(
            info.to_json(),
            "{\"title\":\"Track\",\"durationSecs\":12.500,\"codec\":\"mp4a.40.2\",\
             \"mime\":\"audio/mp4\",\"bytes\":1024,\"segmentCount\":4,\"keyPages\":2,\
             \"haxUrl\":\"https://cdn.hotaudio.net/a/x.hax\",\"keysJson\":{\"v\":1},\
             \"savedTo\":\"out.m4a\"}"
        );

        let untitled = DownloadInfo {
            title: None,
            ..info
        };
        assert!(untitled.to_json().contains("\"title\":null"));
    }

    #[test]
    fn progress_event_is_ndjson() {
        let p = Progress {
            phase: Phase::Fetching,
            loaded: 5,
            total: 10,
        };
        assert_eq!(
            progress_event(p),
            "{\"phase\":\"fetching\",\"loaded\":5,\"total\":10}"
        );
    }

    #[test]
    fn open_output_selects_stdout_for_dash() {
        // `-` must not touch the filesystem.
        assert!(open_output("-", Clobber::Replace).is_ok());
        assert!(open_output("-", Clobber::Refuse).is_ok());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn open_output_creates_missing_parents() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("hotaudio-parents-{pid}"));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("a/b/track.m4a");
        assert!(open_output(file.to_str().unwrap(), Clobber::Replace).is_ok());
        assert!(file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn stream_sink_commits_a_temp_file() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("hotaudio-stream-commit-{pid}"));
        let _ = std::fs::remove_dir_all(&dir);
        let dest = dir.join("track.m4a");
        let dest = dest.to_str().unwrap();
        let temp = format!("{dest}.part");

        let (sink, mut out) = StreamSink::open(dest, Clobber::Replace).unwrap();
        out.write_all(b"audio").unwrap();
        out.flush().unwrap();
        drop(out);
        // Nothing lands at the destination until the whole stream succeeds.
        assert!(std::path::Path::new(&temp).exists(), "temp while streaming");
        assert!(
            !std::path::Path::new(dest).exists(),
            "dest waits for commit"
        );
        sink.commit().unwrap();
        assert_eq!(std::fs::read(dest).unwrap(), b"audio");
        assert!(
            !std::path::Path::new(&temp).exists(),
            "temp removed on commit"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn stream_sink_drops_a_failed_partial() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("hotaudio-stream-fail-{pid}"));
        let _ = std::fs::remove_dir_all(&dir);
        let dest = dir.join("track.m4a");
        let dest = dest.to_str().unwrap();
        let temp = format!("{dest}.part");

        {
            // Dropping without `commit` is a mid-download failure.
            let (sink, mut out) = StreamSink::open(dest, Clobber::Replace).unwrap();
            out.write_all(b"partial").unwrap();
            drop(out);
            drop(sink);
        }
        assert!(!std::path::Path::new(dest).exists(), "no corrupt dest");
        assert!(
            !std::path::Path::new(&temp).exists(),
            "temp cleaned up on drop"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn stream_sink_honors_no_clobber() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("hotaudio-stream-clobber-{pid}"));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("track.m4a");
        let dest = dest.to_str().unwrap();
        std::fs::write(dest, b"old").unwrap();

        assert!(StreamSink::open(dest, Clobber::Refuse).is_err());
        // Under Replace the old file survives until the commit replaces it.
        let (sink, out) = StreamSink::open(dest, Clobber::Replace).unwrap();
        drop(out);
        assert_eq!(std::fs::read(dest).unwrap(), b"old");
        sink.commit().unwrap();
        assert_eq!(std::fs::read(dest).unwrap(), b"");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn no_clobber_refuses_an_existing_file() {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("hotaudio-clobber-{pid}"));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("track.m4a");
        std::fs::write(&file, b"old").unwrap();
        let path = file.to_str().unwrap();

        assert!(open_output(path, Clobber::Refuse).is_err());
        assert!(open_output(path, Clobber::Replace).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn rejects_flags_missing_their_value() {
        assert!(parse(&["--out"]).is_err());
        // The value position holds another flag: almost certainly a mistake,
        // and it would otherwise write a file with that name.
        assert!(parse(&["--out", "--list-tracks"]).is_err());
        assert!(parse(&["--list-tracks=x"]).is_err());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn dash_is_a_valid_value() {
        // `-` means stdout/stdin, not a flag.
        assert_eq!(parse(&["--out", "-"]).unwrap().out.as_deref(), Some("-"));
        assert_eq!(
            parse(&["--input", "-"]).unwrap().input.as_deref(),
            Some("-")
        );
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn multi_output_rejects_single_path_flags() {
        let with = |args: &[&str]| parse(args).unwrap();
        // One source, no --all-tracks: nothing to reject.
        assert_eq!(
            multi_output_conflict(1, false, &with(&["--out", "a.m4a"])),
            None
        );
        // Two sources: the offending single-path flag is named.
        assert_eq!(
            multi_output_conflict(2, false, &with(&["--out", "a.m4a"])),
            Some("--out")
        );
        assert_eq!(
            multi_output_conflict(2, false, &with(&["--stream-to=a.m4a"])),
            Some("--stream-to")
        );
        // --all-tracks writes one file per track, so it needs its own paths too.
        assert_eq!(
            multi_output_conflict(1, true, &with(&["--out", "a.m4a"])),
            Some("--out")
        );
        assert_eq!(
            multi_output_conflict(1, true, &with(&["--save-keys", "k.json"])),
            Some("--save-keys")
        );
        // Flags that apply to the whole run are allowed.
        assert_eq!(
            multi_output_conflict(2, false, &with(&["--json", "--track", "t"])),
            None
        );
        assert_eq!(multi_output_conflict(1, true, &with(&["--json"])), None);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn positionals_skip_flags_and_their_values() {
        let url = "https://x/u/a/b";

        assert_eq!(parse(&[url]).unwrap().sources, vec![url.to_string()]);
        // The value of --out must not be mistaken for the source.
        assert_eq!(
            parse(&["--out", "a.m4a", url]).unwrap().sources,
            vec![url.to_string()]
        );
        assert_eq!(
            parse(&["--out=a.m4a", url]).unwrap().sources,
            vec![url.to_string()]
        );
        // Boolean flags do not consume the next argument.
        assert_eq!(
            parse(&["--list-tracks", url]).unwrap().sources,
            vec![url.to_string()]
        );
        assert_eq!(
            parse(&["--all-tracks", url]).unwrap().sources,
            vec![url.to_string()]
        );
        assert_eq!(
            parse(&["--filename-template", "{title}", url])
                .unwrap()
                .sources,
            vec![url.to_string()]
        );
        assert_eq!(
            parse(&["--retries", "4", "--timeout", "90", "--delay", "500", url])
                .unwrap()
                .sources,
            vec![url.to_string()]
        );
        assert_eq!(
            parse(&["--list-tracks"]).unwrap().sources,
            Vec::<String>::new()
        );
        // Several sources are collected in order.
        assert_eq!(
            parse(&[url, "https://x/u/c/d"]).unwrap().sources,
            vec![url.to_string(), "https://x/u/c/d".to_string()]
        );
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn flag_reads_both_spellings() {
        assert_eq!(
            parse(&["--out", "a.m4a"]).unwrap().out.as_deref(),
            Some("a.m4a")
        );
        assert_eq!(
            parse(&["--out=b.m4a"]).unwrap().out.as_deref(),
            Some("b.m4a")
        );
    }

    #[test]
    fn sanitises_titles_into_filenames() {
        assert_eq!(sanitize("a/b*c"), "abc");
        assert_eq!(sanitize("   "), "hotaudio-track");
        assert_eq!(sanitize("Track One"), "Track One");
    }

    #[test]
    fn sanitize_strips_control_and_edge_characters() {
        assert_eq!(sanitize("a\u{0}b\u{1f}c"), "abc");
        assert_eq!(sanitize("a:b|c"), "abc");
        assert_eq!(sanitize("...hidden"), "hidden");
        assert_eq!(sanitize("trailing. "), "trailing");
        assert_eq!(sanitize("... "), "hotaudio-track");
    }

    #[test]
    fn sanitize_escapes_reserved_names() {
        assert_eq!(sanitize("con"), "_con");
        assert_eq!(sanitize("NUL.m4a"), "_NUL.m4a");
        assert_eq!(sanitize("com9"), "_com9");
        // A longer name that merely starts with a reserved one is fine.
        assert_eq!(sanitize("console"), "console");
    }

    #[test]
    fn sanitize_caps_long_names_deterministically() {
        let long = "a".repeat(400);
        let once = sanitize(&long);
        assert!(once.len() <= MAX_STEM, "len {}", once.len());
        assert_eq!(once, sanitize(&long));
        assert!(once.contains('~'));
        // Different long inputs do not collapse to the same name.
        let other = format!("{}b", "a".repeat(399));
        assert_ne!(sanitize(&other), once);
    }

    #[test]
    fn template_rejects_unknown_and_unclosed_placeholders() {
        assert!(template_error("{index} - {title}.{ext}").is_none());
        assert!(template_error("plain").is_none());
        assert!(template_error("{nope}").is_some());
        assert!(template_error("{title").is_some());
    }

    #[test]
    fn derived_name_expands_placeholders() {
        assert_eq!(derived_name(None, "Track One", 1), "Track One.m4a");
        assert_eq!(
            derived_name(Some("{index} - {title}"), "Song", 2),
            "2 - Song.m4a"
        );
        assert_eq!(derived_name(Some("{ext}/{index}"), "x", 3), "m4a/3.m4a");
        // An explicit extension is not doubled.
        assert_eq!(derived_name(Some("{title}.m4a"), "Song", 1), "Song.m4a");
        // A title cannot inject a directory separator.
        assert_eq!(derived_name(Some("{title}"), "a/b", 1), "ab.m4a");
    }

    #[test]
    fn stem_drops_the_extension() {
        assert_eq!(stem("/tmp/a/b/audio.hax"), "audio");
        assert_eq!(stem("audio"), "audio");
        assert_eq!(stem(".hidden"), ".hidden");
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn usage_documents_every_flag() {
        let help = usage();
        for arg in Cli::command().get_arguments() {
            if arg.is_hide_set() {
                continue;
            }
            if let Some(long) = arg.get_long() {
                let flag = format!("--{long}");
                assert!(help.contains(&flag), "help does not mention {flag}");
            }
        }
        assert!(help.contains("--help"));
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn completions_and_man_page_render() {
        let mut completions = Vec::new();
        clap_complete::generate(
            clap_complete::Shell::Bash,
            &mut Cli::command(),
            BIN_NAME,
            &mut completions,
        );
        assert!(
            String::from_utf8_lossy(&completions).contains(BIN_NAME),
            "bash completions are empty"
        );

        let mut man = Vec::new();
        write_man(&mut man).unwrap();
        let man = String::from_utf8_lossy(&man);
        assert!(man.contains(BIN_NAME), "man page is empty");
        assert!(man.contains(".TH"), "man page is not roff");
    }

    #[test]
    fn config_parses_recognised_keys() {
        let cfg = Config::parse(
            "# defaults\n\
             output_dir = \"/music\"\n\
             filename_template = \"{index} - {title}\"\n\
             api_base = \"https://example.test\"\n\
             proxy = \"http://127.0.0.1:8080\"\n\
             cookie = \"cf_clearance=abc\"\n\
             user_agent = \"Mozilla/5.0 (X11; Linux x86_64) Chrome/156.0.0.0\"\n\
             ip_family = \"ipv6\"\n\
             retries = 4\n\
             timeout = 60\n\
             delay = 250\n",
        )
        .unwrap();
        assert_eq!(cfg.output_dir.as_deref(), Some("/music"));
        assert_eq!(cfg.filename_template.as_deref(), Some("{index} - {title}"));
        assert_eq!(cfg.api_base.as_deref(), Some("https://example.test"));
        assert_eq!(cfg.proxy.as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(cfg.cookie.as_deref(), Some("cf_clearance=abc"));
        assert_eq!(
            cfg.user_agent.as_deref(),
            Some("Mozilla/5.0 (X11; Linux x86_64) Chrome/156.0.0.0")
        );
        assert_eq!(cfg.ip_family.as_deref(), Some("ipv6"));
        assert_eq!(cfg.retries, Some(4));
        assert_eq!(cfg.timeout_secs, Some(60));
        assert_eq!(cfg.delay_ms, Some(250));
    }

    #[test]
    fn config_rejects_bad_keys_and_values() {
        // A table header, an unknown key, a bare (unquoted) string, a non-integer
        // and a bad template all fail rather than being ignored.
        assert!(Config::parse("[defaults]\ntimeout = 5\n").is_err());
        assert!(Config::parse("nope = 1\n").is_err());
        assert!(Config::parse("timeout = \"soon\"\n").is_err());
        assert!(Config::parse("timeout = 5\n").is_ok());
        assert!(Config::parse("api_base = https://x\n").is_err());
        assert!(Config::parse("filename_template = \"{nope}\"\n").is_err());
    }

    #[test]
    fn config_strips_comments_and_unescapes_strings() {
        let cfg = Config::parse(
            r#"output_dir = "/a#b"  # a comment, not part of the value
proxy = "a\\b\tc"
"#,
        )
        .unwrap();
        assert_eq!(cfg.output_dir.as_deref(), Some("/a#b"));
        assert_eq!(cfg.proxy.as_deref(), Some("a\\b\tc"));
    }

    #[test]
    fn config_is_empty_when_unset() {
        let cfg = Config::parse("\n# nothing here\n").unwrap();
        assert!(cfg.output_dir.is_none());
        assert!(cfg.api_base.is_none());
        assert!(cfg.retries.is_none());
    }

    #[test]
    fn exit_codes_distinguish_failure_classes() {
        assert_eq!(exit_code(&Error::Http("x".into())), code::NETWORK);
        assert_eq!(
            exit_code(&Error::Challenge("cloudflare".into())),
            code::NETWORK
        );
        assert_eq!(exit_code(&Error::Io("x".into())), code::IO);
        assert_eq!(exit_code(&Error::Protocol("x".into())), code::PROTOCOL);
        assert_eq!(exit_code(&Error::Sign("x".into())), code::PROTOCOL);
        assert_eq!(exit_code(&Error::Listen("x".into())), code::PROTOCOL);
        assert_eq!(exit_code(&Error::State("x".into())), code::PROTOCOL);
        assert_eq!(exit_code(&Error::Decode("hex")), code::PROTOCOL);
        assert_eq!(exit_code(&Error::Auth), code::PROTOCOL);
        assert_eq!(exit_code(&Error::Cancelled), code::CANCELLED);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn policy_flags_fall_back_to_config() {
        // The flag wins when present.
        assert_eq!(retry_for(Some(5), Some(1)).attempts, 6);
        // Otherwise the config value is used.
        assert_eq!(retry_for(None, Some(3)).attempts, 4);
        // Without either, the built-in default.
        assert_eq!(retry_for(None, None).attempts, Retry::default().attempts);

        assert_eq!(timeout_ms(Some(2), Some(10)).unwrap(), Some(2_000));
        assert_eq!(timeout_ms(None, Some(10)).unwrap(), Some(10_000));
        assert!(timeout_ms(None, Some(0)).is_err());

        assert_eq!(delay_ms(Some(5), Some(250)), 5);
        assert_eq!(delay_ms(None, Some(250)), 250);
        assert_eq!(delay_ms(None, None), 0);
    }

    #[test]
    fn default_output_prefixes_the_output_dir() {
        assert_eq!(default_output(None, None, "Track One", 1), "Track One.m4a");
        // Compare through `Path::join` so the test holds on every separator.
        let under = |dir: &str, name: &str| {
            std::path::Path::new(dir)
                .join(name)
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(
            default_output(None, Some("music"), "Track One", 1),
            under("music", "Track One.m4a")
        );
        assert_eq!(
            default_output(Some("{index}"), Some("out"), "x", 2),
            under("out", "2.m4a")
        );
        // The template's own subdirectories are preserved under the directory.
        assert_eq!(
            default_output(Some("{ext}/{index}"), Some("out"), "x", 3),
            under("out", "m4a/3.m4a")
        );
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn prune_input_removes_one_matching_line() {
        let path = std::env::temp_dir().join(format!(
            "hotaudio-prune-{}-{}.txt",
            std::process::id(),
            hotaudio::clock::monotonic_ms()
        ));
        std::fs::write(&path, "a\n\n# note\nb\na\n").unwrap();
        let path = path.to_string_lossy().into_owned();
        assert!(prune_input_source(&path, "a").unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "\n# note\nb\na\n");
        assert!(prune_input_source(&path, "a").unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "\n# note\nb\n");
        // Already consumed: no rewrite and no error.
        assert!(!prune_input_source(&path, "a").unwrap());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cached_clearance_round_trips() {
        let entry = CachedClearance {
            cookie: "cf_clearance=abc".into(),
            user_agent: "Mozilla/5.0".into(),
            ip_family: Some("ipv6".into()),
            saved_at: 1.5,
        };
        let json = serde_json::to_string(&entry).unwrap();
        // The JSON keys match the webview helper's `ClearancePayload`.
        assert!(json.contains("\"userAgent\""));
        assert!(json.contains("\"ipFamily\""));
        let back: CachedClearance = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cookie, entry.cookie);
        assert_eq!(back.user_agent, entry.user_agent);
        assert_eq!(back.ip_family, entry.ip_family);
        assert!((back.saved_at - entry.saved_at).abs() < f64::EPSILON);

        // `ipFamily` is optional in a file written without family detection.
        let legacy: CachedClearance =
            serde_json::from_str(r#"{"cookie":"c","userAgent":"u","saved_at":0}"#).unwrap();
        assert_eq!(legacy.ip_family, None);
    }
}
