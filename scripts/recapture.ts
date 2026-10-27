#!/usr/bin/env bun
/**
 * Re-capture the signer program from a `nozzle.js` build.
 *
 * Run this when the upstream player changes. It regenerates
 * `src/signer/bytecode.rs`, which MUST be updated together with
 * `src/signer/env.rs` (`PINNED_NOZZLE_VERSION` and `ENV_HASHES`) — the program
 * and the environment fingerprint are two halves of one pinned contract.
 *
 *   bun scripts/recapture.ts                    # from the vendored bundle
 *   bun scripts/recapture.ts path/to/nozzle.js  # from a fresh download
 *
 * Self-contained: needs only `vendor/nozzle.js` (or the bundle you pass) and
 * `scripts/shim.ts`. The TypeScript implementation is not required.
 *
 * The program is the string literal the bundle's VM executes, recovered from
 *
 *   n[e(82)] = n[e(87)] || w(e(177)) + w[e(101)](e(97), e(171))
 *                                  + w(37) + w(e(260)) + w(e(172))
 *
 * The VM then normalises it with `B(c.charCodeAt(0), "34", k(-36))`, which is
 * `charCode - 34`; `PROGRAM` holds that decoded form.
 *
 * After regenerating, follow `docs/MAINTENANCE.md`: update the env table, run
 * the live acceptance suite in `live/`, then `bun scripts/gen-golden.ts`.
 */
import fs from 'node:fs';
import path from 'node:path';
import { BUNDLE_PATH, sign } from './shim.ts';

const root = path.resolve(import.meta.dir, '..');
const outFile = path.join(root, 'src/signer/bytecode.rs');

const args = process.argv.slice(2);
const force = args.includes('--force');
const arg = args.find((a) => !a.startsWith('--'));
const bundleSource = arg ? fs.readFileSync(path.resolve(arg), 'utf8') : fs.readFileSync(BUNDLE_PATH, 'utf8');
const bundleLabel = arg ? path.resolve(arg) : 'vendor/nozzle.js';

// 1. Locate the bytecode expression. The bundle's program register is assigned
//    from a string-concatenation expression inside the signer factory; the
//    minifier renames its locals on every build, so this anchor is the part
//    that has stayed stable.
const LHS = 't[e(-47)]=';
const ANCHOR = LHS + 't[t[e(8)]-e(-15)]||';
const anchorAt = bundleSource.indexOf(ANCHOR);
if (anchorAt < 0) {
  console.error(`Could not find the VM bytecode expression in ${bundleLabel}.`);
  console.error('');
  console.error('The bundle layout changed. Recover the program manually:');
  console.error('  1. Deobfuscate / instrument the bundle until the VM program is visible.');
  console.error('  2. Find the expression feeding the program register.');
  console.error('  3. Update ANCHOR below, and the opcode semantics in src/signer/vm.rs.');
  console.error('  4. See docs/MAINTENANCE.md.');
  process.exit(1);
}
const exprStart = anchorAt + LHS.length;
const exprEnd = bundleSource.indexOf(',typeof', exprStart);
if (exprEnd < 0) {
  console.error('Could not find the end of the bytecode expression.');
  process.exit(1);
}
const expr = bundleSource.slice(exprStart, exprEnd);

// 2. Evaluate the expression inside the bundle's own scope, so the table and
//    string helpers resolve exactly as they do in production. The expression
//    references closure locals, so it cannot be lifted out; instead capture the
//    value at the assignment site while the signer factory is built.
const patched = (
  bundleSource.slice(0, anchorAt) +
  LHS +
  'globalThis.__RECOVERED=String((' +
  expr +
  '))' +
  bundleSource.slice(exprEnd)
).replace('Ot=w(', 'globalThis.__lastDt=Ot=w(');

