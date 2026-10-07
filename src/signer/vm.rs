//! Register VM recovered from the pinned `nozzle.js` player build.
//!
//! `signHotaudioPayload` in the TypeScript original evaluates a 106 KB
//! obfuscated bundle inside a fake-browser sandbox and calls an exposed
//! `Dt(payload)`. `Dt` turns out to be a **register machine interpreter**: it
//! loads a 1314-byte program ([`PROGRAM`]) and runs 657 instructions against an
//! 80-slot register file. The bundle's obfuscation, environment probing and
//! hashing are all inside that program, so this module re-implements the
//! machine rather than the algorithm — which keeps it bit-exact by
//! construction instead of relying on a guess at the algorithm.
//!
//! Machine summary (recovered by tracing the original):
//!
//! * Instruction = `(opcode, operand)`, fetched two bytes at a time from
//!   `PROGRAM`, advancing `reg[0]` (the program counter) by 2.
//! * `opcode < 80` is `MOV reg[opcode] = reg[operand]`.
//! * `opcode >= 80` dispatches; `reg[1]` is the numeric accumulator and
//!   `reg[34]` the string register.
//! * Opcodes 80–88 are the SHA-256 compression primitives, 92/93 rotations,
//!   98–101 the forward/backward branches, 113–131 the string and
//!   environment ops.
//! * `reg[35]` receives the call argument (the payload string).
//!
//! The program is pure arithmetic on 32-bit words plus string building, so the
//! whole signature costs a few microseconds — versus 200–500 ms for the JS
//! bundle (which the original notes has occasionally taken 123 s).

use std::collections::HashSet;
use std::rc::Rc;

use super::bytecode::PROGRAM;
use super::env::{ENV_HASHES, PERF_NOW, nozzle_stack};

/// Register file size. Registers `34..80` start as empty strings.
const NREGS: usize = 80;

/// Numeric accumulator register.
const R_ACC: usize = 1;
/// Program counter register.
const R_PC: usize = 0;
/// String register.
const R_STR: usize = 34;

/// Maximum instructions before we assume the program diverged.
const STEP_LIMIT: u64 = 20_000_000;

/// A VM value.
///
/// Only the shapes the program actually produces are modelled. Numbers carry
/// full `f64` precision because the program mixes 32-bit unsigned semantics
/// (`>>> 0`, `Math.imul`) with ordinary division.
#[derive(Clone, Debug)]
enum Val {
    /// JS number.
    Num(f64),
    /// JS string. Shared via `Rc` because the same string is aliased between
    /// registers constantly (MOV-heavy program).
    Str(Rc<String>),
    /// `undefined`.
    Undef,
    /// JS boolean. Distinct from `Num` because `op 118` stores a raw boolean
    /// result and later opcodes branch on it.
    Bool(bool),
    /// The `Object.create(null)` environment object from `op 126`. Shared
    /// across calls via `Arc`, since the key set is a compile-time constant.
    Env(std::sync::Arc<HashSet<String>>),
    /// A `String` array produced by `op 128` (`split`).
    Arr(Rc<Vec<Val>>),
    /// A shimmed constructor. `display` is what `String(fn)` renders, and
    /// `ctor` is matched by `op 127` (`new d[arg]()`).
    Native {
        /// `String(fn)`, e.g. `function Date() { [native code] }`.
        display: &'static str,
        /// Which constructor this is, for `op 127`.
        ctor: Ctor,
    },
    /// A shimmed object that is not the global: prototypes, `Date.stack`
    /// holders, and so on. `tag` is the `Symbol.toStringTag`, which is what
    /// makes `Object.prototype.toString` render `[object SourceBuffer]`.
    Object {
        /// `String(value)`.
        display: &'static str,
        /// `Object.prototype.toString.call(value)` when a tag is set.
        tag: Option<&'static str>,
    },
    /// `new Date()` result: milliseconds since the epoch.
    Date(f64),
    /// The shimmed global object (`v(-704)` in the bundle). `op 120` installs it
    /// and `op 119` walks its properties to build the environment fingerprint.
    Global,
    /// A caught exception, materialised as the `__FAB` error object.
    Err,
}

