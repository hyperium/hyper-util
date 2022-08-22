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
pub(crate) mod never;

pub(crate) use never::Never;
