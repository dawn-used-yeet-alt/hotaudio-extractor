//! Blocking `UniFFI` bindings over the hotaudio protocol core.
//!
//! The protocol is implemented once, in the `hotaudio` crate. This crate is a
//! thin translation layer: it owns the types `UniFFI` can represent (owned
//! strings, records, a flat error) and the thread hand-off, and nothing else.
//! Kotlin, Swift, Python and Ruby are generated from this file by `UniFFI`; the
//! same functions are available in all four.
//!
//! # Blocking, not async
//!
//! The native pipeline is synchronous under the hood: its `async` boundary is
//! `Poll::Ready` on the first poll (see `hotaudio::exec`). Exposing that as
//! `UniFFI` async would add an executor plus coroutines/concurrency machinery and
//! make the callback and lifetime problems worse for no benefit. Every function
//! here blocks, so call it from a background thread:
//!
//! - Kotlin: `Dispatchers.IO`, never the main thread (an ANR).
//! - Swift: a detached task or a background queue.
//! - Python: a thread, or `asyncio.to_thread`.
//! - Ruby: a thread.
//!
//! # Cancellation
//!
//! [`CancellationToken`] wraps an atomic flag. Create it on whichever thread is
//! convenient and pass it to a blocking call; the token is `Send + Sync`, which
//! the core's native [`hotaudio::download::Cancel`] now is too. The check is
//! coarse: it runs between segments, between key pages, and on every `.hax`
//! transfer chunk, so a cancel during an HTTP retry sleep can take up to the
//! retry delay to be observed.
//!
//! # Panics
//!
//! Every entry point wraps its body in `catch_unwind` and turns a panic into
//! [`FfiError::Panic`]. A panic must never unwind into the JVM, the Swift
//! runtime or `CPython`.
//!
//! # Android
//!
//! `ureq` verifies TLS with the platform verifier. On Android that verifier
//! calls the JVM and must be initialized with the app `Context` before the first
//! HTTPS request; call `init_android_tls` from `Application.onCreate`. iOS,
//! desktop and Python need no equivalent call.
//!
//! The functions and types below are exported to the generated bindings; the
//! `#[uniffi::export]` attribute is the whole interface description.

#![cfg(not(target_arch = "wasm32"))]
#![deny(missing_docs)]
// `UniFFI` owns every exported argument: a `String` or `Vec<u8>` arrives as a
// freshly lowered buffer, so taking it by value is the ABI, not a missed
// borrow. Internal helpers follow the same shape for consistency.
#![allow(clippy::needless_pass_by_value)]

// Exports the FFI symbols and the namespace metadata that library mode reads.
// Without this the clippy/rustc build is fine but the generator sees nothing.
uniffi::setup_scaffolding!();

use std::any::Any;
use std::io::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, RwLock};

use hotaudio::crypto::{Error as CoreError, KeyMap};
use hotaudio::download::{
    self, Cancel, Clients, DownloadOptions as CoreDownloadOptions, Downloaded, Phase,
    Progress as CoreProgress, Streamed,
};
use hotaudio::http::Client;
use hotaudio::{clock, listen};

/// `UniFFI`'s error surface.
///
/// `code` is stable and suitable for programmatic handling; `detail` is for
/// humans and may change. A cancelled call uses `code == "cancelled"` rather
/// than becoming a separate variant, so callers can treat it like any other
/// failure without losing the distinction.
///
/// The field is `detail` rather than `message` because Kotlin exceptions
/// already inherit `Throwable.message`; a subclass field with that name does
/// not compile.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    /// The protocol, transport or local filesystem failed.
    #[error("{detail}")]
    Failed {
        /// Stable machine-readable category (`http`, `listen`, `auth`, `io`, …).
        code: String,
        /// Human-readable detail.
        detail: String,
    },
    /// A panic was caught at the FFI boundary.
    #[error("internal panic: {detail}")]
    Panic {
        /// The panic payload, when it was a string.
        detail: String,
    },
}

impl From<CoreError> for FfiError {
    fn from(error: CoreError) -> Self {
        let code = match &error {
            CoreError::Auth => "auth",
            CoreError::Decode(_) => "decode",
            CoreError::State(_) => "state",
            CoreError::Listen(_) => "listen",
            CoreError::Protocol(_) => "protocol",
            CoreError::Http(_) => "http",
            CoreError::Sign(_) => "sign",
            CoreError::Io(_) => "io",
            CoreError::Cancelled => "cancelled",
        };
        Self::Failed {
            code: code.to_string(),
            detail: error.to_string(),
        }
    }
}

