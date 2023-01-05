#![deny(missing_docs)]
#![cfg_attr(docsrs, feature(doc_auto_cfg, doc_cfg))]

//! hyper utilities
pub use crate::error::{GenericError, Result};

pub mod client;
pub mod common;
pub mod rt;
pub mod server;

mod error;
