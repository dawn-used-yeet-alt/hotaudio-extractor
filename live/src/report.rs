//! The structured result of one acceptance run.
//!
//! The suite always writes a [`Report`] (with `--report`), pass or fail. CI reads
//! it and files **one issue per broken component**, so the component ids below
//! are a stable contract: renaming one orphans its issue.

use serde::{Deserialize, Serialize};

/// Component ids. Every check names exactly one.
pub mod comp {
    /// The track page fetch itself.
    pub const PAGE: &str = "page";
    /// The embedded `__ha_state` payload: extraction and decryption.
    pub const PAGE_STATE: &str = "page-state";
    /// Track listing from the page state.
    pub const TRACKS: &str = "tracks";
    /// The handshake: track selection, server key, key exchange.
    pub const HANDSHAKE: &str = "handshake";
    /// The signature VM and its pinned bytecode/environment.
    pub const SIGNER: &str = "signer";
    /// The encrypted listen API and its response shape.
    pub const LISTEN: &str = "listen";
    /// The `.hax` URL the server issues vs. the one derived from the track key.
    pub const HAX_URL: &str = "hax-url";
    /// `.hax` fetch and HAX0/bencode parse.
    pub const CONTAINER: &str = "container";
    /// Key-tree paging to cover every segment.
    pub const PAGING: &str = "paging";
    /// Segment decryption.
    pub const DECRYPT: &str = "decrypt";
    /// The decrypted MP4: structure and duration.
    pub const VERIFY: &str = "verify";
    /// The saved-keys envelope.
    pub const KEYS: &str = "keys";
    /// Offline decrypt (`--hax` + `--keys`).
    pub const OFFLINE: &str = "offline";
    /// Cached decrypt (saved keys + a fresh container fetch).
    pub const CACHED: &str = "cached";
    /// The ranged, streaming path (`--stream-to`).
    pub const STREAMING: &str = "streaming";
    /// The crypto primitives.
    pub const CRYPTO: &str = "crypto";
    /// The site or network, not the crate: a Cloudflare challenge, DNS failure
    /// or other condition no code change can answer. Never filed as a product
    /// issue.
    pub const ENVIRONMENT: &str = "environment";
    /// The CLI's flags, modes and exit codes.
    pub const CLI: &str = "cli";

    /// Every id, for `--list`.
    pub const ALL: &[&str] = &[
        PAGE,
        PAGE_STATE,
        TRACKS,
        HANDSHAKE,
        SIGNER,
        LISTEN,
        HAX_URL,
        CONTAINER,
        PAGING,
        DECRYPT,
        VERIFY,
        KEYS,
        OFFLINE,
        CACHED,
        STREAMING,
        CRYPTO,
        ENVIRONMENT,
        CLI,
    ];
}

/// One assertion. `component` groups failures for issue filing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub component: String,
    pub name: String,
    pub ok: bool,
    pub detail: String,
    /// Name/value diagnostics, kept out of the summary and into the issue body.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub evidence: Vec<(String, String)>,
}

/// How much traffic the run sent, for the report and the log.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Requests {
    /// Requests to the `hotaudio.net` page host.
    pub page: u32,
    /// Requests to the listen API (key paging included).
    pub listen: u32,
    /// Whole-container transfers.
    pub container: u32,
    /// Ranged container reads.
    pub range: u32,
}

/// The whole run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub url: String,
    pub cli: String,
    /// Revision of the crate under test, when known.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub crate_rev: Option<String>,
    pub started_unix: u64,
    pub duration_secs: f64,
    pub requests: Requests,
    pub passed: usize,
    pub failed: usize,
    pub checks: Vec<Check>,
}