impl From<uniffi::UnexpectedUniFFICallbackError> for FfiError {
    fn from(error: uniffi::UnexpectedUniFFICallbackError) -> Self {
        Self::Failed {
            code: "callback".to_string(),
            detail: error.to_string(),
        }
    }
}

/// Catch a panic before it can cross into foreign code.
fn guarded<T>(body: impl FnOnce() -> Result<T, FfiError>) -> Result<T, FfiError> {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(result) => result,
        Err(payload) => Err(FfiError::Panic {
            detail: panic_message(payload.as_ref()),
        }),
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

fn io_error(error: std::io::Error) -> FfiError {
    FfiError::Failed {
        code: "io".to_string(),
        detail: error.to_string(),
    }
}

fn sign_error(error: hotaudio::signer::SignError) -> FfiError {
    FfiError::Failed {
        code: "sign".to_string(),
        detail: error.to_string(),
    }
}

/// One track listed on a page.
#[derive(Clone, Debug, uniffi::Record)]
pub struct Track {
    /// Track id, as accepted by the `track_id` option.
    pub id: String,
    /// Track key from the page state.
    pub key: String,
    /// Title, when the page provided one.
    pub title: String,
}

/// The first key branch and the `.hax` URL, without any audio.
#[derive(Clone, Debug, uniffi::Record)]
pub struct KeyEnvelope {
    /// Track title, when the page provided one.
    pub title: Option<String>,
    /// Server-issued `.hax` container URL.
    pub hax_url: String,
    /// Saved-keys envelope, ready for [`download_cached_to_file`] or
    /// [`decrypt_hax`].
    pub keys_json: String,
}

/// Options for the downloading entry points.
///
/// Pass `None` for the defaults. Every field is optional in spirit; the record
/// exists because `UniFFI` records are the portable way to express named
/// arguments across four languages.
#[derive(Clone, Debug, Default, uniffi::Record)]
pub struct DownloadOptions {
    /// Select a track on a multi-track page; `None` takes the page default.
    pub track_id: Option<String>,
    /// Override the listen API base URL (mirrors, tests). `None` uses the
    /// production host.
    pub api_base: Option<String>,
    /// Politeness pause between listen/key-page requests, in milliseconds.
    /// `0` disables it; it never affects correctness.
    pub delay_ms: u64,
    /// A saved-keys envelope to seed the key map (resume / offline reuse).
    pub initial_keys_json: Option<String>,
}

/// Everything the download pipeline knows about a track.
#[derive(Clone, Debug, uniffi::Record)]
pub struct TrackInfo {
    /// Track title, when the page or envelope provided one.
    pub title: Option<String>,
    /// Duration in seconds, as declared by the container.
    pub duration_secs: f64,
    /// Duration in milliseconds, as declared by the container.
    pub duration_ms: u64,
    /// Codec string (for example `mp4a.40.2`).
    pub codec: String,
    /// Container MIME type (`audio/mp4`).
    pub mime: String,
    /// Number of plaintext bytes written.
    pub bytes: u64,
    /// Segment count of the container.
    pub segment_count: u32,
    /// Number of key branches that had to be paged.
    pub key_pages: u32,
    /// Server-issued `.hax` URL.
    pub hax_url: String,
    /// Saved-keys envelope covering the whole track, for a later offline run.
    pub keys_json: String,
}

/// A buffered download.
#[derive(Clone, Debug, uniffi::Record)]
pub struct DownloadedTrack {
    /// Track metadata and the reusable key envelope.
    pub info: TrackInfo,
    /// Decrypted MP4 bytes. For a mobile app prefer [`download_to_file`]: a
    /// 15–25 MB buffer crossing the FFI boundary twice is waste.
    pub audio: Vec<u8>,
}

/// One progress report.
///
/// `phase` is `resolving`, `fetching` or `decrypting`. Reports are throttled to
/// roughly ten per second, and the final `loaded == total` report is always
/// delivered.
#[derive(Clone, Debug, uniffi::Record)]
pub struct Progress {
    /// Current stage.
    pub phase: String,
    /// Work completed in this stage (segments, or bytes for a transfer).
    pub loaded: u64,
    /// Work total for this stage, or `0` when the server did not say.
    pub total: u64,
}

/// Receives progress reports from the blocking calls.
///
/// Implementations run on the thread that called the download. Returning an
/// error is allowed and ignored: progress is advisory, and a listener must not
/// be able to fail a download.
#[uniffi::export(foreign)]
pub trait ProgressListener: Send + Sync {
    /// Called with a throttled progress report.
    ///
    /// # Errors
    ///
    /// An error is ignored: progress is advisory, and a listener must not be
    /// able to fail a download.
    fn on_progress(&self, progress: Progress) -> Result<(), FfiError>;
}

/// Receives the crate's warnings.
///
/// Mobile hosts forward these to the platform logger: Android's stderr is not
/// logcat, and iOS's is not `os_log`. The listener also receives warnings from
/// the core crate, which the FFI crate enables the `log` feature for.
#[uniffi::export(foreign)]
pub trait LogListener: Send + Sync {
    /// Called for each warning. `level` is currently always `warn`.
    ///
    /// # Errors
    ///
    /// An error is ignored: logging must not affect the download.
    fn on_log(&self, level: String, message: String) -> Result<(), FfiError>;
}

/// A cancellation flag shared between the host and a running call.
///
/// Create it anywhere, pass it into a blocking call, and call [`cancel`] from
/// the UI thread. The pipeline observes it between segments, between key pages
/// and on every transfer chunk.
///
/// [`cancel`]: CancellationToken::cancel
#[derive(Debug, Default, uniffi::Object)]
pub struct CancellationToken {
    cancelled: AtomicBool,
}

#[uniffi::export]
impl CancellationToken {
    /// A fresh, uncancelled token.
    #[must_use]
    // `UniFFI` constructors return `Arc<Self>`; the lint cannot know that.
    #[allow(clippy::new_ret_no_self)]
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Ask the running call to stop.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Clear the flag, so the same handle can be reused for another call.
    pub fn reset(&self) {
        self.cancelled.store(false, Ordering::SeqCst);
    }
}

impl CancellationToken {
    fn to_cancel(token: Option<&Arc<Self>>) -> Option<Cancel> {
        let token = Arc::clone(token?);
        Some(Cancel::new(move || token.is_cancelled()))
    }
}

/// A progress bridge that throttles core reports before crossing the FFI.
struct ProgressBridge {
    listener: Arc<dyn ProgressListener>,
    last_ms: f64,
    last_phase: &'static str,
}

impl ProgressBridge {
    fn new(listener: Arc<dyn ProgressListener>) -> Self {
        Self {
            listener,
            last_ms: 0.0,
            last_phase: "",
        }
    }

    fn report(&mut self, progress: CoreProgress) {
        let phase = phase_name(progress.phase);
        let now = clock::monotonic_ms();
        let finished = progress.total != 0 && progress.loaded >= progress.total;
        // A phase change and the last report always go through; everything else
        // is capped at ~10 Hz so a fast transfer cannot flood a UI thread.
        if phase == self.last_phase && !finished && now - self.last_ms < 100.0 {
            return;
        }
        self.last_phase = phase;
        self.last_ms = now;
        let _ = self.listener.on_progress(Progress {
            phase: phase.to_string(),
            loaded: progress.loaded,
            total: progress.total,
        });
    }
}

fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Resolving => "resolving",
        Phase::Fetching => "fetching",
        Phase::Decrypting => "decrypting",
    }
}