/// Which shimmed constructor a [`Val::Native`] refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ctor {
    Date,
    Other,
}

impl Val {
    /// `ToNumber` for the arithmetic opcodes.
    fn to_num(&self) -> f64 {
        match self {
            Val::Num(n) => *n,
            Val::Bool(b) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            Val::Undef => f64::NAN,
            // `Date` uses `valueOf`, i.e. epoch milliseconds — not its string.
            Val::Date(ms) => *ms,
            Val::Str(s) => str_to_number(s),
            other => str_to_number(&other.to_js_string()),
        }
    }

    /// `String(value)`.
    fn to_js_string(&self) -> String {
        match self {
            Val::Num(n) => js_number_to_string(*n),
            Val::Str(s) => (**s).clone(),
            Val::Undef => "undefined".to_string(),
            Val::Bool(true) => "true".to_string(),
            Val::Bool(false) => "false".to_string(),
            Val::Env(_) => "[object Object]".to_string(),
            Val::Arr(items) => items
                .iter()
                .map(|v| match v {
                    Val::Undef => String::new(),
                    other => other.to_js_string(),
                })
                .collect::<Vec<_>>()
                .join(","),
            Val::Native { display, .. } => (*display).to_string(),
            Val::Object { display, .. } => (*display).to_string(),
            Val::Date(ms) => js_date_string(*ms),
            Val::Global => "[object Window]".to_string(),
            Val::Err => "TypeError".to_string(),
        }
    }

    /// `Boolean(value)`.
    fn truthy(&self) -> bool {
        match self {
            Val::Num(n) => *n != 0.0 && !n.is_nan(),
            Val::Bool(b) => *b,
            Val::Str(s) => !s.is_empty(),
            Val::Undef => false,
            // Every object/array/native is truthy, including an empty string.
            _ => true,
        }
    }

    /// `.length` for `op 114`.
    fn length(&self) -> f64 {
        match self {
            // JS strings are UTF-16, so `.length` counts code *units*: an astral
            // character (U+10000+) contributes 2, not 1.
            Val::Str(s) => s.encode_utf16().count() as f64,
            Val::Arr(items) => items.len() as f64,
            _ => f64::NAN,
        }
    }
}

/// JS string→number coercion (`Number(s)` / unary `+`).
fn str_to_number(s: &str) -> f64 {
    let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    if t.is_empty() {
        return 0.0;
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16)
            .map(|v| v as f64)
            .unwrap_or(f64::NAN);
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

/// JS `Number.prototype.toString()` for the radix-10 case used by `op 122`/
/// `op 130`. Only needs to be exact for non-negative integers up to 2^32-1;
/// anything else falls back to Rust's shortest round-trip formatting.
fn js_number_to_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".to_string();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if n == 0.0 {
        return "0".to_string();
    }
    if n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 {
        return format!("{}", n as i64);
    }
    format!("{n}")
}

/// `Date.prototype.toString()` — the VM probes this, but the resulting text is
/// timezone-dependent and provably does **not** feed the digest (verified by
/// signing under four different `TZ` values and getting identical output). We
/// render the fixed UTC form so the value stays deterministic.
fn js_date_string(ms: f64) -> String {
    let secs = (ms / 1000.0).floor() as i64;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (y, mo, d) = civil_from_days(days);
    const WD: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let wd = WD[(days.rem_euclid(7)) as usize];
    format!(
        "{wd} {mo:>3} {d:02} {y:04} {h:02}:{mi:02}:{s:02} GMT+0000 (Coordinated Universal Time)"
    )
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, usize, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as i64;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as usize;
    (if m <= 2 { y + 1 } else { y }, m - 1, d)
}

