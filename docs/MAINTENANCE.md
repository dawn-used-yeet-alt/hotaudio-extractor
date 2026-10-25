# Maintenance runbook

Everything needed when the site changes, and in the meantime when something
breaks.

The short version:

```bash
# Which component fails? Build the CLI first: cargo build --release --bin hotaudio-download
cargo run --release --manifest-path live/Cargo.toml -- --cli "$PWD/target/release/hotaudio-download"
# ... and then, if the signer is stale:
bun scripts/recapture.ts               # new bytecode.rs from the new bundle
$EDITOR src/signer/env.rs              # PINNED_NOZZLE_VERSION + ENV_HASHES
$EDITOR scripts/shim.ts                # same two values, for the reference sandbox
# The server is the authority:
cargo run --release --manifest-path live/Cargo.toml -- --cli "$PWD/target/release/hotaudio-download"
bun scripts/gen-golden.ts              # only once the server accepts
cargo test
```

## Diagnosis first

Never guess what broke. Run the suite:

```bash
# Full run. --skip-heavy stops after page + handshake, --no-stream skips the
# ranged streaming check, --url https://host/u/a/b targets a specific page.
cargo run --release --manifest-path live/Cargo.toml -- --cli "$PWD/target/release/hotaudio-download"
```

Every check is tagged with the component that owns it, and the failing
component tells you what to look at:

| Component | Fails when | Usual cause |
| --- | --- | --- |
| `environment` | Cloudflare challenge, DNS failure, no route | The network, not the crate. Never files a product issue. |
| `page` | 403 | Cloudflare challenge (`cf-mitigated: challenge`) or a real origin 403. Check the header before changing anything. |
| `page-state` | `no __ha_state` | Page layout changed. |
| `page-state` | `truncated bencode` / auth error | State key derivation changed. |
| `handshake` | anything | Server key format changed. |
| `signer` | `unexpected signature shape` | VM opcode semantics drifted. See below. |
| `signer` | rejected listen despite a fresh capture | Pinned bytecode/env is stale. |
| `listen` | 401 bad signature | Stale capture. Re-run `recapture.ts`. |
| `listen` | 403 | Cloudflare challenge or a real origin rejection; same check as `page`. |
| `listen` | non-crypt body | Response envelope changed. |
| `hax-url` | issued != derived | CDN layout moved. |
| `container` | `not a HAX0 container`, bencode errors | Container magic, header or metadata keys changed. |
| `paging` | `0 key page(s)` or 401 | Tree geometry changed, or a stale capture. |
| `decrypt` / `verify` | auth failure, no `ftyp`, bad MP4 | Key derivation wrong, cipher/nonce changed, or truncated output. |

The full run is a real end-to-end download: it fetches the container, pages
every key, decrypts all of it, then replays the same bytes offline, cached
and streamed. The red component names what moved.

## The three pinned artefacts

These move together. Changing one without the others yields a signer that
works in every test but is rejected by the server.

| Artefact | Where | What it is |
| --- | --- | --- |
| Bytecode | `src/signer/bytecode.rs` | The 1,314-byte program the player VM runs |
| Environment table | `src/signer/env.rs` → `ENV_HASHES` | Fingerprint property keys, 144 values |
| Build id | `src/signer/env.rs` → `PINNED_NOZZLE_VERSION`, mirrored in `scripts/shim.ts` | Which player build the other two came from |

The reference sandbox (`scripts/shim.ts`) evaluates the real bundle, so signer
recovery and verification are self-contained: they do not depend on the
TypeScript implementation and keep working on branches where it is absent. It
is a verification tool, never part of the shipped crate.

## Re-capturing the signer

### 1. Get the new bundle

The suite's `signer` check prints which build it is pinned to. Fetch the
current one:

```bash
curl -s "https://hotaudio.net/nozzle.js?v=<new version>" -o new-nozzle.js
```

If you do not know the version, inspect the live player page for the
`nozzle.js?v=` query parameter.

### 2. Recover the program

```bash
bun scripts/recapture.ts new-nozzle.js
```

This evaluates the bundle's own decoder to extract the program, applies the
`charCode - 34` normalisation, sanity-checks the result (even length, in
range), and writes `src/signer/bytecode.rs` in rustfmt form.

It refuses to overwrite unless the recovered program actually differs, and
reports the first differing index when it does. An unchanged bundle must
reproduce the committed file: a difference against `vendor/nozzle.js` means
the recovery step broke, not that the site moved, so re-run against the new
bundle you actually intend to pin.