/// Build a progress closure for a core call, or `None` without a listener.
fn progress_sink(listener: Option<Arc<dyn ProgressListener>>) -> Option<impl FnMut(CoreProgress)> {
    listener.map(|listener| {
        let mut bridge = ProgressBridge::new(listener);
        move |progress| bridge.report(progress)
    })
}

/// Reborrow a progress closure as the trait object the core takes.
fn sink(callback: &mut Option<impl FnMut(CoreProgress)>) -> Option<&mut dyn FnMut(CoreProgress)> {
    callback
        .as_mut()
        .map(|callback| callback as &mut dyn FnMut(CoreProgress))
}

fn parse_keys(text: Option<&str>) -> Result<Option<KeyMap>, FfiError> {
    match text {
        Some(text) => Ok(Some(download::parse_saved_keys(text)?.keys)),
        None => Ok(None),
    }
}

fn track_title(handshake: &listen::Handshake) -> Option<String> {
    Some(handshake.track.title.clone()).filter(|title| !title.is_empty())
}

fn info_from_streamed(streamed: Streamed, title: Option<String>) -> TrackInfo {
    let keys_json = download::keys_to_json(&streamed.keys, &streamed.hax_url, title.as_deref());
    TrackInfo {
        title,
        duration_secs: streamed.duration,
        duration_ms: streamed.duration_ms,
        codec: streamed.codec,
        mime: "audio/mp4".to_string(),
        bytes: streamed.bytes,
        segment_count: streamed.segment_count,
        key_pages: streamed.pages,
        hax_url: streamed.hax_url,
        keys_json,
    }
}

