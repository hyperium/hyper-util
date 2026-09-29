use std::error::Error as StdError;
use std::future::poll_fn;
use std::marker::PhantomData;
use std::pin::{Pin, pin};
use std::task::{self, Poll, ready};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Request, StatusCode, Uri};
use hyper::rt::{Read, ReadBufCursor, Write};
use hyper::upgrade::Upgraded;
use pin_project_lite::pin_project;
use tower_service::Service;

use super::tunnel::Headers;
use crate::client::legacy::connect::{Connected, Connection};

/// Tunnel proxy via HTTP CONNECT, preserving early data.
///
/// This is a connector that can be used by the `legacy::Client`. It wraps
/// another connector, and after getting an underlying connection, it
/// establishes a tunnel by sending a real HTTP CONNECT request over an
/// HTTP/1 connection and returning the upgraded IO.
///
/// Unlike [`Tunnel`](super::Tunnel), any bytes the destination sends
/// immediately after the tunnel is established are preserved and replayed
/// on the first reads, which is required for protocols where the server
/// speaks first.
#[derive(Debug, Clone)]
pub struct HttpConnect<C> {
    headers: Headers,
    inner: C,
    proxy_dst: Uri,
}

/// An established CONNECT tunnel returned by [`HttpConnect`].
///
/// Reads first drain any bytes the destination sent immediately after the
/// tunnel was established, then continue on the underlying connection.
pub struct Tunneled {
    inner: Upgraded,
    connected: Connected,
}

/// Error returned by the [`HttpConnect`] connector.
#[derive(Debug)]
#[non_exhaustive]
pub enum HttpConnectError {
    /// The underlying connector failed to connect to the proxy.
    ConnectFailed(Box<dyn StdError + Send + Sync>),
    /// The HTTP/1 handshake with the proxy failed.
    Handshake(hyper::Error),
    /// The destination URI is missing a host.
    MissingHost,
    /// The proxy responded with `407 Proxy Authentication Required`.
    ProxyAuthRequired,
    /// The connection closed before the tunnel was established.
    UnexpectedEof,
    /// The proxy responded with a non-successful status.
    Unsuccessful(StatusCode),
}

pin_project! {
    // Not publicly exported (so missing_docs doesn't trigger).
    //
    // We return this `Future` instead of the `Pin<Box<dyn Future>>` directly
    // so that users don't rely on it fitting in a `Pin<Box<dyn Future>>` slot
    // (and thus we can change the type in the future).
    #[must_use = "futures do nothing unless polled"]
    #[allow(missing_debug_implementations)]
    pub struct HttpConnecting<F> {
        #[pin]
        fut: BoxConnecting,
        _marker: PhantomData<F>,
    }
}

type BoxConnecting = Pin<Box<dyn Future<Output = Result<Tunneled, HttpConnectError>> + Send>>;

impl<C> HttpConnect<C> {
    /// Create a new `HttpConnect` service.
    ///
    /// This wraps an underlying connector, and stores the address of a
    /// tunneling proxy server.
    ///
    /// An `HttpConnect` can then be called with any destination. The `dst`
    /// passed to `call` will not be used to create the underlying connection,
    /// but will be used in an HTTP CONNECT request sent to the proxy
    /// destination.
    pub fn new(proxy_dst: Uri, connector: C) -> Self {
        Self {
            headers: Headers::Empty,
            inner: connector,
            proxy_dst,
        }
    }

    /// Add `proxy-authorization` header value to the CONNECT request.
    pub fn with_auth(mut self, mut auth: HeaderValue) -> Self {
        // just in case the user forgot
        auth.set_sensitive(true);
        match self.headers {
            Headers::Empty => {
                self.headers = Headers::Auth(auth);
            }
            Headers::Auth(ref mut existing) => {
                *existing = auth;
            }
            Headers::Extra(ref mut extra) => {
                extra.insert(http::header::PROXY_AUTHORIZATION, auth);
            }
        }

        self
    }