When reported "cannot find the bytecode expression", the bundle layout
changed, and you must redo the reverse-engineering: see
[PROTOCOL.md](PROTOCOL.md#6-the-signature-vm) and the recovery notes below.

### 3. Update the environment table

Copy the fingerprint keys from the new build into `ENV_HASHES` in **both**
`src/signer/env.rs` and `scripts/shim.ts`, and set `PINNED_NOZZLE_VERSION` in
both files. The VM tests membership of these keys against its environment
(`op 118`), so a wrong set yields a different digest.

### 4. Let the server decide

```bash
cargo run --release --manifest-path live/Cargo.toml -- --cli "$PWD/target/release/hotaudio-download"
```

The `listen` component is the real oracle: the server validates the
signature. Regenerate the golden vectors only once it passes.

### 5. Regenerate golden vectors and test

```bash
bun scripts/gen-golden.ts
cargo test
```

`tests/signer_parity.rs` asserts the Rust VM reproduces all vectors exactly.

## If the VM opcode semantics changed

`src/signer/vm.rs` implements the register machine the recovered program runs
on. When the new build uses a different instruction set, signatures are wrong
even though the program was recovered correctly. Symptoms: the `signer` check
produces a malformed signature, or `listen` returns 401 despite a fresh
capture.

### Differential tracing

The original port was built by diffing the two implementations instruction by
instruction, and that method is still the fastest way to localise drift.
`scripts/shim.ts` evaluates the real bundle, so it can be instrumented:

```ts
// dump every instruction and register
import { rawSigner } from './shim.ts';
// ...
```

Patch the instruction-fetch site in the bundle to log `(opcode, operand,
registers)`, run the Rust VM with the matching tracer, and diff the two
traces. The first differing step names the opcode to re-derive. `vm::trace()`
in `src/signer/vm.rs` emits the Rust side in that format, and `Rendered`
prints objects as `kind` tags (`<env>`, `<global>`, `<date>`, …), so the two
traces are directly comparable.

### Things easy to get wrong

Recorded here because each cost real debugging time during the port:

- **`op 122` reads a register, not the literal operand.** It is
  `(reg[operand] >>> 0).toString(16).padStart(8, "0")`, unlike most opcodes.
- **Program bytes are `charCode - 34`,** not raw character codes. The bundle
  normalises with `B(c.charCodeAt(0), "34", k(-36))`.
- **`op 100`'s guard is a constant `true`.** It looks like
  `I.g[something.charAt(0)] == "h"`, but `I.g` is the literal string `"h"`, so
  the lookup lands on `String.prototype.charAt` and yields `"h"`.
- **`op 130` calls `includes`, not `toString`.** The bundle's second string
  table spells it `includes`; the result is coerced with unary `+`.
- **`op 120` installs the global object,** not `undefined`.
- **`op 118` yields a JS boolean,** not `0`/`1`.
- **`.length` counts UTF-16 code units.** A non-BMP character counts as 2; use
  `encode_utf16().count()`, not `chars().count()`.
- **`op 127` constructs `Date`;** its numeric coercion is `valueOf` (epoch
  milliseconds), not the date string.

## If the container format changed

`src/hax.rs` holds the parser. The suite's `container` check reports the first
bencode failure. Re-check:

- the magic and the three little-endian length fields
- the metadata keys (`codec`, `durationMs`, `segmentCount`, `segments`, `baseKey`)
- the segment table layout (`{offset u32, pts u32}` per segment)
- that slices are still independent ChaCha20-Poly1305 under a zero nonce

`tests/hax_roundtrip.rs` builds a container from scratch; extend it to pin the
new layout.

## If the key tree changed

`KeyTree::new` in `src/hax.rs` encodes the geometry:

```
bit_len = bits(segmentCount - 1)
treeBase = 1 + (1 << (bit_len + 1))
leaf = treeBase + segmentIndex
child = SHA-256(parent || (nodeIndex & 0xff))
```

`tests/key_tree.rs` checks derivation against real branch keys and fails
loudly when this drifts. The `keys` and `paging` checks are the live
equivalent.

## Testing without the network

`cargo test` is fully offline. It covers:

- `tests/signer_parity.rs` (golden vectors from the reference signer)
- `tests/hax_roundtrip.rs` (container round-trip, corruption, wrong keys)
- `tests/key_tree.rs` (derivation against real captured branch keys)
- `tests/http_resume.rs` (resumable transfer and the `Range`-ignoring fallback)
- unit tests in `src/` (bencode, URL parsing, tree geometry, escaping)

Regenerate captured fixtures with a full live run. Keep them small and free of
copyrighted audio: key material and headers only.

## Housekeeping

- `src/signer/bytecode.rs` is generated. Never hand-edit it; re-run
  `bun scripts/recapture.ts`, which writes it in rustfmt form and refuses to
  overwrite unless the recovered program actually differs.
- `cargo clippy --all-targets` and `RUSTDOCFLAGS="-D warnings" cargo doc
  --no-deps` must both stay clean; CI enforces them.
- `crate::HOTAUDIO_UA` is the default `Mozilla/5.0`. `--user-agent` overrides it
  only to match the browser that earned a `cf_clearance` cookie; Cloudflare
  binds a clearance to the exact User-Agent. The default stays minimal because
  the site's gate is network/reputation based, not a fixed Chrome-UA rule.
- Key paging stays sequential by design: the server serialises per track, so
  concurrency adds load without reducing wall time ([PROTOCOL.md](PROTOCOL.md#7-rate-limiting-and-politeness)).
