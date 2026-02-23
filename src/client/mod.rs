//! HTTP client utilities

/// Connectors used by the `Client`.
pub mod connect;

#[cfg(any(feature = "http1", feature = "http2"))]
mod client;

#[cfg(any(feature = "http1", feature = "http2"))]
pub use client::{Builder, Client, Error, ResponseFuture};

/// Legacy implementations of `connect` module and `Client`
#[cfg(feature = "client-legacy")]
pub mod legacy;

#[cfg(feature = "client-pool")]
pub mod pool;

#[cfg(feature = "client-proxy")]
pub mod proxy;
