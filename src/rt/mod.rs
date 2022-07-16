//! Runtime utilities

/// Implementation of [`hyper::rt::Executor`] that utilises [`tokio::spawn`].
pub mod tokio_executor;
