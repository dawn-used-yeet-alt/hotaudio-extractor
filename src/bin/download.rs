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
use std::io::{Read, Write};
use std::process::ExitCode;

use hotaudio::crypto::{Error, KeyMap};
use hotaudio::download::{self, DownloadOptions, Phase, Progress};
use hotaudio::http::Client;
use hotaudio::listen;

/// Program name, printed by `--version`.
const BIN_NAME: &str = "hotaudio-download";

const USAGE: &str = "\
hotaudio-download — download and decrypt a hotaudio.net track to playable .m4a

Usage:
  hotaudio-download <URL|HTML>... [options]   full pipeline (fetch page, listen, download)
  hotaudio-download --keys FILE [options]     cached re-download (no page fetch, no listen)
  hotaudio-download --hax FILE --keys FILE    fully offline decrypt of a local container

Several sources may be given; they are processed in order and the run exits
non-zero if any fails. Flags that name one path (--out, --stream-to, --save-keys,
--save-hax, --keys-only, --list-tracks) require a single source and are not
available with --all-tracks.

Options:
  --out PATH        write the decrypted audio here ('-' writes to stdout)
                    (default: the track title, sanitised, as .m4a)
  --filename-template PATTERN
                    name title-derived outputs from PATTERN: {title}, {index}
                    (1-based) and {ext} are substituted, and '/' separates
                    directories. An explicit --out or --stream-to wins
  --input FILE      read one source per line from FILE ('-' is stdin); blank
                    lines are skipped
  --stream-to PATH  stream segments to PATH instead of buffering: peak memory of
                    one segment, first playable fragment after one round trip
                    ('-' streams to stdout)
  --save-keys PATH  write the saved-keys envelope here for later reuse
  --keys-only       save the first key branch (needs --save-keys) and exit, without
                    fetching the container or decrypting
  --save-hax PATH   write the raw .hax container here for a later offline decrypt
                    (buffered modes only; not with --stream-to)
  --no-clobber      fail instead of replacing an existing output file (audio,
                    keys or .hax); parent directories are still created
  --force           replace existing output files (the default)
  --keys PATH       saved-keys envelope to resume from; accepts a file path or
                    inline JSON. In online mode, seeds the key map so an
                    interrupted download refetches only what is missing
  --hax PATH        decrypt this local .hax container (fully offline)
  --hax-url URL     container URL for cached mode, when the envelope has none
  --track ID        select a track by id on a multi-track page
  --all-tracks      download every track on the page, sequentially
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
    "--input",
    "--filename-template",
];

