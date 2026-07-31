//! Runtime abstraction layer.
//!
//! Re-exports asupersync's async primitives behind an ambient-authority
//! interface, hiding the Cx capability that its API threads through every
//! call. Named `runtime` rather than `asupersync` because call sites should
//! not care which runtime is underneath.
//!
//! This was a two-backend abstraction during the tokio -> asupersync
//! migration. The tokio path is gone; what remains is the shim.

pub mod net {
    pub use asupersync::net::{UnixListener, UnixStream};
    pub use asupersync::net::{
        UnixOwnedReadHalf as OwnedReadHalf, UnixOwnedWriteHalf as OwnedWriteHalf,
    };

    /// Bind a Unix listener.
    pub async fn bind_unix_listener(
        path: impl AsRef<std::path::Path>,
    ) -> std::io::Result<UnixListener> {
        UnixListener::bind(path).await
    }

    /// Shut down the write half of a stream.
    pub fn shutdown_write(stream: &mut UnixStream) -> std::io::Result<()> {
        stream.shutdown(std::net::Shutdown::Write)
    }
}

pub mod io {
    pub use asupersync::io::{AsyncReadExt, AsyncWriteExt, BufReader};

    /// Adds `read_line` as a method, since asupersync exposes it as a
    /// standalone function rather than a trait method.
    pub trait AsyncBufReadExt: asupersync::io::AsyncBufRead + Unpin {
        fn read_line<'a>(&'a mut self, buf: &'a mut String) -> asupersync::io::ReadLine<'a, Self> {
            asupersync::io::read_line(self, buf)
        }
    }

    impl<T: asupersync::io::AsyncBufRead + Unpin + ?Sized> AsyncBufReadExt for T {}

    /// Async stdin backed by a blocking reader on a background thread.
    #[must_use]
    pub const fn stdin() -> Stdin {
        Stdin { _priv: () }
    }

    /// Async stdout backed by a blocking writer on a background thread.
    #[must_use]
    pub const fn stdout() -> Stdout {
        Stdout { _priv: () }
    }

    pub struct Stdin {
        _priv: (),
    }

    impl Stdin {
        /// # Panics
        ///
        /// Panics if the spawned blocking task panics (it never does here; the
        /// closure only performs a stdin read).
        pub async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let mut owned = vec![0u8; buf.len()];
            let n = super::task::spawn_blocking(move || {
                use std::io::Read;
                std::io::stdin().read(&mut owned).map(|n| {
                    owned.truncate(n);
                    (n, owned)
                })
            })
            .await
            .expect("spawn_blocking panicked")?;
            buf[..n.0].copy_from_slice(&n.1);
            Ok(n.0)
        }
    }

    pub struct Stdout {
        _priv: (),
    }

    impl Stdout {
        /// # Panics
        ///
        /// Panics if the spawned blocking task panics (it never does here; the
        /// closure only performs a stdout write).
        pub async fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
            let data = data.to_vec();
            super::task::spawn_blocking(move || {
                use std::io::Write;
                std::io::stdout().write_all(&data)
            })
            .await
            .expect("spawn_blocking panicked")
        }

        /// # Panics
        ///
        /// Panics if the spawned blocking task panics (it never does here; the
        /// closure only performs a stdout flush).
        pub async fn flush(&mut self) -> std::io::Result<()> {
            super::task::spawn_blocking(|| {
                use std::io::Write;
                std::io::stdout().flush()
            })
            .await
            .expect("spawn_blocking panicked")
        }
    }
}

pub mod sync {
    use std::ops::{Deref, DerefMut};
    use std::sync::Arc;

    /// Async mutex that hides asupersync's Cx requirement.
    ///
    /// The inner mutex lives behind an `Arc` so `lock` can return an *owned*
    /// guard. asupersync 0.3.10 deliberately made the borrowed `MutexGuard`
    /// `!Send` (a `PhantomData<*mut ()>` marker), which forbids holding one
    /// across an `.await` inside a spawned task -- and the server does exactly
    /// that, in `handle_connection` and in the per-agent write fan-out.
    /// `OwnedMutexGuard` is `Send` and is the supported way to hold a lock
    /// across a suspension point.
    ///
    /// The extra `Arc` clone per acquisition is not on any hot path: the
    /// manager lock is taken once per request and once per 10ms poll tick.
    pub struct Mutex<T>(Arc<asupersync::sync::Mutex<T>>);

