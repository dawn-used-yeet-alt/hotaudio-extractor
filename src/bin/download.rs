//! `hotaudio-download` — download and decrypt a hotaudio track.
//!
//! Online mode runs the full pipeline and writes playable `.m4a`. Cached mode
//! reuses a saved-keys envelope (no page fetch, no listen calls). Offline mode
//! decrypts a local `.hax` with saved keys (no network at all).
//!
//! Run with `--help` for the full flag list.

use std::collections::HashMap;
use std::io::Write;
use std::process::ExitCode;

use hotaudio::download::{self, DownloadOptions, Phase, Progress};
use hotaudio::listen;
use hotaudio::{crypto::Error, http};

const USAGE: &str = "\
hotaudio-download — download and decrypt a hotaudio.net track to playable .m4a

Usage:
  hotaudio-download <URL|HTML> [options]      full pipeline (fetch page, listen, download)
  hotaudio-download --keys FILE [options]     cached re-download (no page fetch, no listen)
  hotaudio-download --hax FILE --keys FILE    fully offline decrypt of a local container

Options:
  --out PATH        write the decrypted audio here
                    (default: the track title, sanitised, as .m4a)
  --stream-to PATH  stream segments to PATH instead of buffering: peak memory of
                    one segment, first playable fragment after one round trip
  --save-keys PATH  write the saved-keys envelope here for later reuse
  --keys PATH       saved-keys envelope to resume from; accepts a file path or
                    inline JSON. In online mode, seeds the key map so an
                    interrupted download refetches only what is missing
  --hax PATH        decrypt this local .hax container (fully offline)
  --hax-url URL     container URL for cached mode, when the envelope has none
  --track ID        select a track by id on a multi-track page
  --api-base URL    override the listen API base (default: https://hotaudio.net)
  --list-tracks     list the tracks on the page and exit
  -h, --help        show this help

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
];

/// Flags that stand alone.
const BOOL_FLAGS: &[&str] = &["--list-tracks", "--help", "-h"];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
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
    let hax_path = flag(&args, "--hax");
    let hax_url = flag(&args, "--hax-url");
    let keys_path = flag(&args, "--keys");
    let track_id = flag(&args, "--track");
    let api_base = flag(&args, "--api-base");
    let list_tracks = args.iter().any(|a| a == "--list-tracks");

    match run(Run {
        source,
        out,
        stream_to,
        save_keys,
        hax_path,
        hax_url,
        keys_path,
        track_id,
        api_base,
        list_tracks,
    }) {
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
    hax_path: Option<String>,
    hax_url: Option<String>,
    keys_path: Option<String>,
    track_id: Option<String>,
    api_base: Option<String>,
    list_tracks: bool,
}

