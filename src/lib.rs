#![deny(missing_docs)]
#![cfg_attr(docsrs, feature(doc_auto_cfg, doc_cfg))]

//! hyper-util

#[cfg(feature = "client")]
pub mod client;
mod common;
pub mod rt;
#[cfg(feature = "server")]
pub mod server;

mod error;
