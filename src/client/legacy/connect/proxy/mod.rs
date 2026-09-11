//! Proxy helpers
#[cfg(feature = "http1")]
mod http_connect;
mod socks;
mod tunnel;

#[cfg(feature = "http1")]
#[cfg_attr(docsrs, doc(cfg(feature = "http1")))]
pub use self::http_connect::{HttpConnect, HttpConnectError, Tunneled};
pub use self::socks::{SocksV4, SocksV5};
pub use self::tunnel::Tunnel;
