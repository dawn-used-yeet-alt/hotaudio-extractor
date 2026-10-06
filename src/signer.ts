import { ENV_HASHES } from './env_hashes';
import { NOZZLE_RAW } from './nozzle_raw';

/**
 * Player version the bundled nozzle copy was captured from.
 * The signer emulates the browser environment this build expects.
 */
export const PINNED_NOZZLE_VERSION = '1J1Db0bF';

type DtFunction = (payload: string) => string;
let cachedDt: DtFunction | null = null;
let frozenTimestamp: number | null = null;

/**
 * Initialize the sandboxed browser environment required by the bundled
 * `nozzle.js` signature routine and return its signing function.
 *
 * The bundle inspects `MediaSource`, `navigator`, `performance`, `Date`,
 * and error stack formats. The shims below provide the exact values the
 * pinned player version expects. The result is cached per process.
 */
function setupSignerEnvironment(): DtFunction {
  if (cachedDt) return cachedDt;

  const g: any = typeof globalThis !== 'undefined' ? globalThis : (typeof window !== 'undefined' ? window : {});

  const STUB_STRINGS = new Map<any, string>();
  function nativeFn(name: string) {
    const f = function () {};
    Object.defineProperty(f, 'name', { value: name, configurable: true });
    STUB_STRINGS.set(f, 'function ' + name + '() { [native code] }');
    return f;
  }

  class MediaSource {}
  class SourceBuffer {}
  for (const n of ['isTypeSupported', 'addSourceBuffer', 'removeSourceBuffer', 'endOfStream', 'setLiveSeekableRange', 'clearLiveSeekableRange']) {
    (MediaSource as any)[n] = nativeFn(n);
  }
  for (const n of ['appendBuffer', 'abort', 'remove', 'appendStream']) {
    (SourceBuffer.prototype as any)[n] = nativeFn(n);
  }
  for (const n of ['sourceBuffers', 'activeSourceBuffers', 'onsourceopen', 'onsourceended', 'onsourceclose']) {
    try { Object.defineProperty(MediaSource.prototype, n, { get() { return []; }, configurable: true }); } catch {}
  }
  try { Object.defineProperty(MediaSource.prototype, 'duration', { get() { return 0; }, configurable: true }); } catch {}
  for (const n of ['updateend', 'updatestart', 'update', 'error', 'abort']) {
    try { Object.defineProperty(SourceBuffer.prototype, n, { get() { return null; }, configurable: true }); } catch {}
  }
  STUB_STRINGS.set(MediaSource, 'function MediaSource() { [native code] }');
  STUB_STRINGS.set(SourceBuffer, 'function SourceBuffer() { [native code] }');
  try { Object.defineProperty(MediaSource.prototype, Symbol.toStringTag, { value: 'MediaSource', configurable: true }); } catch {}
  try { Object.defineProperty(SourceBuffer.prototype, Symbol.toStringTag, { value: 'SourceBuffer', configurable: true }); } catch {}

  const navObj = { vendor: 'Google Inc.' };
  try { Object.defineProperty(navObj, Symbol.toStringTag, { value: 'Navigator', configurable: true }); } catch {}
  const perfObj = {
    now: () => 123456.789,
    get timeOrigin() {
      return frozenTimestamp !== null ? frozenTimestamp * 1000 : 1787330000000;
    },
  };
  try { Object.defineProperty(perfObj, Symbol.toStringTag, { value: 'Performance', configurable: true }); } catch {}
  try { Object.defineProperty(g, Symbol.toStringTag, { value: 'Window', configurable: true }); } catch {}

  const __origTS = Function.prototype.toString;
  const patchedTS = function (this: any, ...args: any[]): string {
    const target = this === patchedTS ? args[0] : this;
    if (STUB_STRINGS.has(target)) return STUB_STRINGS.get(target)!;
    return __origTS.apply(this === patchedTS ? args[0] : this, (args.length ? [args[0]] : []) as any);
  };
  Function.prototype.toString = patchedTS as any;
  STUB_STRINGS.set(patchedTS, 'function toString() { [native code] }');

  function smartToString(this: any, ...args: any[]): string {
    const target = this === smartToString ? args[0] : this;
    if (STUB_STRINGS.has(target)) return STUB_STRINGS.get(target)!;
    try { return __origTS.call(target); } catch { return String(target); }
  }
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
  if (!g.localStorage) g.localStorage = { getItem: () => null, setItem: () => {}, removeItem: () => {} };
  if (!g.requestAnimationFrame) g.requestAnimationFrame = () => 0;
  g.document = {
    cookie: '',
    createElement(tag: string) { return { tagName: String(tag).toUpperCase(), contentWindow: iframeContentWindow }; },
    documentElement: { appendChild() {}, removeChild() {} },
    addEventListener() {},
  };

  const OrigDate = Date;
  class HookedDate extends OrigDate {
    constructor(...args: any[]) {
      if (args.length === 0 && frozenTimestamp !== null) {
        super(frozenTimestamp * 1000);
      } else {
        super(...(args as [any]));
      }
    }
    static override now() {
      return frozenTimestamp !== null ? frozenTimestamp * 1000 : OrigDate.now();
    }
  }
  STUB_STRINGS.set(HookedDate, 'function Date() { [native code] }');
  g.Date = HookedDate;

  const NOZZLE_URL = `https://hotaudio.net/nozzle.js?v=${PINNED_NOZZLE_VERSION}`;
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
    const v8stack = 'TypeError: ' + v8msg + '\n    at ' + NOZZLE_URL + ':2:' + col + '\n    at S (' + NOZZLE_URL + ':1:37987)';
    return {
      name: 'TypeError',
      message: v8msg,
      get stack() { return v8stack; },
      toString() { return 'TypeError: ' + v8msg; },
    };
  };

  g.__ENVHASHES = ENV_HASHES;

  (0, eval)(NOZZLE_RAW);

  if (typeof g.__lastDt !== 'function') {
    throw new Error('Failed to expose Dt function from nozzle bundle');
  }

  cachedDt = g.__lastDt;
  return cachedDt!;
}

/**
 * Compute the `X-Signature` header value for a listen request payload.
 * Pass `timestampSeconds` to pin the signing clock (tests only).
 */
export function signHotaudioPayload(payload: string, timestampSeconds?: number): string {
  const dt = setupSignerEnvironment();
  if (timestampSeconds !== undefined && timestampSeconds !== null) {
    frozenTimestamp = timestampSeconds;
    try {
      return dt(payload);
    } finally {
      frozenTimestamp = null;
    }
  }
  return dt(payload);
}
