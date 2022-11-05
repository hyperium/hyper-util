use hyper::rt::Executor;
use std::future::Future;

/// Future executor that utilises `tokio` threads.
#[non_exhaustive]
#[derive(Default, Debug, Clone, Copy)]
pub struct TokioExecutor {}

impl<Fut> Executor<Fut> for TokioExecutor
where
    Fut: Future + Send + 'static,
    Fut::Output: Send + 'static,
{
    fn execute(&self, fut: Fut) {
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
    use crate::rt::tokio_executor::TokioExecutor;
    use hyper::rt::Executor;
    use tokio::sync::oneshot;

    #[cfg(not(miri))]
    #[tokio::test]
    async fn simple_execute() -> Result<(), Box<dyn std::error::Error>> {
        let (tx, rx) = oneshot::channel();
        let executor = TokioExecutor::new();
        executor.execute(async move {
            tx.send(()).unwrap();
        });
        rx.await.map_err(Into::into)
    }
}