const patchedFile = path.join(root, '.nozzle.recapture.js');
fs.writeFileSync(patchedFile, patched);
try {
  // Initialise the sandbox first; the evaluation below uses the same globals
  // but reads the bundle passed on the command line, so a fresh capture is
  // self-validating.
  sign('recapture-warmup', 1_700_000_000);
  try {
    (0, eval)(fs.readFileSync(patchedFile, 'utf8'));
  } catch (err) {
    console.error('Bundle evaluation failed before recovery:', String(err));
    process.exit(1);
  }

  const recovered = (globalThis as any).__RECOVERED;
  if (typeof recovered !== 'string') {
    const dt = (globalThis as any).__lastDt;
    if (typeof dt === 'function') dt('recapture-warmup');
  }
  if (typeof (globalThis as any).__RECOVERED !== 'string') {
    console.error('Bytecode expression was not captured.');
    process.exit(1);
  }
  const bc = (globalThis as any).__RECOVERED as string;

  const raw = [...bc].map((c) => c.charCodeAt(0));
  const program = raw.map((c) => c - 34);
  const min = Math.min(...program);
  const max = Math.max(...program);

  if (program.length === 0 || program.length % 2 !== 0 || min < 0 || max > 0xffff) {
    console.error(
      `Implausible program: len=${program.length} (even? ${program.length % 2 === 0}) range=${min}..${max}`,
    );
    process.exit(1);
  }

  // 3. Sanity-check against the committed program. An unchanged bundle must
  //    recover a byte-identical program, so a mismatch here means either the
  //    site genuinely changed or the recovery step is broken. Comparing the
  //    full array (rather than a prefix) means this holds regardless of how
  //    rustfmt has laid the literal out across lines.
  const existing = fs.existsSync(outFile) ? fs.readFileSync(outFile, 'utf8') : null;
  const committed = existing
    ?.match(/pub const PROGRAM: \[u16; PROGRAM_LEN\] = \[([\s\S]*?)\];/)?.[1]
    .match(/\d+/g)
    ?.map(Number);
  const unchanged = committed?.length === program.length && committed.every((v, i) => v === program[i]);

  if (unchanged) {
    console.log(`${outFile} is already up to date with ${bundleLabel}; nothing to write.`);
    console.log('(If you expected a new capture, check that the bundle you passed is current.)');
    process.exit(0);
  }

  if (existing && !force) {
    console.error('Recovered program differs from the committed src/signer/bytecode.rs.');
    if (committed) {
      const firstDiff = committed.findIndex((v, i) => v !== program[i]);
      console.error(`  length: ${committed.length} -> ${program.length}`);
      console.error(`  first difference at index: ${firstDiff < 0 ? '(length only)' : firstDiff}`);
    } else {
      console.error('  could not parse the committed PROGRAM array to diff against');
    }
    console.error('');
    console.error('If the site genuinely changed, re-run with --force to overwrite, then');
    console.error('follow docs/MAINTENANCE.md. Do not use --force to work around a');
    console.error('recovery bug: an unchanged bundle must reproduce the committed file.');
    process.exit(1);
  }

  const lines = [
    '//! VM program recovered from the pinned `nozzle.js` player build.',
    '//!',
    '//! Generated by `scripts/recapture.ts` — do not hand-edit.',
    '//!',
    '//! Recovered from the program string the bundle\'s signer factory builds.',
    '//! The VM normalises it with `charCode - 34` before executing it, so',
    '//! [`PROGRAM`] holds that decoded form — the values the machine sees.',
    '//!',
    `//! Recovered from \`${bundleLabel}\`.`,
    '//!',
    '//! Re-capture together with [`super::env::ENV_HASHES`].',
    '',
    '/// Number of entries. Even: each instruction is `(opcode, operand)`.',
    `pub const PROGRAM_LEN: usize = ${program.length};`,
    '',
    '/// Raw character codes of the program string, before the `- 34` decode.',
    '/// Kept so the recovery step can be re-verified against the bundle.',
    'pub const RAW_CHARCODES: [u16; PROGRAM_LEN] = [',
  ];
  for (let i = 0; i < raw.length; i += 12) lines.push('    ' + raw.slice(i, i + 12).join(', ') + ',');
  lines.push('];', '');
  lines.push('/// Decoded program. Opcodes below 80 are');
  lines.push('/// `MOV dst = reg[op], src = reg[operand]`; 80 and above dispatch.');
  lines.push('pub const PROGRAM: [u16; PROGRAM_LEN] = [');
  for (let i = 0; i < program.length; i += 12) lines.push('    ' + program.slice(i, i + 12).join(', ') + ',');
  lines.push('];', '');

  fs.writeFileSync(outFile, lines.join('\n'));
  // The literal is emitted at a fixed 12-per-line, which is not what rustfmt
  // chooses. Format it here so `cargo fmt --check` stays green and regenerating
  // against an unchanged bundle produces no diff.
  try {
    Bun.spawnSync(['rustfmt', '--edition', '2024', outFile]);
  } catch {
    console.warn('  (rustfmt not found; `cargo fmt --all` will be needed before committing)');
  }
  console.log(`Wrote src/signer/bytecode.rs`);
  console.log(`  ${program.length} entries (${program.length / 2} instructions), range ${min}..${max}`);
  console.log(`  source: ${bundleLabel}`);
  console.log('');
  console.log('Next (see docs/MAINTENANCE.md):');
  console.log('  1. Update PINNED_NOZZLE_VERSION and ENV_HASHES in src/signer/env.rs');
  console.log('     and scripts/shim.ts — they must match the new build.');
  console.log('  2. run live/ (see docs/MAINTENANCE.md)  # the server is the real oracle');
  console.log('  3. bun scripts/gen-golden.ts     # only once the server accepts');
  console.log('  4. cargo test');
} finally {
  fs.rmSync(patchedFile, { force: true });
}