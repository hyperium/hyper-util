//! HTTP client utilities

/// Legacy implementations of `connect` module and `Client`
#[cfg(feature = "client-legacy")]
pub mod legacy;

// for now, no others features use this mod
//#[cfg(feature = "client-proxy-env")]
pub mod proxy;
