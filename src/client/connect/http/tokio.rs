//! TBD

use crate::client::connect::{
    dns::{tokio::TokioGaiResolver, Resolve},
    http::{HttpConnecting, HttpConnector},
    tcp::{tokio::TokioTcpConnector, ConnectError},
};
use crate::rt::TokioIo;
use hyper::Uri;
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;
use tokio::net::TcpStream;

use super::HttpConnect;

/// A connector for the `http` scheme.
///
/// Performs DNS resolution in a thread pool, and then connects over TCP.
///
/// # Note
///
/// Sets the [`HttpInfo`](super::HttpInfo) value on responses, which
/// includes transport information such as the remote socket address used.
#[derive(Clone)]
pub struct TokioHttpConnector<R = TokioGaiResolver> {
    inner: HttpConnector<R, TokioTcpConnector>,
}

impl TokioHttpConnector {
    /// Construct a new TokioHttpConnector.
    pub fn new() -> Self {
        Self::new_with_resolver(TokioGaiResolver::new())
    }
}

impl<R: Default> Default for TokioHttpConnector<R> {
    fn default() -> Self {
        Self::new_with_resolver(R::default())
    }
}

impl<R> TokioHttpConnector<R> {
    /// Construct a new TokioHttpConnector.
    ///
    /// Takes a [`Resolver`](crate::client::legacy::connect::dns#resolvers-are-services) to handle DNS lookups.
    pub fn new_with_resolver(resolver: R) -> Self {
        Self {
            inner: HttpConnector::new(resolver, TokioTcpConnector::new()),
        }
    }
}