/// `String.fromCharCode` for `op 124`/`op 125`.
///
/// Codes above 0xFFFF are truncated by `String.fromCharCode` (unlike
/// `fromCodePoint`), and the program uses 266 to emit U+010A.
fn from_char_code(code: f64) -> String {
    let c = code as i64;
    let u = if (0..=0xFFFF).contains(&c) {
        c as u32
    } else {
        (c as u32) & 0xFFFF
    };
    char::from_u32(u).map(String::from).unwrap_or_default()
}

/// `>>> 0`: ToUint32.
fn to_u32(n: f64) -> u32 {
    if !n.is_finite() || n == 0.0 {
        return 0;
    }
    let trunc = n.trunc();
    let m = trunc.rem_euclid(4_294_967_296.0);
    m as u32
}

/// Members of the shimmed global object that the program reads.
///
/// These reproduce the sandbox the TypeScript signer builds in
/// `signer.ts`: stub constructors whose `toString` reports `[native code]`, a
/// `Date` whose `.stack` is absent (so the read throws and `__FAB` synthesises
/// a V8 stack), and the frozen `performance`. Everything else on the real
/// global is `undefined` for the program's purposes.
fn global_member(key: &str) -> Option<Val> {
    // The sandbox's `nativeFn(name)` renders as `function <name>() { [native code] }`
    // and is a real function object, so it is modelled as a constructor.
    let native = |display: &'static str, ctor: Ctor| Val::Native { display, ctor };
    Some(match key {
        // `Date.stack` is undefined, which is what drives the `__FAB` path.
        "Date" => native("function HookedDate() { [native code] }", Ctor::Date),
        "MediaSource" => native("function MediaSource() { [native code] }", Ctor::Other),
        "SourceBuffer" => native("function SourceBuffer() { [native code] }", Ctor::Other),
        "appendBuffer" => native("function appendBuffer() { [native code] }", Ctor::Other),
        _ => return None,
    })
}

/// `"key" in globalThis` — the members the sandbox actually installs on the
/// global. Everything outside this set reads `undefined`.
/// `"key" in globalThis`.
///
/// The program uses this to probe the sandbox for feature markers. Verified
/// against the original signer: `Date`, `MediaSource` and `SourceBuffer` are
/// installed, while markers like `__ha_chunks` are deliberately absent (their
/// absence is part of the fingerprint).
fn global_has(key: &str) -> bool {
    global_member(key).is_some()
}

/// Members reachable *through* a shimmed global.
fn member_lookup(recv: &Val, key: &str) -> Option<Val> {
    Some(match (recv, key) {
        // `SourceBuffer.prototype` carries a `Symbol.toStringTag`, which is what
        // makes `Object.prototype.toString` render `[object SourceBuffer]`.
        (Val::Native { display, .. }, "prototype")
            if *display == "function SourceBuffer() { [native code] }" =>
        {
            Val::Object {
                display: "function SourceBuffer() { [native code] }",
                tag: Some("SourceBuffer"),
            }
        }
        (Val::Native { .. }, "stack") => Val::Undef,
        (Val::Object { .. }, "stack") => Val::Undef,
        // The `__FAB` error object exposes the synthesised V8 stack that the
        // bundle's error hook builds, which the program folds into the digest.
        (Val::Err, "stack") => Val::Str(Rc::new(nozzle_stack())),
        _ => return None,
    })
}

/// Build the `op 126` environment object: the 144 fingerprint keys, each mapped
/// to `undefined`.
/// Build the `op 126` environment object: the 144 fingerprint keys, each mapped
/// to `undefined`.
///
/// The key set is a compile-time constant, so it is materialised once and
/// shared; the program only ever reads it with `op 118`.
fn env_object() -> Val {
    static KEYS: std::sync::OnceLock<std::sync::Arc<HashSet<String>>> = std::sync::OnceLock::new();
    Val::Env(
        KEYS.get_or_init(|| {
            std::sync::Arc::new(
                ENV_HASHES
                    .iter()
                    .map(|k| k.to_string())
                    .collect::<HashSet<_>>(),
            )
        })
        .clone(),
    )
}

