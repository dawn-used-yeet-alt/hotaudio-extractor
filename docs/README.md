# Documentation index

This book has three parts, by reader:

1. **Use it**: [INSTALL.md](INSTALL.md) · [CLI.md](CLI.md) · [TROUBLESHOOTING.md](TROUBLESHOOTING.md)
2. **Build on it**: [LIBRARY.md](LIBRARY.md) (Rust) · [BROWSER.md](BROWSER.md) (wasm)
3. **Keep it alive**: [ARCHITECTURE.md](ARCHITECTURE.md) · [PROTOCOL.md](PROTOCOL.md) · [MAINTENANCE.md](MAINTENANCE.md)

Plus one frozen page: [MIGRATION.md](MIGRATION.md), the one-time port away
from the TypeScript implementation, kept for historical reference and no
longer updated.

## Where do I start?

| You want to | Read |
| --- | --- |
| Install the downloader | [INSTALL.md](INSTALL.md) |
| Learn the CLI flags | [CLI.md](CLI.md) |
| Save keys and redownload offline | [CLI.md](CLI.md#keys-and-working-offline) |
| Fix a failing install | [INSTALL.md](INSTALL.md#after-unpacking) |
| Fix a failing download | [TROUBLESHOOTING.md](TROUBLESHOOTING.md) |
| Call the protocol from Rust | [LIBRARY.md](LIBRARY.md) |
| Stream audio in a web page | [BROWSER.md](BROWSER.md) |
| Understand a design choice | [ARCHITECTURE.md](ARCHITECTURE.md) |
| Read or implement the wire protocol | [PROTOCOL.md](PROTOCOL.md) |
| Fix a break after a site change | [MAINTENANCE.md](MAINTENANCE.md) |
| Port code from the old TypeScript API | [MIGRATION.md](MIGRATION.md) |

## What lives where, and why

Each page owns exactly one subject and defers to the page that owns the facts.
The split:

- **INSTALL.md** covers getting the binary running, from download to first
  invocation. Its download block is generated in CI from
  `scripts/release-targets.tsv`; the prose around it is hand-written. Install
  problems (wrong architecture, Gatekeeper, `PATH`) live there too, not in
  TROUBLESHOOTING.
- **CLI.md** is the command-line tour: sources, output, keys, network, config,
  exit codes. Supported by `--help` for the full flag list.
- **TROUBLESHOOTING.md** is about download failure: what is transient, what
  means the site changed, what exit codes mean for scripts. It links out
  rather than repeating.
- **LIBRARY.md** is the Rust API quickstart: options, streaming, offline
  decryption, cancellation, errors. It points at rustdoc for the details.
- **BROWSER.md** is the wasm build: packaging, MediaSource playback, and the
  same-origin proxy a page needs (the crate ships none, on purpose).
- **ARCHITECTURE.md** explains the design and carries the measurements behind
  the non-obvious decisions, including the deliberate non-goals.
- **PROTOCOL.md** is the wire protocol reference: everything the server
  actually validates, measured against the live site and the pinned player.
- **MAINTENANCE.md** is the runbook for when the site moves: diagnosis by live
  suite component, recapturing the signer, and what is easy to get wrong.
- **MIGRATION.md** is frozen. Do not extend it; new content belongs in the
  pages above.

Questions of ownership land here: if a fact appears in two pages, one of them
is wrong to carry it.
