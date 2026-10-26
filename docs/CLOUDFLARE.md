# Cloudflare 

Hotaudio is behind Cloudflare. 
So you first have to clear it in your browser and pass the cookie and UA to the cli. 

## Fix

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
Powershell
```pwsh
hotaudio-download 'https://hotaudio.net/u/user/track' `
  --cookie 'cf_clearance=PASTE_VALUE' `
  --user-agent 'PASTE_USER_AGENT'
```
Cmd
```cmd
hotaudio-download "https://hotaudio.net/u/user/track" ^
  --cookie "cf_clearance=PASTE_VALUE" ^
  --user-agent "PASTE_USER_AGENT"
```

Tip: you can also set `HOTAUDIO_COOKIE` and `HOTAUDIO_USER_AGENT` in your shell, or put `cookie` and `user_agent` in `~/.config/hotaudio/config.toml`.

## Stopped working?

Clearances are short lived and tied to your IP and browser. It breaks if you:

- switch networks (Wi-Fi, VPN, proxy, hotspot)
- use a different browser profile
- update your browser
- wait too long

Just pass the challenge again and copy fresh values. If the browser's `Cookie` header has more than `cf_clearance` (like `__cf_bm`), pass the whole header value to `--cookie`.
