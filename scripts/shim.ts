/**
 * The reference browser sandbox for `vendor/nozzle.js`.
 *
 * This is a port of the `signHOTAudioPayload` sandbox from the TypeScript
 * implementation (the `legacy` branch, `src/signer.ts`). It exists so that
 * signer recovery and verification are self-contained: the *authoritative*
 * behaviour is still "whatever this JavaScript does under a faithful sandbox",
 * because that is what the server validates.
 *
 * It is a verification tool, not part of the shipped Rust crate. The crate
 * implements the same machine natively (see `src/signer/`).
 *
 * Usage:
 *   import { sign, PINNED_NOZZLE_VERSION } from './shim.ts';
 *   const sig = sign('{"tid":"7"}', 1700000000);
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));

/** Player build the vendored bundle was captured from. Keep in sync with `src/signer/env.rs`. */
export const PINNED_NOZZLE_VERSION = 'wGUeBtfc';

/**
 * Environment fingerprint keys the bundle expects. Keep in sync with
 * `ENV_HASHES` in `src/signer/env.rs` — the two are one pinned contract.
 * Only keys that are present in the reference browser are listed; a missing
 * key is what makes the program's `in` probe false.
 */
export const ENV_HASHES: number[] = [2878, 21112, 33069];

/** Path to the vendored bundle. */
export const BUNDLE_PATH = path.join(here, '..', 'vendor', 'nozzle.js');

let cached: ((payload: string, tsSeconds?: number | null) => string) | null = null;
let frozen: number | null = null;

const NOZZLE_URL = `https://hotaudio.net/nozzle.js?v=${PINNED_NOZZLE_VERSION}`;

/**
 * Adapt the shipped bundle to the sandbox.
 *
 * The signer is a closure local in current builds, so expose it as
 * `__lastDt` (older builds did this themselves). Current builds also dropped
 * the `__ENVHASHES` hook: re-install it at the environment-probe site so the
 * pinned key set is used instead of this process's global set, which cannot
 * match a real browser.
 */
function patchBundle(source: string): string {
  let patched = source;
  if (!patched.includes('globalThis.__lastDt=Dt=')) {
    patched = patched.replace('Ot=w(', 'globalThis.__lastDt=Ot=w(');
    if (patched === source) throw new Error('could not expose the signer');
  }

  const envSite = 'for(O of t[e(53)])t[e(-34)][e(23)][O[t[e(8)]-e(9)]]=O[e(-63)]';
  if (patched.includes(envSite)) {
    patched = patched.replace(
      envSite,
      envSite +
        ';if(globalThis.__ENVHASHES){t[e(-34)][e(23)]={};' +
        'for(const __h of globalThis.__ENVHASHES)t[e(-34)][e(23)][__h]=void 0;}',
    );
  }
  return patched;
}

/**
 * Build the sandbox and return the bundle's exposed signing function.
 *
 * The bundle inspects `MediaSource`, `navigator`, `performance`, `Date` and
 * error-stack formats; the stubs below reproduce the environment it was built
 * against. Result is cached per process.
 */
