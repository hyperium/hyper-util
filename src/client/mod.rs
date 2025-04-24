//! HTTP client utilities

/// Legacy implementations of `connect` module and `Client`
#[cfg(feature = "client-legacy")]
pub mod legacy;

/// Client services
#[cfg(any(feature = "http1", feature = "http2"))]
pub mod services;
