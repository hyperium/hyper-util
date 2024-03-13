//! Utility to gracefully shutdown a server.
//!
//! This module provides a [`GracefulShutdown`] type,
//! which can be used to gracefully shutdown a server.
//!
//! See <https://github.com/hyperium/hyper-util/blob/master/examples/server_graceful.rs>
//! for an example of how to use this.

use pin_project_lite::pin_project;
use slab::Slab;
use std::{
    fmt::{self, Debug},
    future::Future,
    pin::Pin,
    sync::{atomic::AtomicUsize, Arc, Mutex},
    task::{self, Poll, Waker},
};

/// A graceful shutdown watcher
pub struct GracefulShutdown {
    // state used to keep track of all futures that are being watched
    state: Arc<GracefulState>,
    // state that the watched futures to know when shutdown signal is received
    future_state: Arc<GracefulState>,
}

impl Debug for GracefulShutdown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GracefulShutdown").finish()
    }
}

impl Default for GracefulShutdown {
    fn default() -> Self {
        Self::new()
    }
}

impl GracefulShutdown {
    /// Create a new graceful shutdown watcher
    pub fn new() -> Self {
        Self {
            state: Arc::new(GracefulState {
                counter: AtomicUsize::new(0),
                waker_list: Mutex::new(Slab::new()),
            }),
            future_state: Arc::new(GracefulState {
                counter: AtomicUsize::new(1),
                waker_list: Mutex::new(Slab::new()),
            }),
        }
    }

    /// Get a graceful shutdown watcher
    pub fn watcher(&self) -> GracefulWatcher {
        self.state.subscribe();
        GracefulWatcher {
            state: self.state.clone(),
            future_state: self.future_state.clone(),
        }
    }

    /// Wait for a graceful shutdown
    pub fn shutdown(self) -> GracefulWaiter {
        // prepare futures and signal them to shutdown
        self.future_state.unsubscribe();

        // return the future to wait for shutdown
        GracefulWaiter::new(self.state)
    }
}

/// A graceful shutdown watcher.
pub struct GracefulWatcher {
    state: Arc<GracefulState>,
    future_state: Arc<GracefulState>,
}

impl Drop for GracefulWatcher {
    fn drop(&mut self) {
        self.state.unsubscribe();
    }
}

impl GracefulWatcher {
    /// Watch a future for graceful shutdown,
    /// returning a wrapper that can be awaited on.
    pub fn watch<C: GracefulConnection>(&self, conn: C) -> GracefulFuture<C> {
        // add a counter for this future to ensure it is taken into account
        self.state.subscribe();

        let cancel = GracefulWaiter::new(self.future_state.clone());
        let future = GracefulConnectionFuture::new(conn, cancel);

        // return the graceful future, ready to be shutdown,
        // and handling all the hyper graceful logic
        GracefulFuture {
            future,
            state: Some(self.state.clone()),
        }
    }
}

struct GracefulState {
    counter: AtomicUsize,
    waker_list: Mutex<Slab<Option<Waker>>>,
}

impl GracefulState {
    fn subscribe(&self) {
        self.counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn unsubscribe(&self) {
        if self
            .counter
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst)
            == 1
        {
            let mut waker_list = self.waker_list.lock().unwrap();
            for (_, waker) in waker_list.iter_mut() {
                if let Some(waker) = waker.take() {
                    waker.wake();
                }
            }
        }
    }
}

pin_project! {
    /// A wrapper around a future that's being watched for graceful shutdown.
    ///
    /// This is returned by [`GracefulShutdown::watch`].
    ///
    /// # Panics
    ///
    /// This future might panic if it is polled
    /// after the internal future has already returned `Poll::Ready` before.
    ///
    /// Whether or not this future panics in such cases is an implementation detail
    /// and should not be relied upon.
    pub struct GracefulFuture<C> {
        #[pin]
        future: GracefulConnectionFuture<C, GracefulWaiter>,
        state: Option<Arc<GracefulState>>,
    }
}

