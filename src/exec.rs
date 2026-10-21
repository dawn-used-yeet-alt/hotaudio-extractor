//! Driving the async pipeline from synchronous code.
//!
//! Only the native build has a `main`, but the pipeline is `async` so that one
//! implementation serves both targets (see [`crate::http`]). On native nothing
//! ever suspends: the transport is blocking `ureq`, so a native future is
//! `Poll::Ready` on its first and only poll. That is what makes this a loop
//! rather than a real executor, and why it costs the native build nothing —
//! there is no waker to wake, no queue and no thread.
//!
//! A future that does suspend is a bug here, not a slow request, so it panics
//! with that message instead of spinning. If you add a genuinely asynchronous
//! dependency, replace this with a real executor rather than making it work.

use std::future::Future;
use std::task::{Context, Poll, Waker};

/// Run `fut` to completion on the current thread.
///
/// # Panics
///
/// If `fut` returns [`Poll::Pending`] on its first poll, which means some part
/// of the native path became genuinely asynchronous.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    // A no-op waker because nothing ever suspends: present only to satisfy
    // `Context`. `Waker::noop` is stable as of the 1.85 MSRV.
    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = Box::pin(fut);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!(
            "block_on: a future suspended on a native build. The native transport is \
             blocking, so this means an async dependency was added; use a real executor."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The whole module is native-only, so these need no wasm attribute.
    #[test]
    fn runs_a_ready_future() {
        assert_eq!(block_on(async { 1 + 1 }), 2);
    }

    #[test]
    fn runs_nested_awaits_that_never_suspend() {
        // `inner` must be `async` to be awaited; that it never suspends is what
        // this test asserts `block_on` tolerates.
        #[allow(clippy::unused_async)]
        async fn inner() -> u32 {
            7
        }
        async fn outer() -> u32 {
            inner().await * 6
        }
        assert_eq!(block_on(outer()), 42);
    }
}
