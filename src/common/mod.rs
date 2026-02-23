#![allow(missing_docs)]

pub(crate) mod exec;
#[cfg(any(feature = "client", feature = "client-legacy"))]
mod lazy;
#[cfg(feature = "server")]
// #[cfg(feature = "server-auto")]
pub(crate) mod rewind;
#[cfg(any(feature = "client", feature = "client-legacy"))]
mod sync;
pub(crate) mod timer;

#[cfg(any(feature = "client", feature = "client-legacy"))]
pub(crate) use exec::Exec;

#[cfg(any(feature = "client", feature = "client-legacy"))]
pub(crate) use lazy::{lazy, Started as Lazy};
#[cfg(any(feature = "client", feature = "client-legacy"))]
pub(crate) use sync::SyncWrapper;
