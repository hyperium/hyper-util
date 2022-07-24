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
pub(crate) mod sync_wrapper;

pub(crate) mod exec;
mod lazy;
pub(crate) use self::lazy::{lazy, Started as Lazy};

mod never;
pub(crate) use never::Never;
