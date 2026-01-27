//! DNS Resolution used by the `HttpConnector`.
//!
//! This module contains:
//!
//! - A [`GaiResolver`] that is the default resolver for the `HttpConnector`.
//! - The `Name` type used as an argument to custom resolvers.
//!
//! # Resolvers are `Service`s
//!
//! A resolver is just a
//! `Service<Name, Response = impl Iterator<Item = SocketAddr>>`.
//!
//! A simple resolver that ignores the name and always returns a specific
//! address:
//!
//! ```rust,ignore
//! use std::{convert::Infallible, iter, net::SocketAddr};
//!
//! let resolver = tower::service_fn(|_name| async {
//!     Ok::<_, Infallible>(iter::once(SocketAddr::from(([127, 0, 0, 1], 8080))))
//! });
//! ```
use std::error::Error;
use std::future::Future;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, ToSocketAddrs};
use std::pin::Pin;
use std::str::FromStr;
use std::task::{self, Poll};
use std::{fmt, io, vec};

use tokio::task::JoinHandle;
use tower_service::Service;

pub(super) use self::sealed::Resolve;

/// A domain name to resolve into IP addresses.
#[derive(Clone, Hash, Eq, PartialEq)]
pub struct Name {
    host: Box<str>,
}

/// A resolver using blocking `getaddrinfo` calls in a threadpool.
#[derive(Clone)]
pub struct GaiResolver {
    _priv: (),
}

/// An iterator of IP addresses returned from `getaddrinfo`.
pub struct GaiAddrs {
    inner: SocketAddrs,
}

/// A future to resolve a name returned by `GaiResolver`.
pub struct GaiFuture {
    inner: JoinHandle<Result<SocketAddrs, io::Error>>,
}

impl Name {
    pub(super) fn new(host: Box<str>) -> Name {
        Name { host }
    }

    /// View the hostname as a string slice.
    pub fn as_str(&self) -> &str {
        &self.host
    }
}

impl fmt::Debug for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.host, f)
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.host, f)
    }
}

impl FromStr for Name {
    type Err = InvalidNameError;

    fn from_str(host: &str) -> Result<Self, Self::Err> {
        // Possibly add validation later
        Ok(Name::new(host.into()))
    }
}

/// Error indicating a given string was not a valid domain name.
#[derive(Debug)]
pub struct InvalidNameError(());

impl fmt::Display for InvalidNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Not a valid domain name")
    }
}

impl Error for InvalidNameError {}

impl GaiResolver {
    /// Construct a new `GaiResolver`.
    pub fn new() -> Self {
        GaiResolver { _priv: () }
    }
}

impl Service<Name> for GaiResolver {
    type Response = GaiAddrs;
    type Error = io::Error;
    type Future = GaiFuture;

    fn poll_ready(&mut self, _cx: &mut task::Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, name: Name) -> Self::Future {
        let blocking = tokio::task::spawn_blocking(move || {
            (&*name.host, 0)
                .to_socket_addrs()
                .map(|i| SocketAddrs { iter: i })
        });

        GaiFuture { inner: blocking }
    }
}

impl fmt::Debug for GaiResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("GaiResolver")
    }
}

impl Future for GaiFuture {
    type Output = Result<GaiAddrs, io::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.inner).poll(cx).map(|res| match res {
            Ok(Ok(addrs)) => Ok(GaiAddrs { inner: addrs }),
            Ok(Err(err)) => Err(err),
            Err(join_err) => {
                if join_err.is_cancelled() {
                    Err(io::Error::new(io::ErrorKind::Interrupted, join_err))
                } else {
                    panic!("gai background task failed: {join_err:?}")
                }
            }
        })
    }
}

impl fmt::Debug for GaiFuture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("GaiFuture")
    }
}

impl Drop for GaiFuture {
    fn drop(&mut self) {
        self.inner.abort();
    }
}

impl Iterator for GaiAddrs {
    type Item = SocketAddr;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }
}

impl fmt::Debug for GaiAddrs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("GaiAddrs")
    }
}

pub(super) struct SocketAddrs {
    iter: vec::IntoIter<SocketAddr>,
}

impl SocketAddrs {
    pub(super) fn new(addrs: Vec<SocketAddr>) -> Self {
        SocketAddrs {
            iter: addrs.into_iter(),
        }
    }