    /// Add extra headers to be sent with the CONNECT request.
    ///
    /// If existing headers have been set, these will be merged.
    pub fn with_headers(mut self, mut headers: HeaderMap) -> Self {
        match self.headers {
            Headers::Empty => {
                self.headers = Headers::Extra(headers);
            }
            Headers::Auth(auth) => {
                headers
                    .entry(http::header::PROXY_AUTHORIZATION)
                    .or_insert(auth);
                self.headers = Headers::Extra(headers);
            }
            Headers::Extra(ref mut extra) => {
                extra.extend(headers);
            }
        }

        self
    }
}

impl<C> Service<Uri> for HttpConnect<C>
where
    C: Service<Uri>,
    C::Future: Send + 'static,
    C::Response: Read + Write + Connection + Unpin + Send + 'static,
    C::Error: Into<Box<dyn StdError + Send + Sync>>,
{
    type Response = Tunneled;
    type Error = HttpConnectError;
    type Future = HttpConnecting<C::Future>;

    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.inner.poll_ready(cx)).map_err(|e| HttpConnectError::ConnectFailed(e.into()))?;
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        let connecting = self.inner.call(self.proxy_dst.clone());
        let headers = self.headers.clone();

        HttpConnecting {
            fut: Box::pin(async move {
                let conn = connecting
                    .await
                    .map_err(|e| HttpConnectError::ConnectFailed(e.into()))?;
                let connected = conn.connected();
                handshake(
                    conn,
                    connected,
                    dst.host().ok_or(HttpConnectError::MissingHost)?,
                    dst.port().map(|p| p.as_u16()).unwrap_or(443),
                    &headers,
                )
                .await
            }),
            _marker: PhantomData,
        }
    }
}

impl<F> Future for HttpConnecting<F> {
    type Output = Result<Tunneled, HttpConnectError>;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        self.project().fut.poll(cx)
    }
}

async fn handshake<T>(
    io: T,
    connected: Connected,
    host: &str,
    port: u16,
    headers: &Headers,
) -> Result<Tunneled, HttpConnectError>
where
    T: Read + Write + Unpin + Send + 'static,
{
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(HttpConnectError::Handshake)?;
    let mut conn = pin!(conn.with_upgrades());
    // `conn` must not be polled again once it has resolved.
    let mut conn_done = false;

    // CONNECT uses the authority-form request target.
    let authority = if host.contains(':') {
        // an IPv6 literal must be bracketed
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };

    let mut req = Request::new(EmptyBody);
    *req.method_mut() = Method::CONNECT;
    *req.uri_mut() = authority
        .parse::<Uri>()
        .map_err(|_| HttpConnectError::MissingHost)?;
    req.headers_mut().insert(
        http::header::HOST,
        HeaderValue::from_str(&authority).map_err(|_| HttpConnectError::MissingHost)?,
    );

    match headers {
        Headers::Auth(auth) => {
            req.headers_mut()
                .insert(http::header::PROXY_AUTHORIZATION, auth.clone());
        }
        Headers::Extra(extra) => {
            req.headers_mut().extend(extra.clone());
        }
        Headers::Empty => (),
    }

    // Drive the connection and the request in this task, rather than
    // requiring an executor to spawn onto: the connection future resolves
    // once the tunnel is upgraded (or fails).
    let res = {
        let mut send = pin!(sender.send_request(req));
        poll_fn(|cx| {
            if let Poll::Ready(result) = send.as_mut().poll(cx) {
                return Poll::Ready(result.map_err(HttpConnectError::Handshake));
            }
            if !conn_done {
                match conn.as_mut().poll(cx) {
                    Poll::Ready(Ok(())) => {
                        conn_done = true;
                        // The connection may have delivered the response (and
                        // upgraded) in that same poll.
                        if let Poll::Ready(result) = send.as_mut().poll(cx) {
                            return Poll::Ready(result.map_err(HttpConnectError::Handshake));
                        }
                        return Poll::Ready(Err(HttpConnectError::UnexpectedEof));
                    }
                    Poll::Ready(Err(e)) => {
                        conn_done = true;
                        return Poll::Ready(Err(HttpConnectError::Handshake(e)));
                    }
                    Poll::Pending => (),
                }
            }
            Poll::Pending
        })
        .await?
    };

    if res.status() == StatusCode::PROXY_AUTHENTICATION_REQUIRED {
        return Err(HttpConnectError::ProxyAuthRequired);
    }
    if !res.status().is_success() {
        return Err(HttpConnectError::Unsuccessful(res.status()));
    }

    let mut on_upgrade = pin!(hyper::upgrade::on(res));
    let upgraded = poll_fn(|cx| {
        if let Poll::Ready(result) = on_upgrade.as_mut().poll(cx) {
            return Poll::Ready(result.map_err(HttpConnectError::Handshake));
        }
        if !conn_done {
            match conn.as_mut().poll(cx) {
                Poll::Ready(Ok(())) => {
                    conn_done = true;
                    // A successful resolution means the upgrade was just
                    // fulfilled.
                    if let Poll::Ready(result) = on_upgrade.as_mut().poll(cx) {
                        return Poll::Ready(result.map_err(HttpConnectError::Handshake));
                    }
                    return Poll::Ready(Err(HttpConnectError::UnexpectedEof));
                }
                Poll::Ready(Err(e)) => {
                    conn_done = true;
                    return Poll::Ready(Err(HttpConnectError::Handshake(e)));
                }
                Poll::Pending => (),
            }
        }
        Poll::Pending
    })
    .await?;

    // `sender` is kept alive until here so the connection isn't closed
    // before the upgrade completes.
    drop(sender);

    Ok(Tunneled {
        inner: upgraded,
        connected,
    })
}

