#![deny(missing_docs)]
#![cfg_attr(docsrs, feature(doc_auto_cfg, doc_cfg))]

//! hyper-util

#[cfg(feature = "client")]
pub mod client;
mod common;
pub mod rt;
pub mod server;
#[cfg(all(
    any(feature = "http1", feature = "http2"),
    any(feature = "server", feature = "client")
))]
pub mod service;

mod error;
