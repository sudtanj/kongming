//! What differs between the native daemon and a Worker build: the clock,
//! timers and background tasks. Native uses std and tokio; on
//! wasm32-unknown-unknown (no `native` feature) the clock is `Date.now()`,
//! sleeping is `setTimeout` and tasks run on the JavaScript event loop.
//! `std::time::Instant`/`SystemTime` panic there, so the core calls these.

use std::future::Future;
use std::time::Duration;

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    #[cfg(all(target_arch = "wasm32", not(feature = "native")))]
    {
        js_sys::Date::now() as i64
    }
    #[cfg(not(all(target_arch = "wasm32", not(feature = "native"))))]
    {
        (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
    }
}

/// The current time in UTC.
pub fn now_utc() -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp_nanos(now_ms() as i128 * 1_000_000)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
}

/// Wait `d` without blocking the executor.
pub async fn sleep(d: Duration) {
    #[cfg(feature = "native")]
    {
        tokio::time::sleep(d).await
    }
    #[cfg(all(target_arch = "wasm32", not(feature = "native")))]
    {
        js::sleep(d).await
    }
    #[cfg(all(not(target_arch = "wasm32"), not(feature = "native")))]
    {
        let _ = d;
    }
}

/// The deadline passed before the future finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed;

impl std::fmt::Display for Elapsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("deadline has elapsed")
    }
}

impl std::error::Error for Elapsed {}

/// Run `f` for at most `d`.
pub async fn timeout<F: Future>(d: Duration, f: F) -> Result<F::Output, Elapsed> {
    #[cfg(feature = "native")]
    {
        tokio::time::timeout(d, f).await.map_err(|_| Elapsed)
    }
    #[cfg(not(feature = "native"))]
    {
        let f = std::pin::pin!(f);
        let t = std::pin::pin!(sleep(d));
        match futures_util::future::select(f, t).await {
            futures_util::future::Either::Left((v, _)) => Ok(v),
            futures_util::future::Either::Right(_) => Err(Elapsed),
        }
    }
}

/// Run a task in the background. Code with access to the app state should
/// go through `AppState::spawn`, which the host can intercept.
pub fn spawn<F: Future<Output = ()> + Send + 'static>(f: F) {
    #[cfg(feature = "native")]
    {
        drop(tokio::spawn(f));
    }
    #[cfg(all(target_arch = "wasm32", not(feature = "native")))]
    {
        wasm_bindgen_futures::spawn_local(f);
    }
    #[cfg(all(not(target_arch = "wasm32"), not(feature = "native")))]
    {
        drop(f);
        tracing::warn!("no executor in this build: a background task was dropped");
    }
}

#[cfg(all(target_arch = "wasm32", not(feature = "native")))]
mod js {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use wasm_bindgen::{JsCast, JsValue};

    /// A JavaScript-backed future marked `Send` so the core's futures stay
    /// `Send` on every target. Sound on wasm32-unknown-unknown without
    /// atomics: there is only one thread.
    pub(crate) struct AssertSend<F>(pub F);

    // SAFETY: wasm32 without the atomics feature has a single thread, so the
    // value never moves between threads.
    unsafe impl<F> Send for AssertSend<F> {}

    impl<F: Future> Future for AssertSend<F> {
        type Output = F::Output;
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
            // SAFETY: structural pinning of the only field.
            unsafe { self.map_unchecked_mut(|s| &mut s.0) }.poll(cx)
        }
    }

    pub(crate) fn sleep(d: Duration) -> impl Future<Output = ()> + Send {
        let ms = d.as_millis().min(i32::MAX as u128) as f64;
        let promise = js_sys::Promise::new(&mut |resolve: js_sys::Function, _reject: js_sys::Function| {
            let global = js_sys::global();
            let timer = js_sys::Reflect::get(&global, &JsValue::from_str("setTimeout"))
                .ok()
                .and_then(|f| f.dyn_into::<js_sys::Function>().ok());
            match timer {
                Some(set_timeout) => {
                    let _ = set_timeout.call2(&global, &resolve, &JsValue::from_f64(ms));
                }
                None => {
                    let _ = resolve.call0(&JsValue::NULL);
                }
            }
        });
        let wait = AssertSend(wasm_bindgen_futures::JsFuture::from(promise));
        async move {
            let _ = wait.await;
        }
    }
}
