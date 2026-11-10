# Cloudflare

Hotaudio is behind Cloudflare.
So you first have to clear it in your browser and pass the cookie and UA to the CLI.

## Fix

The simplest way is the bundled helper. It opens the track page in a real
browser window and, once you have passed the challenge, hands the clearance
back to the downloader:

```bash
hotaudio-download --webview 'https://hotaudio.net/u/user/track'
```

The helper is installed next to `hotaudio-download` by the release archives and
`scripts/install.sh`. It waits indefinitely by default (close the window to finish or cancel);
use `--webview-timeout SECS` to bound it, and `HOTAUDIO_WEBVIEW` to point at a
helper installed somewhere else. On a headless machine, pass the values by hand
as below instead. On dual-stack networks, the helper also detects whether IPv4 or IPv6
was used to clear the challenge and instructs the downloader to match it, avoiding IP
mismatch re-challenges.

A missing helper, webview toolkit or display is not fatal: the downloader warns
and continues with any clearance you supplied, then falls back to the normal
page re-handshake. The helper itself is a separate binary, so `hotaudio-download`
never needs GTK/WebKit to start.

You do not have to pass `--webview` every time. Without it, the downloader
probes the track page once and opens the helper only if Cloudflare actually
challenges the request. The cookie, User-Agent and detected IP family are then
cached, so later runs reuse them silently until the site stops trusting them.
Pass `--no-webview` to fail with the challenge instead of opening a window:

```bash
hotaudio-download 'https://hotaudio.net/u/user/track'               # cache, else helper on challenge
hotaudio-download --no-webview 'https://hotaudio.net/u/user/track'  # cache only
```

The cache is `~/.cache/hotaudio/clearance.json` on Linux (and the platform cache
directory elsewhere); set `HOTAUDIO_CLEARANCE` to move it. It is written with
owner-only permissions on Unix. Deleting it only costs one extra browser pass.

The helper finishes on its own once the track page exposes `__ha_state`. Closing
the window also finishes: it prints the browser's cookie and User-Agent, and if
the page state was not captured the downloader re-fetches the page with that
fresh clearance.

The helper keeps its browser profile in the platform data directory
(`~/.local/share/hotaudio-webview` on Linux, `%LOCALAPPDATA%` on Windows,
`Application Support` on macOS), so a passed challenge can be reused on the next
run. It drops the page cache before every run: a cached track page would
otherwise hide a new challenge and leave the helper with no clearance to
report.

A run also re-handshakes by itself when key paging hits the site's per-tick
key-window limit: the server answers with a challenge (or stalls the request),
the downloader fetches a fresh page tick and resumes at the segment that
failed. The existing clearance usually covers that page fetch, so no window
opens; if Cloudflare challenges the page too, the helper runs again (unless
`--no-webview` is set).

## By hand

1. Open the track page in your browser and pass the challenge.
2. Open DevTools (F12) → **Application** → **Cookies** → `https://hotaudio.net` (Firefox: **Storage** → **Cookies**) and copy the `cf_clearance` value.
3. In the **Console**, run `copy(navigator.userAgent)`.
4. Run the downloader on the same machine and network:

### Linux
Bash
```bash
hotaudio-download 'https://hotaudio.net/u/user/track' \
  --cookie 'cf_clearance=PASTE_VALUE' \
  --user-agent 'PASTE_USER_AGENT'
```

### Windows
PowerShell
```pwsh
hotaudio-download 'https://hotaudio.net/u/user/track' `
  --cookie 'cf_clearance=PASTE_VALUE' `
  --user-agent 'PASTE_USER_AGENT'
```
Cmd
```bat
hotaudio-download "https://hotaudio.net/u/user/track" ^
  --cookie "cf_clearance=PASTE_VALUE" ^
  --user-agent "PASTE_USER_AGENT"
```

Tip: you can also set `HOTAUDIO_COOKIE` and `HOTAUDIO_USER_AGENT` in your shell, or put `cookie` and `user_agent` in `~/.config/hotaudio/config.toml`.

## Android
1. Install Firefox.
2. Add the [Cookie Manager](https://addons.mozilla.org/en-US/firefox/addon/a-cookie-manager/) extension by [Rob W](https://github.com/Rob--W/cookie-manager).
3. Go to hotaudio and clear the captcha and keep the tab open.
4. Click the extension and in the extension's tab, search hotaudio.net.
5. That's your cookie.
6. Go to [what-is-my-user-agent](https://www.whatismybrowser.com/detect/what-is-my-user-agent/) and copy that.
7. Copy the command from Linux part.

## Bindings (Android, iOS, Python, Ruby)

The UniFFI bindings expose `set_clearance(cookie, user_agent)` and
`clear_clearance()` in process memory. Mobile apps can capture the cookie from an
in-app WebView (e.g. Android's `CookieManager` + `WebSettings.userAgentString`)
and pass them directly without shell environment variables. See `docs/BINDINGS.md`.

## Stopped working?

Clearances are short lived and tied to your IP and browser. It breaks if you:

- switch networks (Wi-Fi, VPN, proxy, hotspot)
- use a different browser profile
- update your browser
- wait too long

Just pass the challenge again and copy fresh values. If the browser's `Cookie` header has more than `cf_clearance` (like `__cf_bm`), pass the whole header value to `--cookie`.
