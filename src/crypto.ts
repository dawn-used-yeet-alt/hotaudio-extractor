import { chacha20poly1305 } from '@noble/ciphers/chacha.js';
import { x25519 } from '@noble/curves/ed25519.js';
import type { HotaudioState } from './types.ts';

/** Decode a lowercase/uppercase hex string into bytes. */
export function hexToBytes(hex: string): Uint8Array {
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = parseInt(hex.substring(i * 2, i * 2 + 2), 16);
  }
  return bytes;
}

/** Encode bytes as lowercase hex. */
export function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('');
}

/** Decode base64 in Node and browser runtimes. */
export function base64ToBytes(b64: string): Uint8Array {
  if (typeof Buffer !== 'undefined' && typeof Buffer.from === 'function') {
    return new Uint8Array(Buffer.from(b64, 'base64'));
  }
  const rawStr = atob(b64);
  const raw = new Uint8Array(rawStr.length);
  for (let i = 0; i < rawStr.length; i++) raw[i] = rawStr.charCodeAt(i);
  return raw;
}

/** SHA-256 via WebCrypto. */
export async function sha256(data: Uint8Array): Promise<Uint8Array> {
  const view = data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength);
  const hash = await globalThis.crypto.subtle.digest('SHA-256', view as ArrayBuffer);
  return new Uint8Array(hash);
}

/** Decrypt the `__ha_state` base64 payload embedded in the track page. */
export function decryptHotaudioState(stateB64: string): HotaudioState {
  const raw = base64ToBytes(stateB64);
  const key32 = raw.subarray(raw.length - 32);
  const ct = raw.subarray(0, raw.length - 32);
  const nonce = new Uint8Array(12);
  const decrypted = chacha20poly1305(key32, nonce).decrypt(ct);
  return JSON.parse(new TextDecoder().decode(decrypted)) as HotaudioState;
}

export interface KeyExchangeResult {
  clientPubHex: string;
  /** SHA-256 of the X25519 shared secret; encrypts listen payloads. */
  Ee: Uint8Array;
}

/** Generate an ephemeral X25519 keypair and derive the session secret. */
export async function performKeyExchange(serverPubHex: string): Promise<KeyExchangeResult> {
  const privKey = x25519.utils.randomSecretKey();
  const pubKey = x25519.getPublicKey(privKey);
  const serverPub = hexToBytes(serverPubHex);
  const sharedSecret = x25519.getSharedSecret(privKey, serverPub);
  const Ee = await sha256(sharedSecret);
  return { clientPubHex: bytesToHex(pubKey), Ee };
}
