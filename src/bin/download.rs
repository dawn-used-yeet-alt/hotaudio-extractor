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

use std::collections::HashMap;
use std::io::Write;
use std::process::ExitCode;

use hotaudio::crypto::Error;
use hotaudio::download::{self, DownloadOptions, Phase, Progress};
use hotaudio::http::Client;
use hotaudio::listen;

/// Program name, printed by `--version`.
const BIN_NAME: &str = "hotaudio-download";

const USAGE: &str = "\
hotaudio-download — download and decrypt a hotaudio.net track to playable .m4a

Usage:
  hotaudio-download <URL|HTML> [options]      full pipeline (fetch page, listen, download)
  hotaudio-download --keys FILE [options]     cached re-download (no page fetch, no listen)
  hotaudio-download --hax FILE --keys FILE    fully offline decrypt of a local container

Options:
  --out PATH        write the decrypted audio here ('-' writes to stdout)
                    (default: the track title, sanitised, as .m4a)
  --stream-to PATH  stream segments to PATH instead of buffering: peak memory of
                    one segment, first playable fragment after one round trip
                    ('-' streams to stdout)
  --save-keys PATH  write the saved-keys envelope here for later reuse
  --save-hax PATH   write the raw .hax container here for a later offline decrypt
                    (buffered modes only; not with --stream-to)
  --keys PATH       saved-keys envelope to resume from; accepts a file path or
                    inline JSON. In online mode, seeds the key map so an
                    interrupted download refetches only what is missing
  --hax PATH        decrypt this local .hax container (fully offline)
  --hax-url URL     container URL for cached mode, when the envelope has none
  --track ID        select a track by id on a multi-track page
  --api-base URL    override the listen API base (default: https://hotaudio.net)
  -q, --quiet       suppress the progress reporter
  -v, --verbose     report key-page counts and timing
  --json            print a JSON result object on stdout (a track array for
                    --list-tracks)
  --progress FORMAT progress on stderr: 'text' (default) or 'json' (NDJSON)
  --list-tracks     list the tracks on the page and exit
  -h, --help        show this help
  -V, --version     print the version and exit

The user agent is fixed at Mozilla/5.0 and is not configurable: the site returns
403 for Chrome user agents on the track page and the listen endpoint.";

/// Flags that take a value. Anything else starting with `-` is rejected, so a
/// typo fails loudly instead of being silently ignored.
const VALUE_FLAGS: &[&str] = &[
    "--out",
    "--stream-to",
    "--save-keys",
    "--keys",
    "--hax",
    "--hax-url",
    "--track",
    "--api-base",
    "--progress",
    "--save-hax",
];

/// Flags that stand alone.
const BOOL_FLAGS: &[&str] = &[
    "--list-tracks",
    "--help",
    "-h",
    "--version",
    "-V",
    "--quiet",
    "-q",
    "--verbose",
    "-v",
    "--json",
];

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

/// Parse arguments and run. The pipeline is `async` so one implementation serves
/// both transports; natively it never suspends, so driving it is a single poll.
#[cfg(not(target_arch = "wasm32"))]
fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("{BIN_NAME} {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    }
    if let Err(e) = validate(&args) {
        eprintln!("error: {e}\n");
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    }

    let source = positional(&args);
    let out = flag(&args, "--out");
    let stream_to = flag(&args, "--stream-to");
    let save_keys = flag(&args, "--save-keys");
    let save_hax = flag(&args, "--save-hax");
    let hax_path = flag(&args, "--hax");
    let hax_url = flag(&args, "--hax-url");
    let keys_path = flag(&args, "--keys");
    let track_id = flag(&args, "--track");
    let api_base = flag(&args, "--api-base");
    let list_tracks = args.iter().any(|a| a == "--list-tracks");
    let quiet = args.iter().any(|a| a == "--quiet" || a == "-q");
    let verbose = args.iter().any(|a| a == "--verbose" || a == "-v");
    let json = args.iter().any(|a| a == "--json");
    let progress_json = match flag(&args, "--progress").as_deref() {
        None | Some("text") => false,
        Some("json") => true,
        Some(other) => {
            eprintln!("error: --progress must be 'text' or 'json', got '{other}'");
            return ExitCode::FAILURE;
        }
    };

    match hotaudio::block_on(execute(Run {
        source,
        out,
        stream_to,
        save_keys,
        save_hax,
        hax_path,
        hax_url,
        keys_path,
        track_id,
        api_base,
        list_tracks,
        quiet,
        verbose,
        json,
        progress_json,
    })) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Run {
    source: Option<String>,
    out: Option<String>,
    stream_to: Option<String>,
    save_keys: Option<String>,
    save_hax: Option<String>,
    hax_path: Option<String>,
    hax_url: Option<String>,
    keys_path: Option<String>,
    track_id: Option<String>,
    api_base: Option<String>,
    list_tracks: bool,
    quiet: bool,
    verbose: bool,
    json: bool,
    progress_json: bool,
}

async fn execute(r: Run) -> Result<(), Error> {
    // A URL that is not a canonical share link is usually a typo, and the
    // failure would otherwise only surface deep in the pipeline. This is a
    // warning, not a rejection: proxies and mirrors are legitimate.
    if let Some(source) = &r.source {
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

    // ---- Offline: local .hax + saved keys, no network ----
    if let Some(hax_path) = &r.hax_path {
        let saved = load_keys(&r.keys_path)?;
        let keys = saved.keys;
        let title = saved.title;
        let bytes = std::fs::read(hax_path)
            .map_err(|e| Error::Http(format!("cannot read {hax_path}: {e}")))?;
        let hax = hotaudio::hax::Hax0::parse(&bytes)?;
        if let Some(path) = &r.save_hax {
            write_file(path, &bytes)?;
        }
        let (audio, segments, _) = download::decrypt_offline(&bytes, &keys, Some(&mut report))?;
        if r.verbose {
            eprintln!(
                "verbose: {segments} segments in {:.1}s",
                secs_since(started_ms)
            );
        }
        write_saved_keys(&r.save_keys, "", title.as_deref(), &keys)?;
        let title = title.unwrap_or_else(|| stem(hax_path));
        let out = r.out.unwrap_or_else(|| format!("{}.m4a", sanitize(&title)));
        write_file(&out, &audio)?;
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
        let Some(source) = &r.source else {
            eprintln!("{USAGE}");
            return Err(Error::Protocol(
                "--list-tracks needs a URL or HTML file".into(),
            ));
        };
        let tracks = if is_url(source) {
            let client = Client::api();
            let resp = client.request("GET", source, &[], None).await?;
            if !resp.ok() {
                return Err(Error::Http(format!("page returned {}", resp.status)));
            }
            listen::list_tracks(&resp.text())
        } else {
            let html = std::fs::read_to_string(source)
                .map_err(|e| Error::Http(format!("cannot read {source}: {e}")))?;
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

    // ---- Cached mode: saved keys (+ envelope hax URL), no page/listen ----
    if r.keys_path.is_some() && r.source.is_none() {
        let saved = load_keys(&r.keys_path)?;
        let (keys, envelope_url, title) = (saved.keys, saved.hax_url, saved.title);
        let Some(url) = r.hax_url.or(envelope_url) else {
            return Err(Error::Protocol(
                "keys file has no .hax URL — pass --hax-url or use online mode".into(),
            ));
        };
        let client = Client::bulk();
        let res = download::download_cached(
            &client,
            &url,
            &keys,
            Some(&mut report),
            r.save_hax.is_some(),
        )
        .await?;
        if let Some(path) = &r.save_hax {
            if let Some(bytes) = &res.hax_bytes {
                write_file(path, bytes)?;
            }
        }
        if r.verbose {
            eprintln!(
                "verbose: {} segments in {:.1}s",
                res.segment_count,
                secs_since(started_ms)
            );
        }
        write_saved_keys(&r.save_keys, &url, title.as_deref(), &keys)?;
        let title = title.unwrap_or_else(|| "hotaudio-track".into());
        let out = r.out.unwrap_or_else(|| format!("{}.m4a", sanitize(&title)));
        write_file(&out, &res.audio)?;
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
    let Some(source) = r.source else {
        eprintln!("{USAGE}");
        return Err(Error::Protocol("no input given".into()));
    };

    let seed = match &r.keys_path {
        Some(p) => Some(load_keys(&Some(p.clone()))?.keys),
        None => None,
    };

    // ---- Streamed mode: ranged reads, one segment in memory at a time ----
    if let Some(dest) = &r.stream_to {
        let api = Client::api();
        let hs = if is_url(&source) {
            let api_base = r.api_base.as_deref().unwrap_or(hotaudio::HOTAUDIO_API_BASE);
            listen::load_handshake(&api, &source, api_base, r.track_id.as_deref()).await?
        } else {
            let html = std::fs::read_to_string(&source)
                .map_err(|e| Error::Http(format!("cannot read {source}: {e}")))?;
            let api_base = r.api_base.as_deref().unwrap_or(hotaudio::HOTAUDIO_API_BASE);
            let mut hs = listen::handshake_from_html(&html, api_base, r.track_id.as_deref())?
                .ok_or_else(|| {
                    Error::State("could not decrypt __ha_state from HTML file".into())
                })?;
            hs.listen_key = listen::extract_listen_key(&source);
            hs
        };

        let mut writer = std::io::BufWriter::with_capacity(1 << 20, open_output(dest)?);

        let res = download::download_streaming(
            &api,
            &hs,
            seed.as_ref(),
            |fragment| {
                writer
                    .write_all(fragment)
                    .map_err(|e| Error::Http(e.to_string()))
            },
            Some(&mut report),
        )
        .await?;
        writer.flush().map_err(|e| Error::Http(e.to_string()))?;

        let title = Some(hs.track.title.clone()).filter(|t| !t.is_empty());
        write_saved_keys(&r.save_keys, &res.hax_url, title.as_deref(), &res.keys)?;
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

    let res = if is_url(&source) {
        let api = Client::api();
        let bulk = Client::bulk();
        let clients = download::Clients {
            api: &api,
            bulk: &bulk,
        };
        let mut opts = DownloadOptions {
            initial_keys: seed,
            on_progress: Some(&mut report),
            api_base: r.api_base.as_deref(),
            track_id: r.track_id.as_deref(),
            capture_hax: r.save_hax.is_some(),
        };
        clients.download_from_page(&source, &mut opts).await?
    } else {
        let html = std::fs::read_to_string(&source)
            .map_err(|e| Error::Http(format!("cannot read {source}: {e}")))?;
        let api_base = r.api_base.as_deref().unwrap_or(hotaudio::HOTAUDIO_API_BASE);
        let mut hs = listen::handshake_from_html(&html, api_base, r.track_id.as_deref())?
            .ok_or_else(|| Error::State("could not decrypt __ha_state from HTML file".into()))?;
        hs.listen_key = listen::extract_listen_key(&source);
        let api = Client::api();
        let bulk = Client::bulk();
        let clients = download::Clients {
            api: &api,
            bulk: &bulk,
        };
        let mut opts = DownloadOptions {
            initial_keys: seed,
            on_progress: Some(&mut report),
            api_base: r.api_base.as_deref(),
            track_id: r.track_id.as_deref(),
            capture_hax: r.save_hax.is_some(),
        };
        clients.download_with_handshake(&hs, &mut opts).await?
    };

    if let Some(path) = &r.save_hax {
        if let Some(bytes) = &res.hax_bytes {
            write_file(path, bytes)?;
        }
    }
    let title = res.title.clone().unwrap_or_else(|| "hotaudio-track".into());
    write_saved_keys(&r.save_keys, &res.hax_url, Some(&title), &res.keys)?;
    let out = r.out.unwrap_or_else(|| format!("{}.m4a", sanitize(&title)));
    write_file(&out, &res.audio)?;
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
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
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

/// Reject unknown flags and value flags whose value is missing or is itself
/// another flag, so `hotaudio-download --out --list-tracks` fails instead of
/// writing a file called `--list-tracks`.
fn validate(args: &[String]) -> Result<(), String> {
    let quiet = args.iter().any(|a| a == "--quiet" || a == "-q");
    let verbose = args.iter().any(|a| a == "--verbose" || a == "-v");
    if quiet && verbose {
        return Err("--quiet and --verbose are mutually exclusive".into());
    }
    if args
        .iter()
        .any(|a| a == "--save-hax" || a.starts_with("--save-hax="))
        && args
            .iter()
            .any(|a| a == "--stream-to" || a.starts_with("--stream-to="))
    {
        return Err(
            "--save-hax needs the buffered container; it is not available with --stream-to".into(),
        );
    }
    for (i, arg) in args.iter().enumerate() {
        if !arg.starts_with('-') {
            continue;
        }
        let name = arg.split_once('=').map_or(arg.as_str(), |(n, _)| n);
        if BOOL_FLAGS.contains(&name) {
            if arg.contains('=') {
                return Err(format!("{name} does not take a value"));
            }
            continue;
        }
        if !VALUE_FLAGS.contains(&name) {
            return Err(format!("unknown flag {arg}"));
        }
        let value = arg
            .split_once('=')
            .map(|(_, v)| Some(v.to_string()))
            .unwrap_or_else(|| args.get(i + 1).cloned());
        match value {
            None => return Err(format!("{name} needs a value")),
            Some(v) if v.starts_with('-') => {
                return Err(format!("{name} needs a value, but got the flag {v}"));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// The first argument that is neither a flag nor a flag's value.
///
/// Scanning must skip over values: in `--out a.m4a <URL>`, `a.m4a` is the
/// output path, not the source.
fn positional(args: &[String]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if !arg.starts_with('-') {
            return Some(arg.clone());
        }
        let name = arg.split_once('=').map_or(arg.as_str(), |(n, _)| n);
        // A bare value flag consumes the next argument; `--flag=value` does not.
        i += if VALUE_FLAGS.contains(&name) && !arg.contains('=') {
            2
        } else {
            1
        };
    }
    None
}

/// Read a flag's value, accepting both `--flag value` and `--flag=value`.
fn flag(args: &[String], name: &str) -> Option<String> {
    for (i, arg) in args.iter().enumerate() {
        if let Some(value) = arg.strip_prefix(&format!("{name}=")) {
            return Some(value.to_string());
        }
        if arg == name {
            return args.get(i + 1).cloned();
        }
    }
    None
}

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Load saved keys from a path or inline JSON.
fn load_keys(path: &Option<String>) -> Result<download::SavedKeys, Error> {
    let Some(path) = path else {
        return Err(Error::Protocol("--keys <file | JSON> is required".into()));
    };
    let text = if path.trim_start().starts_with('{') {
        path.clone()
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| Error::Http(format!("cannot read {path}: {e}")))?
    };
    download::parse_saved_keys(&text)
}

/// Write the saved-keys envelope, if requested.
fn write_saved_keys(
    path: &Option<String>,
    hax_url: &str,
    title: Option<&str>,
    keys: &HashMap<u32, [u8; 32]>,
) -> Result<(), Error> {
    let Some(path) = path else { return Ok(()) };
    let json = download::keys_to_json(keys, hax_url, title);
    std::fs::write(path, json).map_err(|e| Error::Http(format!("cannot write {path}: {e}")))?;
    eprintln!("Keys saved to {path}");
    Ok(())
}

/// Write bytes to `path`, or to stdout when `path` is `-`.
fn write_file(path: &str, bytes: &[u8]) -> Result<(), Error> {
    let mut out = open_output(path)?;
    out.write_all(bytes)
        .map_err(|e| Error::Http(e.to_string()))?;
    out.flush().map_err(|e| Error::Http(e.to_string()))
}

/// A writer for `path`, or stdout when `path` is `-`.
///
/// `-` lets the audio be piped (`--out - | ffplay -`). Progress and the result
/// line stay on stderr in that case (see [`print_result`]) so the stream is
/// never corrupted.
fn open_output(path: &str) -> Result<Box<dyn Write>, Error> {
    if path == "-" {
        return Ok(Box::new(std::io::stdout()));
    }
    let file = std::fs::File::create(path)
        .map_err(|e| Error::Http(format!("cannot create {path}: {e}")))?;
    Ok(Box::new(file))
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

/// Strip characters that are illegal in filenames.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !matches!(c, '\\' | '/' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "hotaudio-track".to_string()
    } else {
        trimmed.to_string()
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

    #[test]
    fn rejects_unknown_flags() {
        assert!(validate(&["--nope".into(), "x".into()]).is_err());
        assert!(validate(&["-x".into()]).is_err());
    }

    #[test]
    fn accepts_known_flag_forms() {
        assert!(validate(&["--out".into(), "a.m4a".into()]).is_ok());
        assert!(validate(&["--out=a.m4a".into()]).is_ok());
        assert!(validate(&["--list-tracks".into()]).is_ok());
        assert!(validate(&["--quiet".into()]).is_ok());
        assert!(validate(&["-q".into()]).is_ok());
        assert!(validate(&["--verbose".into()]).is_ok());
        assert!(validate(&["-v".into()]).is_ok());
        assert!(validate(&["--json".into()]).is_ok());
        assert!(validate(&["--progress".into(), "json".into()]).is_ok());
        // Inline JSON is a value, not a flag.
        assert!(validate(&["--keys".into(), "{\"a\":1}".into()]).is_ok());
    }

    #[test]
    fn quiet_and_verbose_conflict() {
        let err = validate(&["--quiet".into(), "--verbose".into()]).unwrap_err();
        assert!(err.contains("mutually exclusive"), "got {err}");
        assert!(validate(&["-q".into(), "-v".into()]).is_err());
    }

    #[test]
    fn save_hax_conflicts_with_stream_to() {
        assert!(
            validate(&[
                "--save-hax".into(),
                "c.hax".into(),
                "--stream-to".into(),
                "a.m4a".into()
            ])
            .is_err()
        );
        assert!(validate(&["--save-hax=c.hax".into(), "--stream-to=a.m4a".into()]).is_err());
        assert!(validate(&["--save-hax".into(), "c.hax".into()]).is_ok());
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
        assert!(open_output("-").is_ok());
        // Any other path is opened for writing, so a bad directory errors.
        assert!(open_output("/no/such/dir/out.m4a").is_err());
    }

    #[test]
    fn rejects_flags_missing_their_value() {
        assert!(validate(&["--out".into()]).is_err());
        // The value position holds another flag: almost certainly a mistake,
        // and it would otherwise write a file with that name.
        assert!(validate(&["--out".into(), "--list-tracks".into()]).is_err());
        assert!(validate(&["--list-tracks=x".into()]).is_err());
    }

    #[test]
    fn positional_skips_flags_and_their_values() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        assert_eq!(
            positional(&a(&["https://x/u/a/b"])).as_deref(),
            Some("https://x/u/a/b")
        );
        // The value of --out must not be mistaken for the source.
        assert_eq!(
            positional(&a(&["--out", "a.m4a", "https://x/u/a/b"])).as_deref(),
            Some("https://x/u/a/b")
        );
        assert_eq!(
            positional(&a(&["--out=a.m4a", "https://x/u/a/b"])).as_deref(),
            Some("https://x/u/a/b")
        );
        // Boolean flags do not consume the next argument.
        assert_eq!(
            positional(&a(&["--list-tracks", "https://x/u/a/b"])).as_deref(),
            Some("https://x/u/a/b")
        );
        assert_eq!(positional(&a(&["--list-tracks"])), None);
    }

    #[test]
    fn flag_reads_both_spellings() {
        let args = vec!["url".to_string(), "--out".into(), "a.m4a".into()];
        assert_eq!(flag(&args, "--out").as_deref(), Some("a.m4a"));

        let args = vec!["--out=b.m4a".to_string()];
        assert_eq!(flag(&args, "--out").as_deref(), Some("b.m4a"));

        assert_eq!(flag(&args, "--missing"), None);
    }

    #[test]
    fn sanitises_titles_into_filenames() {
        assert_eq!(sanitize("a/b*c"), "abc");
        assert_eq!(sanitize("   "), "hotaudio-track");
        assert_eq!(sanitize("Track One"), "Track One");
    }

    #[test]
    fn stem_drops_the_extension() {
        assert_eq!(stem("/tmp/a/b/audio.hax"), "audio");
        assert_eq!(stem("audio"), "audio");
        assert_eq!(stem(".hidden"), ".hidden");
    }

    #[test]
    fn usage_documents_every_flag() {
        for f in VALUE_FLAGS.iter().chain(BOOL_FLAGS) {
            assert!(USAGE.contains(f), "USAGE does not mention {f}");
        }
    }
}
