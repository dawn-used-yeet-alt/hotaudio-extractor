//! The acceptance checks.
//!
//! Ordered so the live traffic happens **once**. The online CLI run writes the
//! decrypted audio, the saved-key envelope and the raw container; every later
//! check — offline, cached, streaming, envelope, MP4 — runs against those
//! artifacts and the already-fetched page HTML rather than refetching anything.
//!
//! The one avoidable request is avoided everywhere it can be: the page is
//! fetched once and fed back to the CLI as a file for `--list-tracks` and
//! `--stream-to`, both of which accept a saved page.

use std::path::{Path, PathBuf};
use std::process::Command;

use hotaudio::crypto::KeyMap;
use hotaudio::hax::{Hax0, KeyTree};
use hotaudio::http::Client;
use hotaudio::{Error, HOTAUDIO_UA, download, listen, mp4, signer};

use crate::report::{Check, Requests, comp};

/// Everything a check needs about the run.
pub struct Ctx {
    pub url: String,
    pub api_base: String,
    pub cli: PathBuf,
    pub workdir: PathBuf,
    /// A browser clearance, when the environment supplies one. The CLI
    /// subprocess reads the same variables, so library and binary agree.
    pub cookie: Option<String>,
    /// The exact User-Agent that earned `cookie`.
    pub user_agent: Option<String>,
}

/// Accumulates checks and the request budget.
#[derive(Default)]
pub struct Runner {
    pub checks: Vec<Check>,
    pub requests: Requests,
}

impl Runner {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ok(
        &mut self,
        component: &str,
        name: &str,
        detail: impl Into<String>,
        evidence: Vec<(String, String)>,
    ) {
        self.checks.push(Check {
            component: component.to_string(),
            name: name.to_string(),
            ok: true,
            detail: detail.into(),
            evidence,
        });
    }

    pub fn fail(
        &mut self,
        component: &str,
        name: &str,
        detail: impl Into<String>,
        evidence: Vec<(String, String)>,
    ) {
        self.checks.push(Check {
            component: component.to_string(),
            name: name.to_string(),
            ok: false,
            detail: detail.into(),
            evidence,
        });
    }

    #[must_use]
    pub fn passed(&self) -> usize {
        self.checks.iter().filter(|c| c.ok).count()
    }

    #[must_use]
    pub fn failed(&self) -> usize {
        self.checks.iter().filter(|c| !c.ok).count()
    }

    /// True when a failure names the environment, not the crate. Used to skip
    /// the rest of the network phases and to keep the run green when the site
    /// is challenging everyone.
    #[must_use]
    pub fn environment_failed(&self) -> bool {
        self.checks
            .iter()
            .any(|c| !c.ok && c.component == comp::ENVIRONMENT)
    }
}

/// The live protocol facts the later checks build on.
pub struct Live {
    pub html: String,
    pub hax_url: String,
}

/// What the online CLI run produced.
pub struct Artifacts {
    /// The decrypted bytes every other path must reproduce.
    pub audio_bytes: Vec<u8>,
    pub keys: PathBuf,
    pub hax: PathBuf,
}

// ------------------------------------------------------------------- helpers

fn ev(pairs: Vec<(&str, String)>) -> Vec<(String, String)> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// First 200 characters of a response body, for an error detail.
fn snippet(body: &[u8]) -> String {
    String::from_utf8_lossy(body).chars().take(200).collect()
}

fn file_name(url: &str) -> String {
    url.rsplit('/').next().unwrap_or(url).to_string()
}

