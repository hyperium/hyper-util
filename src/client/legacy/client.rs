//! The legacy HTTP Client from 0.14.x
//!
//! This `Client` will eventually be deconstructed into more composable parts.
//! For now, to enable people to use hyper 1.0 quicker, this `Client` exists
//! in much the same way it did in hyper 0.14.

use std::error::Error as StdError;
use std::fmt;
use std::task::{self, Poll};
use std::time::Duration;

#[cfg(feature = "tokio-net")]
use crate::client::legacy::connect::HttpConnector;

use hyper::rt::Timer;
use hyper::{body::Body, Request, Response, Uri};

use crate::client::client::BoxSendFuture;
use crate::client::connect::{http::HttpConnect as _, Connect};

pub use crate::client::client::{Error, ResponseFuture};

/// A Client to make outgoing HTTP requests.
///
/// `Client` is cheap to clone and cloning is the recommended way to share a `Client`. The
/// underlying connection pool will be reused.
#[cfg_attr(docsrs, doc(cfg(any(feature = "http1", feature = "http2"))))]
pub struct Client<C, B> {
    inner: crate::client::client::Client<C, B>,
}

impl Client<(), ()> {
    /// Create a builder to configure a new `Client`.
    ///
    /// # Example
    ///
    /// ```
    /// # #[cfg(all(feature = "tokio", feature = "http2"))]
    /// # fn run () {
    /// use std::time::Duration;
    /// use hyper_util::client::legacy::Client;
    /// use hyper_util::rt::{TokioExecutor, TokioTimer};
    ///
    /// let client = Client::builder(TokioExecutor::new())
    ///     .pool_timer(TokioTimer::new())
    ///     .pool_idle_timeout(Duration::from_secs(30))
    ///     .http2_only(true)
    ///     .build_http();
    /// # let infer: Client<_, http_body_util::Full<bytes::Bytes>> = client;
    /// # drop(infer);
    /// # }
    /// # fn main() {}
    /// ```
    pub fn builder<E>(executor: E) -> Builder
    where
        E: hyper::rt::Executor<BoxSendFuture> + Send + Sync + Clone + 'static,
    {
        Builder {
            inner: crate::client::client::Client::builder(executor),
        }
    }
}

impl<C, B> Client<C, B>
where
    C: Connect + Clone + Send + Sync + 'static,
    B: Body + Send + 'static + Unpin,
    B::Data: Send,
    B::Error: Into<Box<dyn StdError + Send + Sync>>,
{
    /// Send a `GET` request to the supplied `Uri`.
    ///
    /// # Note
    ///
    /// This requires that the `Body` type have a `Default` implementation.
    /// It *should* return an "empty" version of itself, such that
    /// `Body::is_end_stream` is `true`.
    ///
    /// # Example
    ///
    /// ```
    /// # #[cfg(feature = "tokio")]
    /// # fn run () {
    /// use hyper::Uri;
    /// use hyper_util::client::legacy::Client;
    /// use hyper_util::rt::TokioExecutor;
    /// use bytes::Bytes;
    /// use http_body_util::Full;
    ///
    /// let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new()).build_http();
    ///
    /// let future = client.get(Uri::from_static("http://httpbin.org/ip"));
    /// # }
    /// # fn main() {}
    /// ```
    pub fn get(&self, uri: Uri) -> ResponseFuture
    where
        B: Default,
    {
        self.inner.get(uri)
    }

    /// Send a constructed `Request` using this `Client`.
    ///
    /// # Example
    ///
    /// ```
    /// # #[cfg(feature = "tokio")]
    /// # fn run () {
    /// use hyper::{Method, Request};
    /// use hyper_util::client::legacy::Client;
    /// use http_body_util::Full;
    /// use hyper_util::rt::TokioExecutor;
    /// use bytes::Bytes;
    ///
    /// let client: Client<_, Full<Bytes>> = Client::builder(TokioExecutor::new()).build_http();
    ///
    /// let req: Request<Full<Bytes>> = Request::builder()
    ///     .method(Method::POST)
    ///     .uri("http://httpbin.org/post")
    ///     .body(Full::from("Hallo!"))
    ///     .expect("request builder");
    ///
    /// let future = client.request(req);
    /// # }
    /// # fn main() {}
    /// ```
    pub fn request(&self, req: Request<B>) -> ResponseFuture {
        self.inner.request(req)
    }
}

