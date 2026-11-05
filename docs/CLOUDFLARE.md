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
`scripts/install.sh`. It waits up to five minutes by default; use
`--webview-timeout SECS` to change that, and `HOTAUDIO_WEBVIEW` to point at a
helper installed somewhere else. On a headless machine, pass the values by hand
as below instead.

A missing helper, webview toolkit or display is not fatal: the downloader warns
and continues with any clearance you supplied, then falls back to the normal
page re-handshake. The helper itself is a separate binary, so `hotaudio-download`
never needs GTK/WebKit to start.

A run also re-handshakes by itself when key paging hits the site's per-tick
key-window limit: the server answers with a challenge (or stalls the request),
the downloader fetches a fresh page tick and resumes at the segment that
failed. With `--webview`, that refresh goes through the helper so a stale
`cf_clearance` is renewed too.

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

## Stopped working?

Clearances are short lived and tied to your IP and browser. It breaks if you:

- switch networks (Wi-Fi, VPN, proxy, hotspot)
- use a different browser profile
- update your browser
- wait too long

Just pass the challenge again and copy fresh values. If the browser's `Cookie` header has more than `cf_clearance` (like `__cf_bm`), pass the whole header value to `--cookie`.
