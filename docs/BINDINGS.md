# Bindings: Kotlin, Swift, Python and Ruby

The protocol is implemented once, in the Rust crate. The `bindings/` workspace
adds a [UniFFI](https://mozilla.github.io/uniffi-rs/) surface over it and
generates four language bindings from that one interface. Nothing in the
protocol is reimplemented per language: the generated code marshals arguments,
owns strings and buffers, and calls the same pipeline the CLI and the browser
build call.

The bindings are **blocking**. The native transport is blocking already, and the
crate's `async` boundary never suspends on native. Run a call on a background
thread:

| Language | Dispatch |
| --- | --- |
| Kotlin | `Dispatchers.IO` (never the main thread) |
| Swift | `Task.detached` or a background queue |
| Python | `asyncio.to_thread` or a `threading.Thread` |
| Ruby | a `Thread` |

## API

All four languages expose the same functions:

| Function | What it does |
| --- | --- |
| `list_tracks(html)` | Tracks on a page, no network |
| `track_keys(url, track_id?, api_base?)` | Page handshake + first listen: keys and `.hax` URL |
| `download_to_file(url, path, options?, progress?, cancel?)` | Full download, streamed to a file |
| `download_bytes(url, options?, progress?, cancel?)` | Full download, decrypted bytes returned |
| `download_cached_to_file(hax_url, keys_json, path, progress?, cancel?)` | Re-fetch and decrypt a saved container |
| `decrypt_hax(bytes, keys_json)` | Offline decrypt from memory |
| `decrypt_hax_to_file(input, keys_json, output, progress?)` | Offline decrypt from disk |
| `sign_payload(payload)` | `X-Signature` for the current time |
| `sign_payload_at(payload, timestamp_secs)` | Pinned-clock signature |
| `version()` | Binding crate version |
| `init_logging(listener)` | Route the crate's warnings to the host logger |

`DownloadOptions` is a record with `track_id`, `api_base`, `delay_ms` and
`initial_keys_json`; pass `nil`/`None` for the defaults. `CancellationToken` is
an object with `cancel()`, `reset()` and `is_cancelled()`; create it on one
thread and pass it to a call running on another. `ProgressListener` receives
`(phase, loaded, total)` reports, throttled to about 10 per second.

Only `download_to_file` streams. `download_bytes`, `download_cached_to_file`
and the offline decrypts hold the container and the plaintext in memory
(15–25 MB each), so prefer the file entry points on mobile.

### Errors

Every fallible call throws/raises `FfiError`:

- `Failed { code, detail }`: `code` is stable (`http`, `listen`, `auth`,
  `decode`, `state`, `protocol`, `sign`, `io`, `cancelled`, `callback`), and
  `detail` is for humans. It is named `detail` rather than `message` because
  Kotlin exceptions already inherit `Throwable.message`.
- `Panic { detail }`: a Rust panic was caught at the boundary. This is a bug;
  the process does not unwind into the JVM, Swift or Python.

A cancellation is `Failed` with `code == "cancelled"`.

## Generating and building

The generator is pinned in `bindings/Cargo.lock`, so it cannot drift from the
runtime:

```bash
scripts/build-bindings.sh                  # bindings/out, debug
scripts/build-bindings.sh --release        # bindings/out, optimised
scripts/build-bindings.sh /tmp/bindings    # a different output directory
```

That builds `bindings/target/{debug,release}/libhotaudio_ffi.{so,dylib,dll}`
and writes `Hotaudio.swift`, `HotaudioFFI.h`, `HotaudioFFI.modulemap`, the
Kotlin package `net.hotaudio.extractor`, `hotaudio_ffi.py` and
`hotaudio_ffi.rb` into the output directory.

The bindings workspace is separate from the root crate on purpose. The core's
offline tests, MSRV check and permissive-only license policy do not see UniFFI
(MPL-2.0); the bindings carry their own `deny.toml`. Both policies are enforced
in `.github/workflows/bindings.yml`.

### Python

```bash
scripts/build-python.sh               # bindings/out/python
scripts/build-python.sh /tmp/py --release
```

This stages `bindings/out/python/pkg/hotaudio_ffi/` with the ctypes module and
the shared library side by side, and builds a platform wheel in
`bindings/out/python/dist/`. The wheel is Python-version-independent (ctypes,
not the CPython C API) but platform- and architecture-specific:

```python
import asyncio
from hotaudio_ffi import download_to_file

async def main():
    await asyncio.to_thread(
        download_to_file,
        "https://hotaudio.net/u/user/track",
        "/tmp/track.m4a",
        None,
        None,
        None,
    )

asyncio.run(main())
```

### Ruby

The generated file needs the [`ffi`](https://github.com/ffi/ffi) gem and the
library on the loader path:

```bash
gem install ffi
ruby -I bindings/out -r hotaudio_ffi -e \
  'puts HotaudioFfi.sign_payload_at("x", 0.0)'
```

`LD_LIBRARY_PATH` (or `DYLD_LIBRARY_PATH` on macOS) must include the directory
holding `libhotaudio_ffi`. Ruby is generated from the same interface as the
other three; UniFFI's Ruby support receives fixes but not new features, so
treat it as the scripting-language path while Python is the supported one.

### Android

The generated Kotlin is plain JVM code using JNA. In a Gradle module:

1. Build the `.so` per ABI with `cargo ndk` (`aarch64-linux-android`
   required; `x86_64-linux-android` for emulators):

   ```bash
   cargo ndk -t arm64-v8a -t x86_64 -o app/src/main/jniLibs \
     build --manifest-path bindings/Cargo.toml --release -p hotaudio-ffi
   ```

   `cargo-ndk` installs the NDK linker configuration; use NDK r27 or newer for
   16 KB page alignment, which Google Play requires for apps targeting Android
   15+. Verify with `llvm-readelf -l libhotaudio_ffi.so` and look for
   `LOAD ... 0x4000` alignment.

2. Add JNA (`net.java.dev.jna:jna:5.17.0@aar`) and `INTERNET`:

   ```xml
   <uses-permission android:name="android.permission.INTERNET" />
   ```

3. `minSdk` must be at least 24: platform-verifier's revocation checking needs
   Android 7.0.

4. Initialize TLS once, before the first request:

   ```kotlin
   class App : Application() {
       override fun onCreate() {
           super.onCreate()
           Hotaudio.initAndroidTls(this)
       }
   }
   ```

   `bindings/android/HotaudioTls.kt` is that helper. It loads the same
   `libhotaudio_ffi.so` as the generated code and calls the JNI export
   `Java_net_hotaudio_extractor_Hotaudio_initAndroidTls`, the one piece UniFFI
   cannot generate.

5. Keep the verifier's Kotlin component and R8 rules. The
   `rustls-platform-verifier` version is in `bindings/Cargo.lock`; follow its
   Android README for the Gradle `ValueSource` that pins the matching
   `rustls-platform-verifier-android` artifact, and add:

   ```proguard
   -keep, includedescriptorclasses class org.rustls.platformverifier.** { *; }
   ```

   If the app cannot use the JVM verifier (offline/F-Droid builds, no Maven
   component), the core's `ureq` dependency currently selects
   `platform-verifier`. Changing it to `webpki-roots` is a core Cargo feature
   and a deliberate policy choice; it is not exposed by these bindings yet.

6. Do not strip the `.so`: `bindings/Cargo.toml` sets `strip = false` so crash
   reports can be symbolicated. Upload the native symbols with the App Bundle.

### iOS

The FFI crate also produces a `staticlib`:

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
cargo build --manifest-path bindings/Cargo.toml --release -p hotaudio-ffi \
  --target aarch64-apple-ios
cargo build --manifest-path bindings/Cargo.toml --release -p hotaudio-ffi \
  --target aarch64-apple-ios-sim
xcodebuild -create-xcframework \
  -library bindings/target/aarch64-apple-ios/release/libhotaudio_ffi.a \
  -headers bindings/out/swift \
  -library bindings/target/aarch64-apple-ios-sim/release/libhotaudio_ffi.a \
  -headers bindings/out/swift \
  -output bindings/out/Hotaudio.xcframework
```

Point a Swift package binary target at the XCFramework and the SwiftPM target
at the generated `Hotaudio.swift`; `HotaudioFFI.modulemap` and
`HotaudioFFI.h` belong with the binary target. No TLS initialization is needed:
`rustls-platform-verifier` uses `Security.framework` directly. ATS does not
block HTTPS to hotaudio.net, so no `NSAllowsArbitraryLoads` is needed.

iOS suspends a foreground transfer when the app leaves the screen. For a
download that must survive suspension, use the split below.

### Background downloads (mobile)

The `.hax` container needs neither signing nor the handshake; it only needs a
`User-Agent`. The platform downloader is therefore the better bulk transport:

1. `track_keys(url)` in Rust (small: page + listen).
2. Download `hax_url` with `URLSession` background configuration, OkHttp's
   `DownloadManager`, or Android's `DownloadManager`.
3. `decrypt_hax_to_file(downloaded, keys_json, output)` in Rust.

This avoids holding a 15–25 MB buffer through JNI twice and lets the OS own
the transfer across app suspension. `download_to_file` remains the convenience
path for a foreground download.

## Limitations

- No push callback per segment. Per-segment callbacks are the riskiest part of
  a mobile FFI (thread attachment, GIL, JNA callback threads) and are not needed
  for save-to-file. Streaming playback would need a pull-based fragment reader;
  it is a future addition, not part of this surface.
- `Cancel` is checked between segments, between key pages and on transfer
  chunks. A cancel during an HTTP retry sleep can take up to the retry delay.
- Proxy support is not exposed. `ureq`'s environment-variable proxy is
  meaningless in an app; a system proxy needs an explicit transport change.
- Bindings are native-only. The browser already has `src/wasm.rs`; a second
  wasm entry point would add nothing.
