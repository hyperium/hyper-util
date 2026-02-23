//! DNS Resolution used by the `HttpConnector`.
//!
//! This module contains:
//!
//! - A [`TokioGaiResolver`] that is the default resolver for the `HttpConnector`.
//! - The `Name` type used as an argument to custom resolvers.
//!
//! # Resolvers are `Service`s
//!
//! A resolver is just a
//! `Service<Name, Response = impl Iterator<Item = SocketAddr>>`.
//!
//! A simple resolver that ignores the name and always returns a specific
//! address:
//!
//! ```rust,ignore
//! use std::{convert::Infallible, iter, net::SocketAddr};
//!
//! let resolver = tower::service_fn(|_name| async {
//!     Ok::<_, Infallible>(iter::once(SocketAddr::from(([127, 0, 0, 1], 8080))))
//! });
//! ```
use std::fmt;
use std::future::Future;
use std::io;
use std::net::ToSocketAddrs;
use std::pin::Pin;
use std::task::{self, Poll};
use tokio::task::JoinHandle;

pub use super::{Name, Resolve, SocketAddrs};

/// A resolver using blocking `getaddrinfo` calls in a threadpool.
#[derive(Clone)]
pub struct TokioGaiResolver {
    _priv: (),
}

impl Default for TokioGaiResolver {
    fn default() -> Self {
        TokioGaiResolver::new()
    }
}

impl TokioGaiResolver {
    /// Construct a new `TokioGaiResolver`.
    pub fn new() -> Self {
        TokioGaiResolver { _priv: () }
    }
}

impl Resolve for TokioGaiResolver {
    type Addrs = SocketAddrs;
    type Error = io::Error;
    type Future = TokioGaiFuture;

    fn poll_ready(&mut self, _cx: &mut task::Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn resolve(&mut self, name: Name) -> Self::Future {
        let blocking = tokio::task::spawn_blocking(move || {
            (name.as_str(), 0)
                .to_socket_addrs()
                .map(|i| SocketAddrs::new(i.collect::<Vec<_>>()))
        });

        TokioGaiFuture { inner: blocking }
    }
}

impl fmt::Debug for TokioGaiResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("GaiResolver")
    }
}

/// A future to resolve a name returned by `TokioGaiResolver`.
pub struct TokioGaiFuture {
    inner: JoinHandle<Result<SocketAddrs, io::Error>>,
}

impl Future for TokioGaiFuture {
    type Output = Result<SocketAddrs, io::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.inner).poll(cx).map(|res| match res {
            Ok(Ok(addrs)) => Ok(addrs),
            Ok(Err(err)) => Err(err),
            Err(join_err) => {
                if join_err.is_cancelled() {
                    Err(io::Error::new(io::ErrorKind::Interrupted, join_err))
                } else {
                    panic!("gai background task failed: {join_err:?}")
                }
            }
        })
    }
}

impl fmt::Debug for TokioGaiFuture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("TokioGaiFuture")
    }
}

impl Drop for TokioGaiFuture {
    fn drop(&mut self) {
        self.inner.abort();
    }
}
