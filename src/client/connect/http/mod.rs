use crate::client::connect::{
    config::{Config, TcpKeepaliveConfig},
    dns::{self, Resolve},
    tcp::{ConnectError, ConnectingTcp, TcpConnector},
    Connect,
};
use http::uri::{Scheme, Uri};
use pin_project_lite::pin_project;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{self, ready, Poll};
use std::time::Duration;
use tower_service::Service;
use tracing::trace;

#[cfg(feature = "tokio-net")]
pub mod tokio;

const INVALID_NOT_HTTP: &str = "invalid URL, scheme is not http";
const INVALID_MISSING_SCHEME: &str = "invalid URL, scheme is missing";
const INVALID_MISSING_HOST: &str = "invalid URL, host is missing";

/// TBD
pub trait HttpConnect: Connect + Clone {
    /// Set whether to enforce using the `http` scheme.
    fn enforce_http(&mut self, is_enforced: bool);

    /// Set the TCP keepalive time.
    fn set_keepalive(&mut self, time: Option<Duration>);

    /// Set the TCP keepalive interval.
    fn set_keepalive_interval(&mut self, interval: Option<Duration>);

    /// Set the number of TCP keepalive retries.
    fn set_keepalive_retries(&mut self, retries: Option<u32>);

    /// Set the TCP nodelay option.
    fn set_nodelay(&mut self, nodelay: bool);

    /// Set the TCP send buffer size.
    fn set_send_buffer_size(&mut self, size: Option<usize>);

    /// Set the TCP receive buffer size.
    fn set_recv_buffer_size(&mut self, size: Option<usize>);

    /// Set the local address to bind to.
    fn set_local_address(&mut self, addr: Option<IpAddr>);

    /// Set the local IPv4 and IPv6 addresses to bind to.
    fn set_local_addresses(&mut self, addr_ipv4: Ipv4Addr, addr_ipv6: Ipv6Addr);

    /// Set the connect timeout.
    fn set_connect_timeout(&mut self, dur: Option<Duration>);

    /// Set the happy eyeballs timeout.
    fn set_happy_eyeballs_timeout(&mut self, dur: Option<Duration>);

    /// Set whether to reuse the address.
    fn set_reuse_address(&mut self, reuse_address: bool);

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
    /// Set the network interface to bind to.
    fn set_interface<I: Into<String>>(&mut self, interface: I) -> &mut Self;

    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    /// Set the TCP user timeout.
    fn set_tcp_user_timeout(&mut self, time: Option<Duration>);
}

#[derive(Clone)]
/// A connector for the `http` scheme.
pub struct HttpConnector<R, S> {
    config: Arc<Config>,
    resolver: R,
    connector: S,
}

impl<R, S> HttpConnector<R, S> {
    /// Construct a new HttpConnector.
    pub fn new(resolver: R, connector: S) -> Self {
        HttpConnector {
            config: Arc::new(Config {
                connect_timeout: None,
                enforce_http: true,
                happy_eyeballs_timeout: Some(Duration::from_millis(300)),
                tcp_keepalive_config: TcpKeepaliveConfig::default(),
                local_address_ipv4: None,
                local_address_ipv6: None,
                nodelay: false,
                reuse_address: false,
                send_buffer_size: None,
                recv_buffer_size: None,
                #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
                interface: None,
                #[cfg(any(
                    target_os = "illumos",
                    target_os = "ios",
                    target_os = "macos",
                    target_os = "solaris",
                    target_os = "tvos",
                    target_os = "visionos",
                    target_os = "watchos",
                ))]
                interface: None,
                #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
                tcp_user_timeout: None,
            }),
            resolver,
            connector,
        }
    }

    fn config_mut(&mut self) -> &mut Config {
        Arc::make_mut(&mut self.config)
    }
}

impl<R, S> HttpConnect for HttpConnector<R, S>
where
    R: Resolve + Clone + Send + Sync + 'static,
    R::Future: Send,
    S: TcpConnector,
{
    #[inline]
    fn enforce_http(&mut self, is_enforced: bool) {
        self.config_mut().enforce_http = is_enforced;
    }

    #[inline]
    fn set_keepalive(&mut self, time: Option<Duration>) {
        self.config_mut().tcp_keepalive_config.time = time;
    }

    #[inline]
    fn set_keepalive_interval(&mut self, interval: Option<Duration>) {
        self.config_mut().tcp_keepalive_config.interval = interval;
    }

    #[inline]
    fn set_keepalive_retries(&mut self, retries: Option<u32>) {
        self.config_mut().tcp_keepalive_config.retries = retries;
    }

    #[inline]
    fn set_nodelay(&mut self, nodelay: bool) {
        self.config_mut().nodelay = nodelay;
    }

    #[inline]
    fn set_send_buffer_size(&mut self, size: Option<usize>) {
        self.config_mut().send_buffer_size = size;
    }

    #[inline]
    fn set_recv_buffer_size(&mut self, size: Option<usize>) {
        self.config_mut().recv_buffer_size = size;
    }

    #[inline]
    fn set_local_address(&mut self, addr: Option<IpAddr>) {
        let (v4, v6) = match addr {
            Some(IpAddr::V4(a)) => (Some(a), None),
            Some(IpAddr::V6(a)) => (None, Some(a)),
            _ => (None, None),
        };

        let cfg = self.config_mut();

        cfg.local_address_ipv4 = v4;
        cfg.local_address_ipv6 = v6;
    }

    #[inline]
    fn set_local_addresses(&mut self, addr_ipv4: Ipv4Addr, addr_ipv6: Ipv6Addr) {
        let cfg = self.config_mut();

        cfg.local_address_ipv4 = Some(addr_ipv4);
        cfg.local_address_ipv6 = Some(addr_ipv6);
    }

    #[inline]
    fn set_connect_timeout(&mut self, dur: Option<Duration>) {
        self.config_mut().connect_timeout = dur;
    }

    #[inline]
    fn set_happy_eyeballs_timeout(&mut self, dur: Option<Duration>) {
        self.config_mut().happy_eyeballs_timeout = dur;
    }

    #[inline]
    fn set_reuse_address(&mut self, reuse_address: bool) {
        self.config_mut().reuse_address = reuse_address;
    }

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
        let interface = interface.into();
        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        {
            self.config_mut().interface = Some(interface);
        }
        #[cfg(not(any(target_os = "android", target_os = "fuchsia", target_os = "linux")))]
        {
            let interface = std::ffi::CString::new(interface)
                .expect("interface name should not have nulls in it");
            self.config_mut().interface = Some(interface);
        }
        self
    }

    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    #[inline]
    fn set_tcp_user_timeout(&mut self, time: Option<Duration>) {
        self.config_mut().tcp_user_timeout = time;
    }
}

