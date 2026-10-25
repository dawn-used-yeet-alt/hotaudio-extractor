# Cloudflare challenges

hotaudio.net sits behind Cloudflare. If your network looks automated, Cloudflare
may send a challenge page instead of the track or the listen response. The usual
signs are HTTP 403, a `cf-mitigated: challenge` header, and a body that says
"Just a moment...". A 200 response with that header is a challenge too.

The downloader cannot solve the challenge. Your browser can. Once you pass it,
Cloudflare stores a `cf_clearance` cookie. You can give that cookie to the
downloader, but it only works with the exact User-Agent of the browser that
earned it, from the same network. Cloudflare ties the clearance to the IP
address and the UA.

When the CLI sees a challenge, it exits with status 3 and prints a link to this
page.

## Quick version

1. Open the track page in your normal browser and pass the challenge.
2. Open DevTools. Go to **Application** > **Storage** > **Cookies**, select
   `https://hotaudio.net`, and copy the value of `cf_clearance`.
3. In the **Console**, run `copy(navigator.userAgent)`.
4. Run the downloader on the same machine and network:

   ```bash
   hotaudio-download 'https://hotaudio.net/u/user/track' \
     --cookie 'cf_clearance=PASTE_THE_VALUE' \
     --user-agent 'PASTE_THE_EXACT_USER_AGENT'
   ```

Keep the single quotes. The User-Agent has spaces and parentheses, and the
cookie can contain `;`.

## Copy the cookie

`cf_clearance` is `HttpOnly`, so typing `document.cookie` in the console will
not show it. Use the storage inspector or the Network panel.

### Brave, Chrome, Edge

1. Open the track page and wait for it to load normally.
2. Press <kbd>F12</kbd>, or <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>I</kbd>.
3. Open **Application** > **Storage** > **Cookies** >
   `https://hotaudio.net`.
4. Click the `cf_clearance` row, then double-click the **Value** cell and copy
   the whole value.

If the Application tab is hidden, open the `»` overflow menu in DevTools.

### Firefox

1. Open the track page and wait for it to load normally.
2. Press <kbd>F12</kbd>.
3. Open **Storage** > **Cookies** > `https://hotaudio.net`.
4. Copy the **Value** of `cf_clearance`.

### Network panel instead

1. Open DevTools and switch to **Network**.
2. Reload the track page.
3. Click the first request, which is the page itself.
4. Under **Request Headers**, copy the `Cookie` value and the `User-Agent`
   value.

The `Cookie` value is what `--cookie` expects. Do not include the word
`Cookie:`.

## Copy the User-Agent

Run this in the **Console** on the same tab:

```js
copy(navigator.userAgent)
```

Then paste it with <kbd>Ctrl</kbd>+<kbd>V</kbd>. If `copy()` is not available,
run `navigator.userAgent` and copy the string by hand.

Do not shorten or reformat it. Changing the browser version from
`Chrome/156.0.0.0` to `Chrome/155.0.0.0` is enough to break the clearance.

## Use both values

### One run

```bash
hotaudio-download 'https://hotaudio.net/u/user/track' \
  --cookie 'cf_clearance=...' \
  --user-agent 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36'
```

### Shell or CI job

```bash
export HOTAUDIO_COOKIE='cf_clearance=...'
export HOTAUDIO_USER_AGENT='Mozilla/5.0 (...) ...'
hotaudio-download 'https://hotaudio.net/u/user/track'
```

Flags win over the environment, and the environment wins over the config file.
An empty environment variable counts as unset.

### Config file

The default path is `~/.config/hotaudio/config.toml`, or
`$XDG_CONFIG_HOME/hotaudio/config.toml`. Add both lines:

```toml
cookie = "cf_clearance=..."
user_agent = "Mozilla/5.0 (...) ..."
```

Command-line flags still override the file.

## Why it stops working

A clearance is short lived. It usually stops working for one of these reasons:

- You changed networks. The clearance belongs to the IP that passed the
  challenge, so switching Wi-Fi, VPN, proxy, or phone hotspot invalidates it.
- You used a different browser profile. Each profile has its own clearance and
  its own User-Agent.
- The browser updated, or an extension changed the UA.
- Cloudflare expired the clearance. Pass the challenge again and copy the new
  value.
- The page still shows "Just a moment..." in your browser. In that case the
  network is being challenged before the downloader is involved. Solve it in
  the browser first, then try again later if the downloader is still blocked.
- The request needs more than `cf_clearance`. Some Cloudflare setups also set
  `__cf_bm`. If the browser's `Cookie` header has more than `cf_clearance`,
  pass the whole header value to `--cookie`.

The downloader does not solve, bypass, or farm clearances. It only reuses one
that you obtained yourself.

## Keep it private

`cf_clearance` is a credential for your current session and network. Treat it
like a password:

- Do not commit it, paste it into an issue, or share it.
- Prefer the environment or a local config file over shell history.
- Delete it when you are done.
- Treat a copied `Cookie` header the same way. It may hold other session
  cookies besides `cf_clearance`.

## CI and the live suite

A clearance is tied to the IP that earned it, so a cookie from your machine will
not work on a GitHub runner with a different egress IP. The scheduled live suite
treats a challenge as an environment condition. It reports the challenge, skips
the rest of the network tests, and does not file a product issue.

A fresh WARP address is enough for the page, handshake, signer, and first listen
checks. A full download is different. In testing, Cloudflare started
challenging after roughly 20 to 115 key pages, and `--delay` values of 0, 500,
and 1000 ms all hit the challenge. Pacing does not move that threshold, so a
GitHub-hosted runner with a rotating address cannot finish a full download. The
suite still tries, and records how far it got.

If you need the full download as a CI gate, run the live suite on a self-hosted
runner with a stable egress IP, pass the challenge in a browser on that runner's
network, and set `HOTAUDIO_COOKIE` and `HOTAUDIO_USER_AGENT` from that browser.
Otherwise, leave the two values unset and let the suite skip the blocked parts.
