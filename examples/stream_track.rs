//! Stream a track to a file (or stdout), one decrypted segment at a time.
//!
//! ```text
//! cargo run --example stream_track -- https://hotaudio.net/u/user/track-slug track.m4a
//! ```
//!
//! Peak memory stays at one segment instead of the whole track, and the first
//! playable fragment arrives after a single round trip. `-` writes the audio to
//! stdout so it can be piped (`… - | ffplay -`); progress goes to stderr so the
//! stream stays clean.

#![cfg_attr(target_arch = "wasm32", allow(dead_code, unused_imports))]

use std::io::Write;

use hotaudio::download::{self, Phase, Progress};
use hotaudio::listen;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [url, out] = args.as_slice() else {
        eprintln!("usage: stream_track <page-url> <out.m4a|->");
        return Ok(());
    };

    // The handshake is what every listen/key-paging call for this track reuses.
    let client = hotaudio::http::Client::api();
    let handshake = hotaudio::block_on(listen::load_handshake(
        &client,
        url,
        hotaudio::HOTAUDIO_API_BASE,
        None,
    ))?;

    let mut sink: Box<dyn Write> = if out.as_str() == "-" {
        Box::new(std::io::stdout())
    } else {
        Box::new(std::fs::File::create(out)?)
    };
    let mut report = |p: Progress| {
        let (loaded, total) = (p.loaded, p.total);
        eprintln!("{}: {loaded}/{total}", phase_name(p.phase));
    };

    let streamed = hotaudio::block_on(download::download_streaming(
        &client,
        &handshake,
        None,
        0,
        |fragment| {
            sink.write_all(fragment)
                .map_err(|e| hotaudio::Error::Io(e.to_string()))
        },
        Some(&mut report),
        None,
    ))?;

    sink.flush()?;
    let bytes = streamed.bytes;
    let segments = streamed.segment_count;
    eprintln!("streamed {bytes} bytes in {segments} segments");
    Ok(())
}

/// A stable name for each [`Phase`] in progress lines.
#[cfg(not(target_arch = "wasm32"))]
fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Resolving => "resolving",
        Phase::Fetching => "fetching",
        Phase::Decrypting => "decrypting",
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {}
