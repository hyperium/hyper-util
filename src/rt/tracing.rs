use hyper::rt::Executor;
use tracing::instrument::{Instrument, Instrumented};

/// An executor that propagates the current tracing span to its futures.
///
/// The span is captured when [`execute`](Executor::execute) is called, and is
/// entered each time the future is polled or dropped. Execution is delegated to
/// the wrapped executor, without requiring a particular runtime.
///
/// Requires the `tracing` feature.
///
/// # Example
///
/// ```
/// # #[cfg(feature = "tokio")]
/// # {
/// use hyper_util::rt::{TokioExecutor, TracingExecutor};
///
/// let executor = TracingExecutor::new(TokioExecutor::new());
/// # }
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct TracingExecutor<E> {
    inner: E,
}

impl<E> TracingExecutor<E> {
    /// Wrap an executor to propagate the current tracing span to its futures.
    pub fn new(inner: E) -> Self {
        Self { inner }
    }
}

impl<E, F> Executor<F> for TracingExecutor<E>
where
    E: Executor<Instrumented<F>>,
    F: Future,
{
    fn execute(&self, future: F) {
        self.inner.execute(future.in_current_span());
    }
}

#[cfg(test)]
mod tests {
    use super::TracingExecutor;
    use hyper::rt::Executor;
    use std::{cell::RefCell, future::poll_fn, pin::Pin, task::Poll};

    #[derive(Default)]
    struct DeferredExecutor<'a> {
        future: RefCell<Option<Pin<Box<dyn Future<Output = ()> + 'a>>>>,
    }

    impl<'a, F: Future<Output = ()> + 'a> Executor<F> for &DeferredExecutor<'a> {
        fn execute(&self, future: F) {
            *self.future.borrow_mut() = Some(Box::pin(future));
        }
    }

    #[test]
    fn propagates_span_from_execute_on_each_poll() {
        let _subscriber = tracing::subscriber::set_default(tracing_subscriber::registry());
        let construction_span = tracing::info_span!("construction");
        let execution_span = tracing::info_span!("execution");
        let polling_span = tracing::info_span!("polling");
        assert!(execution_span.id().is_some());

        // Borrowing a local executor and future also checks that the wrapper
        // does not impose Send or 'static bounds on the inner executor.
        let polls = RefCell::new(0);
        let inner = DeferredExecutor::default();
        let executor = construction_span.in_scope(|| TracingExecutor::new(&inner));
        execution_span.in_scope(|| {
            executor.execute(poll_fn(|_| {
                assert_eq!(tracing::Span::current().id(), execution_span.id());
                *polls.borrow_mut() += 1;
                if *polls.borrow() == 1 {
                    Poll::Pending
                } else {
                    Poll::Ready(())
                }
            }));
        });

        let _entered = polling_span.enter();
        let mut task = tokio_test::task::spawn(inner.future.borrow_mut().take().unwrap());
        assert!(task.poll().is_pending());
        assert_eq!(tracing::Span::current().id(), polling_span.id());
        assert!(task.poll().is_ready());
        assert_eq!(tracing::Span::current().id(), polling_span.id());
        assert_eq!(*polls.borrow(), 2);
    }
}
