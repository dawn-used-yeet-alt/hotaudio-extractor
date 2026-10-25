# Cloudflare challenges: reusing a browser clearance

hotaudio.net is behind Cloudflare. When Cloudflare decides a network looks
automated, it answers with a **challenge page** (`HTTP 403`,
`cf-mitigated: challenge`, `Just a moment...`) instead of the track page or the
listen API.

The extractor cannot answer an interactive challenge. A browser can, and it then
stores a `cf_clearance` cookie. You can hand that cookie to the extractor, but
Cloudflare binds the clearance to **the network that earned it and the exact
User-Agent of the browser**. Both must match, or the cookie is ignored and you
get another challenge.

The CLI prints a link to this guide when it detects a challenge.

## 30-second version

1. Open the track page in your normal browser and pass the challenge.
2. Open DevTools → **Application** → **Cookies** → `https://hotaudio.net` →
   copy the **Value** of `cf_clearance`.
3. In the DevTools **Console**, run `copy(navigator.userAgent)` (or type
   `navigator.userAgent` and copy it).
4. Run the extractor **on the same machine and network**:

   ```bash
   hotaudio-download 'https://hotaudio.net/u/user/track' \
     --cookie 'cf_clearance=PASTE_THE_VALUE' \
     --user-agent 'PASTE_THE_EXACT_USER_AGENT'
   ```

Use single quotes: the User-Agent contains spaces and parentheses, and the
cookie may contain `;`.

## Getting the cookie

`cf_clearance` is `HttpOnly`, so `document.cookie` in the console will **not**
show it. Use the browser's storage inspector or the Network panel.

### Brave / Chrome / Edge

1. Open the track page and wait until it loads normally.
2. Press <kbd>F12</kbd> (or <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>I</kbd>).
3. Open **Application** → **Storage** → **Cookies** →
   `https://hotaudio.net`.
4. Click the `cf_clearance` row. Double-click its **Value** cell and copy the
   whole value.

If the Application panel is hidden, click the `»` overflow menu in DevTools.

### Firefox

1. Open the track page and wait until it loads normally.
2. Press <kbd>F12</kbd>.
3. Open **Storage** → **Cookies** → `https://hotaudio.net`.
4. Copy the **Value** of `cf_clearance`.

### Alternative: copy the request header

1. Open DevTools → **Network**.
2. Reload the track page.
3. Click the first request (the document itself).
4. Under **Request Headers**, copy the `Cookie` value (for this site it is just
   `cf_clearance=...`) and the `User-Agent` value.

That `Cookie` value is exactly what `--cookie` expects. Do not include the word
`Cookie:` itself.

## Getting the exact User-Agent

In the DevTools **Console** on the same tab:

```js
copy(navigator.userAgent)
```

Paste it with <kbd>Ctrl</kbd>+<kbd>V</kbd>. In Firefox, `copy()` also works in
the console; otherwise run `navigator.userAgent` and copy the string.

Do not shorten, reformat or “clean up” the User-Agent. Changing even the browser
version (`Chrome/156.0.0.0` → `Chrome/155.0.0.0`) makes the clearance invalid.

## Passing the pair

### Flags (one run)

```bash
hotaudio-download 'https://hotaudio.net/u/user/track' \
  --cookie 'cf_clearance=...' \
  --user-agent 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36'
```

### Environment (per shell / CI job)

```bash
export HOTAUDIO_COOKIE='cf_clearance=...'
export HOTAUDIO_USER_AGENT='Mozilla/5.0 (...) ...'
hotaudio-download 'https://hotaudio.net/u/user/track'
```

Flags win over the environment, which wins over the config file. Empty
environment variables are treated as unset.

### Config file (persistent, on your machine)

The default location is `~/.config/hotaudio/config.toml` (or
`$XDG_CONFIG_HOME/hotaudio/config.toml`). Add:

```toml
cookie = "cf_clearance=..."
user_agent = "Mozilla/5.0 (...) ..."
```

Both must be in the same file. Command-line flags override them.

## Why it still fails sometimes

- **Different network.** The clearance belongs to the IP that passed the
  challenge. Switching Wi-Fi, VPN, proxy or phone hotspot invalidates it.
- **Different browser.** Each browser profile has its own clearance and its own
  User-Agent. Use the same browser that passed the challenge.
- **Different User-Agent.** An extension that changes the UA, a browser update,
  or a hand-edited string is enough to invalidate it.
- **Expired.** Cloudflare can expire a clearance at any time. Pass the challenge
  again and copy the new value.
- **The site is challenging browsers too.** If the page itself shows
  “Just a moment…” even in a normal browser, the site is under attack or the
  network is flagged. Solve it in the browser first. If it still challenges the
  extractor, the site is blocking non-browser clients; retry later.
- **A different cookie is needed.** Some Cloudflare configurations also set
  `__cf_bm`. If the DevTools `Cookie` header contains more than
  `cf_clearance`, pass the whole header value to `--cookie`.

The extractor does not solve, bypass or farm clearances. It only reuses a
clearance you obtained by passing the challenge yourself.

## Security

`cf_clearance` is a credential for your current session and network. Treat it
like a password:

- do not commit it, paste it into an issue, or share it;
- prefer the environment or a local config file over shell history;
- remove it when you no longer need it;
- generate a fresh one instead of reusing an old one.

## CI

A clearance is bound to the IP that earned it. A cookie copied from a developer
machine will not pass from a GitHub runner with a different egress IP. The
scheduled live suite therefore treats a Cloudflare challenge as an **environment
condition**, not a product failure: it reports it, skips the remaining network
phases and does not file a product issue.

WARP's fresh address is enough for the page, handshake, signer and first-listen
checks. A full download is different: the site starts challenging after roughly
20-115 key pages, and pacing does not move that threshold, so a rotating
GitHub-hosted runner cannot finish one. The suite still tries, and records how
far it got before the challenge.

If a runner has a stable egress IP, set the `HOTAUDIO_COOKIE` and
`HOTAUDIO_USER_AGENT` secrets (or environment variables) from a clearance
obtained on that same egress. A self-hosted runner on a trusted network is the
only way to make the full download a real CI gate. Otherwise leave them unset.
