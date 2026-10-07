# Maintenance runbook

Everything needed when the site changes, and when something breaks.

The short version:

```bash
cd rust
bun scripts/verify-live.ts            # which stage fails?
# ... then, if the signer is stale:
bun scripts/recapture.ts               # new bytecode.rs from the new bundle
$EDITOR src/signer/env.rs              # PINNED_NOZZLE_VERSION + ENV_HASHES
$EDITOR scripts/shim.ts                # same two values, for the reference sandbox
bun scripts/verify-live.ts            # the server is the authority
bun scripts/gen-golden.ts             # only once the server accepts
cargo test
```

---

## Diagnosis first

Never guess what broke. Run the probe:

```bash
bun scripts/verify-live.ts                      # 9 stages, ~45s, tiny ranged reads
bun scripts/verify-live.ts --full               # + download and decrypt everything
bun scripts/verify-live.ts --keys keys.json     # offline stages from saved keys
bun scripts/verify-live.ts https://host/u/a/b   # a specific page
```

It prints one line per stage with timing, and the failing stage tells you what
to look at:

| Stage | Fails when | Usual cause |
| --- | --- | --- |
| 1 page fetch | 403 | User-Agent was changed. Must stay `Mozilla/5.0`. |
| 2 state decrypt | `no __ha_state` | Page layout changed. |
| 2 state decrypt | `truncated bencode` / auth error | State key derivation changed. |
| 3 key exchange | anything | Server key format changed. |
| 4 signer | `unexpected signature shape` | VM opcode semantics drifted. See below. |
| 5 listen API | **401 bad signature** | **Stale capture.** Re-run `recapture.ts`. |
| 5 listen API | 403 | User-Agent was changed. |
| 5 listen API | non-crypt body | Response envelope changed. |
| 6 container probe | `not a HAX0 container` | Container magic/header changed. |
| 7 HAX0 parse | bencode errors | Metadata keys changed. |
| 8 key derivation | no key derivable | Tree geometry changed (segment counts). |
| 9 sample decrypt | auth failure | Key derivation wrong, or cipher/zero-nonce changed. |
| 10 full decrypt | paging 401 | Stale capture again. |

The probe is deliberately read-only and cheap: it uses HTTP `Range` to fetch
16 bytes and then the header, not the whole container.

---

## The three pinned artefacts

These move together. Changing one without the others produces a signer that
*looks* fine and is rejected.

| Artefact | Where | What it is |
| --- | --- | --- |
| Bytecode | `rust/src/signer/bytecode.rs` | The 1314-entry program the player VM runs. |
| Environment table | `rust/src/signer/env.rs` → `ENV_HASHES` | Fingerprint property keys, 144 values. |
| Build id | `rust/src/signer/env.rs` → `PINNED_NOZZLE_VERSION`, mirrored in `rust/scripts/shim.ts` | Which player build the other two came from. |

The reference sandbox (`scripts/shim.ts`) exists so that recovery keeps working
after the TypeScript implementation is deleted. It is a **verification tool**,
never part of the shipped crate.

---

## Re-capturing the signer

### 1. Get the new bundle

The probe prints which build it is pinned to. Fetch the current one:

```bash
curl -s "https://hotaudio.net/nozzle.js?v=<new version>" -o new-nozzle.js
```

If you do not know the version, inspect the live player page for the
`nozzle.js?v=` query.

### 2. Recover the program

```bash
bun scripts/recapture.ts new-nozzle.js
```

This evaluates the bundle's own decoder to extract the program, applies the
`charCode - 34` normalisation, sanity-checks the result (even length, in range)
and writes `src/signer/bytecode.rs`. It is reproducible: run it against the
current `vendor/nozzle.js` and you get the committed file byte for byte.