    pub(super) fn try_parse(host: &str, port: u16) -> Option<SocketAddrs> {
        if let Ok(addr) = host.parse::<Ipv4Addr>() {
            let addr = SocketAddrV4::new(addr, port);
            return Some(SocketAddrs {
                iter: vec![SocketAddr::V4(addr)].into_iter(),
            });
        }
        if let Ok(addr) = host.parse::<Ipv6Addr>() {
            let addr = SocketAddrV6::new(addr, port, 0, 0);
            return Some(SocketAddrs {
                iter: vec![SocketAddr::V6(addr)].into_iter(),
            });
        }
        None
    }

    #[inline]
    fn filter(self, predicate: impl FnMut(&SocketAddr) -> bool) -> SocketAddrs {
        SocketAddrs::new(self.iter.filter(predicate).collect())
    }

    pub(super) fn split_by_preference(
        self,
        local_addr_ipv4: Option<Ipv4Addr>,
        local_addr_ipv6: Option<Ipv6Addr>,
    ) -> (SocketAddrs, SocketAddrs) {
        match (local_addr_ipv4, local_addr_ipv6) {
            (Some(_), None) => (self.filter(SocketAddr::is_ipv4), SocketAddrs::new(vec![])),
            (None, Some(_)) => (self.filter(SocketAddr::is_ipv6), SocketAddrs::new(vec![])),
            _ => {
                let preferring_v6 = self
                    .iter
                    .as_slice()
                    .first()
                    .map(SocketAddr::is_ipv6)
                    .unwrap_or(false);

                let (preferred, fallback) = self
                    .iter
                    .partition::<Vec<_>, _>(|addr| addr.is_ipv6() == preferring_v6);

                (SocketAddrs::new(preferred), SocketAddrs::new(fallback))
            }
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.iter.as_slice().is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.iter.as_slice().len()
    }

    /// Create an interleaved address iterator per RFC 8305 (Happy Eyeballs v2) Section 4.
    ///
    /// Takes `first_family_count` addresses from the preferred family,
    /// then interleaves remaining addresses: one fallback, one preferred, repeat.
    ///
    /// Input:  `[v6_1, v6_2, v4_1, v4_2]` (IPv6 preferred)
    /// Output: `[v6_1, v4_1, v6_2, v4_2]` (with `first_family_count=1`)
    pub(super) fn interleave_by_family(self, first_family_count: usize) -> InterleavedAddrs {
        InterleavedAddrs::new(self, first_family_count)
    }
}

/// Iterator over addresses interleaved by family per RFC 8305 (Happy Eyeballs v2).
pub(super) struct InterleavedAddrs {
    inner: vec::IntoIter<SocketAddr>,
    total: usize,
}

impl InterleavedAddrs {
    fn new(addrs: SocketAddrs, first_family_count: usize) -> Self {
        let addrs: Vec<_> = addrs.iter.collect();
        let total = addrs.len();

        if addrs.is_empty() {
            return InterleavedAddrs {
                inner: Vec::new().into_iter(),
                total: 0,
            };
        }

        // Determine preferred family from first address
        let prefer_ipv6 = addrs[0].is_ipv6();

        let (mut preferred, fallback): (Vec<_>, Vec<_>) = if prefer_ipv6 {
            addrs.into_iter().partition(|a| a.is_ipv6())
        } else {
            addrs.into_iter().partition(|a| a.is_ipv4())
        };

        let mut result = Vec::with_capacity(total);

        // Take first_family_count from preferred
        let take_count = first_family_count.min(preferred.len());
        result.extend(preferred.drain(..take_count));

        // Interleave remaining: fallback, preferred, fallback, preferred...
        let mut pref_iter = preferred.into_iter();
        let mut fall_iter = fallback.into_iter();

        loop {
            match (fall_iter.next(), pref_iter.next()) {
                (Some(f), Some(p)) => {
                    result.push(f);
                    result.push(p);
                }
                (Some(f), None) => result.push(f),
                (None, Some(p)) => result.push(p),
                (None, None) => break,
            }
        }

        InterleavedAddrs {
            inner: result.into_iter(),
            total,
        }
    }

    /// Total number of addresses (original count before any iteration).
    pub(super) fn total(&self) -> usize {
        self.total
    }
}

impl Iterator for InterleavedAddrs {
    type Item = SocketAddr;