impl<C> Debug for GracefulFuture<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GracefulFuture").finish()
    }
}

impl<C> Future for GracefulFuture<C>
where
    C: GracefulConnection,
{
    type Output = C::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        match this.future.poll(cx) {
            Poll::Ready(v) => {
                this.state.take().unwrap().unsubscribe();
                Poll::Ready(v)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// A future that blocks until a graceful shutdown is complete.
pub struct GracefulWaiter {
    state: GracefulWaiterState,
}

impl GracefulWaiter {
    fn new(state: Arc<GracefulState>) -> Self {
        Self {
            state: GracefulWaiterState { state, key: None },
        }
    }
}

impl Future for GracefulWaiter {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        let state = &mut self.state;

        if state
            .state
            .counter
            .load(std::sync::atomic::Ordering::SeqCst)
            == 0
        {
            return Poll::Ready(());
        }

        let mut waker_list = state.state.waker_list.lock().unwrap();

        if state
            .state
            .counter
            .load(std::sync::atomic::Ordering::SeqCst)
            == 0
        {
            // check again in case of race condition
            return Poll::Ready(());
        }

        let waker = Some(cx.waker().clone());
        state.key = Some(match state.key.take() {
            Some(key) => {
                *waker_list.get_mut(key).unwrap() = waker;
                key
            }
            None => waker_list.insert(waker),
        });

        Poll::Pending
    }
}

struct GracefulWaiterState {
    state: Arc<GracefulState>,
    key: Option<usize>,
}

impl Drop for GracefulWaiterState {
    /// When the waiter is dropped, we need to remove its waker from the waker list.
    /// As to ensure the graceful waiter is cancel safe.
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            let mut wakers = self.state.waker_list.lock().unwrap();
            wakers.remove(key);
        }
    }
}

pin_project! {
    struct GracefulConnectionFuture<C, F> {
        #[pin]
        conn: C,
        #[pin]
        cancel: F,
        cancelled: bool,
    }
}

impl<C, F> GracefulConnectionFuture<C, F> {
    fn new(conn: C, cancel: F) -> Self {
        Self {
            conn,
            cancel,
            cancelled: false,
        }
    }
}

impl<C, F> Debug for GracefulConnectionFuture<C, F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GracefulConnectionFuture").finish()
    }
}

impl<C, F> Future for GracefulConnectionFuture<C, F>
where
    C: GracefulConnection,
    F: Future,
{
    type Output = C::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        if !*this.cancelled {
            if let Poll::Ready(_) = this.cancel.poll(cx) {
                *this.cancelled = true;
                this.conn.as_mut().graceful_shutdown();
            }
        }
        this.conn.poll(cx)
    }
}

/// An internal utility trait as an umbrella target for all (hyper) connection
/// types that the [`GracefulShutdown`] can watch.
pub trait GracefulConnection: Future<Output = Result<(), Self::Error>> + private::Sealed {
    /// The error type returned by the connection when used as a future.
    type Error;

    /// Start a graceful shutdown process for this connection.
    fn graceful_shutdown(self: Pin<&mut Self>);
}

#[cfg(feature = "http1")]
impl<I, B, S> GracefulConnection for hyper::server::conn::http1::Connection<I, S>
where
    S: hyper::service::HttpService<hyper::body::Incoming, ResBody = B>,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    B: hyper::body::Body + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Error = hyper::Error;

    fn graceful_shutdown(self: Pin<&mut Self>) {
        hyper::server::conn::http1::Connection::graceful_shutdown(self);
    }
}

#[cfg(feature = "http2")]
impl<I, B, S, E> GracefulConnection for hyper::server::conn::http2::Connection<I, S, E>
where
    S: hyper::service::HttpService<hyper::body::Incoming, ResBody = B>,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    B: hyper::body::Body + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    E: hyper::rt::bounds::Http2ServerConnExec<S::Future, B>,
{
    type Error = hyper::Error;

    fn graceful_shutdown(self: Pin<&mut Self>) {
        hyper::server::conn::http2::Connection::graceful_shutdown(self);
    }
}