impl<C, B> tower_service::Service<Request<B>> for Client<C, B>
where
    C: Connect + Clone + Send + Sync + 'static,
    B: Body + Send + 'static + Unpin,
    B::Data: Send,
    B::Error: Into<Box<dyn StdError + Send + Sync>>,
{
    type Response = Response<hyper::body::Incoming>;
    type Error = Error;
    type Future = ResponseFuture;

    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        tower_service::Service::poll_ready(&mut self.inner, cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        tower_service::Service::call(&mut self.inner, req)
    }
}

impl<C, B> tower_service::Service<Request<B>> for &'_ Client<C, B>
where
    C: Connect + Clone + Send + Sync + 'static,
    B: Body + Send + 'static + Unpin,
    B::Data: Send,
    B::Error: Into<Box<dyn StdError + Send + Sync>>,
{
    type Response = Response<hyper::body::Incoming>;
    type Error = Error;
    type Future = ResponseFuture;

    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut inner = &self.inner;
        tower_service::Service::poll_ready(&mut inner, cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let mut inner = &self.inner;
        tower_service::Service::call(&mut inner, req)
    }
}

impl<C: Clone, B> Clone for Client<C, B> {
    fn clone(&self) -> Client<C, B> {
        Client {
            inner: self.inner.clone(),
        }
    }
}

impl<C, B> fmt::Debug for Client<C, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").finish()
    }
}

/// A builder to configure a new [`Client`](Client).
///
/// # Example
///
/// ```
/// # #[cfg(all(feature = "tokio", feature = "http2"))]
/// # fn run () {
/// use std::time::Duration;
/// use hyper_util::client::legacy::Client;
/// use hyper_util::rt::TokioExecutor;
///
/// let client = Client::builder(TokioExecutor::new())
///     .pool_idle_timeout(Duration::from_secs(30))
///     .http2_only(true)
///     .build_http();
/// # let infer: Client<_, http_body_util::Full<bytes::Bytes>> = client;
/// # drop(infer);
/// # }
/// # fn main() {}
/// ```
#[cfg_attr(docsrs, doc(cfg(any(feature = "http1", feature = "http2"))))]
#[derive(Clone)]
pub struct Builder {
    inner: crate::client::client::Builder,
}

impl Builder {
    /// Construct a new Builder.
    pub fn new<E>(executor: E) -> Self
    where
        E: hyper::rt::Executor<BoxSendFuture> + Send + Sync + Clone + 'static,
    {
        Self {
            inner: crate::client::client::Builder::new(executor),
        }
    }

    /// Set an optional timeout for idle sockets being kept-alive.
    /// A `Timer` is required for this to take effect. See `Builder::pool_timer`
    ///
    /// Pass `None` to disable timeout.
    ///
    /// Default is 90 seconds.
    pub fn pool_idle_timeout<D>(&mut self, val: D) -> &mut Self
    where
        D: Into<Option<Duration>>,
    {
        self.inner.pool_idle_timeout(val);
        self
    }

    #[doc(hidden)]
    #[deprecated(note = "renamed to `pool_max_idle_per_host`")]
    pub fn max_idle_per_host(&mut self, max_idle: usize) -> &mut Self {
        #[allow(deprecated)]
        self.inner.max_idle_per_host(max_idle);
        self
    }

    /// Sets the maximum idle connection per host allowed in the pool.
    ///
    /// Default is `usize::MAX` (no limit).
    pub fn pool_max_idle_per_host(&mut self, max_idle: usize) -> &mut Self {
        self.inner.pool_max_idle_per_host(max_idle);
        self
    }

    // HTTP/1 options

    /// Sets the exact size of the read buffer to *always* use.
    #[cfg(feature = "http1")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http1")))]
    pub fn http1_read_buf_exact_size(&mut self, sz: usize) -> &mut Self {
        self.inner.http1_read_buf_exact_size(sz);
        self
    }

    /// Set the maximum buffer size for the connection.
    #[cfg(feature = "http1")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http1")))]
    pub fn http1_max_buf_size(&mut self, max: usize) -> &mut Self {
        self.inner.http1_max_buf_size(max);
        self
    }

