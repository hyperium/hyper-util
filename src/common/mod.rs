#![allow(missing_docs)]

#[cfg(feature = "client-legacy")]
mod lazy;
#[cfg(feature = "server")]
pub(crate) mod rewind;
pub(crate) mod timer;

#[cfg(feature = "client-legacy")]
pub(crate) use lazy::{Started as Lazy, lazy};