#[cfg(feature = "server-auto")]
impl<'a, I, B, S, E> GracefulConnection for crate::server::conn::auto::Connection<'a, I, S, E>
where
    S: hyper::service::Service<http::Request<hyper::body::Incoming>, Response = http::Response<B>>,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    S::Future: 'static,
    I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
    B: hyper::body::Body + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    E: hyper::rt::bounds::Http2ServerConnExec<S::Future, B>,
{
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn graceful_shutdown(self: Pin<&mut Self>) {
        crate::server::conn::auto::Connection::graceful_shutdown(self);
    }
}

#[cfg(feature = "server-auto")]
impl<'a, I, B, S, E> GracefulConnection
    for crate::server::conn::auto::UpgradeableConnection<'a, I, S, E>
where
    S: hyper::service::Service<http::Request<hyper::body::Incoming>, Response = http::Response<B>>,
    S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    S::Future: 'static,
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
    B: hyper::body::Body + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    E: hyper::rt::bounds::Http2ServerConnExec<S::Future, B>,
{
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn graceful_shutdown(self: Pin<&mut Self>) {
        crate::server::conn::auto::UpgradeableConnection::graceful_shutdown(self);
    }
}

mod private {
    pub trait Sealed {}

    #[cfg(feature = "http1")]
    impl<I, B, S> Sealed for hyper::server::conn::http1::Connection<I, S>
    where
        S: hyper::service::HttpService<hyper::body::Incoming, ResBody = B>,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
        B: hyper::body::Body + 'static,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
    }

    #[cfg(feature = "http1")]
    impl<I, B, S> Sealed for hyper::server::conn::http1::UpgradeableConnection<I, S>
    where
        S: hyper::service::HttpService<hyper::body::Incoming, ResBody = B>,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
        B: hyper::body::Body + 'static,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
    }

    #[cfg(feature = "http2")]
    impl<I, B, S, E> Sealed for hyper::server::conn::http2::Connection<I, S, E>
    where
        S: hyper::service::HttpService<hyper::body::Incoming, ResBody = B>,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
        B: hyper::body::Body + 'static,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        E: hyper::rt::bounds::Http2ServerConnExec<S::Future, B>,
    {
    }

    #[cfg(feature = "server-auto")]
    impl<'a, I, B, S, E> Sealed for crate::server::conn::auto::Connection<'a, I, S, E>
    where
        S: hyper::service::Service<
            http::Request<hyper::body::Incoming>,
            Response = http::Response<B>,
        >,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        S::Future: 'static,
        I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static,
        B: hyper::body::Body + 'static,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        E: hyper::rt::bounds::Http2ServerConnExec<S::Future, B>,
    {
    }

    #[cfg(feature = "server-auto")]
    impl<'a, I, B, S, E> Sealed for crate::server::conn::auto::UpgradeableConnection<'a, I, S, E>
    where
        S: hyper::service::Service<
            http::Request<hyper::body::Incoming>,
            Response = http::Response<B>,
        >,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        S::Future: 'static,
        I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
        B: hyper::body::Body + 'static,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        E: hyper::rt::bounds::Http2ServerConnExec<S::Future, B>,
    {
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use pin_project_lite::pin_project;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pin_project! {
        #[derive(Debug)]
        struct DummyConnection<F> {
            #[pin]
            future: F,
            shutdown_counter: Arc<AtomicUsize>,
        }
    }

    impl<F> private::Sealed for DummyConnection<F> {}

    impl<F: Future> GracefulConnection for DummyConnection<F> {
        type Error = ();

        fn graceful_shutdown(self: Pin<&mut Self>) {
            self.shutdown_counter.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl<F: Future> Future for DummyConnection<F> {
        type Output = Result<(), ()>;

        fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
            match self.project().future.poll(cx) {
                Poll::Ready(_) => Poll::Ready(Ok(())),
                Poll::Pending => Poll::Pending,
            }
        }
    }

    #[tokio::test]
    async fn test_graceful_shutdown_ok() {
        let graceful = GracefulShutdown::new();
        let shutdown_counter = Arc::new(AtomicUsize::new(0));
        let (dummy_tx, _) = tokio::sync::broadcast::channel(1);

        for i in 1..=3 {
            let watcher = graceful.watcher();
            let mut dummy_rx = dummy_tx.subscribe();
            let shutdown_counter = shutdown_counter.clone();

            tokio::spawn(async move {
                let future = async move {
                    tokio::time::sleep(std::time::Duration::from_millis(i * 50)).await;
                    let _ = dummy_rx.recv().await;
                };
                let dummy_conn = DummyConnection {
                    future,
                    shutdown_counter,
                };
                let conn = watcher.watch(dummy_conn);
                conn.await.unwrap();
            });
        }

        assert_eq!(shutdown_counter.load(Ordering::SeqCst), 0);
        let _ = dummy_tx.send(());

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                panic!("timeout")
            },
            _ = graceful.shutdown() => {
                assert_eq!(shutdown_counter.load(Ordering::SeqCst), 3);
            }
        }
    }

    #[tokio::test]
    async fn test_graceful_shutdown_delayed_ok() {
        let graceful = GracefulShutdown::new();
        let shutdown_counter = Arc::new(AtomicUsize::new(0));

        for i in 1..=3 {
            let watcher = graceful.watcher();
            let shutdown_counter = shutdown_counter.clone();

            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(i * 5)).await;
                let future = async move {
                    tokio::time::sleep(std::time::Duration::from_millis(i * 50)).await;
                };
                let dummy_conn = DummyConnection {
                    future,
                    shutdown_counter,
                };
                let conn = watcher.watch(dummy_conn);
                conn.await.unwrap();
            });
        }

        assert_eq!(shutdown_counter.load(Ordering::SeqCst), 0);

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                panic!("timeout")
            },
            _ = graceful.shutdown() => {
                assert_eq!(shutdown_counter.load(Ordering::SeqCst), 3);
            }
        }
    }

    #[tokio::test]
    async fn test_graceful_shutdown_multi_per_watcher_ok() {
        let graceful = GracefulShutdown::new();
        let shutdown_counter = Arc::new(AtomicUsize::new(0));

        for i in 1..=3 {
            let watcher = graceful.watcher();
            let shutdown_counter = shutdown_counter.clone();

            tokio::spawn(async move {
                let mut futures = Vec::new();
                for u in 1..=i {
                    let future = tokio::time::sleep(std::time::Duration::from_millis(u * 50));
                    let dummy_conn = DummyConnection {
                        future,
                        shutdown_counter: shutdown_counter.clone(),
                    };
                    let conn = watcher.watch(dummy_conn);
                    futures.push(conn);
                }
                futures_util::future::join_all(futures).await;
            });
        }

        assert_eq!(shutdown_counter.load(Ordering::SeqCst), 0);

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                panic!("timeout")
            },
            _ = graceful.shutdown() => {
                assert_eq!(shutdown_counter.load(Ordering::SeqCst), 6);
            }
        }
    }

    #[tokio::test]
    async fn test_graceful_shutdown_timeout() {
        let graceful = GracefulShutdown::new();
        let shutdown_counter = Arc::new(AtomicUsize::new(0));

        for i in 1..=3 {
            let watcher = graceful.watcher();
            let shutdown_counter = shutdown_counter.clone();

            tokio::spawn(async move {
                let future = async move {
                    if i == 1 {
                        std::future::pending::<()>().await
                    } else {
                        std::future::ready(()).await
                    }
                };
                let dummy_conn = DummyConnection {
                    future,
                    shutdown_counter,
                };
                let conn = watcher.watch(dummy_conn);
                conn.await.unwrap();
            });
        }

        assert_eq!(shutdown_counter.load(Ordering::SeqCst), 0);

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {
                assert_eq!(shutdown_counter.load(Ordering::SeqCst), 3);
            },
            _ = graceful.shutdown() => {
                panic!("shutdown should not be completed: as not all our conns finish")
            }
        }
    }
}