/// Flags that name a single output or source, so they cannot be combined with
/// more than one positional/`--input` source.
const SINGLE_SOURCE_FLAGS: &[&str] = &[
    "--out",
    "--stream-to",
    "--save-keys",
    "--save-hax",
    "--keys",
    "--hax",
    "--keys-only",
    "--list-tracks",
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
    "--keys-only",
    "--all-tracks",
    "--no-clobber",
    "--force",
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

    let out = flag(&args, "--out");
    let filename_template = flag(&args, "--filename-template");
    let stream_to = flag(&args, "--stream-to");
    let save_keys = flag(&args, "--save-keys");
    let save_hax = flag(&args, "--save-hax");
    let hax_path = flag(&args, "--hax");
    let hax_url = flag(&args, "--hax-url");
    let keys_path = flag(&args, "--keys");
    let track_id = flag(&args, "--track");
    let api_base = flag(&args, "--api-base");
    let list_tracks = args.iter().any(|a| a == "--list-tracks");
    let keys_only = args.iter().any(|a| a == "--keys-only");
    let all_tracks = args.iter().any(|a| a == "--all-tracks");
    let clobber = if args.iter().any(|a| a == "--no-clobber") {
        Clobber::Refuse
    } else {
        Clobber::Replace
    };
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

    let sources = match collect_sources(&args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}\n");
            return ExitCode::FAILURE;
        }
    };
    if all_tracks && sources.len() != 1 {
        eprintln!("error: --all-tracks needs exactly one page URL or HTML file\n");
        return ExitCode::FAILURE;
    }
    if let Some(name) = multi_output_conflict(&sources, all_tracks, &args) {
        eprintln!(
            "error: {name} names one path; it cannot be used with several sources or --all-tracks\n"
        );
        return ExitCode::FAILURE;
    }

    match hotaudio::block_on(execute(Run {
        sources,
        out,
        filename_template,
        stream_to,
        save_keys,
        save_hax,
        hax_path,
        hax_url,
        keys_path,
        track_id,
        api_base,
        list_tracks,
        keys_only,
        all_tracks,
        clobber,
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
    sources: Vec<String>,
    out: Option<String>,
    filename_template: Option<String>,
    stream_to: Option<String>,
    save_keys: Option<String>,
    save_hax: Option<String>,
    hax_path: Option<String>,
    hax_url: Option<String>,
    keys_path: Option<String>,
    track_id: Option<String>,
    api_base: Option<String>,
    list_tracks: bool,
    keys_only: bool,
    all_tracks: bool,
    clobber: Clobber,
    quiet: bool,
    verbose: bool,
    json: bool,
    progress_json: bool,
}

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
    let template = r.filename_template.clone();
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
        let saved = load_keys(&r.keys_path)?;
        let keys = saved.keys;
        let title = saved.title;
        let bytes = std::fs::read(hax_path)
            .map_err(|e| Error::Http(format!("cannot read {hax_path}: {e}")))?;
        let hax = hotaudio::hax::Hax0::parse(&bytes)?;
        if let Some(path) = &r.save_hax {
            write_file(path, &bytes, clobber)?;
        }
        let (audio, segments, _) = download::decrypt_offline(&bytes, &keys, Some(&mut report))?;
        if r.verbose {
            eprintln!(
                "verbose: {segments} segments in {:.1}s",
                secs_since(started_ms)
            );
        }
        write_saved_keys(&r.save_keys, "", title.as_deref(), &keys, clobber)?;
        let title = title.unwrap_or_else(|| stem(hax_path));
        let out = r
            .out
            .unwrap_or_else(|| derived_name(template.as_deref(), &title, 1));
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
        let client = Client::api();
        let bundle = download::fetch_keys_only(
            &client,
            source,
            r.api_base.as_deref(),
            r.track_id.as_deref(),
        )
        .await?;
        write_saved_keys(
            &r.save_keys,
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
        write_saved_keys(&r.save_keys, &url, title.as_deref(), &keys, clobber)?;
        let title = title.unwrap_or_else(|| "hotaudio-track".into());
        let out = r
            .out
            .unwrap_or_else(|| derived_name(template.as_deref(), &title, 1));
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

        let mut writer = std::io::BufWriter::with_capacity(1 << 20, open_output(dest, clobber)?);

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
        write_saved_keys(
            &r.save_keys,
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
        &source,
        seed,
        &mut report,
        r.api_base.as_deref(),
        r.track_id.as_deref(),
        r.save_hax.is_some(),
    )
    .await?;

    if let Some(path) = &r.save_hax {
        if let Some(bytes) = &res.hax_bytes {
            write_file(path, bytes, clobber)?;
        }
    }
    let title = res.title.clone().unwrap_or_else(|| "hotaudio-track".into());
    write_saved_keys(&r.save_keys, &res.hax_url, Some(&title), &res.keys, clobber)?;
    let out = r
        .out
        .unwrap_or_else(|| derived_name(template.as_deref(), &title, 1));
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
/// Shared by single-source and batch modes. `initial_keys` seeds a resume; the
/// HTML branch recovers the handshake from a saved page.
async fn fetch_online(
    source: &str,
    initial_keys: Option<KeyMap>,
    report: download::ProgressFn<'_>,
    api_base: Option<&str>,
    track_id: Option<&str>,
    capture_hax: bool,
) -> Result<download::Downloaded, Error> {
    let api = Client::api();
    let bulk = Client::bulk();
    let clients = download::Clients {
        api: &api,
        bulk: &bulk,
    };
    let mut opts = DownloadOptions {
        initial_keys,
        on_progress: Some(report),
        api_base,
        track_id,
        capture_hax,
    };
    if is_url(source) {
        clients.download_from_page(source, &mut opts).await
    } else {
        let html = std::fs::read_to_string(source)
            .map_err(|e| Error::Http(format!("cannot read {source}: {e}")))?;
        let base = api_base.unwrap_or(hotaudio::HOTAUDIO_API_BASE);
        let mut hs = listen::handshake_from_html(&html, base, track_id)?
            .ok_or_else(|| Error::State("could not decrypt __ha_state from HTML file".into()))?;
        hs.listen_key = listen::extract_listen_key(source);
        clients.download_with_handshake(&hs, &mut opts).await
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
        if let Err(e) = batch_one(r, source, &mut *report, &mut written, None, i + 1).await {
            eprintln!("error: {source}: {e}");
            failures += 1;
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
        let client = Client::api();
        let resp = client.request("GET", source, &[], None).await?;
        if !resp.ok() {
            return Err(Error::Http(format!("page returned {}", resp.status)));
        }
        resp.text()
    } else {
        std::fs::read_to_string(source)
            .map_err(|e| Error::Http(format!("cannot read {source}: {e}")))?
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
    let res = fetch_online(source, None, report, r.api_base.as_deref(), track_id, false).await?;
    let title = res.title.clone().unwrap_or_else(|| "hotaudio-track".into());
    let out = derived_name(r.filename_template.as_deref(), &title, index);
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

/// Whether `name` appears as a flag, in either `--name` or `--name=value` form.
fn has_flag(args: &[String], name: &str) -> bool {
    args.iter()
        .any(|a| a.split_once('=').map_or(a.as_str(), |(n, _)| n) == name)
}

/// The first single-path flag present, so a multi-source run can reject it.
fn single_source_flag(args: &[String]) -> Option<&'static str> {
    SINGLE_SOURCE_FLAGS
        .iter()
        .copied()
        .find(|f| has_flag(args, f))
}

/// The single-path flag that rules out a multi-output run, if any.
///
/// A run is multi-output when it has several sources or is `--all-tracks`.
fn multi_output_conflict(
    sources: &[String],
    all_tracks: bool,
    args: &[String],
) -> Option<&'static str> {
    if sources.len() < 2 && !all_tracks {
        return None;
    }
    single_source_flag(args)
}

/// Reject unknown flags and value flags whose value is missing or is itself
/// another flag, so `hotaudio-download --out --list-tracks` fails instead of
/// writing a file called `--list-tracks`.
fn validate(args: &[String]) -> Result<(), String> {
    let quiet = has_flag(args, "--quiet") || has_flag(args, "-q");
    let verbose = has_flag(args, "--verbose") || has_flag(args, "-v");
    if quiet && verbose {
        return Err("--quiet and --verbose are mutually exclusive".into());
    }
    if has_flag(args, "--no-clobber") && has_flag(args, "--force") {
        return Err("--no-clobber and --force are mutually exclusive".into());
    }
    if has_flag(args, "--save-hax") && has_flag(args, "--stream-to") {
        return Err(
            "--save-hax needs the buffered container; it is not available with --stream-to".into(),
        );
    }
    if has_flag(args, "--keys-only") {
        if !has_flag(args, "--save-keys") {
            return Err("--keys-only needs --save-keys".into());
        }
        if has_flag(args, "--hax") || has_flag(args, "--keys") || has_flag(args, "--stream-to") {
            return Err(
                "--keys-only only fetches keys; it conflicts with --hax, --keys and --stream-to"
                    .into(),
            );
        }
    }
    if has_flag(args, "--all-tracks") && has_flag(args, "--track") {
        return Err("--all-tracks downloads every track; drop --track".into());
    }
    let template = flag(args, "--filename-template").unwrap_or_default();
    if let Some(message) = template_error(&template) {
        return Err(message);
    }
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if !arg.starts_with('-') {
            i += 1;
            continue;
        }
        let name = arg.split_once('=').map_or(arg.as_str(), |(n, _)| n);
        if BOOL_FLAGS.contains(&name) {
            if arg.contains('=') {
                return Err(format!("{name} does not take a value"));
            }
            i += 1;
            continue;
        }
        if !VALUE_FLAGS.contains(&name) {
            return Err(format!("unknown flag {arg}"));
        }
        // A bare value flag consumes the next argument, so it must be skipped:
        // otherwise a `-` value would be scanned again and read as a flag.
        let (value, step) = match arg.split_once('=') {
            Some((_, v)) => (Some(v.to_string()), 1),
            None => (args.get(i + 1).cloned(), 2),
        };
        match value {
            None => return Err(format!("{name} needs a value")),
            // `-` is a real value (stdout/stdin), so only a longer dash-word is
            // treated as a flag that was passed where a value belongs.
            Some(v) if v.starts_with('-') && v != "-" => {
                return Err(format!("{name} needs a value, but got the flag {v}"));
            }
            Some(_) => {}
        }
        i += step;
    }
    Ok(())
}

/// Every argument that is neither a flag nor a flag's value, in order.
///
/// Scanning must skip over values: in `--out a.m4a <URL>`, `a.m4a` is the
/// output path, not the source.
fn positionals(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if !arg.starts_with('-') {
            out.push(arg.clone());
            i += 1;
            continue;
        }
        let name = arg.split_once('=').map_or(arg.as_str(), |(n, _)| n);
        // A bare value flag consumes the next argument; `--flag=value` does not.
        i += if VALUE_FLAGS.contains(&name) && !arg.contains('=') {
            2
        } else {
            1
        };
    }
    out
}

/// Positional sources plus one per line of `--input` (`-` means stdin).
///
/// Blank lines are skipped. Positionals come first, then the file, in the order
/// given.
fn collect_sources(args: &[String]) -> Result<Vec<String>, String> {
    let mut sources = positionals(args);
    let Some(path) = flag(args, "--input") else {
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
        std::fs::read_to_string(&path).map_err(|e| format!("cannot read {path}: {e}"))?
    };
    for line in text.lines() {
        let line = line.trim();
        if !line.is_empty() {
            sources.push(line.to_string());
        }
    }
    Ok(sources)
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
    clobber: Clobber,
) -> Result<(), Error> {
    let Some(path) = path else { return Ok(()) };
    check_clobber(path, clobber)?;
    ensure_parent(path)?;
    let json = download::keys_to_json(keys, hax_url, title);
    std::fs::write(path, json).map_err(|e| Error::Http(format!("cannot write {path}: {e}")))?;
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
            .map_err(|e| Error::Http(format!("cannot create {}: {e}", parent.display())))?;
    }
    Ok(())
}

/// Write bytes to `path`, or to stdout when `path` is `-`.
fn write_file(path: &str, bytes: &[u8], clobber: Clobber) -> Result<(), Error> {
    let mut out = open_output(path, clobber)?;
    out.write_all(bytes)
        .map_err(|e| Error::Http(e.to_string()))?;
    out.flush().map_err(|e| Error::Http(e.to_string()))
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
    if !parts[last].ends_with(".m4a") {
        parts[last].push_str(".m4a");
    }
    parts.join("/")
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
        assert!(validate(&["--all-tracks".into()]).is_ok());
        assert!(validate(&["--no-clobber".into()]).is_ok());
        assert!(validate(&["--force".into()]).is_ok());
        assert!(validate(&["--filename-template".into(), "{index}-{title}".into()]).is_ok());
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
    fn no_clobber_and_force_conflict() {
        let err = validate(&["--no-clobber".into(), "--force".into()]).unwrap_err();
        assert!(err.contains("mutually exclusive"), "got {err}");
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
    fn keys_only_requires_save_keys() {
        assert!(validate(&["--keys-only".into()]).is_err());
        assert!(validate(&["--keys-only".into(), "--save-keys".into(), "k.json".into()]).is_ok());
        assert!(validate(&["--keys-only".into(), "--save-keys=k.json".into()]).is_ok());
    }

    #[test]
    fn keys_only_conflicts_with_other_modes() {
        let base = [
            "--keys-only".to_string(),
            "--save-keys".to_string(),
            "k.json".to_string(),
        ];
        for extra in [
            ["--hax", "in.hax"],
            ["--stream-to", "a.m4a"],
            ["--keys", "k2.json"],
        ] {
            let mut args = base.to_vec();
            args.extend(extra.iter().map(|s| s.to_string()));
            assert!(validate(&args).is_err(), "{extra:?}");
        }
    }

    #[test]
    fn all_tracks_conflicts_with_track() {
        assert!(validate(&["--all-tracks".into(), "--track".into(), "t1".into()]).is_err());
        assert!(validate(&["--all-tracks".into(), "--track=t1".into()]).is_err());
        assert!(validate(&["--all-tracks".into(), "--json".into()]).is_ok());
    }

    #[test]
    fn filename_template_rejects_unknown_placeholders() {
        let err = validate(&["--filename-template".into(), "{nope}".into()]).unwrap_err();
        assert!(err.contains("nope"), "got {err}");
        assert!(validate(&["--filename-template".into(), "{title}".into()]).is_ok());
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
    fn rejects_flags_missing_their_value() {
        assert!(validate(&["--out".into()]).is_err());
        // The value position holds another flag: almost certainly a mistake,
        // and it would otherwise write a file with that name.
        assert!(validate(&["--out".into(), "--list-tracks".into()]).is_err());
        assert!(validate(&["--list-tracks=x".into()]).is_err());
    }

    #[test]
    fn dash_is_a_valid_value() {
        // `-` means stdout/stdin, not a flag.
        assert!(validate(&["--out".into(), "-".into()]).is_ok());
        assert!(validate(&["--input".into(), "-".into()]).is_ok());
        // A real flag in the value position is still rejected.
        assert!(validate(&["--out".into(), "--list-tracks".into()]).is_err());
    }

    #[test]
    fn multi_output_rejects_single_path_flags() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let one = a(&["u1"]);
        let two = a(&["u1", "u2"]);
        // One source, no --all-tracks: nothing to reject.
        assert_eq!(
            multi_output_conflict(&one, false, &a(&["--out", "a.m4a"])),
            None
        );
        // Two sources: the offending single-path flag is named.
        assert_eq!(
            multi_output_conflict(&two, false, &a(&["--out", "a.m4a"])),
            Some("--out")
        );
        assert_eq!(
            multi_output_conflict(&two, false, &a(&["--stream-to=a.m4a"])),
            Some("--stream-to")
        );
        // --all-tracks writes one file per track, so it needs its own paths too.
        assert_eq!(
            multi_output_conflict(&one, true, &a(&["--out", "a.m4a"])),
            Some("--out")
        );
        assert_eq!(
            multi_output_conflict(&one, true, &a(&["--save-keys", "k.json"])),
            Some("--save-keys")
        );
        // Flags that apply to the whole run are allowed.
        assert_eq!(
            multi_output_conflict(&two, false, &a(&["--json", "--track", "t"])),
            None
        );
        assert_eq!(multi_output_conflict(&one, true, &a(&["--json"])), None);
    }

    #[test]
    fn positionals_skip_flags_and_their_values() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let url = || "https://x/u/a/b".to_string();

        assert_eq!(positionals(&a(&["https://x/u/a/b"])), vec![url()]);
        // The value of --out must not be mistaken for the source.
        assert_eq!(
            positionals(&a(&["--out", "a.m4a", "https://x/u/a/b"])),
            vec![url()]
        );
        assert_eq!(
            positionals(&a(&["--out=a.m4a", "https://x/u/a/b"])),
            vec![url()]
        );
        // Boolean flags do not consume the next argument.
        assert_eq!(
            positionals(&a(&["--list-tracks", "https://x/u/a/b"])),
            vec![url()]
        );
        assert_eq!(
            positionals(&a(&["--all-tracks", "https://x/u/a/b"])),
            vec![url()]
        );
        // The template value is skipped like any other flag value.
        assert_eq!(
            positionals(&a(&["--filename-template", "{title}", "https://x/u/a/b"])),
            vec![url()]
        );
        assert!(positionals(&a(&["--list-tracks"])).is_empty());
        // Several sources are collected in order.
        assert_eq!(
            positionals(&a(&["https://x/u/a/b", "https://x/u/c/d"])),
            vec![url(), "https://x/u/c/d".to_string()]
        );
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
    fn usage_documents_every_flag() {
        for f in VALUE_FLAGS.iter().chain(BOOL_FLAGS) {
            assert!(USAGE.contains(f), "USAGE does not mention {f}");
        }
    }
}
