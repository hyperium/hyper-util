//! HTTP client utilities

//mod client;
pub mod connect;
#[cfg(all(feature = "http1", feature = "http2"))]
pub mod legacy;
#[doc(hidden)]
pub mod pool;
