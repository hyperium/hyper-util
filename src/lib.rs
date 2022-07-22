#![deny(missing_docs)]

//! hyper utilities
pub use crate::error::{GenericError, Result};

pub mod client;
pub mod common;
pub mod rt;

mod error;
