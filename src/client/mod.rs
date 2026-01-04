//! HTTP client utilities

#[cfg(any(feature = "http1", feature = "http2"))]
pub mod conn;

/// Legacy implementations of `connect` module and `Client`
#[cfg(feature = "client-legacy")]
pub mod legacy;

#[cfg(feature = "client-pool")]
pub mod pool;

#[cfg(feature = "client-proxy")]
pub mod proxy;
