//! Singleton pools
//!
//! The singleton pool combines a MakeService that should only produce a single
//! active connection. It can bundle all concurrent calls to it, so that only
//! one connection is made. All calls to the singleton will return a clone of
//! the inner service once established.
//!
//! This fits the HTTP/2 case well.

use std::sync::{Arc, Mutex};
use std::task::{self, Poll};

use tokio::sync::oneshot;
use tower_service::Service;

use self::internal::{DitchGuard, SingletonError, SingletonFuture};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A singleton pool over an inner service.
///
/// The singleton wraps an inner service maker, bundling all calls to ensure
/// only one service is created. Once made, it returns clones of the made
/// service.
#[derive(Debug)]
pub struct Singleton<M, Dst>
where
    M: Service<Dst>,
{
    mk_svc: M,
    state: Arc<Mutex<State<M::Response>>>,
}

#[derive(Debug)]
enum State<S> {
    Empty,
    Making(Vec<oneshot::Sender<S>>),
    Made(S),
}

impl<M, Target> Singleton<M, Target>
where
    M: Service<Target>,
    M::Response: Clone,
{
    /// Create a new singleton pool over an inner make service.
    pub fn new(mk_svc: M) -> Self {
        Singleton {
            mk_svc,
            state: Arc::new(Mutex::new(State::Empty)),
        }
    }

    // pub fn reset?
    // pub fn retain?
}

impl<M, Target> Service<Target> for Singleton<M, Target>
where
    M: Service<Target>,
    M::Response: Clone,
    M::Error: Into<BoxError>,
{
    type Response = M::Response;
    type Error = SingletonError;
    type Future = SingletonFuture<M::Future, M::Response>;

    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        if let State::Empty = *self.state.lock().unwrap() {
            return self
                .mk_svc
                .poll_ready(cx)
                .map_err(|e| SingletonError(e.into()));
        }
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, dst: Target) -> Self::Future {
        let mut locked = self.state.lock().unwrap();
        match *locked {
            State::Empty => {
                let fut = self.mk_svc.call(dst);
                *locked = State::Making(Vec::new());
                SingletonFuture::Driving {
                    future: fut,
                    singleton: DitchGuard(Arc::downgrade(&self.state)),
                }
            }
            State::Making(ref mut waiters) => {
                let (tx, rx) = oneshot::channel();
                waiters.push(tx);
                SingletonFuture::Waiting { rx }
            }
            State::Made(ref svc) => SingletonFuture::Made {
                svc: Some(svc.clone()),
            },
        }
    }
}

impl<M, Target> Clone for Singleton<M, Target>
where
    M: Service<Target> + Clone,
{
    fn clone(&self) -> Self {
        Self {
            mk_svc: self.mk_svc.clone(),
            state: self.state.clone(),
        }
    }
}