    /// Set whether HTTP/1 connections will accept spaces between header names
    /// and the colon that follow them in responses.
    #[cfg(feature = "http1")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http1")))]
    pub fn http1_allow_spaces_after_header_name_in_responses(&mut self, val: bool) -> &mut Self {
        self.inner
            .http1_allow_spaces_after_header_name_in_responses(val);
        self
    }

    /// Set whether HTTP/1 connections will accept obsolete line folding for
    /// header values.
    #[cfg(feature = "http1")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http1")))]
    pub fn http1_allow_obsolete_multiline_headers_in_responses(&mut self, val: bool) -> &mut Self {
        self.inner
            .http1_allow_obsolete_multiline_headers_in_responses(val);
        self
    }

    /// Sets whether invalid header lines should be silently ignored in HTTP/1 responses.
    #[cfg(feature = "http1")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http1")))]
    pub fn http1_ignore_invalid_headers_in_responses(&mut self, val: bool) -> &mut Builder {
        self.inner.http1_ignore_invalid_headers_in_responses(val);
        self
    }

    /// Set whether HTTP/1 connections should try to use vectored writes,
    /// or always flatten into a single buffer.
    #[cfg(feature = "http1")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http1")))]
    pub fn http1_writev(&mut self, enabled: bool) -> &mut Builder {
        self.inner.http1_writev(enabled);
        self
    }

    /// Set whether HTTP/1 connections will write header names as title case at
    /// the socket level.
    #[cfg(feature = "http1")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http1")))]
    pub fn http1_title_case_headers(&mut self, val: bool) -> &mut Self {
        self.inner.http1_title_case_headers(val);
        self
    }

    /// Set whether to support preserving original header cases.
    #[cfg(feature = "http1")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http1")))]
    pub fn http1_preserve_header_case(&mut self, val: bool) -> &mut Self {
        self.inner.http1_preserve_header_case(val);
        self
    }

    /// Set the maximum number of headers.
    #[cfg(feature = "http1")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http1")))]
    pub fn http1_max_headers(&mut self, val: usize) -> &mut Self {
        self.inner.http1_max_headers(val);
        self
    }

    /// Set whether HTTP/0.9 responses should be tolerated.
    #[cfg(feature = "http1")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http1")))]
    pub fn http09_responses(&mut self, val: bool) -> &mut Self {
        self.inner.http09_responses(val);
        self
    }

    /// Set whether the connection **must** use HTTP/2.
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_only(&mut self, val: bool) -> &mut Self {
        self.inner.http2_only(val);
        self
    }

    /// Configures the maximum number of pending reset streams allowed before a GOAWAY will be sent.
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_max_pending_accept_reset_streams(
        &mut self,
        max: impl Into<Option<usize>>,
    ) -> &mut Self {
        self.inner.http2_max_pending_accept_reset_streams(max);
        self
    }

    /// Sets the [`SETTINGS_INITIAL_WINDOW_SIZE`][spec] option for HTTP2
    /// stream-level flow control.
    ///
    /// [spec]: https://http2.github.io/http2-spec/#SETTINGS_INITIAL_WINDOW_SIZE
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_initial_stream_window_size(&mut self, sz: impl Into<Option<u32>>) -> &mut Self {
        self.inner.http2_initial_stream_window_size(sz);
        self
    }

    /// Sets the max connection-level flow control for HTTP2
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_initial_connection_window_size(
        &mut self,
        sz: impl Into<Option<u32>>,
    ) -> &mut Self {
        self.inner.http2_initial_connection_window_size(sz);
        self
    }

    /// Sets the initial maximum of locally initiated (send) streams.
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_initial_max_send_streams(
        &mut self,
        initial: impl Into<Option<usize>>,
    ) -> &mut Self {
        self.inner.http2_initial_max_send_streams(initial);
        self
    }

    /// Sets whether to use an adaptive flow control.
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_adaptive_window(&mut self, enabled: bool) -> &mut Self {
        self.inner.http2_adaptive_window(enabled);
        self
    }

    /// Sets the maximum frame size to use for HTTP2.
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_max_frame_size(&mut self, sz: impl Into<Option<u32>>) -> &mut Self {
        self.inner.http2_max_frame_size(sz);
        self
    }

