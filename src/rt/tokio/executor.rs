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
/// use hyper_util::rt::tokio::TokioLocalExecutor;
///
/// let runtime = tokio::runtime::Builder::new_current_thread()
///     .build()
///     .unwrap();
/// let local_set = tokio::task::LocalSet::new();
/// local_set.block_on(&runtime, async move {
///     let executor = TokioLocalExecutor::new();
///
///     // Use the executor...
/// });
/// ```
///
/// Execute tasks within a [`LocalRuntime`][tokio::runtime::LocalRuntime].
///
/// ```
/// use hyper_util::rt::tokio::TokioLocalExecutor;
///
/// let runtime = tokio::runtime::LocalRuntime::new().unwrap();
/// runtime.block_on(async move {
///     let executor = TokioLocalExecutor::new();
///
///     // Use the executor...
/// });
/// ```
#[non_exhaustive]
#[derive(Default, Debug, Clone)]
pub struct TokioLocalExecutor {}

/// Future executor backed by a runtime [`Handle`].
///
/// This executor, like [`TokioExecutor`], utilises [`tokio`] threads. This
/// executor spawns tasks using [`Handle::spawn()`] rather than
/// [`tokio::spawn()`], however.
///
/// A runtime handle may be obtained by calling [`Runtime::handle()`].
///
/// This is useful for situations in which you wish to run tasks on a
/// *separate* runtime. If your application only runs using a single tokio
/// runtime, [`TokioExecutor`] should be used instead.
///
/// This may be applicable to those configuring hyper clients and servers in
/// applications running on NUMA (Non-Uniform Memory Awareneses) systems, or
/// if you wish to manage provision separate resources for background tasks
/// associated with a client or server.
///
/// See the [`tokio::runtime`] documentation for more information about
/// choosing the correct runtime for your application.
///
/// # Examples
///
/// ```
/// use hyper_util::rt::tokio::TokioHandleExecutor;
///
/// let runtime = tokio::runtime::Builder::new_current_thread()
///     .build()
///     .unwrap();
/// let handle = runtime.handle().clone();
/// let executor = TokioHandleExecutor::new(handle);
/// ```
///
/// [`Handle`]: tokio::runtime::Handle
/// [`Handle::spawn()`]: tokio::runtime::Handle::spawn
/// [`Runtime::handle()`]: tokio::runtime::Runtime::handle
pub struct TokioHandleExecutor {
    handle: tokio::runtime::Handle,
}

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

// ===== impl TokioLocalExecutor =====

impl TokioLocalExecutor {
    /// Create a new executor that relies on [`tokio::task::spawn_local()`] to execute futures.
    pub fn new() -> Self {
        Self {}
    }
}

impl<Fut> Executor<Fut> for TokioLocalExecutor
where
    Fut: Future + 'static,
    Fut::Output: 'static,
{
    fn execute(&self, fut: Fut) {
        tokio::task::spawn_local(fut);
    }
}

// ===== impl TokioHandleExecutor =====

impl TokioHandleExecutor {
    /// TK
    pub fn new(handle: tokio::runtime::Handle) -> Self {
        Self { handle }
    }
}

impl<Fut> Executor<Fut> for TokioHandleExecutor
where
    Fut: Future + Send + 'static,
    Fut::Output: Send + 'static,
{
    fn execute(&self, fut: Fut) {
        self.handle.spawn(fut);
    }
}

#[cfg(test)]
mod tests {
    use crate::rt::{
        TokioExecutor,
        tokio::{TokioHandleExecutor, TokioLocalExecutor},
    };
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
            let executor = TokioLocalExecutor::new();
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
            let executor = TokioLocalExecutor::new();
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

    #[test]
    fn handle_executor_can_execute_task_on_separate_runtime() {
        // Create a "foreground" runtime we will run our top-level on.
        let rt = tokio::runtime::Builder::new_current_thread()
            .worker_threads(1)
            .name("foreground")
            .build()
            .unwrap();

        // Create a "background" runtime, whose handle will be used to spawn
        // background tasks by our executor.
        let background = tokio::runtime::Builder::new_current_thread()
            .worker_threads(1)
            .name("background")
            .build()
            .unwrap();
        let handle = background.handle().clone();
        let executor = TokioHandleExecutor::new(handle);

        // Begin running the background runtime on a separate worker thread.
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let worker = std::thread::Builder::new()
            .name("execute-task-on-separate-runtime-worker".into())
            .spawn(move || {
                use futures_util::FutureExt;
                let fut = shutdown_rx.map(drop);
                background.block_on(fut);
            })
            .expect("should spawn thread");

        // Run a future that, when polled, spawns a background task onto the
        // handle executor. This background task retrieves the name of the
        // runtime that it is running on, and sends the name back to its
        // caller. The parent then asserts that the child was run on the
        // "background" runtime.
        rt.block_on(async move {
            let handle = tokio::runtime::Handle::current();
            let name = handle.name().unwrap().to_string();
            assert_eq!(
                name, "foreground",
                "future should be spawned onto foreground runtime"
            );

            let (tx, rx) = oneshot::channel();
            let fut = async move {
                let handle = tokio::runtime::Handle::current();
                let name = handle.name().unwrap().to_string();
                tx.send(name).unwrap();
            };

            executor.execute(fut);
            let name = rx.await.unwrap();
            assert_eq!(
                name, "background",
                "worker should be spawned onto background runtime"
            );
        });

        // Signal to the background runtime that it should shutdown now, and
        // then wait for the thread running it to finish.
        shutdown_tx
            .send(())
            .expect("shutdown signal should be sent");
        worker.join().expect("worker thread should finish");
    }
}
