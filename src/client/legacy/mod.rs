#[cfg(any(feature = "http1", feature = "http2"))]
mod client;
#[cfg(any(feature = "http1", feature = "http2"))]
pub use client::Client;
#[cfg(any(feature = "http1", feature = "http2"))]
pub use client::Error;

pub mod connect;