    /// Sets the max size of received header frames for HTTP2.
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_max_header_list_size(&mut self, max: u32) -> &mut Self {
        self.inner.http2_max_header_list_size(max);
        self
    }

    /// Sets an interval for HTTP2 Ping frames should be sent to keep a
    /// connection alive.
    ///
    /// Pass `None` to disable HTTP2 keep-alive.
    ///
    /// Default is currently disabled.
    ///
    /// # Cargo Feature
    ///
    /// Requires the `tokio` cargo feature to be enabled.
    #[cfg(feature = "tokio")]
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_keep_alive_interval(
        &mut self,
        interval: impl Into<Option<Duration>>,
    ) -> &mut Self {
        self.inner.http2_keep_alive_interval(interval);
        self
    }

    /// Sets a timeout for receiving an acknowledgement of the keep-alive ping.
    ///
    /// If the ping is not acknowledged within the timeout, the connection will
    /// be closed. Does nothing if `http2_keep_alive_interval` is disabled.
    ///
    /// Default is 20 seconds.
    ///
    /// # Cargo Feature
    ///
    /// Requires the `tokio` cargo feature to be enabled.
    #[cfg(feature = "tokio")]
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_keep_alive_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.inner.http2_keep_alive_timeout(timeout);
        self
    }

    /// Sets whether HTTP2 keep-alive should apply while the connection is idle.
    ///
    /// If disabled, keep-alive pings are only sent while there are open
    /// request/responses streams. If enabled, pings are also sent when no
    /// streams are active. Does nothing if `http2_keep_alive_interval` is
    /// disabled.
    ///
    /// Default is `false`.
    ///
    /// # Cargo Feature
    ///
    /// Requires the `tokio` cargo feature to be enabled.
    #[cfg(feature = "tokio")]
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_keep_alive_while_idle(&mut self, enabled: bool) -> &mut Self {
        self.inner.http2_keep_alive_while_idle(enabled);
        self
    }

    /// Sets the maximum number of HTTP2 concurrent locally reset streams.
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_max_concurrent_reset_streams(&mut self, max: usize) -> &mut Self {
        self.inner.http2_max_concurrent_reset_streams(max);
        self
    }

    /// Provide a timer to be used for h2
    pub fn timer<M>(&mut self, timer: M) -> &mut Self
    where
        M: Timer + Send + Sync + 'static,
    {
        self.inner.timer(timer);
        self
    }

    /// Provide a timer to be used for timeouts and intervals in connection pools.
    pub fn pool_timer<M>(&mut self, timer: M) -> &mut Self
    where
        M: Timer + Clone + Send + Sync + 'static,
    {
        self.inner.pool_timer(timer);
        self
    }

    /// Set the maximum write buffer size for each HTTP/2 stream.
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_max_send_buf_size(&mut self, max: usize) -> &mut Self {
        self.inner.http2_max_send_buf_size(max);
        self
    }

    /// Set whether to retry requests that get disrupted before ever starting
    /// to write.
    #[inline]
    pub fn retry_canceled_requests(&mut self, val: bool) -> &mut Self {
        self.inner.retry_canceled_requests(val);
        self
    }

    /// Set whether to automatically add the `Host` header to requests.
    #[inline]
    pub fn set_host(&mut self, val: bool) -> &mut Self {
        self.inner.set_host(val);
        self
    }

    /// Build a client with this configuration and the default `TokioHttpConnector`.
    #[cfg(feature = "tokio-net")]
    pub fn build_http<B>(&self) -> Client<HttpConnector, B>
    where
        B: Body + Send,
        B::Data: Send,
    {
        let mut connector = HttpConnector::new();
        if self.inner.pool_config.is_enabled() {
            connector.set_keepalive(self.inner.pool_config.idle_timeout);
        }
        Client {
            inner: self.inner.build(connector),
        }
    }

    /// Combine the configuration of this builder with a connector to create a `Client`.
    pub fn build<C, B>(&self, connector: C) -> Client<C, B>
    where
        C: Connect + Clone,
        B: Body + Send,
        B::Data: Send,
    {
        Client {
            inner: self.inner.build(connector),
        }
    }
}

impl fmt::Debug for Builder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Builder").field(&self.inner).finish()
    }
}
