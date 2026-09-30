//! A connector that establishes connections over Unix domain sockets.

use std::fmt;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use http::Uri;
use tokio::net::UnixStream;
use tower_service::Service;

use crate::rt::TokioIo;

/// A connector that always connects to a fixed Unix domain socket path.
///
/// The [`Uri`] passed to the connector only used for the `Host` header and request
/// target. The actual connection is always made to the socket path with which this
/// connector was created.
///
/// # Example
///
/// ```rust,no_run
/// use hyper_util::client::legacy::Client;
/// use hyper_util::client::legacy::connect::UnixConnector;
/// use hyper_util::rt::TokioExecutor;
///
/// let connector = UnixConnector::new("/var/run/httpd.sock");
/// let client: Client<UnixConnector, String> =
///     Client::builder(TokioExecutor::new()).build(connector);
/// ```
#[derive(Clone)]
pub struct UnixConnector {
    path: Arc<Path>,
}

impl UnixConnector {
    /// Create a new connector which always connects to the given socket path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Arc::from(path.into()),
        }
    }
}

impl fmt::Debug for UnixConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnixConnector")
            .field("path", &self.path)
            .finish()
    }
}

impl Service<Uri> for UnixConnector {
    type Response = TokioIo<UnixStream>;
    type Error = io::Error;
    // We can't "name" an `async` generated future.
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // This connector is always ready.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: Uri) -> Self::Future {
        let path = self.path.clone();
        Box::pin(async move {
            let stream = UnixStream::connect(&*path).await?;
            Ok(TokioIo::new(stream))
        })
    }
}