/// Run the program, recording `(opcode, operand, registers)` before each
/// instruction executes. Used by the differential test against the reference
/// implementation to locate divergence; not on the hot path.
pub fn trace(
    payload: &str,
    timestamp_secs: f64,
) -> Result<Vec<(usize, usize, Vec<Rendered>)>, SignError> {
    let mut out = Vec::new();
    run(payload, timestamp_secs, Some(&mut out))?;
    Ok(out)
}

/// One register value rendered the way the reference tracer renders it.
#[derive(Clone, Debug)]
pub enum Rendered {
    /// JS number.
    Num(f64),
    /// JS string (truncated like the reference tracer).
    Str(String),
    /// `undefined`.
    Undef,
    /// Object / array / function, rendered as `<kind#id>` with a stable
    /// first-seen id so object identity is comparable against the reference.
    Other(String),
}

impl std::fmt::Display for Rendered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Rendered::Num(n) => {
                if n.fract() == 0.0 && n.abs() < 9e15 {
                    write!(f, "{}", *n as i64)
                } else {
                    write!(f, "{n}")
                }
            }
            Rendered::Str(s) => write!(f, "{}", serde_json::to_string(s).unwrap_or_default()),
            Rendered::Undef => write!(f, "undefined"),
            Rendered::Other(s) => write!(f, "{s}"),
        }
    }
}

/// Compute the `X-Signature` for `payload` at `timestamp_secs` (Unix seconds).
///
/// Returns `"<prefix><8 hex timestamp><24 hex tag>"`.
pub fn sign(payload: &str, timestamp_secs: f64) -> Result<String, SignError> {
    run(payload, timestamp_secs, None)
}

