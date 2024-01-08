//! Utility to gracefully shutdown a server.
//! 
//! This module provides a [`GracefulShutdown`] type,
//! which can be used to gracefully shutdown a server.
//! 
//! # Example
//! 
//! TODO

use pin_project_lite::pin_project;
use slab::Slab;
use std::{
    fmt::{self, Debug, Display},
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize},
        Arc, Mutex,
    },
    task::{self, Poll, Waker},
};

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
    pub fn watch<F, T>(&self, f: F) -> Result<GracefulFuture<F>, GracefulShutdownError<F>>
    where
        F: Future<Output = T>,
    {
        if self
            .state
            .shutdown
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(GracefulShutdownError { future: f });
        }
        Ok(GracefulFuture {
            future: f,
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
    pub struct GracefulFuture<F> {
        #[pin]
        future: F,
        state: Option<Arc<GracefulState>>,
    }
}

impl<F> Debug for GracefulFuture<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GracefulFuture").finish()
    }
}

impl<F> Future for GracefulFuture<F>
where
    F: Future,
{
    type Output = F::Output;

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
pub struct GracefulShutdownError<F> {
    future: F,
}

impl<F> GracefulShutdownError<F> {
    /// Get back the future that errored with a [`GracefulShutdownError`].
    pub fn into_future(self) -> F {
        self.future
    }
}

impl<F> Display for GracefulShutdownError<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("graceful shutdown already in progress")
    }
}

impl<F> Debug for GracefulShutdownError<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

impl<F> std::error::Error for GracefulShutdownError<F> {}
