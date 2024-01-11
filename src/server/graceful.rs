//! Utility to gracefully shutdown a server.
//!
//! This module provides a [`GracefulShutdown`] type,
//! which can be used to gracefully shutdown a server.
//!
//! # Example
//!
//! TODO

use futures_util::FutureExt;
use pin_project_lite::pin_project;
use slab::Slab;
use std::{
    fmt::{self, Debug, Display},
    future::Future,
    pin::{pin, Pin},
    sync::{
        atomic::{AtomicBool, AtomicUsize},
        Arc, Mutex,
    },
    task::{self, Poll, Waker},
};
use tokio::select;

/// A graceful shutdown watcher
#[derive(Clone)]
pub struct GracefulShutdown {
    state: Arc<GracefulState>,
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
                shutdown: AtomicBool::new(false),
                waker_list: Mutex::new(Slab::new()),
            }),
        }
    }

    /// Watch a future for graceful shutdown,
    /// returning a wrapper that can be awaited on.
    pub fn watch<C, F>(
        &self,
        conn: C,
        cancel: F,
    ) -> Result<GracefulFuture<C::Error>, GracefulShutdownError<C>>
    where
        C: GracefulConnection + Send + Sync + 'static,
        F: Future + Send + Sync + 'static,
    {
        if self
            .state
            .shutdown
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(GracefulShutdownError { conn });
        }
        Ok(GracefulFuture {
            future: Box::pin(async move {
                let mut conn = pin!(conn);
                let mut cancel = pin!(cancel.fuse());

                loop {
                    select! {
                        _ = cancel.as_mut() => {
                            tracing::trace!("signal received: initiate graceful shutdown");
                            conn.as_mut().graceful_shutdown();
                        }
                        result = conn.as_mut() => {
                            tracing::trace!("connection finished");
                            return result;
                        }
                    }
                }
            }),
            state: Some(self.state.clone()),
        })
    }

    /// Wait for a graceful shutdown
    pub fn shutdown(self) -> GracefulWaiter {
        if self
            .state
            .shutdown
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return GracefulWaiter {
                status: GracefulWaiterStatus::Closed,
            };
        }
        GracefulWaiter {
            status: GracefulWaiterStatus::Open(GracefulWaiterState {
                state: self.state.clone(),
                key: None,
            }),
        }
    }
}

struct GracefulState {
    counter: AtomicUsize,
    shutdown: AtomicBool,
    waker_list: Mutex<Slab<Option<Waker>>>,
}

impl GracefulState {
    fn unwatch(&self) {
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
    pub struct GracefulFuture<E> {
        #[pin]
        future: BoxFuture<E>,
        state: Option<Arc<GracefulState>>,
    }
}

impl<F> Debug for GracefulFuture<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GracefulFuture").finish()
    }
}

impl<E> Future for GracefulFuture<E> {
    type Output = Result<(), E>;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        match this.future.poll(cx) {
            Poll::Ready(v) => {
                this.state.take().unwrap().unwatch();
                Poll::Ready(v)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// A future that blocks until a graceful shutdown is complete.
pub struct GracefulWaiter {
    status: GracefulWaiterStatus,
}

impl Future for GracefulWaiter {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        match self.status {
            GracefulWaiterStatus::Closed => Poll::Ready(()),
            GracefulWaiterStatus::Open(ref mut state) => {
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
    }
}

enum GracefulWaiterStatus {
    Closed,
    Open(GracefulWaiterState),
}

struct GracefulWaiterState {
    state: Arc<GracefulState>,
    key: Option<usize>,
}

/// The error type returned by [`GracefulShutdown::watch`],
/// when a graceful shutdown is already in progress.
pub struct GracefulShutdownError<C> {
    conn: C,
}

impl<C> GracefulShutdownError<C> {
    /// Get back the connection that errored with a [`GracefulShutdownError`].
    pub fn into_connection(self) -> C {
        self.conn
    }
}

impl<C> Display for GracefulShutdownError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("graceful shutdown already in progress")
    }
}

impl<C> Debug for GracefulShutdownError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

impl<F> std::error::Error for GracefulShutdownError<F> {}

type BoxFuture<E> = Pin<Box<dyn Future<Output = Result<(), E>> + Send + Sync + 'static>>;

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
    // TODO
}
