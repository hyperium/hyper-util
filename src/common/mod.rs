#![allow(missing_docs)]

macro_rules! ready {
    ($e:expr) => {
        match $e {
            std::task::Poll::Ready(v) => v,
            std::task::Poll::Pending => return std::task::Poll::Pending,
        }
    };
}

pub(crate) use ready;
pub(crate) mod exec;
#[cfg(feature = "client")]
mod lazy;
pub(crate) mod rewind;
#[cfg(feature = "client")]
mod sync;

#[cfg(feature = "client")]
pub(crate) use exec::Exec;

#[cfg(feature = "client")]
pub(crate) use lazy::{lazy, Started as Lazy};
#[cfg(feature = "client")]
pub(crate) use sync::SyncWrapper;