struct EmptyBody;

impl http_body::Body for EmptyBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut task::Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(None)
    }

    fn is_end_stream(&self) -> bool {
        true
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::SizeHint::with_exact(0)
    }
}

impl Read for Tunneled {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: ReadBufCursor<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl Write for Tunneled {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

impl Connection for Tunneled {
    fn connected(&self) -> Connected {
        self.connected.clone()
    }
}

impl std::fmt::Debug for Tunneled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tunneled").finish_non_exhaustive()
    }
}

impl std::fmt::Display for HttpConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("http connect error: ")?;

        match self {
            HttpConnectError::ConnectFailed(_) => {
                f.write_str("failed to create underlying connection")
            }
            HttpConnectError::Handshake(_) => f.write_str("handshake failed"),
            HttpConnectError::MissingHost => f.write_str("missing destination host"),
            HttpConnectError::ProxyAuthRequired => f.write_str("proxy authorization required"),
            HttpConnectError::UnexpectedEof => {
                f.write_str("connection closed before tunnel established")
            }
            HttpConnectError::Unsuccessful(status) => write!(f, "unsuccessful status ({status})"),
        }
    }
}

impl std::error::Error for HttpConnectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            HttpConnectError::ConnectFailed(e) => Some(&**e),
            HttpConnectError::Handshake(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use std::time::Duration;

    use http::HeaderValue;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    use super::{Headers, HttpConnectError, Tunneled, handshake};
    use crate::client::legacy::connect::Connected;
    use crate::rt::TokioIo;

    async fn read_request_head(server: &mut DuplexStream) -> String {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            server.read_exact(&mut byte).await.unwrap();
            head.push(byte[0]);
        }
        String::from_utf8(head).unwrap().to_lowercase()
    }

    async fn start(
        headers: Headers,
    ) -> (
        Result<Tunneled, HttpConnectError>,
        tokio::sync::mpsc::UnboundedReceiver<String>,
        tokio::task::JoinHandle<DuplexStream>,
    ) {
        let (client, server) = tokio::io::duplex(1024);
        let (head_tx, head_rx) = tokio::sync::mpsc::unbounded_channel();

        let server = tokio::spawn(async move {
            let mut server = server;
            let head = read_request_head(&mut server).await;
            head_tx.send(head).unwrap();
            server
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .unwrap();
            server
        });

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            handshake(
                TokioIo::new(client),
                Connected::new(),
                "example.com",
                443,
                &headers,
            ),
        )
        .await
        .expect("handshake should not hang");

        (result, head_rx, server)
    }

    #[tokio::test]
    async fn established() {
        let (result, mut heads, server) = start(Headers::Empty).await;
        result.expect("200 response should establish the tunnel");
        let head = heads.recv().await.unwrap();
        assert!(
            head.starts_with("connect example.com:443 http/1.1\r\n"),
            "unexpected request line: {head:?}"
        );
        assert!(
            head.contains("host: example.com:443\r\n"),
            "missing host header: {head:?}"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn auth_header_is_sent() {
        let (result, mut heads, server) =
            start(Headers::Auth(HeaderValue::from_static("Basic dGVzdA=="))).await;
        result.expect("200 response should establish the tunnel");
        let head = heads.recv().await.unwrap();
        assert!(
            head.contains("proxy-authorization: basic dgvzda==\r\n"),
            "missing auth header: {head:?}"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn early_data_is_preserved() {
        let (client, mut server) = tokio::io::duplex(1024);
        let server = tokio::spawn(async move {
            read_request_head(&mut server).await;
            // Early data in the same write as the response.
            server
                .write_all(b"HTTP/1.1 200 OK\r\n\r\nHELLO")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            server.write_all(b" WORLD").await.unwrap();
        });

        let mut io = tokio::time::timeout(
            Duration::from_secs(1),
            handshake(
                TokioIo::new(client),
                Connected::new(),
                "example.com",
                443,
                &Headers::Empty,
            ),
        )
        .await
        .expect("handshake should not hang")
        .expect("early data must not prevent establishing the tunnel");

        let mut buf = [0u8; 16];
        let mut received = Vec::new();
        while received.len() < b"HELLO WORLD".len() {
            let n = crate::rt::read(&mut io, &mut buf).await.unwrap();
            assert_ne!(n, 0, "eof before all data was received");
            received.extend_from_slice(&buf[..n]);
        }
        // The early bytes must come through first, in order.
        assert_eq!(received, b"HELLO WORLD");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn tunnel_is_bidirectional() {
        let (client, mut server) = tokio::io::duplex(1024);
        let server = tokio::spawn(async move {
            read_request_head(&mut server).await;
            server
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .unwrap();
            let mut ping = [0u8; 4];
            server.read_exact(&mut ping).await.unwrap();
            assert_eq!(&ping, b"ping");
            server.write_all(b"pong").await.unwrap();
        });

        let mut io = tokio::time::timeout(
            Duration::from_secs(1),
            handshake(
                TokioIo::new(client),
                Connected::new(),
                "example.com",
                443,
                &Headers::Empty,
            ),
        )
        .await
        .expect("handshake should not hang")
        .expect("200 response should establish the tunnel");

        crate::rt::write_all(&mut io, b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        let n = crate::rt::read(&mut io, &mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"pong");
        server.await.unwrap();
    }

    async fn failing_handshake(response: &'static [u8]) -> HttpConnectError {
        let (client, mut server) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            read_request_head(&mut server).await;
            server.write_all(response).await.unwrap();
        });

        tokio::time::timeout(
            Duration::from_secs(1),
            handshake(
                TokioIo::new(client),
                Connected::new(),
                "example.com",
                443,
                &Headers::Empty,
            ),
        )
        .await
        .expect("handshake should not hang")
        .expect_err("non-200 response should fail the handshake")
    }

    #[tokio::test]
    async fn proxy_auth_required() {
        let err = failing_handshake(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").await;
        assert!(matches!(err, HttpConnectError::ProxyAuthRequired));
    }

    #[tokio::test]
    async fn non_2xx_is_unsuccessful() {
        let err = failing_handshake(b"HTTP/1.1 500 Internal Server Error\r\n\r\n").await;
        match err {
            HttpConnectError::Unsuccessful(status) => assert_eq!(status, 500),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn eof_before_response() {
        let (client, mut server) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            read_request_head(&mut server).await;
            drop(server);
        });

        let err = tokio::time::timeout(
            Duration::from_secs(1),
            handshake(
                TokioIo::new(client),
                Connected::new(),
                "example.com",
                443,
                &Headers::Empty,
            ),
        )
        .await
        .expect("handshake should not hang")
        .expect_err("eof should fail the handshake");
        assert!(matches!(
            err,
            HttpConnectError::Handshake(_) | HttpConnectError::UnexpectedEof
        ));
    }
}
