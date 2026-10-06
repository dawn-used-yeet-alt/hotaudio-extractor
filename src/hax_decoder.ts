import { chacha20poly1305 } from '@noble/ciphers/chacha.js';
import { sha256 } from './crypto.ts';
import type { Hax0Container, Hax0Segment } from './types.ts';

/** Minimal bencode decoder for HAX0 metadata dictionaries. */
export function decodeBencode(
  buf: Uint8Array,
  offset: number,
): { value: unknown; nextOffset: number } {
  const byte = buf[offset];

  if (byte === 0x69) {
    let end = offset + 1;
    while (buf[end] !== 0x65 && end < buf.length) end++;
    const str = new TextDecoder().decode(buf.subarray(offset + 1, end));
    return { value: parseInt(str, 10), nextOffset: end + 1 };
  }

  if (byte === 0x64) {
    let curr = offset + 1;
    const dict: Record<string, unknown> = {};
    while (buf[curr] !== 0x65 && curr < buf.length) {
      const keyDec = decodeBencode(buf, curr);
      const keyStr = new TextDecoder().decode(keyDec.value as Uint8Array);
      curr = keyDec.nextOffset;
      const valDec = decodeBencode(buf, curr);
      dict[keyStr] = valDec.value;
      curr = valDec.nextOffset;
    }
    return { value: dict, nextOffset: curr + 1 };
  }

  let colon = offset;
  while (colon < buf.length && buf[colon] >= 0x30 && buf[colon] <= 0x39) colon++;
  if (buf[colon] === 0x3a) {
    const lenStr = new TextDecoder().decode(buf.subarray(offset, colon));
    const len = parseInt(lenStr, 10);
    const start = colon + 1;
    const data = buf.subarray(start, start + len);
    return { value: data, nextOffset: start + len };
  }

  throw new Error(`Unsupported bencode token at offset ${offset}: ${byte}`);
}

interface Hax0Meta {
  codec: string | Uint8Array;
  durationMs: number;
  segmentCount: number;
  segments: Uint8Array;
  baseKey: Uint8Array;
}

/** Parse the HAX0 container header and segment table. */
export function parseHax0Header(buffer: Uint8Array): Hax0Container {
  const view = new DataView(buffer.buffer, buffer.byteOffset, buffer.byteLength);
  const magic = new TextDecoder().decode(buffer.subarray(0, 4));
  if (magic !== 'HAX0') throw new Error(`Invalid HAX0 magic: ${magic}`);

  const fileLength = view.getUint32(4, true);
  const headerLength = view.getUint32(8, true);
  const extraLength = view.getUint32(12, true);

  const meta = decodeBencode(buffer.subarray(16, headerLength), 0).value as Hax0Meta;
  const codec = typeof meta.codec === 'string' ? meta.codec : new TextDecoder().decode(meta.codec);
  const rawSegments: Uint8Array = meta.segments;
  const segView = new DataView(
    rawSegments.buffer,
    rawSegments.byteOffset,
    rawSegments.byteLength,
  );
  const segments: Hax0Segment[] = [];
  for (let i = 0; i < meta.segmentCount; i++) {
    segments.push({
      offset: segView.getUint32(i * 8, true),
      pts: segView.getUint32(i * 8 + 4, true),
    });
  }

  return {
    fileLength,
    headerLength,
    extraLength,
    baseKey: meta.baseKey,
    codec,
    durationMs: meta.durationMs,
    segmentCount: meta.segmentCount,
    segments,
  };
}

/**
 * Derive the decryption key for one segment.
 *
 * Walks from the nearest known ancestor in `keysMap` down to the leaf,
 * hashing at each tree level. `cache` memoizes intermediate node keys
 * within a single extraction; it must be cleared whenever new branch keys
 * are merged into `keysMap`.
 */
export async function deriveSegmentKey(
  keysMap: Record<number, Uint8Array>,
  segmentCount: number,
  segIdx: number,
  cache?: Map<number, Uint8Array>,
): Promise<Uint8Array> {
  const bitLen = (segmentCount - 1).toString(2).length;
  const treeBase = 1 + (1 << (bitLen + 1));
  const e = treeBase + segIdx;
  const t = e.toString(2).length - 1;

  let startLevel = -1;
  let currKey: Uint8Array | null = null;
  for (let a = 0; a <= t; a++) {
    const ancestorIdx = e >> (t - a);
    if (keysMap[ancestorIdx]) {
      startLevel = a;
      currKey = keysMap[ancestorIdx];
      break;
    }
  }
  if (!currKey || startLevel === -1) {
    throw new Error(`Key missing in keys map for segment index ${segIdx}`);
  }

  for (let a = startLevel + 1; a <= t; a++) {
    const nodeIdx = e >> (t - a);
    const hit = cache?.get(nodeIdx);
    if (hit) {
      currKey = hit;
      continue;
    }
    const branchByte = new Uint8Array([(e >> (t - a)) & 0xff]);
    const merged = new Uint8Array(currKey.length + 1);
    merged.set(currKey, 0);
    merged.set(branchByte, currKey.length);
    currKey = await sha256(merged);
    cache?.set(nodeIdx, currKey);
  }

  return currKey;
}

/** Decrypt one HAX0 segment slice with its derived key (zero nonce). */
export function decryptSegmentSlice(haxSlice: Uint8Array, key: Uint8Array): Uint8Array {
  return chacha20poly1305(key, new Uint8Array(12)).decrypt(haxSlice);
}
