//! Decrypt a local `.hax` container with a saved-keys file, fully offline.
//!
//! ```text
//! cargo run --example decrypt_offline -- audio.hax keys.json track.m4a
//! ```
//!
//! The keys file can be the JSON envelope written by `--save-keys` (or
//! [`download::keys_to_json`]) or a bare `{ "node": "hex" }` map. No page fetch
//! and no listen call happen here; see `download_track` for the online path.

#![cfg_attr(target_arch = "wasm32", allow(dead_code, unused_imports))]

use hotaudio::download;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [hax, keys, out] = args.as_slice() else {
        eprintln!("usage: decrypt_offline <audio.hax> <keys.json> <out.m4a>");
        return Ok(());
    };

    let hax_bytes = std::fs::read(hax)?;
    let envelope = std::fs::read_to_string(keys)?;
    let saved = download::parse_saved_keys(&envelope)?;

    let (audio, segments, seconds) = download::decrypt_offline(&hax_bytes, &saved.keys, None)?;
    std::fs::write(out, &audio)?;

    let bytes = audio.len();
    eprintln!("wrote {out} ({bytes} bytes, {seconds:.1}s, {segments} segments)");
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn main() {}
