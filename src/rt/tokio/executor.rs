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

#[cfg(test)]
mod tests {
    use crate::rt::TokioExecutor;
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
}
