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

pub use crate::client::connect::dns::{
    tokio::{TokioGaiFuture as GaiFuture, TokioGaiResolver as GaiResolver},
    InvalidNameError, Name, SocketAddrs as GaiAddrs,
};