function setup(): (payload: string, tsSeconds?: number | null) => string {
  if (cached) return cached;

  const g: any = globalThis;
  const NOZZLE_RAW = patchBundle(fs.readFileSync(BUNDLE_PATH, 'utf8'));

  const STUB_STRINGS = new Map<any, string>();
  const nativeFn = (name: string) => {
    const f = function () {};
    Object.defineProperty(f, 'name', { value: name, configurable: true });
    STUB_STRINGS.set(f, 'function ' + name + '() { [native code] }');
    return f;
  };

  class MediaSource {}
  class SourceBuffer {}
  for (const n of [
    'isTypeSupported',
    'addSourceBuffer',
    'removeSourceBuffer',
    'endOfStream',
    'setLiveSeekableRange',
    'clearLiveSeekableRange',
  ]) {
    (MediaSource as any)[n] = nativeFn(n);
  }
  for (const n of ['appendBuffer', 'abort', 'remove', 'appendStream']) {
    (SourceBuffer.prototype as any)[n] = nativeFn(n);
  }
  for (const n of [
    'sourceBuffers',
    'activeSourceBuffers',
    'onsourceopen',
    'onsourceended',
    'onsourceclose',
  ]) {
    try {
      Object.defineProperty(MediaSource.prototype, n, { get() { return []; }, configurable: true });
    } catch {}
  }
  try {
    Object.defineProperty(MediaSource.prototype, 'duration', {
      get() { return 0; },
      configurable: true,
    });
  } catch {}
  for (const n of ['updateend', 'updatestart', 'update', 'error', 'abort']) {
    try {
      Object.defineProperty(SourceBuffer.prototype, n, {
        get() { return null; },
        configurable: true,
      });
    } catch {}
  }
  STUB_STRINGS.set(MediaSource, 'function MediaSource() { [native code] }');
  STUB_STRINGS.set(SourceBuffer, 'function SourceBuffer() { [native code] }');
  try {
    Object.defineProperty(MediaSource.prototype, Symbol.toStringTag, {
      value: 'MediaSource',
      configurable: true,
    });
    Object.defineProperty(SourceBuffer.prototype, Symbol.toStringTag, {
      value: 'SourceBuffer',
      configurable: true,
    });
  } catch {}

  const navObj = { vendor: 'Google Inc.' };
  try {
    Object.defineProperty(navObj, Symbol.toStringTag, { value: 'Navigator', configurable: true });
  } catch {}
  const perfObj = {
    now: () => 123456.789,
    get timeOrigin() {
      return frozen !== null ? frozen * 1000 : 1787330000000;
    },
  };
  try {
    Object.defineProperty(perfObj, Symbol.toStringTag, { value: 'Performance', configurable: true });
  } catch {}
  try {
    Object.defineProperty(g, Symbol.toStringTag, { value: 'Window', configurable: true });
  } catch {}

  const origTS = Function.prototype.toString;
  const patchedTS = function (this: any, ...args: any[]): string {
    const target = this === patchedTS ? args[0] : this;
    if (STUB_STRINGS.has(target)) return STUB_STRINGS.get(target)!;
    return origTS.apply(this === patchedTS ? args[0] : this, (args.length ? [args[0]] : []) as any);
  };
  Function.prototype.toString = patchedTS as any;
  STUB_STRINGS.set(patchedTS, 'function toString() { [native code] }');

  const smartToString = function (this: any, ...args: any[]): string {
    const target = this === smartToString ? args[0] : this;
    if (STUB_STRINGS.has(target)) return STUB_STRINGS.get(target)!;
    try {
      return origTS.call(target);
    } catch {
      return String(target);
    }
  };
  STUB_STRINGS.set(smartToString, 'function toString() { [native code] }');
  const iframeContentWindow = { Function: { prototype: { toString: smartToString } } };

  g.window = g;
  g.self = g;
  g.navigator = navObj;
  g.performance = perfObj;
  g.MediaSource = MediaSource;
  g.SourceBuffer = SourceBuffer;
  g.addEventListener = () => {};
  if (!g.atob) g.atob = (s: string) => decodeURIComponent(escape(atob(s)));
  if (!g.btoa) g.btoa = (s: string) => btoa(unescape(encodeURIComponent(s)));
  if (!g.localStorage) {
    g.localStorage = { getItem: () => null, setItem: () => {}, removeItem: () => {} };
  }
  if (!g.requestAnimationFrame) g.requestAnimationFrame = () => 0;
  g.document = {
    cookie: '',
    createElement(tag: string) {
      return { tagName: String(tag).toUpperCase(), contentWindow: iframeContentWindow };
    },
    documentElement: { appendChild() {}, removeChild() {} },
    addEventListener() {},
  };

  const OrigDate = Date;
  class HookedDate extends OrigDate {
    constructor(...args: any[]) {
      if (args.length === 0 && frozen !== null) {
        super(frozen * 1000);
      } else {
        super(...(args as [any]));
      }
    }
    static override now() {
      return frozen !== null ? frozen * 1000 : OrigDate.now();
    }
  }
  STUB_STRINGS.set(HookedDate, 'function Date() { [native code] }');
  g.Date = HookedDate;

  // Error stacks are part of the fingerprint; synthesise V8-shaped ones rooted
  // at the pinned player URL.
  g.__FAB = function (err: any) {
    if (!err || typeof err !== 'object') return err;
    const m = String(err.message || err);
    let v8msg = m;
    const jsc = m.match(/undefined is not an object \(evaluating '([^']*)'\)/);
    if (jsc) {
      const expr = jsc[1];
      const mm = expr.match(/\['([^']*)'\]\s*$/) || expr.match(/\.([A-Za-z_$][\w$]*)\s*$/);
      v8msg = "Cannot read properties of undefined (reading '" + (mm ? mm[1] : 'stack') + "')";
    }
    const col = '3472';
    const v8stack =
      'TypeError: ' +
      v8msg +
      '\n    at ' +
      NOZZLE_URL +
      ':2:' +
      col +
      '\n    at S (' +
      NOZZLE_URL +
      ':1:37987)';
    return {
      name: 'TypeError',
      message: v8msg,
      get stack() {
        return v8stack;
      },
      toString() {
        return 'TypeError: ' + v8msg;
      },
    };
  };

  g.__ENVHASHES = ENV_HASHES;

  (0, eval)(NOZZLE_RAW);

  if (typeof g.__lastDt !== 'function') {
    throw new Error('bundle did not expose Dt');
  }
  cached = g.__lastDt;
  return cached;
}

/**
 * Compute the reference `X-Signature` for `payload`.
 *
 * `timestampSeconds` pins the signing clock; omit it to use the real clock.
 */
export function sign(payload: string, timestampSeconds?: number | null): string {
  const dt = setup();
  if (timestampSeconds !== undefined && timestampSeconds !== null) {
    frozen = timestampSeconds;
    try {
      return dt(payload);
    } finally {
      frozen = null;
    }
  }
  return dt(payload);
}

/** The bundle's exposed signing function, for tests that need direct access. */
export function rawSigner(): (payload: string, ts?: number | null) => string {
  return setup();
}