fn run(
    payload: &str,
    timestamp_secs: f64,
    mut sink: Option<&mut Vec<(usize, usize, Vec<Rendered>)>>,
) -> Result<String, SignError> {
    // `new Array(80).fill(0)`, then `for (S = 34; S < 80; S++) d[S] = ""`.
    // Registers 0..33 are the number 0, registers 34..79 are empty strings.
    let mut regs: Vec<Val> = (0..NREGS)
        .map(|i| {
            if i >= 34 {
                Val::Str(Rc::new(String::new()))
            } else {
                Val::Num(0.0)
            }
        })
        .collect();

    // `for (d[35] = n[0]; ...)` — the call argument lands in register 35.
    regs[35] = Val::Str(Rc::new(payload.to_string()));

    let mut pc: usize = 0;
    let mut steps: u64 = 0;

    loop {
        if pc + 1 >= PROGRAM.len() {
            break;
        }
        steps += 1;
        if steps > STEP_LIMIT {
            return Err(SignError::StepLimit);
        }

        let op = PROGRAM[pc] as usize;
        let arg = PROGRAM[pc + 1] as usize;
        pc += 2;

        // The original increments the program-counter register as part of the
        // fetch expression, so the post-fetch state already reflects `pc`.
        regs[R_PC] = Val::Num(pc as f64);

        if let Some(s) = sink.as_deref_mut() {
            s.push((op, arg, regs.iter().map(render).collect()));
        }

        if op < 80 {
            regs[op] = clone_val(&regs[arg]);
            continue;
        }

        // Helper closures for the "accumulator in `reg[1]`" opcodes.
        macro_rules! acc_bin {
            ($f:expr) => {{
                let a = regs[R_ACC].to_num();
                let b = regs[arg].to_num();
                regs[R_ACC] = Val::Num($f(a, b));
            }};
        }

        match op {
            80 => acc_bin!(|a, b| to_u32(a + b) as f64),
            81 => acc_bin!(|a, b| to_u32(a - b) as f64),
            // Math.imul: 32-bit signed multiply, wrapped to u32.
            82 => {
                acc_bin!(|a, b| { (to_u32(a) as i32).wrapping_mul(to_u32(b) as i32) as u32 as f64 })
            }
            83 => acc_bin!(|a, b| (to_u32(a) ^ to_u32(b)) as f64),
            84 => acc_bin!(|a, b| (to_u32(a) & to_u32(b)) as f64),
            85 => acc_bin!(|a, b| (to_u32(a) | to_u32(b)) as f64),
            87 => acc_bin!(|a: f64, b: f64| (to_u32(a) >> (to_u32(b) & 31)) as f64),
            88 => acc_bin!(|a: f64, b: f64| (to_u32(a) << (to_u32(b) & 31)) as f64),
            // `d[arg]++` / `d[arg]--` on the register operand itself.
            89 => {
                let v = regs[arg].to_num();
                regs[arg] = Val::Num(v + 1.0);
            }
            90 => {
                let v = regs[arg].to_num();
                regs[arg] = Val::Num(v - 1.0);
            }
            91 => regs[R_ACC] = Val::Num(arg as f64),
            94 => {
                // B(x, re = -30) => -x
                regs[R_ACC] = Val::Num(-regs[arg].to_num());
            }
            95 => {
                // B(x, re = 40) => +x
                regs[R_ACC] = Val::Num(regs[arg].to_num());
            }
            96 => acc_bin!(|a, b| to_u32(a / b) as f64),
            97 => regs[R_ACC] = Val::Num((to_u32(regs[R_ACC].to_num()) << (arg & 31)) as f64),
            98 => {
                if regs[R_ACC].truthy() {
                    pc = jump(pc, arg as isize, true);
                }
            }
            99 => {
                if !regs[R_ACC].truthy() {
                    pc = jump(pc, arg as isize, true);
                }
            }
            100 => {
                // Backward branch: `d[1] && I.g.charAt("0") == "h" && (pc -= arg)`.
                // `I.g` is the literal string `"h"`, so `I.g.charAt("0")` is
                // always `"h"` and the second guard is a constant true — the
                // `charAt` lookup lands on `String.prototype.charAt`.
                if regs[R_ACC].truthy() {
                    pc = jump(pc, arg as isize, false);
                }
            }
            101 => {
                // Backward branch on falsy accumulator (`B(d[1], re = -16)`, i.e. `!d[1]`).
                if !regs[R_ACC].truthy() {
                    pc = jump(pc, arg as isize, false);
                }
            }

            // ---- string ops ----
            113 => {
                // d34 += d[arg]
                let mut s = regs[R_STR].to_js_string();
                s.push_str(&regs[arg].to_js_string());
                regs[R_STR] = Val::Str(Rc::new(s));
            }
            114 => regs[R_ACC] = Val::Num(regs[arg].length()),
            115 => {
                // d1 = d34.charCodeAt(d[arg])
                let s = regs[R_STR].to_js_string();
                let i = regs[arg].to_num();
                let code = utf16_at(&s, i);
                regs[R_ACC] = match code {
                    Some(c) => Val::Num(c),
                    // charCodeAt past the end yields NaN.
                    None => Val::Num(f64::NAN),
                };
            }
            118 => {
                // `d1 = (reg[arg] in d34)` — the environment fingerprint
                // membership tests. The result is a JS boolean, not a number,
                // and later opcodes branch on it directly.
                let key = match &regs[arg] {
                    Val::Num(n) => js_number_to_string(*n),
                    other => other.to_js_string(),
                };
                let present = match &regs[R_STR] {
                    Val::Env(set) => set.contains(&key),
                    // `in` against the global object: the fingerprint probes
                    // for members the shim does not define, which is the point.
                    Val::Global => global_has(&key),
                    // `in` on a primitive throws; the VM's catch converts it to
                    // the `__FAB` error value.
                    _ => {
                        regs[R_STR] = Val::Err;
                        continue;
                    }
                };
                regs[R_ACC] = Val::Bool(present);
            }
            119 => {
                // d34 = d34[d[arg]]  — global then member lookup.
                let key = regs[arg].to_js_string();
                let recv = clone_val(&regs[R_STR]);
                let next = match &recv {
                    Val::Undef => {
                        // `undefined.stack` → TypeError → `__FAB` error object.
                        regs[R_STR] = Val::Err;
                        continue;
                    }
                    Val::Str(s) => utf16_at(s, regs[arg].to_num())
                        .map(Val::Num)
                        .unwrap_or(Val::Undef),
                    other => match member_lookup(other, &key) {
                        Some(v) => v,
                        None => match global_member(&key) {
                            Some(v) => v,
                            None => Val::Undef,
                        },
                    },
                };
                regs[R_STR] = next;
            }
            // `v(-704)` is the global object, not `undefined`. The program
            // walks it to discover the environment surface.
            120 => regs[R_STR] = Val::Global,
            121 => {
                // d34 = d34.apply(undefined, [])  — only reachable for callables
                // the program never invokes; treat as `undefined`.
                regs[R_STR] = Val::Undef;
            }
            122 => {
                // `(reg[arg] >>> 0).toString(16).padStart(8, "0")` — the operand
                // selects a *register*, unlike most ops which read the operand
                // directly.
                let v = to_u32(regs[arg].to_num());
                regs[R_STR] = Val::Str(Rc::new(format!("{v:08x}")));
            }
            123 => regs[R_STR] = Val::Str(Rc::new(String::new())),
            124 => regs[R_STR] = Val::Str(Rc::new(from_char_code(arg as f64))),
            125 => {
                let mut s = regs[R_STR].to_js_string();
                s.push_str(&from_char_code(arg as f64));
                regs[R_STR] = Val::Str(Rc::new(s));
            }
            126 => regs[R_STR] = env_object(),
            127 => {
                // `new d[arg]()` — the only constructor the program news up is
                // `Date`, from the shimmed global's `Date` binding. The shim's
                // HookedDate returns the frozen timestamp, so the instance is
                // pinned to `timestamp_secs`.
                regs[R_STR] = match &regs[arg] {
                    Val::Native {
                        ctor: Ctor::Date, ..
                    } => Val::Date(timestamp_secs * 1000.0),
                    _ => Val::Undef,
                };
            }
            128 => {
                // d34.split(d[arg])
                let s = regs[R_STR].to_js_string();
                let sep = regs[arg].to_js_string();
                let parts: Vec<Val> = if sep.is_empty() {
                    s.chars()
                        .map(|c| Val::Str(Rc::new(c.to_string())))
                        .collect()
                } else {
                    s.split(sep.as_str())
                        .map(|p| Val::Str(Rc::new(p.to_string())))
                        .collect()
                };
                regs[R_STR] = Val::Arr(Rc::new(parts));
            }
            129 => {
                // String(d[arg]), with the native-stub fallback.
                let mut s = regs[arg].to_js_string();
                if s.contains("native") {
                    if let Val::Native { display, .. } = &regs[arg] {
                        s = (*display).to_string();
                    }
                }
                regs[R_STR] = Val::Str(Rc::new(s));
            }
            130 => {
                // `d1 = B(d34.includes(d[arg]), 40)` — the bundle's second-layer
                // string table spells the method `includes`, not `toString`.
                // `B(..., re = 40)` is unary `+`, so the boolean becomes 1/0.
                let haystack = regs[R_STR].to_js_string();
                let needle = regs[arg].to_js_string();
                let found = haystack.contains(&needle);
                // `B(x, re = 40)` is unary `+`, so the boolean is coerced to 1/0.
                regs[R_ACC] = Val::Num(if found { 1.0 } else { 0.0 });
            }
            131 => {
                // B(d34 === d[arg], 40) => +boolean
                let eq = match (&regs[R_STR], &regs[arg]) {
                    (Val::Num(a), Val::Num(b)) => a == b,
                    (Val::Bool(a), Val::Bool(b)) => a == b,
                    (Val::Str(a), Val::Str(b)) => a == b,
                    (Val::Undef, Val::Undef) => true,
                    _ => false,
                };
                regs[R_ACC] = Val::Num(if eq { 1.0 } else { 0.0 });
            }
            other => return Err(SignError::UnknownOpcode(other as u16)),
        }
    }

    // The program returns `reg[34]`.
    Ok(regs[R_STR].to_js_string())
}

