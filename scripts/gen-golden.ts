#!/usr/bin/env bun
/**
 * Regenerate `tests/data/signer_golden.json` from the reference sandbox.
 *
 *   bun scripts/gen-golden.ts
 *
 * Run this only AFTER `bun scripts/verify-live.ts` confirms the server accepts
 * the new capture — the server, not this script, is the authority. Golden
 * vectors exist to pin the Rust VM against the reference JavaScript between
 * then and the next site change.
 *
 * Self-contained: needs only `vendor/nozzle.js` and `scripts/shim.ts`.
 */
import fs from 'node:fs';
import path from 'node:path';
import { sign } from './shim.ts';

const root = path.resolve(import.meta.dir, '..');

/**
 * Payload corpus. Chosen to cover what the VM is sensitive to:
 * realistic listen payloads, empty/short input, SHA-256 block-padding
 * boundaries, non-ASCII (including astral, where JS `.length` counts UTF-16
 * code units rather than scalar values), and JSON escaping.
 */
const payloads: string[] = [
  '{"tid":"7","pid":"p1","key":"k1","tick":"t1","first":-1}',
  '{"tid":"7","pid":"p1","key":"k1","tick":"t1","first":0}',
  '{"tid":"7","pid":"p1","key":"k1","tick":"t1","first":8}',
  '{"tid":"123456","pid":"abc","key":"def","tick":"ghi","first":899}',
  '{"tid":"7","pid":"p","key":"k","tick":"t","first":-1}',
  '{"quoted":"a\\"b","back\\\\slash":"x"}',
  '',
  'a',
  'hello world',
  ...[1, 2, 3, 4, 7, 8, 15, 16, 17, 31, 32, 33, 55, 56, 57, 63, 64, 65, 127, 128, 255, 256].map(
    (n) => 'x'.repeat(n),
  ),
  'ÿþ',
  'Āā',
  '🦀',
  '{"unicode":"🦀🦀"}',
];

/** Timestamps: around zero, the 31-bit boundary, and the 32-bit wrap. */
const timestamps = [0, 1, 1_700_000_000, 2_147_483_647, 2_147_483_648, 4_294_967_295, 4_294_967_296];

const rows: string[] = [];
for (const ts of timestamps) {
  for (const payload of payloads) {
    rows.push(JSON.stringify([payload, ts, sign(payload, ts)]));
  }
}

const out = path.join(root, 'tests/data/signer_golden.json');
fs.mkdirSync(path.dirname(out), { recursive: true });
fs.writeFileSync(out, `[\n${rows.join(',\n')}\n]\n`);

console.log(`Wrote ${rows.length} golden vectors (${payloads.length} payloads x ${timestamps.length} timestamps)`);
console.log('Run: cargo test --test signer_parity');