impl<R: fmt::Debug, S: fmt::Debug> fmt::Debug for HttpConnector<R, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpConnector").finish()
    }
}

impl<R, S> Service<Uri> for HttpConnector<R, S>
where
    R: Resolve + Clone + Send + Sync + 'static,
    R::Future: Send,
    S: TcpConnector,
    S::TcpStream: From<socket2::Socket>,
{
    type Response = S::Connection;
    type Error = ConnectError;
    type Future = HttpConnecting<R, S>;

    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.resolver.poll_ready(cx)).map_err(ConnectError::dns)?;
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        let mut self_ = self.clone();
        HttpConnecting {
            fut: Box::pin(async move { self_.call_async(dst).await }),
            _marker: PhantomData,
        }
    }
}

fn get_host_port<'u>(config: &Config, dst: &'u Uri) -> Result<(&'u str, u16), ConnectError> {
    trace!(
        "Http::connect; scheme={:?}, host={:?}, port={:?}",
        dst.scheme(),
        dst.host(),
        dst.port(),
    );

    if config.enforce_http {
        if dst.scheme() != Some(&Scheme::HTTP) {
            return Err(ConnectError {
                msg: INVALID_NOT_HTTP,
                addr: None,
                cause: None,
            });
        }
    } else if dst.scheme().is_none() {
        return Err(ConnectError {
            msg: INVALID_MISSING_SCHEME,
            addr: None,
            cause: None,
        });
    }

    let host = match dst.host() {
        Some(s) => s,
        None => {
            return Err(ConnectError {
                msg: INVALID_MISSING_HOST,
                addr: None,
                cause: None,
            });
        }
    };
    let port = match dst.port() {
        Some(port) => port.as_u16(),
        None => {
            if dst.scheme() == Some(&Scheme::HTTPS) {
                443
            } else {
                80
            }
        }
    };

    Ok((host, port))
}

impl<R, S> HttpConnector<R, S>
where
    R: Resolve,
    S: TcpConnector,
    S::TcpStream: From<socket2::Socket>,
{
    async fn call_async(&mut self, dst: Uri) -> Result<S::Connection, ConnectError> {
        let config = &self.config;

        let (host, port) = get_host_port(config, &dst)?;
        let host = host.trim_start_matches('[').trim_end_matches(']');

        let addrs = if let Some(addrs) = dns::SocketAddrs::try_parse(host, port) {
            addrs
        } else {
            let addrs = dns::resolve(&mut self.resolver, dns::Name::new(host.into()))
                .await
                .map_err(ConnectError::dns)?;
            let addrs = addrs
                .map(|mut addr| {
                    set_port(&mut addr, port, dst.port().is_some());
                    addr
                })
                .collect();
            dns::SocketAddrs::new(addrs)
        };

        let c = ConnectingTcp::new(addrs, config, self.connector.clone());
        let sock = c.connect(config).await?;
        Ok(sock)
    }
}

/// Extra information about the transport when an HttpConnector is used.
#[derive(Clone, Debug)]
pub struct HttpInfo {
    pub(crate) remote_addr: SocketAddr,
    pub(crate) local_addr: SocketAddr,
}

impl HttpInfo {
    /// The remote address of the connection.
    pub fn remote_addr(&self) -> SocketAddr {
        self.remote_addr
    }

    /// The local address of the connection.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}

pin_project! {
    #[must_use = "futures do nothing unless polled"]
    #[allow(missing_debug_implementations)]
    /// Future returned by `HttpConnector::call`.
    pub struct HttpConnecting<R, S: TcpConnector> {

        #[pin]
        fut: BoxConnecting<S>,
        _marker: PhantomData<R>,
    }
}

type ConnectResult<S> = Result<<S as TcpConnector>::Connection, ConnectError>;
type BoxConnecting<S> = Pin<Box<dyn Future<Output = ConnectResult<S>> + Send>>;

impl<R, S> Future for HttpConnecting<R, S>
where
    R: Resolve,
    S: TcpConnector,
{
    type Output = ConnectResult<S>;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        self.project().fut.poll(cx)
    }
}

fn set_port(addr: &mut SocketAddr, host_port: u16, explicit: bool) {
    if explicit || addr.port() == 0 {
        addr.set_port(host_port)
    };
}