fn info_from_downloaded(downloaded: &Downloaded) -> TrackInfo {
    let keys_json = download::keys_to_json(
        &downloaded.keys,
        &downloaded.hax_url,
        downloaded.title.as_deref(),
    );
    TrackInfo {
        title: downloaded.title.clone(),
        duration_secs: downloaded.duration,
        duration_ms: downloaded.duration_ms,
        codec: downloaded.codec.clone(),
        mime: downloaded.mime.to_string(),
        bytes: downloaded.audio.len() as u64,
        segment_count: downloaded.segment_count,
        key_pages: downloaded.pages,
        hax_url: downloaded.hax_url.clone(),
        keys_json,
    }
}

fn warn_if_not_canonical(page_url: &str) {
    if !hotaudio::is_hotaudio_url(page_url) {
        log::warn!(
            target: "hotaudio",
            "'{page_url}' is not a canonical share link (https://hotaudio.net/u/<user>/<track>)"
        );
    }
}

/// List the tracks on a track page, without any network call.
///
/// Takes page HTML, not a URL: a host that already fetched the page does not
/// need to fetch it twice. A page with no decryptable state returns an empty
/// list rather than an error, which is also what a non-track HTML page does.
#[must_use]
#[uniffi::export]
pub fn list_tracks(html: String) -> Vec<Track> {
    listen::list_tracks(&html)
        .unwrap_or_default()
        .into_iter()
        .map(|(id, key, title)| Track { id, key, title })
        .collect()
}

/// Run the page handshake and the first listen call, and return the keys plus
/// the `.hax` URL without downloading audio.
///
/// This is the mobile background-download split: fetch the small keys here,
/// let `URLSession`/`OkHttp` download the container with platform background
/// support, then call [`decrypt_hax`] or [`decrypt_hax_to_file`] on it.
///
/// # Errors
///
/// Returns the handshake or listen error.
#[uniffi::export]
pub fn track_keys(
    page_url: String,
    track_id: Option<String>,
    api_base: Option<String>,
) -> Result<KeyEnvelope, FfiError> {
    guarded(|| {
        warn_if_not_canonical(&page_url);
        let client = Client::api();
        let bundle = hotaudio::block_on(download::fetch_keys_only(
            &client,
            &page_url,
            api_base.as_deref(),
            track_id.as_deref(),
            0,
        ))?;
        let keys_json =
            download::keys_to_json(&bundle.keys, &bundle.hax_url, bundle.title.as_deref());
        Ok(KeyEnvelope {
            title: bundle.title,
            hax_url: bundle.hax_url,
            keys_json,
        })
    })
}

/// Download a track straight to `path`, streaming each segment as it decrypts.
///
/// The file is removed if the run fails, so a cancelled or interrupted download
/// does not leave a half-file that looks playable.
///
/// # Errors
///
/// Returns a handshake, listen, transfer, decrypt, cancellation or filesystem
/// error.
#[uniffi::export]
pub fn download_to_file(
    page_url: String,
    path: String,
    options: Option<DownloadOptions>,
    progress: Option<Arc<dyn ProgressListener>>,
    cancel: Option<Arc<CancellationToken>>,
) -> Result<TrackInfo, FfiError> {
    guarded(|| {
        warn_if_not_canonical(&page_url);
        let options = options.unwrap_or_default();
        let initial_keys = parse_keys(options.initial_keys_json.as_deref())?;
        let cancel = CancellationToken::to_cancel(cancel.as_ref());
        let mut callback = progress_sink(progress);

        let client = Client::api();
        let api_base = options
            .api_base
            .as_deref()
            .unwrap_or(hotaudio::HOTAUDIO_API_BASE);
        let handshake = hotaudio::block_on(listen::load_handshake(
            &client,
            &page_url,
            api_base,
            options.track_id.as_deref(),
        ))?;
        let title = track_title(&handshake);

        let mut file = std::fs::File::create(&path).map_err(io_error)?;
        let result = hotaudio::block_on(download::download_streaming(
            &client,
            &handshake,
            initial_keys.as_ref(),
            options.delay_ms,
            |fragment| {
                file.write_all(fragment)
                    .map_err(|error| CoreError::Io(error.to_string()))
            },
            sink(&mut callback),
            cancel,
        ));
        drop(file);

        match result {
            Ok(streamed) => Ok(info_from_streamed(streamed, title)),
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                Err(error.into())
            }
        }
    })
}