    impl<T> Mutex<T> {
        pub fn new(value: T) -> Self {
            Self(Arc::new(asupersync::sync::Mutex::new(value)))
        }

        /// # Panics
        ///
        /// Panics if called outside an async context, or if the underlying
        /// mutex has been poisoned by a panic while held.
        pub async fn lock(&self) -> MutexGuard<T> {
            let cx = asupersync::Cx::current().expect("Mutex::lock called outside async context");
            let guard = asupersync::sync::OwnedMutexGuard::lock(Arc::clone(&self.0), &cx)
                .await
                .expect("Mutex should not be poisoned");
            MutexGuard { guard }
        }
    }

    /// Wrapper around asupersync's `OwnedMutexGuard` to keep the API opaque.
    pub struct MutexGuard<T> {
        guard: asupersync::sync::OwnedMutexGuard<T>,
    }

    impl<T> Deref for MutexGuard<T> {
        type Target = T;
        fn deref(&self) -> &T {
            &self.guard
        }
    }

    impl<T> DerefMut for MutexGuard<T> {
        fn deref_mut(&mut self) -> &mut T {
            &mut self.guard
        }
    }

    pub mod broadcast {
        /// Error types, grouped in an `error` submodule.
        pub mod error {
            pub use asupersync::channel::broadcast::{RecvError, SendError};
        }

        /// Create a broadcast channel with the given capacity.
        #[must_use]
        pub fn channel<T: Clone + Send + 'static>(capacity: usize) -> (Sender<T>, Receiver<T>) {
            let (tx, rx) = asupersync::channel::broadcast::channel(capacity);
            (Sender(tx), Receiver(rx))
        }

        /// Broadcast sender that hides Cx requirement on `send()`.
        #[derive(Clone)]
        pub struct Sender<T>(asupersync::channel::broadcast::Sender<T>);

        impl<T: Clone + Send + 'static> Sender<T> {
            /// # Panics
            ///
            /// Panics if called outside an async context.
            pub fn send(
                &self,
                value: T,
            ) -> Result<usize, asupersync::channel::broadcast::SendError<T>> {
                let cx = asupersync::Cx::current()
                    .expect("broadcast::send called outside async context");
                self.0.send(&cx, value)
            }

            #[must_use]
            pub fn subscribe(&self) -> Receiver<T> {
                Receiver(self.0.subscribe())
            }
        }

        /// Broadcast receiver that hides Cx requirement on `recv()`.
        pub struct Receiver<T>(asupersync::channel::broadcast::Receiver<T>);

        impl<T: Clone + Send + 'static> Receiver<T> {
            /// # Panics
            ///
            /// Panics if called outside an async context.
            pub async fn recv(&mut self) -> Result<T, asupersync::channel::broadcast::RecvError> {
                let cx = asupersync::Cx::current()
                    .expect("broadcast::recv called outside async context");
                self.0.recv(&cx).await
            }
        }
    }
}

pub mod time {
    pub use std::time::Duration;
    pub use std::time::Instant;

    /// Sleep for the given duration.
    #[must_use]
    pub fn sleep(duration: Duration) -> asupersync::time::Sleep {
        asupersync::time::sleep(asupersync::time::wall_now(), duration)
    }

    /// Wrap a future with a timeout.
    pub fn timeout<F: std::future::Future>(
        duration: Duration,
        future: F,
    ) -> asupersync::time::TimeoutFuture<F> {
        asupersync::time::timeout(asupersync::time::wall_now(), duration, future)
    }

    /// Create an async interval timer.
    #[must_use]
    pub fn interval(period: Duration) -> Interval {
        Interval {
            inner: asupersync::time::interval(asupersync::time::wall_now(), period),
        }
    }

    /// Async interval that wraps asupersync's sync `tick()` with sleep.
    pub struct Interval {
        inner: asupersync::time::Interval,
    }