    #[inline]
    fn next(&mut self) -> Option<SocketAddr> {
        self.inner.next()
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl ExactSizeIterator for InterleavedAddrs {}

impl Iterator for SocketAddrs {
    type Item = SocketAddr;
    #[inline]
    fn next(&mut self) -> Option<SocketAddr> {
        self.iter.next()
    }
}

mod sealed {
    use std::future::Future;
    use std::task::{self, Poll};

    use super::{Name, SocketAddr};
    use tower_service::Service;

    // "Trait alias" for `Service<Name, Response = Addrs>`
    pub trait Resolve {
        type Addrs: Iterator<Item = SocketAddr>;
        type Error: Into<Box<dyn std::error::Error + Send + Sync>>;
        type Future: Future<Output = Result<Self::Addrs, Self::Error>>;

        fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>>;
        fn resolve(&mut self, name: Name) -> Self::Future;
    }

    impl<S> Resolve for S
    where
        S: Service<Name>,
        S::Response: Iterator<Item = SocketAddr>,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        type Addrs = S::Response;
        type Error = S::Error;
        type Future = S::Future;

        fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
            Service::poll_ready(self, cx)
        }

        fn resolve(&mut self, name: Name) -> Self::Future {
            Service::call(self, name)
        }
    }
}

pub(super) async fn resolve<R>(resolver: &mut R, name: Name) -> Result<R::Addrs, R::Error>
where
    R: Resolve,
{
    crate::common::future::poll_fn(|cx| resolver.poll_ready(cx)).await?;
    resolver.resolve(name).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn test_ip_addrs_split_by_preference() {
        let ip_v4 = Ipv4Addr::new(127, 0, 0, 1);
        let ip_v6 = Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1);
        let v4_addr = (ip_v4, 80).into();
        let v6_addr = (ip_v6, 80).into();

        let (mut preferred, mut fallback) = SocketAddrs {
            iter: vec![v4_addr, v6_addr].into_iter(),
        }
        .split_by_preference(None, None);
        assert!(preferred.next().unwrap().is_ipv4());
        assert!(fallback.next().unwrap().is_ipv6());

        let (mut preferred, mut fallback) = SocketAddrs {
            iter: vec![v6_addr, v4_addr].into_iter(),
        }
        .split_by_preference(None, None);
        assert!(preferred.next().unwrap().is_ipv6());
        assert!(fallback.next().unwrap().is_ipv4());

        let (mut preferred, mut fallback) = SocketAddrs {
            iter: vec![v4_addr, v6_addr].into_iter(),
        }
        .split_by_preference(Some(ip_v4), Some(ip_v6));
        assert!(preferred.next().unwrap().is_ipv4());
        assert!(fallback.next().unwrap().is_ipv6());

        let (mut preferred, mut fallback) = SocketAddrs {
            iter: vec![v6_addr, v4_addr].into_iter(),
        }
        .split_by_preference(Some(ip_v4), Some(ip_v6));
        assert!(preferred.next().unwrap().is_ipv6());
        assert!(fallback.next().unwrap().is_ipv4());

        let (mut preferred, fallback) = SocketAddrs {
            iter: vec![v4_addr, v6_addr].into_iter(),
        }
        .split_by_preference(Some(ip_v4), None);
        assert!(preferred.next().unwrap().is_ipv4());
        assert!(fallback.is_empty());

        let (mut preferred, fallback) = SocketAddrs {
            iter: vec![v4_addr, v6_addr].into_iter(),
        }
        .split_by_preference(None, Some(ip_v6));
        assert!(preferred.next().unwrap().is_ipv6());
        assert!(fallback.is_empty());
    }

    #[test]
    fn test_name_from_str() {
        const DOMAIN: &str = "test.example.com";
        let name = Name::from_str(DOMAIN).expect("Should be a valid domain");
        assert_eq!(name.as_str(), DOMAIN);
        assert_eq!(name.to_string(), DOMAIN);
    }

    // === RFC 8305 Address Interleaving Tests ===

    #[test]
    fn test_interleave_by_family_basic() {
        // IPv6 preferred (first in list), interleave with IPv4
        let v6_1: SocketAddr = "[2001:db8::1]:80".parse().unwrap();
        let v6_2: SocketAddr = "[2001:db8::2]:80".parse().unwrap();
        let v4_1: SocketAddr = "192.0.2.1:80".parse().unwrap();
        let v4_2: SocketAddr = "192.0.2.2:80".parse().unwrap();

        let addrs = SocketAddrs::new(vec![v6_1, v6_2, v4_1, v4_2]);
        let result: Vec<_> = addrs.interleave_by_family(1).collect();

        // RFC 8305: first_family_count=1 means v6, v4, v6, v4...
        assert_eq!(result, vec![v6_1, v4_1, v6_2, v4_2]);
    }