/// Download a track and return the decrypted bytes.
///
/// The whole container and the whole plaintext are held in memory. Use
/// [`download_to_file`] unless the caller really needs the bytes in-process.
///
/// # Errors
///
/// Returns a handshake, listen, transfer, decrypt or cancellation error.
#[uniffi::export]
pub fn download_bytes(
    page_url: String,
    options: Option<DownloadOptions>,
    progress: Option<Arc<dyn ProgressListener>>,
    cancel: Option<Arc<CancellationToken>>,
) -> Result<DownloadedTrack, FfiError> {
    guarded(|| {
        warn_if_not_canonical(&page_url);
        let options = options.unwrap_or_default();
        let initial_keys = parse_keys(options.initial_keys_json.as_deref())?;
        let cancel = CancellationToken::to_cancel(cancel.as_ref());
        let mut callback = progress_sink(progress);

        let api = Client::api();
        let bulk = Client::bulk();
        let clients = Clients {
            api: &api,
            bulk: &bulk,
        };
        let mut core_options = CoreDownloadOptions {
            initial_keys,
            on_progress: sink(&mut callback),
            api_base: options.api_base.as_deref(),
            track_id: options.track_id.as_deref(),
            delay_ms: options.delay_ms,
            cancel,
            ..CoreDownloadOptions::default()
        };
        let downloaded =
            hotaudio::block_on(clients.download_from_page(&page_url, &mut core_options))?;
        let info = info_from_downloaded(&downloaded);
        Ok(DownloadedTrack {
            info,
            audio: downloaded.audio,
        })
    })
}

/// Download a `.hax` container by URL and decrypt it with saved keys.
///
/// No page fetch and no listen calls. This is the second half of the mobile
/// split described on [`track_keys`], and the buffered path means the container
/// and its plaintext are held in memory.
///
/// # Errors
///
/// Returns a parse, transfer, decrypt or cancellation error.
#[uniffi::export]
pub fn download_cached_to_file(
    hax_url: String,
    keys_json: String,
    path: String,
    progress: Option<Arc<dyn ProgressListener>>,
    cancel: Option<Arc<CancellationToken>>,
) -> Result<TrackInfo, FfiError> {
    guarded(|| {
        let saved = download::parse_saved_keys(&keys_json)?;
        let cancel = CancellationToken::to_cancel(cancel.as_ref());
        let mut callback = progress_sink(progress);
        let client = Client::bulk();
        let downloaded = hotaudio::block_on(download::download_cached(
            &client,
            &hax_url,
            &saved.keys,
            sink(&mut callback),
            false,
            cancel,
        ))?;

        if let Err(error) = std::fs::write(&path, &downloaded.audio) {
            let _ = std::fs::remove_file(&path);
            return Err(io_error(error));
        }
        let mut info = info_from_downloaded(&downloaded);
        info.title = saved.title;
        Ok(info)
    })
}

/// Decrypt a `.hax` container already in memory.
///
/// `keys_json` is the envelope produced by [`track_keys`] or a download, or a
/// bare `{ index: hex }` map. Pure and offline.
///
/// # Errors
///
/// Returns a parse or decryption error, including saved keys that do not cover
/// every segment.
#[uniffi::export]
pub fn decrypt_hax(hax_bytes: Vec<u8>, keys_json: String) -> Result<Vec<u8>, FfiError> {
    guarded(|| {
        let saved = download::parse_saved_keys(&keys_json)?;
        let (audio, _, _) = download::decrypt_offline(&hax_bytes, &saved.keys, None)?;
        Ok(audio)
    })
}

