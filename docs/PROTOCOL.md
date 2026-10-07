# Protocol reference

Reverse-engineered from the player's behaviour and the pinned `nozzle.js`
build. Everything here is what the server actually validates.

---

## 1. Page state

The track page embeds:

```html
<script>var __ha_state = "<base64>";</script>
```

Decode the base64, then split: the **last 32 bytes are the key**, everything
before is a ChaCha20-Poly1305 ciphertext under a **zero 96-bit nonce**.

```json
{
  "pid": "62464",
  "tick": "<per-load nonce>",
  "key": "<server static X25519 public key, hex>",
  "tracks": { "118346": { "key": "<track key>", "title": "<title>" } },
  "order": [118346, 14574]
}
```

`pid`, `tick` and `tracks[tid].key` must be server-issued — the page fetch
cannot be skipped. Measured: a garbage `pid` yields 400, a garbage `tick` yields
401 (`bad signature`, plaintext body). `tick` stays valid for hours across
sessions, so caching a state is viable; keys are not derived from it.

## 2. Key exchange

Standard X25519. The client generates an ephemeral keypair; the shared secret is
hashed to produce the session key:

```
Ee = SHA-256(X25519(claude_private, server_public))
```

One keypair per extraction is reused for every listen request. Each request
still gets a fresh signature and therefore a fresh nonce, so nonces never repeat
under a fixed `Ee`.

`client_pub` is sent as the hex `X-Key` header.

## 3. Listen request

```
POST /api/v1/audio/listen[?key=<page-url key>]
Content-Type: application/vnd.hotaudio.crypt+json
X-Signature: <signature>
X-Key: <client ephemeral public key, hex>
User-Agent: Mozilla/5.0
Origin: https://hotaudio.net
Referer: https://hotaudio.net/
```

The `?key=` query is the page URL's `key` parameter, forwarded verbatim for
private/unlisted tracks.

### Payload

Field order is significant: the server validates the signature over exactly the
serialised string, so it must match the player's `JSON.stringify` order.

```json
{"tid":"<track id>","pid":"<page pid>","key":"<track key>","tick":"<page tick>","first":<n>}
```

`first = -1` is the initial call. `first = <segmentIndex>` pages key branches for
long tracks.

### Nonces

```
request_nonce  = SHA-256(signature)[0..12]
body           = ChaCha20-Poly1305(Ee, request_nonce, payload)
response_nonce = request_nonce with byte 0 incremented by one
plaintext      = ChaCha20-Poly1305-open(Ee, response_nonce, body)
```

Incrementing byte 0 separates the request and response nonce domains under one
secret.

### Response

```json
{
  "url": "https://cdn.hotaudio.net/a/<track key>.hax",
  "keys": { "<node index>": "<32-byte key, hex>" }
}
```

Success bodies carry `Content-Type: application/vnd.hotaudio.crypt+json` and
are encrypted. A plaintext body is an error to surface, not to decrypt.

Status handling mirrors the player: `401` is retried once then reported as
"bad sig"; `403` is fatal ("refresh page").

### The `.hax` URL

Derivable without a listen call:

```
https://cdn.hotaudio.net/a/<track key>.hax
```

Stable across tracks and sessions. The downloader still uses the server-issued
URL; this is documented as a tripwire — if the two ever disagree, the CDN layout
changed.

## 4. HAX0 container

```
offset 0:  magic "HAX0"
offset 4:  fileLength    u32 LE
offset 8:  headerLength  u32 LE
offset 12: extraLength   u32 LE
offset 16: bencoded metadata dict, through headerLength
```

Metadata keys: `codec`, `durationMs`, `segmentCount`, `segments` (a byte string
of `segmentCount` × `{offset u32 LE, pts u32 LE}`), `baseKey`.

Segment `i` occupies `[segments[i].offset, segments[i+1].offset)`, and the last
segment runs to `fileLength`. Each slice is an **independent** ChaCha20-Poly1305
ciphertext under a **zero nonce**, keyed by the segment's derived key. There is
no chaining between segments.

## 5. Segment key tree

Keys form a complete binary tree above the leaves. Given `segmentCount`:

```
bit_len   = bit_length(segmentCount - 1)          # 0 when segmentCount == 1
depth     = bit_len + 1
tree_base = 1 + (1 << depth)
leaf(i)   = tree_base + i
```

To derive leaf `i`, walk from the root down. `node(level) = leaf(i) >> (depth - level)`.
At each level, starting from the nearest ancestor whose key is already known:

```
child = SHA-256(parent_key || (node & 0xff))      # one appended byte
```

An error means no ancestor of that leaf is present in the key map — that is the
signal to page more branches.

Consecutive segments share most of their path, so memoising intermediate node
keys makes derivation effectively free. **Clear the cache whenever new branch
keys are merged**: a new ancestor changes every key beneath it.

### Branch semantics

Measured against the live API on an 899-segment track:

- `first <= 0` is the initial call: returns the `.hax` URL plus one branch
  (here node `16`, covering ~127 leaves).
- `first = n` (n ≥ 1) returns a small window starting at segment `n`
  (typically 8 leaves), without a URL.
- Out-of-range `first` clamps to the last leaf key.
- Branch keys are deterministic per `(track, first)` and byte-identical across
  sessions with different `tick`s.
- Branch **width** rotates per request (8, 32, up to 128 observed). Width is not
  a function of `first`, so no seeding strategy beats miss-driven paging — which
  already harvests lucky wide branches by merging everything returned.