    impl Interval {
        /// Wait for the next tick. First call returns immediately.
        pub async fn tick(&mut self) {
            let now = asupersync::time::wall_now();
            let deadline = self.inner.tick(now);
            // If deadline is in the future, sleep until then
            let now_nanos = now.as_nanos();
            let deadline_nanos = deadline.as_nanos();
            if deadline_nanos > now_nanos {
                let wait = Duration::from_nanos(deadline_nanos - now_nanos);
                asupersync::time::sleep(now, wait).await;
            }
        }
    }
}

pub mod signal {
    pub use asupersync::signal::{Signal, SignalKind, signal};
}

pub mod task {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};

    static RUNTIME_HANDLE: OnceLock<asupersync::runtime::RuntimeHandle> = OnceLock::new();

    /// Store the runtime handle for later `spawn()` calls.
    /// Must be called once during startup (from `block_on` context).
    pub fn set_runtime_handle(handle: asupersync::runtime::RuntimeHandle) {
        // In tests, the handle may already be set by a previous test.
        let _ = RUNTIME_HANDLE.set(handle);
    }

    fn handle() -> &'static asupersync::runtime::RuntimeHandle {
        RUNTIME_HANDLE
            .get()
            .expect("runtime handle not set — call set_runtime_handle first")
    }

    /// Run an async future on a fresh asupersync runtime.
    /// Used for tests and any context that needs a one-shot runtime.
    ///
    /// # Panics
    ///
    /// Panics if a fresh asupersync runtime cannot be built.
    pub fn block_on<F: std::future::Future + Send + 'static>(f: F) -> F::Output
    where
        F::Output: Send + 'static,
    {
        let rt = asupersync::runtime::RuntimeBuilder::new()
            .build()
            .expect("failed to build runtime");
        let h = rt.handle();
        set_runtime_handle(h.clone());
        let join = h.spawn(f);
        rt.block_on(join)
    }

    /// Spawn a future onto the runtime.
    pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        JoinHandle(JoinHandleInner::Async(handle().spawn(future)))
    }

    /// Run a blocking closure on a background thread, returning a future
    /// that resolves to the closure's return value.
    ///
    /// # Panics
    ///
    /// Panics if the internal result/waker mutex is poisoned by a panic.
    pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let state = Arc::new(BlockingState {
            result: std::sync::Mutex::new(None),
            waker: std::sync::Mutex::new(None),
            done: AtomicBool::new(false),
        });
        let state2 = Arc::clone(&state);
        std::thread::spawn(move || {
            let result = f();
            *state2
                .result
                .lock()
                .expect("blocking result mutex poisoned") = Some(result);
            state2.done.store(true, Ordering::Release);
            let waker = state2
                .waker
                .lock()
                .expect("blocking waker mutex poisoned")
                .take();
            if let Some(waker) = waker {
                waker.wake();
            }
        });
        JoinHandle(JoinHandleInner::Blocking(state))
    }

    struct BlockingState<T> {
        result: std::sync::Mutex<Option<T>>,
        waker: std::sync::Mutex<Option<std::task::Waker>>,
        done: AtomicBool,
    }

    /// Join handle for a spawned or blocking task.
    pub struct JoinHandle<T>(JoinHandleInner<T>);

    enum JoinHandleInner<T> {
        Async(asupersync::runtime::JoinHandle<T>),
        Blocking(Arc<BlockingState<T>>),
    }

    impl<T> JoinHandle<T> {
        /// Cancel the task. Best-effort — blocking tasks run to completion.
        pub const fn abort(&self) {
            // asupersync JoinHandle doesn't have abort;
            // blocking tasks can't be cancelled mid-execution.
            // This is a no-op for compatibility.
        }
    }

    impl<T> std::future::Future for JoinHandle<T> {
        type Output = Result<T, JoinError>;

        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            let inner = &mut self.get_mut().0;
            match inner {
                JoinHandleInner::Async(handle) => std::pin::Pin::new(handle).poll(cx).map(Ok),
                JoinHandleInner::Blocking(state) => {
                    if state.done.load(Ordering::Acquire) {
                        let result = state
                            .result
                            .lock()
                            .expect("blocking result mutex poisoned")
                            .take()
                            .expect("blocking result already taken");
                        std::task::Poll::Ready(Ok(result))
                    } else {
                        *state.waker.lock().expect("blocking waker mutex poisoned") =
                            Some(cx.waker().clone());
                        // Double-check after setting waker to avoid race
                        if state.done.load(Ordering::Acquire) {
                            let result = state
                                .result
                                .lock()
                                .expect("blocking result mutex poisoned")
                                .take()
                                .expect("blocking result already taken");
                            std::task::Poll::Ready(Ok(result))
                        } else {
                            std::task::Poll::Pending
                        }
                    }
                }
            }
        }
    }

    /// Placeholder error type for join handle failures.
    #[derive(Debug)]
    pub struct JoinError;

    impl std::fmt::Display for JoinError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "task panicked")
        }
    }

    impl std::error::Error for JoinError {}
}

