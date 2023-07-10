//! Runtime utilities

/// Implementation of [`hyper::rt::Executor`] that utilises [`tokio::spawn`].
pub mod tokio_executor;
mod tokio_io;

pub use tokio_executor::TokioExecutor;
pub use tokio_io::TokioIo;