- Concurrent requests show no latency benefit; the server serialises per track.

Hence the policy in `download.rs`: on a miss, request exactly the missing index.

## 6. The signature VM

This is the unusual part, and the part worth reading carefully.

### What it is not

The `X-Signature` looks like a MAC. It is not a call to any crypto primitive.
Upstream it is produced by evaluating `nozzle.js` — ~106 KB of deliberately
obfuscated JavaScript — inside a fake-browser sandbox. Deobfuscating it shows the
payload is a **register machine**.

### The program

657 instructions, two bytes each, recovered from the bundle expression:

```
n[e(82)] = n[e(87)] || w(e(177)) + w[e(101)](e(97), e(171)) + w(37) + w(e(260)) + w(e(172))
```

The VM normalises the program before running it:

```js
n[3] = [...bc].map(c => B(c.charCodeAt(0), "34", k(-36)))
```

`B` dispatches on a captured flag `re`; with `re === -36` it returns `n[0] - n[1]`,
so each entry becomes `charCode - 34`. `src/signer/bytecode.rs` stores that
already-decoded form.

### The machine

- 80 registers. `reg[0]` is the program counter, `reg[1]` the numeric
  accumulator, `reg[34]` the string register, `reg[35]` the call argument.
- Registers 0..33 start as the number `0`; 34..79 start as empty strings.
- `opcode < 80` is `MOV reg[opcode] = reg[operand]`.
- `opcode >= 80` dispatches. Instructions are fetched two bytes at a time and the
  counter advances by 2.

Instruction set:

| Opcode | Effect |
| --- | --- |
| 80–88 | `+`, `-`, `Math.imul`, `^`, `&`, `\|`, `>>>`, `<<` on the accumulator, with `>>> 0` coercion |
| 89, 90 | `reg[operand]++` / `--` |
| 91 | `acc = operand` |
| 92, 93 | rotate right / left |
| 94, 95 | negate / numeric-coerce the operand register |
| 96 | `acc = (acc / reg[operand]) >>> 0` |
| 97 | `acc = acc << operand` |
| 98–101 | forward / backward branches on truthiness |
| 113 | `reg[34] += reg[operand]` |
| 114 | `acc = reg[operand].length` |
| 115 | `acc = reg[34].charCodeAt(reg[operand])` |
| 118 | `acc = (reg[operand] in reg[34])` → a **boolean** |
| 119 | `reg[34] = reg[34][reg[operand]]` (global, then member lookup) |
| 120 | `reg[34] = globalThis` |
| 122 | `reg[34] = (reg[operand] >>> 0).toString(16).padStart(8, "0")` |
| 123–125 | string reset, `fromCharCode`, and append |
| 126 | build the environment fingerprint object |
| 127 | `new reg[operand]()` — the `Date` constructor |
| 128 | `reg[34].split(reg[operand])` |
| 129 | `String(reg[operand])`, with a native-stub fallback |
| 130 | `+reg[34].includes(reg[operand])` |
| 131 | `+(reg[34] === reg[operand])` |

Several entries above deserve distrust, because each was wrong in a first
reading and each produced a silently incorrect signature. See
[MAINTENANCE.md](MAINTENANCE.md#things-that-are-easy-to-get-wrong).

### The environment probe

`op 126` builds an object from `ENV_HASHES` (144 keys, all values `undefined`).
The program then tests membership with `op 118` and reads a few globals with
`op 119`, building a fingerprint that depends on the *absence* of markers as well
as their presence. The observable surface is small:

| Property | Value |
| --- | --- |
| `Date` | function; `Date.stack` is `undefined`, which throws |
| `MediaSource` | `function MediaSource() { [native code] }` |
| `SourceBuffer` | `function SourceBuffer() { [native code] }` |
| `SourceBuffer.prototype` | `Symbol.toStringTag` = `SourceBuffer` → `[object SourceBuffer]` |
| `appendBuffer` | `function appendBuffer() { [native code] }` |
| `__ha_chunks` | **absent** — its absence is part of the fingerprint |

The `Date.stack` throw is converted by the bundle's `__FAB` hook into a
V8-formatted stack rooted at the pinned player URL, which is folded into the
digest. `src/signer/env.rs::nozzle_stack` reproduces it verbatim.

Verified: the signature is timezone-independent. `Date.toString()` includes a
local timezone, so one might expect it to matter — signing the same payload under
`UTC`, `America/New_York`, `Asia/Kolkata` and `Pacific/Kiritimati` produces
identical output.

### Output

```
"9:" + big_endian_u32(floor(now_seconds)) + 24 lowercase hex chars (12 bytes)
```

for example `9:6553f1006bc31e581c8825bd656a3c75` at `2023-11-14T22:13:20Z`.

Because the 4-byte timestamp is embedded in cleartext, a server can bound
signature freshness, and the 12-byte tag is truncated output rather than a full
digest width.

### Version pinning

The program, the environment table and the build id
(`PINNED_NOZZLE_VERSION = "1J1Db0bF"`) are one contract. When the upstream player
changes, signatures are rejected with 401 and all three must be re-captured —
see [MAINTENANCE.md](MAINTENANCE.md).

## 7. Rate limiting and politeness

Measured: a per-request server-side floor of about 1 s dominates; concurrency
changes nothing. The downloader is sequential by design.