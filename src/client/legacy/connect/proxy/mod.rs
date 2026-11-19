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

/// The port to ask the proxy for, defaulting by scheme like `HttpConnector`.
fn dst_port(dst: &http::Uri) -> u16 {
    dst.port_u16().unwrap_or_else(|| {
        if dst.scheme() == Some(&http::uri::Scheme::HTTP) {
            80
        } else {
            443
        }
    })
}

#[cfg(test)]
mod tests {
    use super::dst_port;

    #[test]
    fn dst_port_defaults_by_scheme() {
        assert_eq!(dst_port(&"http://example.com/".parse().unwrap()), 80);
        assert_eq!(dst_port(&"https://example.com/".parse().unwrap()), 443);
        assert_eq!(dst_port(&"http://example.com:8080/".parse().unwrap()), 8080);
        assert_eq!(dst_port(&"example.com:8443".parse().unwrap()), 8443);
    }
}
