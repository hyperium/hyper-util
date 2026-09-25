//! Proxy utilities

pub mod matcher;

#[cfg(feature = "client-proxy-system")]
#[cfg(windows)]
mod win;

#[cfg(feature = "client-proxy-system")]
#[cfg(target_os = "macos")]
mod mac;