struct CliOut {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run_cli(ctx: &Ctx, args: &[String]) -> Result<CliOut, String> {
    if !ctx.cli.exists() {
        return Err(format!(
            "CLI not found at {} — build it (cargo build --release --bin hotaudio-download) \
             or pass --cli",
            ctx.cli.display()
        ));
    }
    let out = Command::new(&ctx.cli)
        .args(args)
        .current_dir(&ctx.workdir)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", ctx.cli.display()))?;
    Ok(CliOut {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// The JSON result the CLI prints on stdout.
///
/// The object is not one line: its `keysJson` field embeds the saved-keys
/// envelope verbatim, and that envelope is pretty-printed, so the whole document
/// spans many lines. Parse it as a document, falling back to the first `{` in
/// case anything precedes it.
fn json_value(stdout: &str) -> Option<serde_json::Value> {
    let text = stdout.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        return Some(value);
    }
    let start = text.find('{')?;
    serde_json::from_str::<serde_json::Value>(&text[start..]).ok()
}

fn u64_field(v: &serde_json::Value, key: &str) -> Option<u64> {
    v.get(key).and_then(serde_json::Value::as_u64)
}

fn str_field<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(serde_json::Value::as_str)
}

fn path(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The first line worth putting in an issue title.
///
/// The CLI's human progress reporter uses carriage returns, so `str::lines`
/// alone treats a whole progress sequence as one line. Split both terminators,
/// then skip the progress fragments: an issue title must be the error, not
/// `fetching: 99.8%decrypting: ...`.
pub(crate) fn first_line(s: &str) -> String {
    s.split(['\n', '\r'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .find(|line| {
            !line.starts_with("fetching:")
                && !line.starts_with("decrypting:")
                && !line.starts_with("resolving:")
                && !line.starts_with("{\"phase\"")
        })
        .unwrap_or("")
        .to_string()
}

/// True when a CLI failure was a Cloudflare challenge rather than a crate bug.
fn challenge_in(stderr: &str) -> bool {
    stderr.contains("site challenge:")
        || stderr.contains("Cloudflare challenge")
        || stderr.contains("Just a moment...")
}

fn tail(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().skip(s.chars().count() - n).collect()
    }
}

// --------------------------------------------------------------- live (net)

/// Fetch the page once and verify the protocol contracts that need the live
/// site: page, page state, tracks, handshake, signer and the first listen.
pub async fn run_live(r: &mut Runner, ctx: &Ctx) -> Option<Live> {
    let client = match (&ctx.cookie, &ctx.user_agent) {
        (Some(cookie), Some(ua)) => Client::api()
            .with_cookie(cookie.as_str())
            .with_user_agent(ua.as_str()),
        (Some(cookie), None) => Client::api().with_cookie(cookie.as_str()),
        (None, Some(ua)) => Client::api().with_user_agent(ua.as_str()),
        (None, None) => Client::api(),
    };
    let resp = match client
        .request("GET", &ctx.url, &[("User-Agent", HOTAUDIO_UA)], None)
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            // `request` returns `Ok` for every HTTP status, so an `Err` here is
            // a transport failure (DNS, connect, TLS, timeout), not a page
            // verdict. That is the network's condition, not the crate's.
            let component = match &e {
                Error::Http(_) | Error::Challenge(_) => comp::ENVIRONMENT,
                _ => comp::PAGE,
            };
            r.fail(component, "fetch track page", e.to_string(), Vec::new());
            return None;
        }
    };
    r.requests.page += 1;
    if let Some(detail) = resp.challenge() {
        r.fail(
            comp::ENVIRONMENT,
            "site reachable",
            format!("{detail}; the site is challenging this network"),
            ev(vec![
                ("status", resp.status.to_string()),
                (
                    "cf-mitigated",
                    resp.header("cf-mitigated").unwrap_or("-").to_string(),
                ),
                ("cf-ray", resp.header("cf-ray").unwrap_or("-").to_string()),
                ("body", snippet(&resp.body)),
                (
                    "cookie",
                    if ctx.cookie.is_some() {
                        "configured".to_string()
                    } else {
                        "none".to_string()
                    },
                ),
            ]),
        );
        return None;
    }
    if !resp.ok() {
        r.fail(
            comp::PAGE,
            "fetch track page",
            format!("HTTP {}", resp.status),
            ev(vec![
                ("status", resp.status.to_string()),
                ("body", snippet(&resp.body)),
            ]),
        );
        return None;
    }
    let html = resp.text();
    r.ok(
        comp::PAGE,
        "fetch track page",
        format!("HTTP {} · {} bytes", resp.status, resp.body.len()),
        ev(vec![
            ("status", resp.status.to_string()),
            ("bytes", resp.body.len().to_string()),
            ("server", resp.header("server").unwrap_or("-").to_string()),
            (
                "content-type",
                resp.header("content-type").unwrap_or("-").to_string(),
            ),
        ]),
    );

    // --- page state ---
    let Some(state_b64) = listen::extract_ha_state(&html) else {
        r.fail(
            comp::PAGE_STATE,
            "extract __ha_state",
            "marker `var __ha_state = \"` not found in the page HTML (page layout changed?)",
            ev(vec![("html_bytes", html.len().to_string())]),
        );
        return None;
    };
    r.ok(
        comp::PAGE_STATE,
        "extract __ha_state",
        format!("{} base64 chars", state_b64.len()),
        Vec::new(),
    );
    let state = match listen::decrypt_state(&state_b64) {
        Ok(state) => state,
        Err(e) => {
            r.fail(
                comp::PAGE_STATE,
                "decrypt __ha_state",
                e.to_string(),
                Vec::new(),
            );
            return None;
        }
    };
    r.ok(
        comp::PAGE_STATE,
        "decrypt __ha_state",
        format!("pid={} · {} track(s)", state.pid, state.tracks.len()),
        ev(vec![
            ("pid", state.pid.clone()),
            ("tick_len", state.tick.len().to_string()),
            ("tracks", state.tracks.len().to_string()),
            ("server_key", state.key.clone()),
            ("order", format!("{:?}", state.order)),
        ]),
    );

    // --- tracks ---
    match listen::list_tracks(&html) {
        Some(tracks) if !tracks.is_empty() => {
            let names: Vec<String> = tracks
                .iter()
                .map(|(id, _key, title)| format!("{id}:{title}"))
                .collect();
            r.ok(
                comp::TRACKS,
                "list tracks",
                format!("{} track(s)", tracks.len()),
                ev(vec![("tracks", names.join(", "))]),
            );
        }
        _ => r.fail(
            comp::TRACKS,
            "list tracks",
            "no tracks recovered from the page state",
            Vec::new(),
        ),
    }

    // --- handshake ---
    let hs = match listen::handshake_from_html(&html, &ctx.api_base, None) {
        Ok(Some(hs)) => {
            r.ok(
                comp::HANDSHAKE,
                "build handshake",
                format!("tid={} · title={:?}", hs.tid, hs.track.title),
                ev(vec![
                    ("tid", hs.tid.clone()),
                    ("track_key", hs.track.key.clone()),
                    ("title", hs.track.title.clone()),
                    ("client_pub", hs.client_pub_hex.clone()),
                    ("secret_len", hs.secret.len().to_string()),
                ]),
            );
            hs
        }
        Ok(None) => {
            r.fail(
                comp::HANDSHAKE,
                "build handshake",
                "no decryptable state or no track selected",
                Vec::new(),
            );
            return None;
        }
        Err(e) => {
            r.fail(
                comp::HANDSHAKE,
                "build handshake",
                e.to_string(),
                Vec::new(),
            );
            return None;
        }
    };

    run_signer(r);

    // --- first listen ---
    let initial = match listen::listen(&client, &hs, -1, 0).await {
        Ok(resp) => {
            r.requests.listen += 1;
            resp
        }
        Err(e) => {
            let component = if matches!(&e, Error::Challenge(_)) {
                comp::ENVIRONMENT
            } else {
                comp::LISTEN
            };
            r.fail(
                component,
                "initial listen",
                e.to_string(),
                ev(vec![
                    ("pinned_version", signer::PINNED_NOZZLE_VERSION.to_string()),
                    ("client_pub", hs.client_pub_hex.clone()),
                ]),
            );
            return None;
        }
    };
    if initial.url.is_empty() {
        r.fail(
            comp::LISTEN,
            "listen response",
            "response carried no `.hax` URL",
            ev(vec![("keys", initial.keys.len().to_string())]),
        );
        return None;
    }
    r.ok(
        comp::LISTEN,
        "listen response",
        format!(
            "{} branch key(s) · {}",
            initial.keys.len(),
            file_name(&initial.url)
        ),
        ev(vec![
            ("keys", initial.keys.len().to_string()),
            ("hax_url", initial.url.clone()),
        ]),
    );

    // --- issued vs derived .hax URL (tripwire) ---
    let derived = listen::hax_url_for_key(&hs.track.key);
    if derived == initial.url {
        r.ok(
            comp::HAX_URL,
            "issued .hax URL matches the derived one",
            derived,
            Vec::new(),
        );
    } else {
        r.fail(
            comp::HAX_URL,
            "issued .hax URL matches the derived one",
            format!(
                "issued `{}` != derived `{}` — the CDN layout may have moved",
                initial.url, derived
            ),
            ev(vec![("issued", initial.url.clone()), ("derived", derived)]),
        );
    }

    Some(Live {
        html,
        hax_url: initial.url,
    })
}

/// The signature VM's pinned contract and determinism, plus the crypto
/// primitives it rests on. The *proof* is that the listen call is accepted;
/// this catches a re-capture slip and a broken primitive early.
fn run_signer(r: &mut Runner) {
    let mut evidence = ev(vec![
        ("pinned_version", signer::PINNED_NOZZLE_VERSION.to_string()),
        ("env_hashes", signer::ENV_HASHES.len().to_string()),
        ("program_len", signer::PROGRAM_LEN.to_string()),
        ("program", signer::PROGRAM.len().to_string()),
    ]);
    let a = signer::sign_payload_at("hotaudio-live", 1_700_000_000.0);
    let b = signer::sign_payload_at("hotaudio-live", 1_700_000_000.0);
    match (a, b) {
        (Ok(a), Ok(b)) => {
            evidence.push(("signature_len".to_string(), a.len().to_string()));
            if a != b {
                r.fail(
                    comp::SIGNER,
                    "deterministic signature",
                    "sign_payload_at returned two different signatures for one payload and time",
                    evidence,
                );
            } else if signer::PINNED_NOZZLE_VERSION.is_empty()
                || signer::ENV_HASHES.is_empty()
                || signer::PROGRAM.len() != signer::PROGRAM_LEN
            {
                r.fail(
                    comp::SIGNER,
                    "pinned bytecode contract",
                    "pinned version, environment table and program length disagree",
                    evidence,
                );
            } else {
                r.ok(
                    comp::SIGNER,
                    "signature + pinned bytecode",
                    format!(
                        "{} char signature · build {}",
                        a.len(),
                        signer::PINNED_NOZZLE_VERSION
                    ),
                    evidence,
                );
            }
        }
        (Err(e), _) | (_, Err(e)) => r.fail(
            comp::SIGNER,
            "deterministic signature",
            format!("the VM failed: {e:?}"),
            evidence,
        ),
    }

    run_crypto(r);
}

fn run_crypto(r: &mut Runner) {
    use hotaudio::crypto;
    let mut problems: Vec<String> = Vec::new();
    let empty =
        crypto::hex_decode("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    let sha_ok = match empty {
        Ok(digest) => crypto::sha256(b"").as_slice() == digest.as_slice(),
        Err(_) => false,
    };
    if !sha_ok {
        problems.push("sha256(empty)".into());
    }
    let key = [7u8; 32];
    match crypto::seal_zero(&key, b"live-check") {
        Ok(ct) => {
            if crypto::open_zero(&key, &ct).ok().as_deref() != Some(&b"live-check"[..]) {
                problems.push("seal_zero round-trip".into());
            }
        }
        Err(_) => problems.push("seal_zero".into()),
    }
    let nonce = [3u8; 12];
    match crypto::seal_with_nonce(&key, &nonce, b"nonce-check") {
        Ok(ct) => {
            if crypto::open_with_nonce(&key, &nonce, &ct).ok().as_deref()
                != Some(&b"nonce-check"[..])
            {
                problems.push("seal_with_nonce round-trip".into());
            }
        }
        Err(_) => problems.push("seal_with_nonce".into()),
    }
    if crypto::hex_encode(&[0xde, 0xad]) != "dead"
        || crypto::hex_decode("dead").ok().as_deref() != Some(&[0xde, 0xad][..])
    {
        problems.push("hex round-trip".into());
    }
    if problems.is_empty() {
        r.ok(
            comp::CRYPTO,
            "primitives round-trip",
            "sha256, hex, seal/open (zero and explicit nonce)",
            Vec::new(),
        );
    } else {
        r.fail(
            comp::CRYPTO,
            "primitives round-trip",
            problems.join("; "),
            Vec::new(),
        );
    }
}

// --------------------------------------------------------- online CLI (net)

/// One real online download: the binary's whole happy path — page, handshake,
/// listen, key paging, container fetch, decrypt and `--verify`.
pub fn run_cli_online(r: &mut Runner, ctx: &Ctx, live: &Live) -> Option<Artifacts> {
    let audio = ctx.workdir.join("track.m4a");
    let keys = ctx.workdir.join("keys.json");
    let hax = ctx.workdir.join("track.hax");
    let args = vec![
        ctx.url.clone(),
        "--out".into(),
        path(&audio),
        "--save-keys".into(),
        path(&keys),
        "--save-hax".into(),
        path(&hax),
        "--verify".into(),
        "--json".into(),
        "--quiet".into(),
    ];
    let out = match run_cli(ctx, &args) {
        Ok(out) => out,
        Err(e) => {
            r.fail(comp::CLI, "online run", e, Vec::new());
            return None;
        }
    };
    if out.code != 0 {
        let component = if challenge_in(&out.stderr) {
            comp::ENVIRONMENT
        } else {
            comp::DECRYPT
        };
        r.fail(
            component,
            "online run",
            format!("exit {} · {}", out.code, first_line(&out.stderr)),
            ev(vec![("stderr", tail(&out.stderr, 400))]),
        );
        return None;
    }
    r.ok(
        comp::CLI,
        "online run",
        "exit 0",
        ev(vec![("stderr", tail(&out.stderr, 200))]),
    );

    let Some(value) = json_value(&out.stdout) else {
        r.fail(
            comp::DECRYPT,
            "online run --json",
            "no JSON result on stdout",
            ev(vec![("stdout", tail(&out.stdout, 200))]),
        );
        return None;
    };

    let audio_bytes = match std::fs::read(&audio) {
        Ok(bytes) => bytes,
        Err(e) => {
            r.fail(
                comp::DECRYPT,
                "online run wrote the audio",
                format!("cannot read {}: {e}", audio.display()),
                Vec::new(),
            );
            return None;
        }
    };

    // Requests actually spent: one page, one listen per key page, one container.
    let pages = u64_field(&value, "keyPages").unwrap_or(0);
    r.requests.page += 1;
    r.requests.listen += pages as u32 + 1;
    r.requests.container += 1;

    if audio_bytes.len() < 8 || &audio_bytes[4..8] != b"ftyp" {
        r.fail(
            comp::DECRYPT,
            "online output is MP4",
            "decrypted bytes have no `ftyp` box at offset 4",
            ev(vec![("bytes", audio_bytes.len().to_string())]),
        );
        return None;
    }
    let json_bytes = u64_field(&value, "bytes");
    if json_bytes != Some(audio_bytes.len() as u64) {
        r.fail(
            comp::DECRYPT,
            "online --json byte count",
            format!(
                "json bytes {json_bytes:?} != file {} bytes",
                audio_bytes.len()
            ),
            Vec::new(),
        );
    } else {
        r.ok(
            comp::DECRYPT,
            "online decrypt",
            format!(
                "{} bytes · ftyp ok · {}",
                audio_bytes.len(),
                describe(&value)
            ),
            ev(vec![
                ("bytes", audio_bytes.len().to_string()),
                (
                    "segment_count",
                    u64_field(&value, "segmentCount").unwrap_or(0).to_string(),
                ),
                (
                    "codec",
                    str_field(&value, "codec").unwrap_or("?").to_string(),
                ),
                (
                    "duration_secs",
                    value
                        .get("durationSecs")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0)
                        .to_string(),
                ),
            ]),
        );
    }