    #[test]
    fn test_interleave_by_family_empty() {
        let addrs = SocketAddrs::new(vec![]);
        let result: Vec<_> = addrs.interleave_by_family(1).collect();
        assert!(result.is_empty());
    }

    #[test]
    fn test_interleave_by_family_single_family() {
        // All IPv4 - no interleaving needed
        let v4_1: SocketAddr = "192.0.2.1:80".parse().unwrap();
        let v4_2: SocketAddr = "192.0.2.2:80".parse().unwrap();
        let v4_3: SocketAddr = "192.0.2.3:80".parse().unwrap();

        let addrs = SocketAddrs::new(vec![v4_1, v4_2, v4_3]);
        let result: Vec<_> = addrs.interleave_by_family(1).collect();

        assert_eq!(result, vec![v4_1, v4_2, v4_3]);
    }

    #[test]
    fn test_interleave_by_family_count_2() {
        // first_family_count=2: take 2 from preferred, then interleave
        let v6_1: SocketAddr = "[2001:db8::1]:80".parse().unwrap();
        let v6_2: SocketAddr = "[2001:db8::2]:80".parse().unwrap();
        let v6_3: SocketAddr = "[2001:db8::3]:80".parse().unwrap();
        let v4_1: SocketAddr = "192.0.2.1:80".parse().unwrap();
        let v4_2: SocketAddr = "192.0.2.2:80".parse().unwrap();

        let addrs = SocketAddrs::new(vec![v6_1, v6_2, v6_3, v4_1, v4_2]);
        let result: Vec<_> = addrs.interleave_by_family(2).collect();

        // First 2 v6, then interleave: v4, v6, v4
        assert_eq!(result, vec![v6_1, v6_2, v4_1, v6_3, v4_2]);
    }

    #[test]
    fn test_interleave_by_family_count_0() {
        // first_family_count=0: immediate interleave, fallback first
        let v6_1: SocketAddr = "[2001:db8::1]:80".parse().unwrap();
        let v6_2: SocketAddr = "[2001:db8::2]:80".parse().unwrap();
        let v4_1: SocketAddr = "192.0.2.1:80".parse().unwrap();
        let v4_2: SocketAddr = "192.0.2.2:80".parse().unwrap();

        let addrs = SocketAddrs::new(vec![v6_1, v6_2, v4_1, v4_2]);
        let result: Vec<_> = addrs.interleave_by_family(0).collect();

        // Fallback first: v4, v6, v4, v6
        assert_eq!(result, vec![v4_1, v6_1, v4_2, v6_2]);
    }

    #[test]
    fn test_interleave_by_family_count_exceeds() {
        // first_family_count exceeds available preferred addresses
        let v6_1: SocketAddr = "[2001:db8::1]:80".parse().unwrap();
        let v4_1: SocketAddr = "192.0.2.1:80".parse().unwrap();
        let v4_2: SocketAddr = "192.0.2.2:80".parse().unwrap();

        let addrs = SocketAddrs::new(vec![v6_1, v4_1, v4_2]);
        let result: Vec<_> = addrs.interleave_by_family(5).collect();

        // Only 1 v6, take it, then all v4s
        assert_eq!(result, vec![v6_1, v4_1, v4_2]);
    }

    #[test]
    fn test_interleave_by_family_ipv4_preferred() {
        // IPv4 first in list means IPv4 is preferred
        let v4_1: SocketAddr = "192.0.2.1:80".parse().unwrap();
        let v4_2: SocketAddr = "192.0.2.2:80".parse().unwrap();
        let v6_1: SocketAddr = "[2001:db8::1]:80".parse().unwrap();
        let v6_2: SocketAddr = "[2001:db8::2]:80".parse().unwrap();

        let addrs = SocketAddrs::new(vec![v4_1, v4_2, v6_1, v6_2]);
        let result: Vec<_> = addrs.interleave_by_family(1).collect();

        // v4 preferred: v4, v6, v4, v6
        assert_eq!(result, vec![v4_1, v6_1, v4_2, v6_2]);
    }
}
