/** Shared hotaudio constants. */

/**
 * Default User-Agent for page and API requests.
 *
 * Cloudflare returns HTTP 403 (`cf-mitigated: challenge`) for Chrome
 * user-agents on the track page and the listen endpoint. The bare
 * `Mozilla/5.0` token is accepted.
 */
export const HOTAUDIO_UA = 'Mozilla/5.0';

/** Base URL for the encrypted listen handshake. */
export const HOTAUDIO_API_BASE = 'https://hotaudio.net';

/** Time the stream-first path waits for the first segment before falling back to full download. */
export const HOTAUDIO_STREAM_READY_TIMEOUT_MS = 20000;
