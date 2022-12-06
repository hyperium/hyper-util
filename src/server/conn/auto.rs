//! Http1 or Http2 connection.

use crate::{common::rewind::Rewind, rt::tokio_executor::TokioExecutor, Result};
use bytes::Bytes;
use http::{Request, Response};
use http_body::Body;
use hyper::{
    body::Incoming,
    rt::Timer,
    server::conn::{http1, http2},
    service::Service,
};
use std::{error::Error as StdError, marker::Unpin, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};

const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// Http1 or Http2 connection builder.
pub struct Builder {
    http1: http1::Builder,
    http2: http2::Builder<TokioExecutor>,
}

impl Builder {
    /// Create a new auto connection builder.
    pub fn new() -> Self {
        Self {
            http1: http1::Builder::new(),
            http2: http2::Builder::new(TokioExecutor::new()),
        }
    }
    /// Set whether HTTP/1 connections should support half-closures.
    ///
    /// Clients can chose to shutdown their write-side while waiting
    /// for the server to respond. Setting this to `true` will
    /// prevent closing the connection immediately if `read`
    /// detects an EOF in the middle of a request.
    ///
    /// Default is `false`.
    pub fn http1_half_close(&mut self, val: bool) -> &mut Self {
        self.http1.http1_half_close(val);
        self
    }
    /// Enables or disables HTTP/1 keep-alive.
    ///
    /// Default is true.
    pub fn http1_keep_alive(&mut self, val: bool) -> &mut Self {
        self.http1.http1_keep_alive(val);
        self
    }
    /// Set whether HTTP/1 connections will write header names as title case at
    /// the socket level.
    ///
    /// Note that this setting does not affect HTTP/2.
    ///
    /// Default is false.
    pub fn http1_title_case_headers(&mut self, enabled: bool) -> &mut Self {
        self.http1.http1_title_case_headers(enabled);
        self
    }
    /// Set whether to support preserving original header cases.
    ///
    /// Currently, this will record the original cases received, and store them
    /// in a private extension on the `Request`. It will also look for and use
    /// such an extension in any provided `Response`.
    ///
    /// Since the relevant extension is still private, there is no way to
    /// interact with the original cases. The only effect this can have now is
    /// to forward the cases in a proxy-like fashion.
    ///
    /// Note that this setting does not affect HTTP/2.
    ///
    /// Default is false.
    pub fn http1_preserve_header_case(&mut self, enabled: bool) -> &mut Self {
        self.http1.http1_preserve_header_case(enabled);
        self
    }
    /// Set a timeout for reading client request headers. If a client does not
    /// transmit the entire header within this time, the connection is closed.
    ///
    /// Default is None.
    pub fn http1_header_read_timeout(&mut self, read_timeout: Duration) -> &mut Self {
        self.http1.http1_header_read_timeout(read_timeout);
        self
    }
    /// Set whether HTTP/1 connections should try to use vectored writes,
    /// or always flatten into a single buffer.
    ///
    /// Note that setting this to false may mean more copies of body data,
    /// but may also improve performance when an IO transport doesn't
    /// support vectored writes well, such as most TLS implementations.
    ///
    /// Setting this to true will force hyper to use queued strategy
    /// which may eliminate unnecessary cloning on some TLS backends
    ///
    /// Default is `auto`. In this mode hyper will try to guess which
    /// mode to use
    pub fn http1_writev(&mut self, val: bool) -> &mut Self {
        self.http1.http1_writev(val);
        self
    }
    /// Set the maximum buffer size for the connection.
    ///
    /// Default is ~400kb.
    ///
    /// # Panics
    ///
    /// The minimum value allowed is 8192. This method panics if the passed `max` is less than the minimum.
    #[cfg(feature = "http1")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http1")))]
    pub fn http1_max_buf_size(&mut self, max: usize) -> &mut Self {
        self.http1.max_buf_size(max);
        self
    }
    /// Aggregates flushes to better support pipelined responses.
    ///
    /// Experimental, may have bugs.
    ///
    /// Default is false.
    pub fn http1_pipeline_flush(&mut self, enabled: bool) -> &mut Self {
        self.http1.pipeline_flush(enabled);
        self
    }
    /// Set the timer used in background tasks.
    pub fn http1_timer<M>(&mut self, timer: M) -> &mut Self
    where
        M: Timer + Send + Sync + 'static,
    {
        self.http1.timer(timer);
        self
    }
    /// Sets the [`SETTINGS_INITIAL_WINDOW_SIZE`][spec] option for HTTP2
    /// stream-level flow control.
    ///
    /// Passing `None` will do nothing.
    ///
    /// If not set, hyper will use a default.
    ///
    /// [spec]: https://http2.github.io/http2-spec/#SETTINGS_INITIAL_WINDOW_SIZE
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_initial_stream_window_size(&mut self, sz: impl Into<Option<u32>>) -> &mut Self {
        self.http2.http2_initial_stream_window_size(sz);
        self
    }
    /// Sets the max connection-level flow control for HTTP2.
    ///
    /// Passing `None` will do nothing.
    ///
    /// If not set, hyper will use a default.
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_initial_connection_window_size(
        &mut self,
        sz: impl Into<Option<u32>>,
    ) -> &mut Self {
        self.http2.http2_initial_connection_window_size(sz);
        self
    }
    /// Sets whether to use an adaptive flow control.
    ///
    /// Enabling this will override the limits set in
    /// `http2_initial_stream_window_size` and
    /// `http2_initial_connection_window_size`.
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_adaptive_window(&mut self, enabled: bool) -> &mut Self {
        self.http2.http2_adaptive_window(enabled);
        self
    }
    /// Sets the maximum frame size to use for HTTP2.
    ///
    /// Passing `None` will do nothing.
    ///
    /// If not set, hyper will use a default.
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_max_frame_size(&mut self, sz: impl Into<Option<u32>>) -> &mut Self {
        self.http2.http2_max_frame_size(sz);
        self
    }
    /// Sets the [`SETTINGS_MAX_CONCURRENT_STREAMS`][spec] option for HTTP2
    /// connections.
    ///
    /// Default is no limit (`std::u32::MAX`). Passing `None` will do nothing.
    ///
    /// [spec]: https://http2.github.io/http2-spec/#SETTINGS_MAX_CONCURRENT_STREAMS
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_max_concurrent_streams(&mut self, max: impl Into<Option<u32>>) -> &mut Self {
        self.http2.http2_max_concurrent_streams(max);
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
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_keep_alive_interval(
        &mut self,
        interval: impl Into<Option<Duration>>,
    ) -> &mut Self {
        self.http2.http2_keep_alive_interval(interval);
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
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_keep_alive_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.http2.http2_keep_alive_timeout(timeout);
        self
    }
    /// Set the maximum write buffer size for each HTTP/2 stream.
    ///
    /// Default is currently ~400KB, but may change.
    ///
    /// # Panics
    ///
    /// The value must be no larger than `u32::MAX`.
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_max_send_buf_size(&mut self, max: usize) -> &mut Self {
        self.http2.http2_max_send_buf_size(max);
        self
    }
    /// Enables the [extended CONNECT protocol].
    ///
    /// [extended CONNECT protocol]: https://datatracker.ietf.org/doc/html/rfc8441#section-4
    #[cfg(feature = "http2")]
    pub fn http2_enable_connect_protocol(&mut self) -> &mut Self {
        self.http2.http2_enable_connect_protocol();
        self
    }
    /// Sets the max size of received header frames.
    ///
    /// Default is currently ~16MB, but may change.
    #[cfg(feature = "http2")]
    #[cfg_attr(docsrs, doc(cfg(feature = "http2")))]
    pub fn http2_max_header_list_size(&mut self, max: u32) -> &mut Self {
        self.http2.http2_max_header_list_size(max);
        self
    }
    /// Set the timer used in background tasks.
    pub fn http2_timer<M>(&mut self, timer: M) -> &mut Self
    where
        M: Timer + Send + Sync + 'static,
    {
        self.http2.timer(timer);
        self
    }

    /// Bind a connection together with a [`Service`].
    pub async fn serve_connection<I, S, B>(&self, mut io: I, service: S) -> Result<()>
    where
        S: Service<Request<Incoming>, Response = Response<B>> + Send,
        S::Future: Send + 'static,
        S::Error: Into<Box<dyn StdError + Send + Sync>>,
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn StdError + Send + Sync>>,
        I: AsyncRead + AsyncWrite + Unpin + 'static,
    {
        let mut buf = Vec::new();

        let protocol = loop {
            if buf.len() < 24 {
                io.read_buf(&mut buf).await?;

                let len = buf.len().min(H2_PREFACE.len());

                if buf[0..len] != H2_PREFACE[0..len] {
                    break Protocol::H1;
                }
            } else {
                break Protocol::H2;
            }
        };

        let io = Rewind::new_buffered(io, Bytes::from(buf));

        match protocol {
            Protocol::H1 => self.http1.serve_connection(io, service).await?,
            Protocol::H2 => self.http2.serve_connection(io, service).await?,
        }

        Ok(())
    }
}

enum Protocol {
    H1,
    H2,
}

#[cfg(test)]
mod tests {
    use super::Builder;
    use crate::rt::tokio_executor::TokioExecutor;
    use http::{Request, Response};
    use http_body::Body;
    use http_body_util::{BodyExt, Empty, Full};
    use hyper::{body, body::Bytes, client, service::service_fn};
    use std::{convert::Infallible, error::Error as StdError, net::SocketAddr};
    use tokio::net::{TcpListener, TcpStream};