impl<R> HttpConnect for TokioHttpConnector<R>
where
    R: Resolve + Clone + Send + Sync + 'static,
    R::Future: Send,
{
    /// Option to enforce all `Uri`s have the `http` scheme.
    ///
    /// Enabled by default.
    #[inline]
    fn enforce_http(&mut self, is_enforced: bool) {
        self.inner.enforce_http(is_enforced);
    }

    /// Set that all sockets have `SO_KEEPALIVE` set with the supplied duration.
    /// to remain idle before sending TCP keepalive probes.
    ///
    /// If `None`, keepalive is disabled.
    ///
    /// Default is `None`.
    #[inline]
    fn set_keepalive(&mut self, time: Option<Duration>) {
        self.inner.set_keepalive(time);
    }

    /// Set the duration between two successive TCP keepalive retransmissions,
    /// if acknowledgement to the previous keepalive transmission is not received.
    #[inline]
    fn set_keepalive_interval(&mut self, interval: Option<Duration>) {
        self.inner.set_keepalive_interval(interval);
    }

    /// Set the number of retransmissions to be carried out before declaring that remote end is not available.
    #[inline]
    fn set_keepalive_retries(&mut self, retries: Option<u32>) {
        self.inner.set_keepalive_retries(retries);
    }

    /// Set that all sockets have `SO_NODELAY` set to the supplied value `nodelay`.
    ///
    /// Default is `false`.
    #[inline]
    fn set_nodelay(&mut self, nodelay: bool) {
        self.inner.set_nodelay(nodelay);
    }

    /// Sets the value of the SO_SNDBUF option on the socket.
    #[inline]
    fn set_send_buffer_size(&mut self, size: Option<usize>) {
        self.inner.set_send_buffer_size(size);
    }

    /// Sets the value of the SO_RCVBUF option on the socket.
    #[inline]
    fn set_recv_buffer_size(&mut self, size: Option<usize>) {
        self.inner.set_recv_buffer_size(size);
    }

    /// Set that all sockets are bound to the configured address before connection.
    ///
    /// If `None`, the sockets will not be bound.
    ///
    /// Default is `None`.
    #[inline]
    fn set_local_address(&mut self, addr: Option<std::net::IpAddr>) {
        self.inner.set_local_address(addr);
    }

    /// Set that all sockets are bound to the configured IPv4 or IPv6 address (depending on host's
    /// preferences) before connection.
    #[inline]
    fn set_local_addresses(&mut self, addr_ipv4: Ipv4Addr, addr_ipv6: Ipv6Addr) {
        self.inner.set_local_addresses(addr_ipv4, addr_ipv6);
    }

    /// Set the connect timeout.
    ///
    /// If a domain resolves to multiple IP addresses, the timeout will be
    /// evenly divided across them.
    ///
    /// Default is `None`.
    #[inline]
    fn set_connect_timeout(&mut self, dur: Option<Duration>) {
        self.inner.set_connect_timeout(dur);
    }

    /// Set timeout for [RFC 6555 (Happy Eyeballs)][RFC 6555] algorithm.
    ///
    /// If hostname resolves to both IPv4 and IPv6 addresses and connection
    /// cannot be established using preferred address family before timeout
    /// elapses, then connector will in parallel attempt connection using other
    /// address family.
    ///
    /// If `None`, parallel connection attempts are disabled.
    ///
    /// Default is 300 milliseconds.
    ///
    /// [RFC 6555]: https://tools.ietf.org/html/rfc6555
    #[inline]
    fn set_happy_eyeballs_timeout(&mut self, dur: Option<Duration>) {
        self.inner.set_happy_eyeballs_timeout(dur);
    }

    /// Set that all socket have `SO_REUSEADDR` set to the supplied value `reuse_address`.
    ///
    /// Default is `false`.
    #[inline]
    fn set_reuse_address(&mut self, reuse_address: bool) {
        self.inner.set_reuse_address(reuse_address);
    }

    /// Sets the name of the interface to bind sockets produced by this
    /// connector.
    ///
    /// On Linux, this sets the `SO_BINDTODEVICE` option on this socket (see
    /// [`man 7 socket`] for details). On macOS (and macOS-derived systems like
    /// iOS), illumos, and Solaris, this will instead use the `IP_BOUND_IF`
    /// socket option (see [`man 7p ip`]).
    ///
    /// If a socket is bound to an interface, only packets received from that particular
    /// interface are processed by the socket. Note that this only works for some socket
    /// types, particularly `AF_INET`` sockets.
    ///
    /// On Linux it can be used to specify a [VRF], but the binary needs
    /// to either have `CAP_NET_RAW` or to be run as root.
    ///
    /// This function is only available on the following operating systems:
    /// - Linux, including Android
    /// - Fuchsia
    /// - illumos and Solaris
    /// - macOS, iOS, visionOS, watchOS, and tvOS
    ///
    /// [VRF]: https://www.kernel.org/doc/Documentation/networking/vrf.txt
    /// [`man 7 socket`]: https://man7.org/linux/man-pages/man7/socket.7.html
    /// [`man 7p ip`]: https://docs.oracle.com/cd/E86824_01/html/E54777/ip-7p.html
    #[cfg(any(
        target_os = "android",
        target_os = "fuchsia",
        target_os = "illumos",
        target_os = "ios",
        target_os = "linux",
        target_os = "macos",
        target_os = "solaris",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "watchos",
    ))]
    #[inline]
    fn set_interface<I: Into<String>>(&mut self, interface: I) -> &mut Self {
        self.inner.set_interface(interface);
        self
    }

    /// Sets the value of the TCP_USER_TIMEOUT option on the socket.
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    #[inline]
    fn set_tcp_user_timeout(&mut self, time: Option<Duration>) {
        self.inner.set_tcp_user_timeout(time);
    }
}

// R: Debug required for now to allow adding it to debug output later...
impl<R: fmt::Debug> fmt::Debug for TokioHttpConnector<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokioHttpConnector").finish()
    }
}

impl<R> tower_service::Service<Uri> for TokioHttpConnector<R>
where
    R: Resolve + Clone + Send + Sync + 'static,
    R::Future: Send,
{
    type Response = TokioIo<TcpStream>;
    type Error = ConnectError;
    type Future = HttpConnecting<R, TokioTcpConnector>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, dst: http::Uri) -> Self::Future {
        self.inner.call(dst)
    }
}