fn run(r: Run) -> Result<(), Error> {
    // ---- Offline: local .hax + saved keys, no network ----
    if let Some(hax_path) = &r.hax_path {
        let saved = load_keys(&r.keys_path)?;
        let keys = saved.keys;
        let title = saved.title;
        let bytes = std::fs::read(hax_path)
            .map_err(|e| Error::Http(format!("cannot read {hax_path}: {e}")))?;
        let (audio, _, _) = download::decrypt_offline(&bytes, &keys, Some(&mut progress))?;
        write_saved_keys(&r.save_keys, "", title.as_deref(), &keys)?;
        let title = title.unwrap_or_else(|| stem(hax_path));
        let out = r.out.unwrap_or_else(|| format!("{}.m4a", sanitize(&title)));
        write_file(&out, &audio)?;
        println!("Saved {}", std::path::Path::new(&out).display());
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
            let agent = http::api_agent();
            let resp = http::request(&agent, "GET", source, &[], None)?;
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
        for (id, _key, title) in tracks {
            println!("{id}\t{title}");
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
        let agent = http::bulk_agent();
        let (audio, _, _) = download::download_cached(&agent, &url, &keys, Some(&mut progress))?;
        write_saved_keys(&r.save_keys, &url, title.as_deref(), &keys)?;
        let title = title.unwrap_or_else(|| "hotaudio-track".into());
        let out = r.out.unwrap_or_else(|| format!("{}.m4a", sanitize(&title)));
        write_file(&out, &audio)?;
        println!("Saved {}", std::path::Path::new(&out).display());
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
        let api = http::api_agent();
        let hs = if is_url(&source) {
            let api_base = r.api_base.as_deref().unwrap_or(hotaudio::HOTAUDIO_API_BASE);
            listen::load_handshake(&api, &source, api_base)?
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

        let file = std::fs::File::create(dest)
            .map_err(|e| Error::Http(format!("cannot create {dest}: {e}")))?;
        let mut writer = std::io::BufWriter::with_capacity(1 << 20, file);

        let res = download::download_streaming(
            &api,
            &hs,
            &mut writer,
            seed.as_ref(),
            Some(&mut progress),
            Some(&mut || {
                eprintln!("\rfirst fragment written — file is playable from here");
            }),
        )?;
        std::io::Write::flush(&mut writer).map_err(|e| Error::Http(e.to_string()))?;

        let title = Some(hs.track.title.clone()).filter(|t| !t.is_empty());
        write_saved_keys(&r.save_keys, &res.hax_url, title.as_deref(), &res.keys)?;
        eprintln!(
            "streamed {} bytes across {} segments ({} page(s)) to {}",
            res.bytes, res.segment_count, res.pages, dest
        );
        return Ok(());
    }

    let (audio, title, hax_url, keys) = if is_url(&source) {
        let api = http::api_agent();
        let bulk = http::bulk_agent();
        // Bound the callback before handing it over: `Some(&mut progress)`
        // inline would extend the borrow only to the end of the statement.
        let on_progress = &mut progress;
        let mut opts = DownloadOptions {
            initial_keys: seed,
            on_progress: Some(on_progress),
            api_base: r.api_base.as_deref(),
            track_id: r.track_id.as_deref(),
        };
        let res = download_from_page_split(&api, &bulk, &source, &mut opts)?;
        (res.audio, res.title, res.hax_url, res.keys)
    } else {
        let html = std::fs::read_to_string(&source)
            .map_err(|e| Error::Http(format!("cannot read {source}: {e}")))?;
        let api_base = r.api_base.as_deref().unwrap_or(hotaudio::HOTAUDIO_API_BASE);
        let mut hs = listen::handshake_from_html(&html, api_base, r.track_id.as_deref())?
            .ok_or_else(|| Error::State("could not decrypt __ha_state from HTML file".into()))?;
        hs.listen_key = listen::extract_listen_key(&source);
        let api = http::api_agent();
        let bulk = http::bulk_agent();
        let on_progress = &mut progress;
        let mut opts = DownloadOptions {
            initial_keys: seed,
            on_progress: Some(on_progress),
            api_base: r.api_base.as_deref(),
            track_id: r.track_id.as_deref(),
        };
        let res = download_with_agents(&api, &bulk, &hs, &mut opts)?;
        (res.audio, res.title, res.hax_url, res.keys)
    };

    let title = title.unwrap_or_else(|| "hotaudio-track".into());
    write_saved_keys(&r.save_keys, &hax_url, Some(&title), &keys)?;
    let out = r.out.unwrap_or_else(|| format!("{}.m4a", sanitize(&title)));
    write_file(&out, &audio)?;
    println!("Saved {}", std::path::Path::new(&out).display());
    Ok(())
}

/// Online download that keeps the small API calls on the timeout-bound agent and
/// the bulk `.hax` fetch on the untimed one.
fn download_from_page_split(
    api: &ureq::Agent,
    bulk: &ureq::Agent,
    page_url: &str,
    opts: &mut DownloadOptions<'_>,
) -> Result<download::Downloaded, Error> {
    if let Some(cb) = opts.on_progress.as_mut() {
        cb(Progress {
            phase: Phase::Resolving,
            loaded: 0,
            total: 0,
        });
    }
    let api_base = opts.api_base.unwrap_or(hotaudio::HOTAUDIO_API_BASE);
    let hs = listen::load_handshake(api, page_url, api_base)?;
    download_with_agents(api, bulk, &hs, opts)
}

/// Full pipeline with separate API and bulk agents.
fn download_with_agents(
    api: &ureq::Agent,
    bulk: &ureq::Agent,
    hs: &listen::Handshake,
    opts: &mut DownloadOptions<'_>,
) -> Result<download::Downloaded, Error> {
    let initial = listen::listen(api, hs, -1)?;
    if initial.url.is_empty() {
        return Err(Error::Listen("listen API returned no .hax url".into()));
    }
    let resp = http::request(bulk, "GET", &initial.url, &[], None)?;
    if !resp.ok() {
        return Err(Error::Http(format!(".hax fetch returned {}", resp.status)));
    }
    if let Some(cb) = opts.on_progress.as_mut() {
        let n = resp.body.len() as u64;
        cb(Progress {
            phase: Phase::Fetching,
            loaded: n,
            total: n,
        });
    }
    download::decrypt_container(api, hs, &resp.body, &initial, opts)
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

fn write_file(path: &str, bytes: &[u8]) -> Result<(), Error> {
    std::fs::write(path, bytes).map_err(|e| Error::Http(format!("cannot write {path}: {e}")))
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
        // Inline JSON is a value, not a flag.
        assert!(validate(&["--keys".into(), "{\"a\":1}".into()]).is_ok());
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
