//! Connectors used by the `Client`.
//!
//! This module contains:
//!
//! - A default [`HttpConnector`][] that does DNS resolution and establishes
//!   connections over TCP.
//! - Types to build custom connectors.
//!
//! # Connectors
//!
//! A "connector" is a [`Service`][] that takes a [`Uri`][] destination, and
//! its `Response` is some type implementing [`Read`][], [`Write`][],
//! and [`Connection`][].
//!
//! ## Custom Connectors
//!
//! A simple connector that ignores the `Uri` destination and always returns
//! a TCP connection to the same address could be written like this:
//!
//! ```rust,ignore
//! let connector = tower::service_fn(|_dst| async {
//!     tokio::net::TcpStream::connect("127.0.0.1:1337")
//! })
//! ```
//!
//! Or, fully written out:
//!
//! ```
//! # #[cfg(not(feature = "tokio-net"))]
//! # fn main() {}
//! # #[cfg(feature = "tokio-net")]
//! # fn main() {
//! use std::{future::Future, net::SocketAddr, pin::Pin, task::{self, Poll}};
//! use http::Uri;
//! use tokio::net::TcpStream;
//! use tower_service::Service;
//!
//! #[derive(Clone)]
//! struct LocalConnector;
//!
//! impl Service<Uri> for LocalConnector {
//!     type Response = TcpStream;
//!     type Error = std::io::Error;
//!     // We can't "name" an `async` generated future.
//!     type Future = Pin<Box<
//!         dyn Future<Output = Result<Self::Response, Self::Error>> + Send
//!     >>;
//!
//!     fn poll_ready(&mut self, _: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
//!         // This connector is always ready, but others might not be.
//!         Poll::Ready(Ok(()))
//!     }
//!
//!     fn call(&mut self, _: Uri) -> Self::Future {
//!         Box::pin(TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], 1337))))
//!     }
//! }
//! # }
//! ```
//!
//! It's worth noting that for `TcpStream`s, the [`HttpConnector`][] is a
//! better starting place to extend from.
//!
//! [`HttpConnector`]: HttpConnector
//! [`Service`]: tower_service::Service
//! [`Uri`]: ::http::Uri
//! [`Read`]: hyper::rt::Read
//! [`Write`]: hyper::rt::Write
//! [`Connection`]: Connection

pub use crate::client::connect::{Connect, Connected, Connection};

#[cfg(feature = "tokio-net")]
pub use self::http::{HttpConnector, HttpInfo};

#[cfg(feature = "tokio-net")]
pub mod dns;
#[cfg(feature = "tokio-net")]
mod http;

pub mod proxy;

pub(crate) mod capture;
pub use capture::{capture_connection, CaptureConnection};