If it reports that it cannot find the bytecode expression, the bundle layout
changed and you must redo the reverse-engineering — see
[docs/PROTOCOL.md](PROTOCOL.md#the-signature-vm) and the recovery notes below.

### 3. Update the environment table

Copy the fingerprint keys from the new build into `ENV_HASHES` in **both**
`src/signer/env.rs` and `scripts/shim.ts`, and set `PINNED_NOZZLE_VERSION` in
both. The VM tests membership of these keys against its environment object
(`op 118`), so a wrong set yields a different digest.

### 4. Let the server decide

```bash
bun scripts/verify-live.ts
```

Stage 5 is the real oracle: the server validates the signature. Only once it
returns 200 may you regenerate the golden vectors.

### 5. Regenerate golden vectors and test

```bash
bun scripts/gen-golden.ts
cargo test
```

`tests/signer_parity.rs` asserts the Rust VM reproduces all vectors exactly.

---

## If the VM opcode semantics changed

The recovered program is a register machine; `src/signer/vm.rs` implements it.
If the new build uses a different instruction set, signatures will be wrong even
though the program was recovered correctly. Symptoms: stage 4 produces a
malformed signature, or stage 5 returns 401 despite a fresh capture.

### Differential tracing

The original port was built by diffing the two implementations instruction by
instruction, and that method is still the fastest way to localise drift.
`scripts/shim.ts` evaluates the real bundle, so you can instrument it:

```ts
// trace-vm.ts — dump every instruction and register
import { rawSigner } from './shim.ts';
// ...
```

Concretely: patch the instruction-fetch site in the bundle to log
`(opcode, operand, registers)`, run the Rust VM with the matching tracer, and
diff the two traces. The first differing step names the opcode to re-derive.
`vm::trace()` in `src/signer/vm.rs` emits the Rust side in that format, and
`Rendered` prints objects as `kind` tags (`<env>`, `<global>`, `<date>`, …) so
the two are directly comparable.

### Things that are easy to get wrong

Recorded here because each one cost real debugging time during the port:

- **`op 122` reads a register, not the literal operand.** It is
  `(reg[operand] >>> 0).toString(16).padStart(8, "0")`, unlike most opcodes.
- **Program bytes are `charCode - 34`,** not the raw character codes. The
  bundle normalises with `B(c.charCodeAt(0), "34", k(-36))`.
- **`op 100`'s guard is a constant `true`.** It looks like
  `I.g[something.charAt(0)] == "h"`, but `I.g` is the literal string `"h"`, so
  the lookup lands on `String.prototype.charAt` and yields `"h"`.
- **`op 130` calls `includes`, not `toString`.** The bundle's second string
  table spells it `includes`; the result is coerced with unary `+`.
- **`op 120` installs the global object,** not `undefined`.
- **`op 118` yields a JS boolean,** not `0`/`1`.
- **`.length` counts UTF-16 code units,** so a non-BMP character is 2. Use
  `encode_utf16().count()`, not `chars().count()`.
- **`op 127` news up `Date`;** its numeric coercion is `valueOf` (epoch
  milliseconds), not its date string.

---

## If the container format changed

`src/hax.rs` holds the parser. The probe's stage 7 will report the first
bencode failure. Things to re-check:

- magic and the three little-endian length fields
- metadata keys (`codec`, `durationMs`, `segmentCount`, `segments`, `baseKey`)
- the segment table layout (`{offset u32, pts u32}` per segment)
- that slices are still independent ChaCha20-Poly1305 under a zero nonce

`tests/hax_roundtrip.rs` builds a container from scratch; extend it to pin the
new layout.

---

## If the key tree changed

`KeyTree::new` in `src/hax.rs` encodes the geometry:

```
bit_len = bits(segmentCount - 1)
treeBase = 1 + (1 << (bit_len + 1))
leaf = treeBase + segmentIndex
child = SHA-256(parent || (nodeIndex & 0xff))
```

`tests/key_tree.rs` checks derivation against real branch keys and would fail
loudly if this drifted. Stage 8 of the probe is the live equivalent.

---

## Testing without the network

`cargo test` is fully offline. It covers:

- `tests/signer_parity.rs` — 98 golden vectors from the reference signer
- `tests/hax_roundtrip.rs` — container round-trip, corruption, wrong keys
- `tests/key_tree.rs` — derivation against real captured branch keys
- unit tests in `src/` — bencode, URL parsing, tree geometry, escaping

Regenerate captured fixtures with `--full` runs of the probe; keep them small
and free of copyrighted audio (key material and headers only).

---

## Housekeeping

- Never edit `src/signer/bytecode.rs` by hand; it is generated.
- `cargo clippy --all-targets` should stay at zero warnings.
- `--user-agent` must remain `Mozilla/5.0`; Cloudflare 403s Chrome UAs.
- Key paging is intentionally sequential: the server serialises per track, so
  concurrency adds load without reducing wall time.