/// Select macro for asupersync using nested `Select<A, B>`.
///
/// Supports 2-5 branches with optional guards:
///   select! {
///       pat = `future_expr` => { body }
///       pat = `future_expr`, if guard => { body }
///   }
macro_rules! select {
    // ── 2 branches ──────────────────────────────────────────────────
    (
        $p1:pat = $f1:expr $(, if $g1:expr)? => $b1:block
        $p2:pat = $f2:expr $(, if $g2:expr)? => $b2:block
    ) => {{
        use asupersync::combinator::{Select, Either};
        let fut1 = $crate::runtime::select_arm!($f1 $(, $g1)?);
        let fut2 = $crate::runtime::select_arm!($f2 $(, $g2)?);
        match Select::new(Box::pin(fut1), Box::pin(fut2)).await {
            Ok(Either::Left($p1)) => $b1
            Ok(Either::Right($p2)) => $b2
            Err(_) => unreachable!("select future polled after completion"),
        }
    }};

    // ── 3 branches ──────────────────────────────────────────────────
    (
        $p1:pat = $f1:expr $(, if $g1:expr)? => $b1:block
        $p2:pat = $f2:expr $(, if $g2:expr)? => $b2:block
        $p3:pat = $f3:expr $(, if $g3:expr)? => $b3:block
    ) => {{
        use asupersync::combinator::{Select, Either};
        let fut1 = $crate::runtime::select_arm!($f1 $(, $g1)?);
        let fut2 = $crate::runtime::select_arm!($f2 $(, $g2)?);
        let fut3 = $crate::runtime::select_arm!($f3 $(, $g3)?);
        match Select::new(
            Box::pin(fut1),
            Box::pin(Select::new(Box::pin(fut2), Box::pin(fut3))),
        ).await {
            Ok(Either::Left($p1)) => $b1
            Ok(Either::Right(Ok(Either::Left($p2)))) => $b2
            Ok(Either::Right(Ok(Either::Right($p3)))) => $b3
            _ => unreachable!("select future polled after completion"),
        }
    }};

    // ── 4 branches ──────────────────────────────────────────────────
    (
        $p1:pat = $f1:expr $(, if $g1:expr)? => $b1:block
        $p2:pat = $f2:expr $(, if $g2:expr)? => $b2:block
        $p3:pat = $f3:expr $(, if $g3:expr)? => $b3:block
        $p4:pat = $f4:expr $(, if $g4:expr)? => $b4:block
    ) => {{
        use asupersync::combinator::{Select, Either};
        let fut1 = $crate::runtime::select_arm!($f1 $(, $g1)?);
        let fut2 = $crate::runtime::select_arm!($f2 $(, $g2)?);
        let fut3 = $crate::runtime::select_arm!($f3 $(, $g3)?);
        let fut4 = $crate::runtime::select_arm!($f4 $(, $g4)?);
        match Select::new(
            Box::pin(Select::new(Box::pin(fut1), Box::pin(fut2))),
            Box::pin(Select::new(Box::pin(fut3), Box::pin(fut4))),
        ).await {
            Ok(Either::Left(Ok(Either::Left($p1)))) => $b1
            Ok(Either::Left(Ok(Either::Right($p2)))) => $b2
            Ok(Either::Right(Ok(Either::Left($p3)))) => $b3
            Ok(Either::Right(Ok(Either::Right($p4)))) => $b4
            _ => unreachable!("select future polled after completion"),
        }
    }};

    // ── 5 branches ──────────────────────────────────────────────────
    (
        $p1:pat = $f1:expr $(, if $g1:expr)? => $b1:block
        $p2:pat = $f2:expr $(, if $g2:expr)? => $b2:block
        $p3:pat = $f3:expr $(, if $g3:expr)? => $b3:block
        $p4:pat = $f4:expr $(, if $g4:expr)? => $b4:block
        $p5:pat = $f5:expr $(, if $g5:expr)? => $b5:block
    ) => {{
        use asupersync::combinator::{Select, Either};
        let fut1 = $crate::runtime::select_arm!($f1 $(, $g1)?);
        let fut2 = $crate::runtime::select_arm!($f2 $(, $g2)?);
        let fut3 = $crate::runtime::select_arm!($f3 $(, $g3)?);
        let fut4 = $crate::runtime::select_arm!($f4 $(, $g4)?);
        let fut5 = $crate::runtime::select_arm!($f5 $(, $g5)?);
        match Select::new(
            Box::pin(Select::new(Box::pin(fut1), Box::pin(fut2))),
            Box::pin(Select::new(
                Box::pin(fut3),
                Box::pin(Select::new(Box::pin(fut4), Box::pin(fut5))),
            )),
        ).await {
            Ok(Either::Left(Ok(Either::Left($p1)))) => $b1
            Ok(Either::Left(Ok(Either::Right($p2)))) => $b2
            Ok(Either::Right(Ok(Either::Left($p3)))) => $b3
            Ok(Either::Right(Ok(Either::Right(Ok(Either::Left($p4)))))) => $b4
            Ok(Either::Right(Ok(Either::Right(Ok(Either::Right($p5)))))) => $b5
            _ => unreachable!("select future polled after completion"),
        }
    }};
}

