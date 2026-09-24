use hyper::rt::Executor;

#[cfg(feature = "rt-tracing-exec-force")]
use tracing::instrument::Instrument;

/// Future executor that utilises `tokio` threads.
///
/// Spawned futures do not inherit the current tracing span, even when the
/// `tracing` feature is enabled. To propagate spans, wrap this executor in
/// one of the components from [`rt::tracing`](crate::rt::tracing), such as
/// [`CurrentSpanExecutor`](crate::rt::CurrentSpanExecutor) (available with
/// the `tracing` feature).
///
/// See the module-level documentation of [`rt::tracing`](crate::rt::tracing)
/// for more information about propagating [`tracing`] spans to spawned tasks.
///
/// The temporary `rt-tracing-exec-force` feature restores propagation of the
/// current span for libraries that do not allow customizing their executor.
/// It is excluded from `full` and may be removed in a future breaking release.
#[non_exhaustive]
#[derive(Default, Debug, Clone)]
pub struct TokioExecutor {}

/// Future executor that utilises local `tokio` threads.
///
/// This executor relies on [`tokio::task::spawn_local()`] to execute futures.
/// This is of use when dealing with a server or client implementation that is
/// `!Send`, i.e. it must be run on the same thread.
///
/// *Note:* This cannot be used within a task spawned with [`tokio::spawn()`].
///
/// # Examples
///
/// Execute tasks within a [`LocalSet`][tokio::task::LocalSet].
///
/// ```
/// use hyper_util::rt::tokio::LocalExecutor;
///
/// let runtime = tokio::runtime::Builder::new_current_thread()
///     .build()
///     .unwrap();
/// let local_set = tokio::task::LocalSet::new();
/// local_set.block_on(&runtime, async move {
///     let executor = LocalExecutor::new();
///
///     // Use the executor...
/// });
/// ```
///
/// Execute tasks within a [`LocalRuntime`][tokio::runtime::LocalRuntime].
///
/// ```
/// use hyper_util::rt::tokio::LocalExecutor;
///
/// let runtime = tokio::runtime::LocalRuntime::new().unwrap();
/// runtime.block_on(async move {
///     let executor = LocalExecutor::new();
///
///     // Use the executor...
/// });
/// ```
#[non_exhaustive]
#[derive(Default, Debug, Clone)]
pub struct LocalExecutor {}

// ===== impl TokioExecutor =====

impl<Fut> Executor<Fut> for TokioExecutor
where
    Fut: Future + Send + 'static,
    Fut::Output: Send + 'static,
{
    fn execute(&self, fut: Fut) {
        #[cfg(feature = "rt-tracing-exec-force")]
        tokio::spawn(fut.in_current_span());

        #[cfg(not(feature = "rt-tracing-exec-force"))]
        tokio::spawn(fut);
    }
}

impl TokioExecutor {
    /// Create new executor that relies on [`tokio::spawn`] to execute futures.
    pub fn new() -> Self {
        Self {}
    }
}

// ===== impl LocalExecutor =====

impl LocalExecutor {
    /// Create a new executor that relies on [`tokio::task::spawn_local()`] to execute futures.
    pub fn new() -> Self {
        Self {}
    }
}

impl<Fut> Executor<Fut> for LocalExecutor
where
    Fut: Future + 'static,
    Fut::Output: 'static,
{
    fn execute(&self, fut: Fut) {
        tokio::task::spawn_local(fut);
    }
}

#[cfg(test)]
mod tests {
    use crate::rt::{TokioExecutor, tokio::executor::LocalExecutor};
    use hyper::rt::Executor;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn simple_execute() -> Result<(), Box<dyn std::error::Error>> {
        let (tx, rx) = oneshot::channel();
        let executor = TokioExecutor::new();
        executor.execute(async move {
            tx.send(()).unwrap();
        });
        rx.await.map_err(Into::into)
    }

    #[cfg(feature = "tracing")]
    #[tokio::test]
    async fn execute_tracing_span() {
        // The current-thread runtime keeps the subscriber active while the
        // spawned future is polled, after the caller has exited its span.
        let _subscriber = tracing::subscriber::set_default(tracing_subscriber::registry());
        let span = tracing::info_span!("caller");
        assert!(span.id().is_some());
        let (tx, rx) = oneshot::channel();

        {
            let _entered = span.enter();
            TokioExecutor::new().execute(async move {
                tx.send(tracing::Span::current().id()).unwrap();
            });
        }

        let spawned_span = rx.await.unwrap();
        if cfg!(feature = "rt-tracing-exec-force") {
            assert_eq!(spawned_span, span.id());
        } else {
            assert_eq!(spawned_span, None);
        }
    }

    #[test]
    fn local_executor_works_with_local_set() {
        // A future that will spawn a background task that increments a
        // (single-threaded) reference-counted integer, indicating via a
        // oneshot channel when that has been done.
        //
        // This is a `!Send` future, because it uses `Rc<T>`.
        let fut = async {
            use std::{rc::Rc, sync::Mutex};
            let here = Rc::new(Mutex::new(0));
            let (tx, rx) = oneshot::channel();

            // Spawn the background task on the local set.
            let executor = LocalExecutor::new();
            let there = here.clone();
            let fut = async move {
                // *there += 42;
                *there.lock().unwrap() = 42;
                tx.send(()).unwrap();
            };

            executor.execute(fut);

            rx.await.unwrap();
            assert_eq!(*here.lock().unwrap(), 42);
        };

        // NOTE: We can't assert negative bounds but this can be uncommented
        // to check that `fut` above is a `!Send` future.
        // fn assert_sync<T: Send>(_: &T) {}
        // assert_sync(&fut);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let local_set = tokio::task::LocalSet::new();
        local_set.block_on(&runtime, fut);
    }

    #[test]
    fn local_executor_works_with_local_runtime() {
        // A future that will spawn a background task that increments a
        // (single-threaded) reference-counted integer, indicating via a
        // oneshot channel when that has been done.
        //
        // This is a `!Send` future, because it uses `Rc<T>`.
        let fut = async {
            use std::{rc::Rc, sync::Mutex};
            let here = Rc::new(Mutex::new(0));
            let (tx, rx) = oneshot::channel();

            // Spawn the background task on the local set.
            let executor = LocalExecutor::new();
            let there = here.clone();
            let fut = async move {
                // *there += 42;
                *there.lock().unwrap() = 42;
                tx.send(()).unwrap();
            };

            executor.execute(fut);

            rx.await.unwrap();
            assert_eq!(*here.lock().unwrap(), 42);
        };

        // NOTE: We can't assert negative bounds but this can be uncommented
        // to check that `fut` above is a `!Send` future.
        // fn assert_sync<T: Send>(_: &T) {}
        // assert_sync(&fut);

        let runtime = tokio::runtime::LocalRuntime::new().unwrap();
        runtime.block_on(fut);
    }
}
