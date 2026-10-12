//! Wall clock, monotonic clock and sleeping, per target.
//!
//! `std::time::SystemTime` and `Instant` both **panic** on
//! `wasm32-unknown-unknown` ("time not implemented on this platform"), and
//! `thread::sleep` blocks the only thread a browser page has. Every clock read
//! and every wait in the crate therefore goes through this module, which has a
//! `performance.now()` implementation for wasm and the `std` one everywhere
//! else. Nothing else in the crate may name `SystemTime` or `Instant`.
//!
//! The values themselves agree: `Date.now()` is Unix milliseconds and
//! `performance.now()` is monotonic, so the signer sees the same
//! Unix-seconds clock it does natively.

use std::time::Duration;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

/// Unix time in seconds, fractional — the clock the signer stamps into
/// `X-Signature`.
///
/// Rounded down to the millisecond on wasm, which is finer than the whole-second
/// floor the signature format uses, so signatures are unaffected.
#[must_use]
pub fn unix_secs() -> f64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64())
    }
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now() / 1000.0
    }
}

/// A monotonic millisecond reading, used only for measuring how long a retry
/// sequence has already taken. The absolute value is meaningless; only
/// differences are.
#[must_use]
pub fn monotonic_ms() -> f64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        // `Instant` has no epoch, and `SystemTime` is not monotonic; the
        // absolute value is never used, so measure from process start.
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_secs_f64()
            * 1000.0
    }
    #[cfg(target_arch = "wasm32")]
    {
        // Present on `Window` and on the worker global scope; neither is
        // guaranteed, and an absent clock only affects retry pacing.
        performance_now().unwrap_or(0.0)
    }
}

/// `performance.now()` from whichever global scope is running.
#[cfg(target_arch = "wasm32")]
fn performance_now() -> Option<f64> {
    let global: wasm_bindgen::JsValue = js_sys::global().into();
    let scope: Option<web_sys::Performance> = global
        .dyn_ref::<web_sys::Window>()
        .and_then(web_sys::Window::performance)
        .or_else(|| {
            global
                .dyn_ref::<web_sys::WorkerGlobalScope>()
                .and_then(web_sys::WorkerGlobalScope::performance)
        });
    scope.map(|p| p.now())
}

/// Wait for `d`.
///
/// On wasm this yields to the event loop rather than blocking it, which is what
/// makes the retry backoff usable in a page at all.
pub async fn sleep(d: Duration) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::sleep(d);
    }
    #[cfg(target_arch = "wasm32")]
    {
        let ms = d.as_millis().min(i32::MAX as u128) as i32;
        let promise = js_sys::Promise::new(&mut |resolve, _reject| {
            // The callback outlives this frame, so it is leaked rather than
            // dropped: `forget` is required, and the retry policy bounds how many
            // waits one request can make.
            let settled = wasm_bindgen::closure::Closure::<dyn FnMut()>::once(move || {
                let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
            });
            let settled_fn: js_sys::Function = settled
                .as_ref()
                .clone()
                .dyn_into()
                .expect("a Closure is a JS function");
            // Hand the Rust side over to the timer; `forget` before it goes out
            // of scope, since the closure must outlive this frame.
            settled.forget();
            if set_timer(&settled_fn, ms).is_err() {
                // No way to wait. Resolve now rather than hang: this loses the
                // backoff but keeps the request moving, and a scope with no
                // `setTimeout` is not one this will run in anyway.
                let _ = settled_fn.call0(&wasm_bindgen::JsValue::NULL);
            }
        });
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }
}

/// Run `callback` after `ms` milliseconds on whichever global scope is running,
/// returning the timer id for [`clear_timer`].
///
/// Takes a [`js_sys::Function`] rather than a `Closure` so callers can convert
/// and release the `Closure` first — the callback has to outlive this frame, and
/// a `Closure` dropped before its timer fires is a use-after-free.
#[cfg(target_arch = "wasm32")]
pub(crate) fn set_timer(
    callback: &js_sys::Function,
    ms: i32,
) -> Result<i32, wasm_bindgen::JsValue> {
    let global: wasm_bindgen::JsValue = js_sys::global().into();
    if let Some(window) = global.dyn_ref::<web_sys::Window>() {
        return window.set_timeout_with_callback_and_timeout_and_arguments_0(callback, ms);
    }
    if let Some(worker) = global.dyn_ref::<web_sys::WorkerGlobalScope>() {
        return worker.set_timeout_with_callback_and_timeout_and_arguments_0(callback, ms);
    }
    Err(wasm_bindgen::JsValue::from_str(
        "no setTimeout on this global scope",
    ))
}

/// Cancel a timer scheduled by [`set_timer`]. Cancelling after the callback has
/// already run is a no-op, so this is always safe to call.
#[cfg(target_arch = "wasm32")]
pub(crate) fn clear_timer(id: i32) {
    let global: wasm_bindgen::JsValue = js_sys::global().into();
    if let Some(window) = global.dyn_ref::<web_sys::Window>() {
        window.clear_timeout_with_handle(id);
    } else if let Some(worker) = global.dyn_ref::<web_sys::WorkerGlobalScope>() {
        worker.clear_timeout_with_handle(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn unix_secs_is_plausible() {
        let now = unix_secs();
        // Later than 2020 and before 2100: catches a unit mistake (millis for
        // seconds, or seconds for millis) without depending on the real date.
        assert!(now > 1_577_836_800.0, "unix_secs looks like millis: {now}");
        assert!(now < 4_102_444_800.0, "unix_secs is too far ahead: {now}");
    }

    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn monotonic_ms_never_goes_backwards() {
        let a = monotonic_ms();
        let b = monotonic_ms();
        assert!(b >= a, "monotonic clock went backwards: {a} then {b}");
    }
}