/// Helper: conditionally substitute `pending()` when a guard is false.
/// `Box::pins` inner futures so `SelectEither` gets Unpin inputs.
macro_rules! select_arm {
    // With guard — wrap in SelectEither with Box::pin'd inner futures
    ($fut:expr, $guard:expr) => {
        if $guard {
            $crate::runtime::select_either::left(Box::pin($fut))
        } else {
            $crate::runtime::select_either::right(Box::pin(std::future::pending()))
        }
    };
    // Without guard — return future as-is (gets Box::pin'd in select! body)
    ($fut:expr) => {
        $fut
    };
}

pub(crate) use select;
pub(crate) use select_arm;

/// Helper module for guard-based select arms.
/// When a guard is present, both branches must have the same type,
/// so we wrap in an enum that resolves to the inner future's output.
pub(crate) mod select_either {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    pub enum SelectEither<A, B> {
        Left(A),
        Right(B),
    }

    pub const fn left<A, B>(a: A) -> SelectEither<A, B> {
        SelectEither::Left(a)
    }

    pub const fn right<A, B>(b: B) -> SelectEither<A, B> {
        SelectEither::Right(b)
    }

    impl<A, B, T> Future for SelectEither<A, B>
    where
        A: Future<Output = T> + Unpin,
        B: Future<Output = T> + Unpin,
    {
        type Output = T;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
            match self.get_mut() {
                Self::Left(a) => Pin::new(a).poll(cx),
                Self::Right(b) => Pin::new(b).poll(cx),
            }
        }
    }
}

/// Declare an async test.
///
/// Wraps the body in a one-shot runtime, since asupersync has no attribute
/// macro equivalent to `#[tokio::test]`.
///
/// Usage:
/// ```ignore
/// crate::runtime::async_test! {
///     async fn test_something() {
///         // async test body
///     }
/// }
/// ```
#[macro_export]
macro_rules! async_test {
    (async fn $name:ident() $body:block) => {
        #[test]
        fn $name() {
            $crate::runtime::task::block_on(async $body)
        }
    };
}
