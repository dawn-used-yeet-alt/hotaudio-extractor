//! Download a track from its page URL to a playable `.m4a`.
//!
//! ```text
//! cargo run --example download_track -- https://hotaudio.net/u/user/track-slug track.m4a
//! ```
//!
//! This is the buffered path: the whole track is decrypted in memory and then
//! written once. For low memory (one segment at a time), see `stream_track`.
//!
//! The two clients mirror the CLI: latency-bound page/listen/key-paging calls
//! run under a timeout on `api`, while the 15–25 MB container transfers on
//! `bulk` with no timeout, since a slow link is normal for it.

#![cfg_attr(target_arch = "wasm32", allow(dead_code, unused_imports))]

use hotaudio::download::{Clients, DownloadOptions};

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [url, out] = args.as_slice() else {
        eprintln!("usage: download_track <page-url> <out.m4a>");
        return Ok(());
    };

    let api = hotaudio::http::Client::api();
    let bulk = hotaudio::http::Client::bulk();
    let clients = Clients {
        api: &api,
        bulk: &bulk,
    };

    let track =
        hotaudio::block_on(clients.download_from_page(url, &mut DownloadOptions::default()))?;
    std::fs::write(out, &track.audio)?;

    let bytes = track.audio.len();
    let seconds = track.duration;
    let segments = track.segment_count;
    eprintln!("wrote {out} ({bytes} bytes, {seconds:.1}s, {segments} segments)");
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn main() {}