    if pages == 0 {
        r.fail(
            comp::PAGING,
            "key paging",
            "the run reported 0 key pages; the first branch covered every segment, so paging \
             was not exercised",
            Vec::new(),
        );
    } else {
        r.ok(
            comp::PAGING,
            "key paging",
            format!("{pages} key page(s)"),
            ev(vec![("key_pages", pages.to_string())]),
        );
    }

    run_container(r, &hax, &value);
    run_envelope(r, &keys, &hax, live, &value);
    run_verify(r, &audio);

    Some(Artifacts {
        audio_bytes,
        keys,
        hax,
    })
}

fn run_container(r: &mut Runner, hax_path: &Path, value: &serde_json::Value) {
    let bytes = match std::fs::read(hax_path) {
        Ok(b) => b,
        Err(e) => {
            r.fail(
                comp::CONTAINER,
                "read saved .hax",
                format!("cannot read {}: {e}", hax_path.display()),
                Vec::new(),
            );
            return;
        }
    };
    match Hax0::parse(&bytes) {
        Ok(hax) => {
            let json_segments = u64_field(value, "segmentCount");
            let mut mismatches: Vec<String> = Vec::new();
            if &bytes[..4] != b"HAX0" {
                mismatches.push("magic is not HAX0".into());
            }
            if hax.file_length as usize != bytes.len() {
                mismatches.push("fileLength != actual size".into());
            }
            if json_segments != Some(u64::from(hax.segment_count)) {
                mismatches.push("segmentCount disagrees with --json".into());
            }
            if mismatches.is_empty() {
                r.ok(
                    comp::CONTAINER,
                    "parse HAX0 container",
                    format!(
                        "{} segments · {} ms · {} · header {} B",
                        hax.segment_count, hax.duration_ms, hax.codec, hax.header_length
                    ),
                    ev(vec![
                        ("file_length", hax.file_length.to_string()),
                        ("header_length", hax.header_length.to_string()),
                        ("extra_length", hax.extra_length.to_string()),
                        ("segment_count", hax.segment_count.to_string()),
                        ("codec", hax.codec.clone()),
                        ("duration_ms", hax.duration_ms.to_string()),
                        ("base_key_len", hax.base_key.len().to_string()),
                    ]),
                );
            } else {
                r.fail(
                    comp::CONTAINER,
                    "parse HAX0 container",
                    mismatches.join("; "),
                    ev(vec![
                        ("segment_count", hax.segment_count.to_string()),
                        ("json_segment_count", format!("{json_segments:?}")),
                    ]),
                );
            }
        }
        Err(e) => r.fail(
            comp::CONTAINER,
            "parse HAX0 container",
            e.to_string(),
            ev(vec![("bytes", bytes.len().to_string())]),
        ),
    }
}

fn run_envelope(
    r: &mut Runner,
    keys_path: &Path,
    hax_path: &Path,
    live: &Live,
    value: &serde_json::Value,
) {
    let text = match std::fs::read_to_string(keys_path) {
        Ok(t) => t,
        Err(e) => {
            r.fail(
                comp::KEYS,
                "read saved keys",
                format!("cannot read {}: {e}", keys_path.display()),
                Vec::new(),
            );
            return;
        }
    };
    let saved = match download::parse_saved_keys(&text) {
        Ok(saved) => saved,
        Err(e) => {
            r.fail(comp::KEYS, "parse saved keys", e.to_string(), Vec::new());
            return;
        }
    };

    // Every segment must be derivable from the saved branch keys alone: that is
    // what makes the offline and streaming checks request-free.
    let coverage = std::fs::read(hax_path)
        .ok()
        .and_then(|b| Hax0::parse(&b).ok())
        .map(|hax| {
            let tree = KeyTree::new(hax.segment_count.max(1));
            let mut cache = KeyMap::new();
            let mut missing = 0u32;
            for i in 0..hax.segment_count {
                if tree.derive(&saved.keys, i, &mut cache).is_err() {
                    missing += 1;
                }
            }
            (hax.segment_count, missing)
        });

    let json_hax_url = str_field(value, "haxUrl");
    let url_matches = saved.hax_url.as_deref() == json_hax_url
        || saved.hax_url.as_deref() == Some(live.hax_url.as_str());

    if !url_matches {
        r.fail(
            comp::KEYS,
            "saved envelope carries the .hax URL",
            format!(
                "envelope url {:?} != live {:?}",
                saved.hax_url, live.hax_url
            ),
            Vec::new(),
        );
    } else {
        let missing = coverage.map(|(_, m)| m).unwrap_or(0);
        if missing > 0 {
            r.fail(
                comp::KEYS,
                "saved keys cover every segment",
                format!("{missing} segment(s) are not derivable from the envelope"),
                ev(vec![("keys", saved.keys.len().to_string())]),
            );
        } else {
            let covered = coverage
                .map(|(total, _)| format!(" · {total} segments covered"))
                .unwrap_or_default();
            r.ok(
                comp::KEYS,
                "saved-keys envelope",
                format!(
                    "{} node key(s){covered} · {}",
                    saved.keys.len(),
                    file_name(&live.hax_url)
                ),
                ev(vec![
                    ("keys", saved.keys.len().to_string()),
                    ("title", format!("{:?}", saved.title)),
                    ("hax_url", format!("{:?}", saved.hax_url)),
                ]),
            );
        }
    }
}

fn run_verify(r: &mut Runner, audio: &Path) {
    let file = match std::fs::File::open(audio) {
        Ok(f) => f,
        Err(e) => {
            r.fail(
                comp::VERIFY,
                "open output",
                format!("cannot read {}: {e}", audio.display()),
                Vec::new(),
            );
            return;
        }
    };
    match mp4::verify(file) {
        Ok(info) => r.ok(
            comp::VERIFY,
            "MP4 structure",
            format!(
                "{} boxes · fragmented={} · duration {}",
                info.boxes,
                info.fragmented,
                info.duration_ms
                    .map_or("unknown".to_string(), |d| format!("{d} ms"))
            ),
            ev(vec![
                ("boxes", info.boxes.to_string()),
                ("fragmented", info.fragmented.to_string()),
                ("duration_ms", format!("{:?}", info.duration_ms)),
            ]),
        ),
        Err(e) => r.fail(comp::VERIFY, "MP4 structure", e.to_string(), Vec::new()),
    }
}

// ------------------------------------------------------- offline (no net)

/// Decrypt from the saved container and keys, through both the library and the
/// CLI, and confirm both reproduce the online bytes exactly. No network at all.
pub fn run_offline(r: &mut Runner, ctx: &Ctx, art: &Artifacts) {
    let hax_bytes = std::fs::read(&art.hax).unwrap_or_default();
    let envelope = std::fs::read_to_string(&art.keys).unwrap_or_default();
    match download::parse_saved_keys(&envelope) {
        Ok(saved) => match download::decrypt_offline(&hax_bytes, &saved.keys, None) {
            Ok((audio, segments, duration)) => {
                if audio == art.audio_bytes {
                    r.ok(
                        comp::OFFLINE,
                        "decrypt_offline matches the online bytes",
                        format!("{segments} segments · {duration:.3} s"),
                        ev(vec![("bytes", audio.len().to_string())]),
                    );
                } else {
                    r.fail(
                        comp::OFFLINE,
                        "decrypt_offline matches the online bytes",
                        format!("{} bytes vs online {}", audio.len(), art.audio_bytes.len()),
                        Vec::new(),
                    );
                }
            }
            Err(e) => r.fail(comp::OFFLINE, "decrypt_offline", e.to_string(), Vec::new()),
        },
        Err(e) => r.fail(comp::OFFLINE, "parse envelope", e.to_string(), Vec::new()),
    }

    let audio = ctx.workdir.join("offline.m4a");
    let args = vec![
        "--hax".into(),
        path(&art.hax),
        "--keys".into(),
        path(&art.keys),
        "--out".into(),
        path(&audio),
        "--verify".into(),
        "--json".into(),
        "--quiet".into(),
    ];
    match run_cli(ctx, &args) {
        Ok(out) if out.code == 0 => {
            let got = std::fs::read(&audio).unwrap_or_default();
            if got == art.audio_bytes {
                r.ok(
                    comp::OFFLINE,
                    "CLI --hax --keys matches the online bytes",
                    format!("{} bytes", got.len()),
                    Vec::new(),
                );
            } else {
                r.fail(
                    comp::OFFLINE,
                    "CLI --hax --keys matches the online bytes",
                    format!("{} bytes vs online {}", got.len(), art.audio_bytes.len()),
                    Vec::new(),
                );
            }
        }
        Ok(out) => r.fail(
            comp::OFFLINE,
            "CLI offline decrypt",
            format!("exit {} · {}", out.code, first_line(&out.stderr)),
            ev(vec![("stderr", tail(&out.stderr, 400))]),
        ),
        Err(e) => r.fail(comp::OFFLINE, "CLI offline decrypt", e, Vec::new()),
    }

    // The no-clobber guard must refuse the output that now exists.
    let mut guard = args.clone();
    guard.push("--no-clobber".into());
    match run_cli(ctx, &guard) {
        Ok(out) if out.code != 0 => r.ok(
            comp::CLI,
            "--no-clobber refuses an existing output",
            format!("exit {}", out.code),
            Vec::new(),
        ),
        Ok(out) => r.fail(
            comp::CLI,
            "--no-clobber refuses an existing output",
            "the CLI replaced an existing file despite --no-clobber",
            ev(vec![("exit", out.code.to_string())]),
        ),
        Err(e) => r.fail(
            comp::CLI,
            "--no-clobber refuses an existing output",
            e,
            Vec::new(),
        ),
    }
}

/// Cached mode: saved keys plus a fresh container fetch, no page, no listen.
pub fn run_cached(r: &mut Runner, ctx: &Ctx, art: &Artifacts) {
    let audio = ctx.workdir.join("cached.m4a");
    let args = vec![
        "--keys".into(),
        path(&art.keys),
        "--out".into(),
        path(&audio),
        "--verify".into(),
        "--json".into(),
        "--quiet".into(),
    ];
    match run_cli(ctx, &args) {
        Ok(out) if out.code == 0 => {
            r.requests.container += 1;
            let got = std::fs::read(&audio).unwrap_or_default();
            if got == art.audio_bytes {
                r.ok(
                    comp::CACHED,
                    "cached decrypt (saved keys + container) matches",
                    format!("{} bytes", got.len()),
                    Vec::new(),
                );
            } else {
                r.fail(
                    comp::CACHED,
                    "cached decrypt (saved keys + container) matches",
                    format!("{} bytes vs online {}", got.len(), art.audio_bytes.len()),
                    Vec::new(),
                );
            }
        }
        Ok(out) => {
            let component = if challenge_in(&out.stderr) {
                comp::ENVIRONMENT
            } else {
                comp::CACHED
            };
            r.fail(
                component,
                "cached decrypt",
                format!("exit {} · {}", out.code, first_line(&out.stderr)),
                ev(vec![("stderr", tail(&out.stderr, 400))]),
            );
        }
        Err(e) => r.fail(comp::CACHED, "cached decrypt", e, Vec::new()),
    }
}

/// Streaming: ranged reads, no paging (the envelope seeds every key).
pub fn run_streaming(r: &mut Runner, ctx: &Ctx, art: &Artifacts, page_html: &Path) {
    let audio = ctx.workdir.join("streamed.m4a");
    let args = vec![
        "--keys".into(),
        path(&art.keys),
        "--stream-to".into(),
        path(&audio),
        "--verify".into(),
        "--json".into(),
        "--quiet".into(),
        path(page_html),
    ];
    match run_cli(ctx, &args) {
        Ok(out) if out.code == 0 => {
            let value = json_value(&out.stdout).unwrap_or(serde_json::Value::Null);
            let segments = u64_field(&value, "segmentCount").unwrap_or(0);
            r.requests.listen += 1;
            r.requests.range += segments as u32 + 2; // two header probes
            let got = std::fs::read(&audio).unwrap_or_default();
            if got == art.audio_bytes {
                r.ok(
                    comp::STREAMING,
                    "streamed output matches the buffered bytes",
                    format!("{} bytes over ~{segments} ranges", got.len()),
                    ev(vec![("segment_count", segments.to_string())]),
                );
            } else {
                r.fail(
                    comp::STREAMING,
                    "streamed output matches the buffered bytes",
                    format!("{} bytes vs online {}", got.len(), art.audio_bytes.len()),
                    Vec::new(),
                );
            }
        }
        Ok(out) => {
            let component = if challenge_in(&out.stderr) {
                comp::ENVIRONMENT
            } else {
                comp::STREAMING
            };
            r.fail(
                component,
                "streamed CLI run",
                format!("exit {} · {}", out.code, first_line(&out.stderr)),
                ev(vec![("stderr", tail(&out.stderr, 400))]),
            );
        }
        Err(e) => r.fail(comp::STREAMING, "streamed CLI run", e, Vec::new()),
    }
}

/// `--keys-only`: the page plus a single listen, no container.
pub fn run_keys_only(r: &mut Runner, ctx: &Ctx) {
    let keys = ctx.workdir.join("branch.json");
    let args = vec![
        "--keys-only".into(),
        "--save-keys".into(),
        path(&keys),
        "--json".into(),
        "--quiet".into(),
        ctx.url.clone(),
    ];
    match run_cli(ctx, &args) {
        Ok(out) if out.code == 0 => {
            r.requests.page += 1;
            r.requests.listen += 1;
            let saved = std::fs::read_to_string(&keys)
                .ok()
                .and_then(|t| download::parse_saved_keys(&t).ok());
            match saved {
                Some(saved) if !saved.keys.is_empty() => r.ok(
                    comp::LISTEN,
                    "--keys-only saves the first branch",
                    format!("{} node key(s)", saved.keys.len()),
                    ev(vec![("keys", saved.keys.len().to_string())]),
                ),
                _ => r.fail(
                    comp::LISTEN,
                    "--keys-only saves the first branch",
                    "no usable envelope was written",
                    Vec::new(),
                ),
            }
        }
        Ok(out) => {
            let component = if challenge_in(&out.stderr) {
                comp::ENVIRONMENT
            } else {
                comp::LISTEN
            };
            r.fail(
                component,
                "--keys-only",
                format!("exit {} · {}", out.code, first_line(&out.stderr)),
                ev(vec![("stderr", tail(&out.stderr, 400))]),
            );
        }
        Err(e) => r.fail(comp::LISTEN, "--keys-only", e, Vec::new()),
    }
}

// ------------------------------------------------------ CLI surface (no net)

/// `--list-tracks` from the saved page, plus the argument-validation surface.
/// None of this needs the network, so it runs even when the site is down.
pub fn run_cli_surface(r: &mut Runner, ctx: &Ctx, page_html: Option<&Path>) {
    if let Some(page) = page_html {
        let args = vec!["--list-tracks".into(), path(page)];
        match run_cli(ctx, &args) {
            Ok(out) if out.code == 0 => {
                let lines: Vec<&str> = out
                    .stdout
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .collect();
                r.ok(
                    comp::CLI,
                    "--list-tracks a saved page",
                    format!("{} track line(s)", lines.len()),
                    ev(vec![("stdout", out.stdout.trim().to_string())]),
                );
            }
            Ok(out) => r.fail(
                comp::CLI,
                "--list-tracks a saved page",
                format!("exit {} · {}", out.code, first_line(&out.stderr)),
                Vec::new(),
            ),
            Err(e) => r.fail(comp::CLI, "--list-tracks a saved page", e, Vec::new()),
        }
    }

    // Each of these must be rejected with exit 2 before any network call, so a
    // lone URL is enough to reach the parser.
    let cases: &[(&str, &[&str])] = &[
        (
            "unknown flag",
            &["--definitely-not-a-flag", "https://hotaudio.net/u/a/b"],
        ),
        (
            "--quiet with --verbose",
            &["--quiet", "--verbose", "https://hotaudio.net/u/a/b"],
        ),
        (
            "--keys-only without --save-keys",
            &["--keys-only", "https://hotaudio.net/u/a/b"],
        ),
        (
            "--verify with --stream-to -",
            &["--verify", "--stream-to", "-", "https://hotaudio.net/u/a/b"],
        ),
        ("no arguments", &[]),
    ];
    let mut failures: Vec<String> = Vec::new();
    for (name, args) in cases {
        let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        match run_cli(ctx, &args) {
            Ok(out) if out.code == 2 => {}
            Ok(out) => failures.push(format!("{name}: exit {} (want 2)", out.code)),
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }
    if failures.is_empty() {
        r.ok(
            comp::CLI,
            "argument validation exits 2",
            format!("{} rejected cases", cases.len()),
            Vec::new(),
        );
    } else {
        r.fail(
            comp::CLI,
            "argument validation exits 2",
            failures.join("; "),
            Vec::new(),
        );
    }

    let mut ok = true;
    match run_cli(ctx, &["--version".into()]) {
        Ok(out) if out.code == 0 && out.stdout.contains("hotaudio-download") => {}
        _ => ok = false,
    }
    match run_cli(ctx, &["--help".into()]) {
        Ok(out) if out.code == 0 && out.stdout.contains("Usage:") => {}
        _ => ok = false,
    }
    if ok {
        r.ok(
            comp::CLI,
            "--help and --version",
            "exit 0 with the expected text",
            Vec::new(),
        );
    } else {
        r.fail(
            comp::CLI,
            "--help and --version",
            "wrong exit code or output",
            Vec::new(),
        );
    }
}

fn describe(value: &serde_json::Value) -> String {
    let codec = str_field(value, "codec").unwrap_or("?");
    let dur = value
        .get("durationSecs")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let segments = u64_field(value, "segmentCount").unwrap_or(0);
    format!("{codec} · {dur:.1} s · {segments} segments")
}