/// Decrypt a `.hax` file on disk to an MP4 file, without any network access.
///
/// This is the last step of the platform-downloader split: let the OS download
/// the container, then run this on the file. The output file is removed if the
/// decrypt or write fails.
///
/// # Errors
///
/// Returns a read, parse, decryption or write error.
#[uniffi::export]
pub fn decrypt_hax_to_file(
    input_path: String,
    keys_json: String,
    output_path: String,
    progress: Option<Arc<dyn ProgressListener>>,
) -> Result<TrackInfo, FfiError> {
    guarded(|| {
        let saved = download::parse_saved_keys(&keys_json)?;
        let bytes = std::fs::read(&input_path).map_err(io_error)?;
        let hax = hotaudio::hax::Hax0::parse(&bytes)?;
        let mut callback = progress_sink(progress);
        let (audio, segment_count, duration_secs) =
            download::decrypt_offline(&bytes, &saved.keys, sink(&mut callback))?;

        if let Err(error) = std::fs::write(&output_path, &audio) {
            let _ = std::fs::remove_file(&output_path);
            return Err(io_error(error));
        }
        let keys_json = download::keys_to_json(
            &saved.keys,
            saved.hax_url.as_deref().unwrap_or_default(),
            saved.title.as_deref(),
        );
        Ok(TrackInfo {
            title: saved.title,
            duration_secs,
            duration_ms: hax.duration_ms,
            codec: hax.codec,
            mime: "audio/mp4".to_string(),
            bytes: audio.len() as u64,
            segment_count,
            key_pages: 0,
            hax_url: saved.hax_url.unwrap_or_default(),
            keys_json,
        })
    })
}

/// The `X-Signature` this crate would send for `payload` right now.
///
/// # Errors
///
/// Returns a signer error if the payload cannot be rendered or the VM stops.
#[uniffi::export]
pub fn sign_payload(payload: String) -> Result<String, FfiError> {
    guarded(|| hotaudio::signer::sign_payload(&payload).map_err(sign_error))
}

/// The `X-Signature` for `payload` at a pinned Unix timestamp (seconds,
/// fractional allowed).
///
/// Unlike [`sign_payload`] this does not read the clock, so it reproduces a
/// known vector and is the function binding smoke tests use.
///
/// # Errors
///
/// Returns a signer error if the payload cannot be rendered or the VM stops.
#[uniffi::export]
pub fn sign_payload_at(payload: String, timestamp_secs: f64) -> Result<String, FfiError> {
    guarded(|| hotaudio::signer::sign_payload_at(&payload, timestamp_secs).map_err(sign_error))
}

/// The binding crate's version.
#[must_use]
#[uniffi::export]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

static LOG_LISTENER: RwLock<Option<Arc<dyn LogListener>>> = RwLock::new(None);
static LOGGER_INIT: Once = Once::new();
static LOGGER: Logger = Logger;

struct Logger;

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let listener = LOG_LISTENER
            .read()
            .ok()
            .and_then(|listener| listener.clone());
        if let Some(listener) = listener {
            let _ = listener.on_log(
                record.level().as_str().to_ascii_lowercase(),
                format!("{}", record.args()),
            );
        }
    }

    fn flush(&self) {}
}

/// Install a listener for the crate's warnings, and route them through `log`.
///
/// Call once at startup, before the first download. Calling it again replaces
/// the listener; if another logger was already installed in the process, that
/// logger wins and this listener receives nothing.
///
/// # Errors
///
/// Returns an error if the listener lock is poisoned, which means a previous
/// listener panicked while being called.
#[uniffi::export]
pub fn init_logging(listener: Arc<dyn LogListener>) -> Result<(), FfiError> {
    guarded(|| {
        *LOG_LISTENER.write().map_err(|_| FfiError::Failed {
            code: "logging".to_string(),
            detail: "the log listener lock is poisoned".to_string(),
        })? = Some(listener);
        LOGGER_INIT.call_once(|| {
            if log::set_logger(&LOGGER).is_ok() {
                log::set_max_level(log::LevelFilter::Warn);
            }
        });
        Ok(())
    })
}

/// Initialize the Android TLS verifier with the application `Context`.
///
/// `rustls-platform-verifier` verifies certificates through the JVM's
/// `X509TrustManager`, so it needs the app context once before the first HTTPS
/// request. Call this from `Application.onCreate` (or the first activity)
/// through the Kotlin helper shipped in `bindings/android/HotaudioTls.kt`.
///
/// This is a JNI export, not a `UniFFI` function: it is the one initialization
/// step `UniFFI` cannot generate, and it exists only on Android.
///
/// # Safety
///
/// The JVM calls this with a valid `Env` and `Context`; it must not be called
/// from Rust. It is `extern "system"` with JNI's method signature.
#[cfg(target_os = "android")]
#[unsafe(export_name = "Java_net_hotaudio_extractor_Hotaudio_initAndroidTls")]
pub extern "system" fn init_android_tls(
    mut env: jni::EnvUnowned<'_>,
    _class: jni::objects::JClass<'_>,
    context: jni::objects::JObject<'_>,
) {
    use jni::errors::ThrowRuntimeExAndDefault;
    env.with_env(|env| rustls_platform_verifier::android::init_with_env(env, context))
        .resolve::<ThrowRuntimeExAndDefault>();
}