// Holds some "pub" items that otherwise shouldn't be public.
mod internal {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Mutex, Weak};
    use std::task::{self, Poll};

    use futures_core::ready;
    use pin_project_lite::pin_project;
    use tokio::sync::oneshot;

    use super::{BoxError, State};

    pin_project! {
        #[project = SingletonFutureProj]
        pub enum SingletonFuture<F, S> {
            Driving {
                #[pin]
                future: F,
                singleton: DitchGuard<S>,
            },
            Waiting {
                rx: oneshot::Receiver<S>,
            },
            Made {
                svc: Option<S>,
            },
        }
    }

    // XXX: pub because of the enum SingletonFuture
    pub struct DitchGuard<S>(pub(super) Weak<Mutex<State<S>>>);

    impl<F, S, E> Future for SingletonFuture<F, S>
    where
        F: Future<Output = Result<S, E>>,
        E: Into<BoxError>,
        S: Clone,
    {
        type Output = Result<S, SingletonError>;

        fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
            match self.project() {
                SingletonFutureProj::Driving { future, singleton } => {
                    match ready!(future.poll(cx)) {
                        Ok(svc) => {
                            if let Some(state) = singleton.0.upgrade() {
                                let mut locked = state.lock().unwrap();
                                singleton.0 = Weak::new();
                                match std::mem::replace(&mut *locked, State::Made(svc.clone())) {
                                    State::Making(waiters) => {
                                        for tx in waiters {
                                            let _ = tx.send(svc.clone());
                                        }
                                    }
                                    State::Empty | State::Made(_) => {
                                        // shouldn't happen!
                                    }
                                }
                            }
                            Poll::Ready(Ok(svc))
                        }
                        Err(e) => {
                            if let Some(state) = singleton.0.upgrade() {
                                let mut locked = state.lock().unwrap();
                                singleton.0 = Weak::new();
                                *locked = State::Empty;
                            }
                            Poll::Ready(Err(SingletonError(e.into())))
                        }
                    }
                }
                SingletonFutureProj::Waiting { rx } => match ready!(Pin::new(rx).poll(cx)) {
                    Ok(svc) => Poll::Ready(Ok(svc)),
                    Err(_canceled) => Poll::Ready(Err(SingletonError(Canceled.into()))),
                },
                SingletonFutureProj::Made { svc } => Poll::Ready(Ok(svc.take().unwrap())),
            }
        }
    }

    impl<S> Drop for DitchGuard<S> {
        fn drop(&mut self) {
            if let Some(state) = self.0.upgrade() {
                if let Ok(mut locked) = state.lock() {
                    *locked = State::Empty;
                }
            }
        }
    }

    // An opaque error type. By not exposing the type, nor being specifically
    // Box<dyn Error>, we can _change_ the type once we no longer need the Canceled
    // error type. This will be possible with the refactor to baton passing.
    #[derive(Debug)]
    pub struct SingletonError(pub(super) BoxError);

    impl std::fmt::Display for SingletonError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("singleton connection error")
        }
    }

    impl std::error::Error for SingletonError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&*self.0)
        }
    }

    #[derive(Debug)]
    struct Canceled;

    impl std::fmt::Display for Canceled {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("singleton connection canceled")
        }
    }

    impl std::error::Error for Canceled {}
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::Poll;

    use tower_service::Service;

    use super::Singleton;

    #[tokio::test]
    async fn first_call_drives_subsequent_wait() {
        let (mock_svc, mut handle) = tower_test::mock::pair::<(), &'static str>();

        let mut singleton = Singleton::new(mock_svc);

        handle.allow(1);
        crate::common::future::poll_fn(|cx| singleton.poll_ready(cx))
            .await
            .unwrap();
        // First call: should go into Driving
        let fut1 = singleton.call(());
        // Second call: should go into Waiting
        let fut2 = singleton.call(());

        // Expect exactly one request to the inner service
        let ((), send_response) = handle.next_request().await.unwrap();
        send_response.send_response("svc");

        // Both futures should resolve to the same value
        assert_eq!(fut1.await.unwrap(), "svc");
        assert_eq!(fut2.await.unwrap(), "svc");
    }

    #[tokio::test]
    async fn made_state_returns_immediately() {
        let (mock_svc, mut handle) = tower_test::mock::pair::<(), &'static str>();
        let mut singleton = Singleton::new(mock_svc);

        handle.allow(1);
        crate::common::future::poll_fn(|cx| singleton.poll_ready(cx))
            .await
            .unwrap();
        // Drive first call to completion
        let fut1 = singleton.call(());
        let ((), send_response) = handle.next_request().await.unwrap();
        send_response.send_response("svc");
        assert_eq!(fut1.await.unwrap(), "svc");

        // Second call should not hit inner service
        let res = singleton.call(()).await.unwrap();
        assert_eq!(res, "svc");
    }

    #[tokio::test]
    async fn cancel_waiter_does_not_affect_others() {
        let (mock_svc, mut handle) = tower_test::mock::pair::<(), &'static str>();
        let mut singleton = Singleton::new(mock_svc);

        crate::common::future::poll_fn(|cx| singleton.poll_ready(cx))
            .await
            .unwrap();
        let fut1 = singleton.call(());
        let fut2 = singleton.call(());
        drop(fut2); // cancel one waiter

        let ((), send_response) = handle.next_request().await.unwrap();
        send_response.send_response("svc");

        assert_eq!(fut1.await.unwrap(), "svc");
    }

    // TODO: this should be able to be improved with a cooperative baton refactor
    #[tokio::test]
    async fn cancel_driver_cancels_all() {
        let (mock_svc, mut handle) = tower_test::mock::pair::<(), &'static str>();
        let mut singleton = Singleton::new(mock_svc);

        crate::common::future::poll_fn(|cx| singleton.poll_ready(cx))
            .await
            .unwrap();
        let mut fut1 = singleton.call(());
        let fut2 = singleton.call(());

        // poll driver just once, and then drop
        crate::common::future::poll_fn(move |cx| {
            let _ = Pin::new(&mut fut1).poll(cx);
            Poll::Ready(())
        })
        .await;

        let ((), send_response) = handle.next_request().await.unwrap();
        send_response.send_response("svc");

        assert_eq!(
            fut2.await.unwrap_err().0.to_string(),
            "singleton connection canceled"
        );
    }
}