    const BODY: &'static [u8] = b"Hello, world!";

    #[tokio::test]
    async fn http1() {
        let addr = start_server().await;
        let mut sender = connect_h1(addr).await;

        let response = sender
            .send_request(Request::new(Empty::<Bytes>::new()))
            .await
            .unwrap();

        let body = response.into_body().collect().await.unwrap().to_bytes();

        assert_eq!(body, BODY);
    }

    #[tokio::test]
    async fn http2() {
        let addr = start_server().await;
        let mut sender = connect_h2(addr).await;

        let response = sender
            .send_request(Request::new(Empty::<Bytes>::new()))
            .await
            .unwrap();

        let body = response.into_body().collect().await.unwrap().to_bytes();

        assert_eq!(body, BODY);
    }

    async fn connect_h1<B>(addr: SocketAddr) -> client::conn::http1::SendRequest<B>
    where
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn StdError + Send + Sync>>,
    {
        let stream = TcpStream::connect(addr).await.unwrap();
        let (sender, connection) = client::conn::http1::handshake(stream).await.unwrap();

        tokio::spawn(connection);

        sender
    }

    async fn connect_h2<B>(addr: SocketAddr) -> client::conn::http2::SendRequest<B>
    where
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn StdError + Send + Sync>>,
    {
        let stream = TcpStream::connect(addr).await.unwrap();
        let (sender, connection) = client::conn::http2::Builder::new()
            .executor(TokioExecutor::new())
            .handshake(stream)
            .await
            .unwrap();

        tokio::spawn(connection);

        sender
    }

    async fn start_server() -> SocketAddr {
        let addr: SocketAddr = ([127, 0, 0, 1], 0).into();
        let listener = TcpListener::bind(addr).await.unwrap();

        let local_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::task::spawn(async move {
                    let _ = Builder::new()
                        .serve_connection(stream, service_fn(hello))
                        .await;
                });
            }
        });

        local_addr
    }

    async fn hello(_req: Request<body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
        Ok(Response::new(Full::new(Bytes::from(BODY))))
    }
}
