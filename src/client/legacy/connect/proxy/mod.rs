//! Proxy helpers
// Upgrades, which the tunnel is established with, require a protocol.
#[cfg(any(feature = "http1", feature = "http2"))]
mod http_connect;
mod socks;
mod tunnel;

#[cfg(any(feature = "http1", feature = "http2"))]
#[cfg_attr(docsrs, doc(cfg(any(feature = "http1", feature = "http2"))))]
pub use self::http_connect::{HttpConnect, HttpConnectError, Tunneled};
pub use self::socks::{SocksV4, SocksV5};
pub use self::tunnel::Tunnel;
