use socket2::TcpKeepalive;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;

#[derive(Clone)]
pub(crate) struct Config {
    pub connect_timeout: Option<Duration>,
    pub enforce_http: bool,
    pub happy_eyeballs_timeout: Option<Duration>,
    pub tcp_keepalive_config: TcpKeepaliveConfig,
    pub local_address_ipv4: Option<Ipv4Addr>,
    pub local_address_ipv6: Option<Ipv6Addr>,
    pub nodelay: bool,
    pub reuse_address: bool,
    pub send_buffer_size: Option<usize>,
    pub recv_buffer_size: Option<usize>,
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    pub interface: Option<String>,
    #[cfg(any(
        target_os = "illumos",
        target_os = "ios",
        target_os = "macos",
        target_os = "solaris",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "watchos",
    ))]
    pub interface: Option<std::ffi::CString>,
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    pub tcp_user_timeout: Option<Duration>,
}

#[derive(Default, Debug, Clone, Copy)]
pub(crate) struct TcpKeepaliveConfig {
    pub time: Option<Duration>,
    pub interval: Option<Duration>,
    pub retries: Option<u32>,
}

impl TcpKeepaliveConfig {
    pub(crate) fn into_tcpkeepalive(self) -> Option<TcpKeepalive> {
        let mut dirty = false;
        let mut ka = TcpKeepalive::new();
        if let Some(time) = self.time {
            ka = ka.with_time(time);
            dirty = true
        }
        if let Some(interval) = self.interval {
            ka = Self::ka_with_interval(ka, interval, &mut dirty)
        };
        if let Some(retries) = self.retries {
            ka = Self::ka_with_retries(ka, retries, &mut dirty)
        };
        if dirty {
            Some(ka)
        } else {
            None
        }
    }

    #[cfg(any(
        target_os = "android",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "fuchsia",
        target_os = "illumos",
        target_os = "ios",
        target_os = "visionos",
        target_os = "linux",
        target_os = "macos",
        target_os = "netbsd",
        target_os = "tvos",
        target_os = "watchos",
        target_os = "windows",
    ))]
    fn ka_with_interval(ka: TcpKeepalive, interval: Duration, dirty: &mut bool) -> TcpKeepalive {
        *dirty = true;
        ka.with_interval(interval)
    }

    #[cfg(not(any(
        target_os = "android",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "fuchsia",
        target_os = "illumos",
        target_os = "ios",
        target_os = "visionos",
        target_os = "linux",
        target_os = "macos",
        target_os = "netbsd",
        target_os = "tvos",
        target_os = "watchos",
        target_os = "windows",
    )))]
    fn ka_with_interval(ka: TcpKeepalive, _: Duration, _: &mut bool) -> TcpKeepalive {
        ka
    }

    #[cfg(any(
        target_os = "android",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "fuchsia",
        target_os = "illumos",
        target_os = "ios",
        target_os = "visionos",
        target_os = "linux",
        target_os = "macos",
        target_os = "netbsd",
        target_os = "tvos",
        target_os = "watchos",
    ))]
    fn ka_with_retries(ka: TcpKeepalive, retries: u32, dirty: &mut bool) -> TcpKeepalive {
        *dirty = true;
        ka.with_retries(retries)
    }

    #[cfg(not(any(
        target_os = "android",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "fuchsia",
        target_os = "illumos",
        target_os = "ios",
        target_os = "visionos",
        target_os = "linux",
        target_os = "macos",
        target_os = "netbsd",
        target_os = "tvos",
        target_os = "watchos",
    )))]
    fn ka_with_retries(ka: TcpKeepalive, _: u32, _: &mut bool) -> TcpKeepalive {
        ka
    }
}

impl Config {
    pub(crate) fn tcp_keepalive(&self) -> Option<TcpKeepalive> {
        self.tcp_keepalive_config.into_tcpkeepalive()
    }

    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    pub(crate) fn tcp_user_timeout(&self) -> Option<Duration> {
        self.tcp_user_timeout
    }

    pub(crate) fn connect_timeout(&self) -> Option<Duration> {
        self.connect_timeout
    }

    pub(crate) fn happy_eyeballs_timeout(&self) -> Option<Duration> {
        self.happy_eyeballs_timeout
    }

    pub(crate) fn local_address_ipv4(&self) -> Option<Ipv4Addr> {
        self.local_address_ipv4
    }

    pub(crate) fn local_address_ipv6(&self) -> Option<Ipv6Addr> {
        self.local_address_ipv6
    }

    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    pub(crate) fn interface(&self) -> &Option<String> {
        &self.interface
    }

    #[cfg(any(
        target_os = "illumos",
        target_os = "ios",
        target_os = "macos",
        target_os = "solaris",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "watchos",
    ))]
    pub(crate) fn interface(&self) -> &Option<std::ffi::CString> {
        &self.interface
    }

    pub(crate) fn nodelay(&self) -> bool {
        self.nodelay
    }

    pub(crate) fn reuse_address(&self) -> bool {
        self.reuse_address
    }

    pub(crate) fn send_buffer_size(&self) -> Option<usize> {
        self.send_buffer_size
    }

    pub(crate) fn recv_buffer_size(&self) -> Option<usize> {
        self.recv_buffer_size
    }
}