/// Apply a branch displacement. Forward jumps add, backward jumps subtract, and
/// both then land on the *next* pair, matching the original's post-fetch
/// `pc += n` / `pc -= n` on an already-advanced counter.
fn jump(pc: usize, disp: isize, forward: bool) -> usize {
    if forward {
        pc.wrapping_add(disp as usize) & !(1)
    } else {
        pc.wrapping_sub(disp as usize) & !(1)
    }
}

/// `String.prototype.charCodeAt`, returning `NaN` past the end.
fn utf16_at(s: &str, idx: f64) -> Option<f64> {
    if idx < 0.0 || !idx.is_finite() {
        return None;
    }
    let idx = idx as usize;
    s.encode_utf16().nth(idx).map(|u| u as f64)
}

/// Render one register the way the reference tracer does, so traces diff
/// cleanly. Numbers print as integers when integral, strings as JSON.
fn render(v: &Val) -> Rendered {
    match v {
        Val::Num(n) => Rendered::Num(*n),
        Val::Bool(b) => Rendered::Other(b.to_string()),
        Val::Str(s) => {
            // Truncate at 80 chars to match the reference tracer exactly.
            let t: String = s.chars().take(80).collect();
            Rendered::Str(if s.chars().count() > 80 {
                format!("{t}\u{2026}")
            } else {
                t
            })
        }
        Val::Undef => Rendered::Undef,
        // Reference-compatible kind tags. Object *identity* is deliberately not
        // rendered: the program reuses a single environment object, a single
        // global and a single Date, so the kind is all that must agree.
        Val::Env(_) => Rendered::Other("<env>".to_string()),
        Val::Arr(_) => Rendered::Other("<arr>".to_string()),
        Val::Native { .. } => Rendered::Other("<fn>".to_string()),
        Val::Object { .. } => Rendered::Other("<obj>".to_string()),
        Val::Date(_) => Rendered::Other("<date>".to_string()),
        Val::Err => Rendered::Other("<err>".to_string()),
        Val::Global => Rendered::Other("<global>".to_string()),
    }
}

/// Cheap clone: the program MOVs between registers constantly, so we avoid
/// re-allocating strings.
fn clone_val(v: &Val) -> Val {
    match v {
        Val::Str(s) => Val::Str(Rc::clone(s)),
        Val::Env(s) => Val::Env(std::sync::Arc::clone(s)),
        Val::Arr(a) => Val::Arr(Rc::clone(a)),
        Val::Object { display, tag } => Val::Object { display, tag: *tag },
        other => other.clone(),
    }
}

/// Signer failures.
#[derive(Debug, PartialEq, Eq)]
pub enum SignError {
    /// The program did not terminate within `STEP_LIMIT` instructions, i.e. the
    /// captured program and the opcode semantics no longer agree.
    StepLimit,
    /// An opcode outside the recovered set was executed.
    UnknownOpcode(u16),
}

impl std::fmt::Display for SignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignError::StepLimit => write!(f, "signer step limit exceeded"),
            SignError::UnknownOpcode(o) => write!(f, "signer hit unknown opcode {o}"),
        }
    }
}

impl std::error::Error for SignError {}

/// `performance.now()` the environment reports, exposed for tests.
pub const PERF_NOW_VALUE: f64 = PERF_NOW;
