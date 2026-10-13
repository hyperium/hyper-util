use std::error::Error as StdError;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{self, Poll, ready};

use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, Uri};
use http_body::Body;
use hyper::body::Incoming;
use hyper::rt::{Read, ReadBufCursor, Write};
use hyper::upgrade::{OnUpgrade, Upgraded};
use pin_project_lite::pin_project;
use tower_service::Service;

use super::tunnel::Headers;
use crate::client::legacy::connect::{Connected, Connection};

/// Tunnel proxy via HTTP CONNECT, preserving early data.
///
/// This wraps an HTTP service that talks to a proxy, and establishes a tunnel
/// by sending a CONNECT request over it and returning the upgraded IO.
///
/// Unlike [`Tunnel`](super::Tunnel), bytes the destination sends immediately
/// after the tunnel is established are preserved and replayed on the first
/// reads, which is required for protocols where the server speaks first.
///
/// # Wrapping a service
///
/// The wrapped service must send requests to the proxy, whatever destination
/// `HttpConnect` is called with: the `Uri` passed to `call` is only used as
/// the target of the CONNECT request.
///
/// Its response must carry the upgrade of the connection underneath, so that
/// connection must support upgrades (for HTTP/1, it must have been created
/// with `with_upgrades`). A service that rebuilds the response without its
/// extensions will lose the upgrade, and the tunnel then fails with
/// [`HttpConnectError::NoUpgrade`].
///
/// The request body type must be empty, since a CONNECT request cannot carry
/// one. `B::default()` is used to create it.
///
/// # HTTP versions
///
/// Since the tunnel is established with a regular request, it works over any
/// HTTP version the service speaks.
///
/// Over HTTP/1, the connection is consumed by the tunnel, so a service built
/// on it can establish only one. Note also that
/// `hyper::client::conn::http1::SendRequest` is not `Clone`, which the
/// `legacy::Client` requires of a connector; using `HttpConnect` with that
/// client needs a service that is.
///
/// Over HTTP/2, each tunnel is a single stream, so one connection to the
/// proxy can carry several of them. Plain CONNECT tunneling does not need the
/// extended CONNECT protocol, which only governs requests carrying a
/// `:protocol` (such as WebSockets over HTTP/2).
pub struct HttpConnect<S, B> {
    headers: Headers,
    inner: S,
    // `fn() -> B` so that `HttpConnect` is `Send`/`Sync` regardless of `B`.
    _body: PhantomData<fn() -> B>,
}

/// An established CONNECT tunnel returned by [`HttpConnect`].
///
/// Reads first drain any bytes the destination sent immediately after the
/// tunnel was established, then continue on the underlying connection.
///
/// The wrapped service hides the underlying transport, so [`Connection`]
/// reports no extra information about it.
pub struct Tunneled {
    inner: Upgraded,
}

/// Error returned by the [`HttpConnect`] connector.
#[derive(Debug)]
#[non_exhaustive]
pub enum HttpConnectError {
    /// The destination could not be used as a CONNECT target.
    InvalidTarget(Box<dyn StdError + Send + Sync>),
    /// The destination URI is missing a host.
    MissingHost,
    /// The response was successful, but carried no tunnel.
    ///
    /// Either the status was not one the proxy upgrades on (over HTTP/2, only
    /// `200` is), or a service in between rebuilt the response without its
    /// extensions.
    NoUpgrade(StatusCode),
    /// The proxy responded with `407 Proxy Authentication Required`.
    ProxyAuthRequired,
    /// The wrapped service failed to send the CONNECT request.
    Service(Box<dyn StdError + Send + Sync>),
    /// The tunnel could not be upgraded.
    Upgrade(hyper::Error),
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

impl<S, B> HttpConnect<S, B> {
    /// Create a new `HttpConnect` service.
    ///
    /// This wraps an HTTP service that is connected to a tunneling proxy
    /// server. The service must accept requests with an empty body, and its
    /// connection must support upgrades.
    ///
    /// An `HttpConnect` can then be called with any destination, which will
    /// be used as the target of an HTTP CONNECT request sent to the proxy.
    pub fn new(service: S) -> Self {
        Self {
            headers: Headers::Empty,
            inner: service,
            _body: PhantomData,
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
    /// If existing headers have been set, these will be merged, appending
    /// values for names that are already present.
    ///
    /// A single `host` header set here replaces the one that would otherwise
    /// be generated from the destination. Over HTTP/2 that is best avoided: a
    /// `host` disagreeing with the request target makes the request
    /// malformed, since the target is also sent as `:authority`.
    pub fn with_headers(mut self, mut headers: HeaderMap) -> Self {
        // just in case the user passed credentials this way
        for (name, value) in headers.iter_mut() {
            if name == http::header::PROXY_AUTHORIZATION {
                value.set_sensitive(true);
            }
        }

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

impl<S, B> Service<Uri> for HttpConnect<S, B>
where
    S: Service<Request<B>, Response = Response<Incoming>>,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn StdError + Send + Sync>>,
    B: Body + Default,
{
    type Response = Tunneled;
    type Error = HttpConnectError;
    type Future = HttpConnecting<S::Future>;

    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.inner.poll_ready(cx)).map_err(|e| HttpConnectError::Service(e.into()))?;
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        let sending = match connect_request(&dst, &self.headers) {
            Ok(req) => Ok(self.inner.call(req)),
            Err(e) => Err(e),
        };

        HttpConnecting {
            fut: Box::pin(async move {
                // The response is consumed before awaiting the upgrade, so it
                // is never held across an await point.
                let on_upgrade = {
                    let res = sending?
                        .await
                        .map_err(|e| HttpConnectError::Service(e.into()))?;

                    let status = res.status();
                    if status == StatusCode::PROXY_AUTHENTICATION_REQUIRED {
                        return Err(HttpConnectError::ProxyAuthRequired);
                    }
                    if !status.is_success() {
                        return Err(HttpConnectError::Unsuccessful(status));
                    }
                    // A successful response doesn't guarantee an upgrade: an
                    // HTTP/2 proxy only upgrades on `200`, and a service in
                    // between may have rebuilt the response without the
                    // extension. Report that instead of a vague upgrade error.
                    if res.extensions().get::<OnUpgrade>().is_none() {
                        return Err(HttpConnectError::NoUpgrade(status));
                    }

                    hyper::upgrade::on(res)
                };

                let upgraded = on_upgrade.await.map_err(HttpConnectError::Upgrade)?;

                Ok(Tunneled { inner: upgraded })
            }),
            _marker: PhantomData,
        }
    }
}

fn connect_request<B>(dst: &Uri, headers: &Headers) -> Result<Request<B>, HttpConnectError>
where
    B: Body + Default,
{
    let host = dst.host().ok_or(HttpConnectError::MissingHost)?;
    let port = dst.port_u16().unwrap_or(443);

    // CONNECT uses the authority-form request target. `Uri::host` already
    // brackets an IPv6 literal, so this must not bracket it again.
    let authority = format!("{host}:{port}");

    let mut req = Request::new(B::default());
    *req.method_mut() = Method::CONNECT;
    *req.uri_mut() = authority
        .parse::<Uri>()
        .map_err(|e| HttpConnectError::InvalidTarget(e.into()))?;

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

    // Last, and only if the user didn't provide one: extending appends, so
    // inserting first would send two `Host` headers.
    let host =
        HeaderValue::from_str(&authority).map_err(|e| HttpConnectError::InvalidTarget(e.into()))?;
    req.headers_mut().entry(http::header::HOST).or_insert(host);

    Ok(req)
}

impl<F> Future for HttpConnecting<F> {
    type Output = Result<Tunneled, HttpConnectError>;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        self.project().fut.poll(cx)
    }
}

// Manual impls so that `B`, which is only a marker, doesn't get bounded.
impl<S: Clone, B> Clone for HttpConnect<S, B> {
    fn clone(&self) -> Self {
        Self {
            headers: self.headers.clone(),
            inner: self.inner.clone(),
            _body: PhantomData,
        }
    }
}

impl<S: std::fmt::Debug, B> std::fmt::Debug for HttpConnect<S, B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpConnect")
            .field("headers", &self.headers)
            .field("inner", &self.inner)
            .finish()
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
        Connected::new()
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
            HttpConnectError::InvalidTarget(_) => f.write_str("invalid destination"),
            HttpConnectError::MissingHost => f.write_str("missing destination host"),
            HttpConnectError::NoUpgrade(status) => {
                write!(f, "proxy replied {status} without upgrading the connection")
            }
            HttpConnectError::ProxyAuthRequired => f.write_str("proxy authorization required"),
            HttpConnectError::Service(_) => f.write_str("failed to send CONNECT request"),
            HttpConnectError::Upgrade(_) => f.write_str("failed to upgrade tunnel"),
            HttpConnectError::Unsuccessful(status) => write!(f, "unsuccessful status ({status})"),
        }
    }
}

impl std::error::Error for HttpConnectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            HttpConnectError::InvalidTarget(e) => Some(&**e),
            HttpConnectError::Service(e) => Some(&**e),
            HttpConnectError::Upgrade(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(all(test, feature = "tokio", feature = "http1"))]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;
    use http::{HeaderMap, HeaderValue, Request, Response};
    use http_body_util::Empty;
    use hyper::body::Incoming;
    use hyper::client::conn::http1;
    use hyper::rt::Write as _;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
    use tower_service::Service;

    use super::{Headers, HttpConnect, HttpConnectError, Tunneled};
    use crate::rt::TokioIo;

    // `SendRequest` doesn't implement `tower_service::Service`, so wrap it.
    struct SendRequestService(http1::SendRequest<Empty<Bytes>>);

    impl Service<Request<Empty<Bytes>>> for SendRequestService {
        type Response = Response<Incoming>;
        type Error = hyper::Error;
        type Future =
            std::pin::Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(
            &mut self,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Self::Error>> {
            self.0.poll_ready(cx)
        }

        fn call(&mut self, req: Request<Empty<Bytes>>) -> Self::Future {
            Box::pin(self.0.send_request(req))
        }
    }

    /// Read a request head, up to and including the empty line.
    ///
    /// Header names are lowercased, values are left alone.
    async fn read_request_head(server: &mut DuplexStream) -> String {
        let mut head = Vec::new();

        // A timeout, so that a request that never arrives fails the test
        // instead of hanging it in `proxy.await`.
        tokio::time::timeout(Duration::from_secs(1), async {
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                server.read_exact(&mut byte).await.unwrap();
                head.push(byte[0]);
            }
        })
        .await
        .expect("a request head should arrive");

        String::from_utf8(head)
            .unwrap()
            .lines()
            .enumerate()
            .map(|(i, line)| match line.split_once(':') {
                // The request line is left alone; it contains a colon too.
                Some((name, value)) if i > 0 => {
                    format!("{}:{}\r\n", name.to_lowercase(), value)
                }
                _ => format!("{line}\r\n"),
            })
            .collect()
    }

    /// Read from a tunnel until `expected` has arrived, or fail.
    async fn read_exactly(io: &mut Tunneled, expected: &[u8]) -> Vec<u8> {
        let mut received = Vec::new();
        let mut buf = [0u8; 64];

        // A timeout, so that losing data fails the test instead of hanging it.
        tokio::time::timeout(Duration::from_secs(1), async {
            while received.len() < expected.len() {
                let n = crate::rt::read(io, &mut buf).await.unwrap();
                assert_ne!(n, 0, "eof before all data was received");
                received.extend_from_slice(&buf[..n]);
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for {expected:?}, only received {:?}",
                String::from_utf8_lossy(&received)
            )
        });

        received
    }

    /// Tunnel to `dst` through a proxy that is scripted by `proxy`.
    async fn connect_to<P, F>(
        headers: Headers,
        dst: &str,
        proxy: P,
    ) -> (
        Result<Tunneled, HttpConnectError>,
        tokio::task::JoinHandle<()>,
    )
    where
        P: FnOnce(DuplexStream) -> F + Send + 'static,
        F: Future<Output = ()> + Send + 'static,
    {
        let (client, server) = tokio::io::duplex(1024);
        let proxy = tokio::spawn(proxy(server));

        let (sender, conn) = http1::handshake(TokioIo::new(client)).await.unwrap();
        tokio::spawn(conn.with_upgrades());

        // Through the public builders, so that they're covered too.
        let service = SendRequestService(sender);
        let mut connector = match headers {
            Headers::Empty => HttpConnect::new(service),
            Headers::Auth(auth) => HttpConnect::new(service).with_auth(auth),
            Headers::Extra(extra) => HttpConnect::new(service).with_headers(extra),
        };

        let result =
            tokio::time::timeout(Duration::from_secs(1), connector.call(dst.parse().unwrap()))
                .await
                .expect("connecting should not hang");

        (result, proxy)
    }

    /// Connect `HttpConnect` to a proxy that is scripted by `proxy`.
    async fn connect<P, F>(
        headers: Headers,
        proxy: P,
    ) -> (
        Result<Tunneled, HttpConnectError>,
        tokio::task::JoinHandle<()>,
    )
    where
        P: FnOnce(DuplexStream) -> F + Send + 'static,
        F: Future<Output = ()> + Send + 'static,
    {
        connect_to(headers, "https://example.com", proxy).await
    }

    /// A proxy that accepts the tunnel, handing the request head back.
    async fn accepting(
        mut server: DuplexStream,
        head_tx: tokio::sync::oneshot::Sender<String>,
    ) -> DuplexStream {
        let head = read_request_head(&mut server).await;
        head_tx.send(head).unwrap();
        server
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await
            .unwrap();
        server
    }

    #[tokio::test]
    async fn established() {
        let (head_tx, head_rx) = tokio::sync::oneshot::channel();
        let (result, proxy) = connect(Headers::Empty, |server| async move {
            accepting(server, head_tx).await;
        })
        .await;

        result.expect("200 response should establish the tunnel");
        let head = head_rx.await.unwrap();
        assert!(
            head.starts_with("CONNECT example.com:443 HTTP/1.1\r\n"),
            "unexpected request line: {head:?}"
        );
        assert!(
            head.contains("host: example.com:443\r\n"),
            "missing host header: {head:?}"
        );
        proxy.await.unwrap();
    }

    #[tokio::test]
    async fn auth_header_is_sent() {
        let (head_tx, head_rx) = tokio::sync::oneshot::channel();
        let auth = Headers::Auth(HeaderValue::from_static("Basic dGVzdA=="));
        let (result, proxy) = connect(auth, |server| async move {
            accepting(server, head_tx).await;
        })
        .await;

        result.expect("200 response should establish the tunnel");
        let head = head_rx.await.unwrap();
        assert!(
            head.contains("proxy-authorization: Basic dGVzdA==\r\n"),
            "missing auth header: {head:?}"
        );
        proxy.await.unwrap();
    }

    #[tokio::test]
    async fn extra_headers_are_sent() {
        let (head_tx, head_rx) = tokio::sync::oneshot::channel();
        let mut extra = HeaderMap::new();
        extra.insert("x-proxy-tag", HeaderValue::from_static("tagged"));

        let (result, proxy) = connect(Headers::Extra(extra), |server| async move {
            accepting(server, head_tx).await;
        })
        .await;

        result.expect("200 response should establish the tunnel");
        let head = head_rx.await.unwrap();
        assert!(
            head.contains("x-proxy-tag: tagged\r\n"),
            "missing extra header: {head:?}"
        );
        proxy.await.unwrap();
    }

    #[tokio::test]
    async fn host_header_is_sent_once() {
        let (head_tx, head_rx) = tokio::sync::oneshot::channel();
        // Extending a `HeaderMap` appends, so a generated `host` would be
        // sent alongside this one, which makes the request malformed.
        let mut extra = HeaderMap::new();
        extra.insert(http::header::HOST, HeaderValue::from_static("proxy.test"));

        let (result, proxy) = connect(Headers::Extra(extra), |server| async move {
            accepting(server, head_tx).await;
        })
        .await;

        result.expect("200 response should establish the tunnel");
        let head = head_rx.await.unwrap();
        assert_eq!(
            head.matches("host:").count(),
            1,
            "expected exactly one host header: {head:?}"
        );
        assert!(
            head.contains("host: proxy.test\r\n"),
            "user host header should win: {head:?}"
        );
        proxy.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_closes_the_tunnel() {
        let (head_tx, _head_rx) = tokio::sync::oneshot::channel();
        let (result, proxy) = connect(Headers::Empty, |server| async move {
            let mut server = accepting(server, head_tx).await;
            // The peer must see the shutdown as an end of stream. Timed out,
            // so that a shutdown that doesn't arrive fails instead of hanging.
            let mut buf = [0u8; 8];
            let n = tokio::time::timeout(Duration::from_secs(1), server.read(&mut buf))
                .await
                .expect("shutdown should reach the peer")
                .unwrap();
            assert_eq!(n, 0, "expected eof after shutdown");
        })
        .await;

        let mut io = result.expect("200 response should establish the tunnel");
        std::future::poll_fn(|cx| std::pin::Pin::new(&mut io).poll_shutdown(cx))
            .await
            .expect("shutdown");
        proxy.await.unwrap();
    }

    #[tokio::test]
    async fn early_data_is_preserved() {
        let (result, proxy) = connect(Headers::Empty, |mut server| async move {
            read_request_head(&mut server).await;
            // Early data in the same write as the response.
            server
                .write_all(b"HTTP/1.1 200 OK\r\n\r\nHELLO")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            server.write_all(b" WORLD").await.unwrap();
        })
        .await;

        let mut io = result.expect("early data must not prevent establishing the tunnel");

        // The early bytes must come through first, in order.
        let received = read_exactly(&mut io, b"HELLO WORLD").await;
        assert_eq!(received, b"HELLO WORLD");
        proxy.await.unwrap();
    }

    #[tokio::test]
    async fn tunnel_is_bidirectional() {
        let (head_tx, _head_rx) = tokio::sync::oneshot::channel();
        let (result, proxy) = connect(Headers::Empty, |server| async move {
            let mut server = accepting(server, head_tx).await;
            let mut ping = [0u8; 4];
            server.read_exact(&mut ping).await.unwrap();
            assert_eq!(&ping, b"ping");
            server.write_all(b"pong").await.unwrap();
        })
        .await;

        let mut io = result.expect("200 response should establish the tunnel");

        crate::rt::write_all(&mut io, b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        let n = crate::rt::read(&mut io, &mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"pong");
        proxy.await.unwrap();
    }

    async fn failing_connect(response: &'static [u8]) -> HttpConnectError {
        let (result, proxy) = connect(Headers::Empty, move |mut server| async move {
            read_request_head(&mut server).await;
            server.write_all(response).await.unwrap();
        })
        .await;

        let err = result.expect_err("non-200 response should fail the handshake");
        proxy.await.unwrap();
        err
    }

    #[tokio::test]
    async fn proxy_auth_required() {
        let err = failing_connect(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").await;
        assert!(matches!(err, HttpConnectError::ProxyAuthRequired));
    }

    #[tokio::test]
    async fn non_2xx_is_unsuccessful() {
        let err = failing_connect(b"HTTP/1.1 500 Internal Server Error\r\n\r\n").await;
        match err {
            HttpConnectError::Unsuccessful(status) => assert_eq!(status, 500),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn eof_before_response() {
        let (result, proxy) = connect(Headers::Empty, |mut server| async move {
            read_request_head(&mut server).await;
            drop(server);
        })
        .await;

        let err = result.expect_err("eof should fail the handshake");
        assert!(matches!(err, HttpConnectError::Service(_)));
        proxy.await.unwrap();
    }

    #[tokio::test]
    async fn success_without_an_upgrade_is_reported() {
        // A 2xx that hyper doesn't treat as an upgrade must not come back as a
        // vague upgrade failure.
        let err = failing_connect(b"HTTP/1.1 204 No Content\r\n\r\n").await;
        match err {
            HttpConnectError::NoUpgrade(status) => assert_eq!(status, 204),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn ipv6_target_is_bracketed_once() {
        let (head_tx, head_rx) = tokio::sync::oneshot::channel();
        let (result, proxy) = connect_to(
            Headers::Empty,
            "https://[2001:db8::1]:8443",
            |server| async move {
                accepting(server, head_tx).await;
            },
        )
        .await;

        result.expect("an IPv6 destination should establish the tunnel");
        let head = head_rx.await.unwrap();
        assert!(
            head.starts_with("CONNECT [2001:db8::1]:8443 HTTP/1.1\r\n"),
            "unexpected request line: {head:?}"
        );
        assert!(
            head.contains("host: [2001:db8::1]:8443\r\n"),
            "unexpected host header: {head:?}"
        );
        proxy.await.unwrap();
    }

    #[tokio::test]
    async fn missing_host_is_rejected() {
        let (client, _server) = tokio::io::duplex(1024);
        let (sender, conn) = http1::handshake(TokioIo::new(client)).await.unwrap();
        tokio::spawn(conn.with_upgrades());

        let mut connector = HttpConnect::new(SendRequestService(sender));
        let err = connector
            .call("/no-host".parse().unwrap())
            .await
            .expect_err("a uri without a host should be rejected");
        assert!(matches!(err, HttpConnectError::MissingHost));
    }

    // `HttpConnect` must satisfy the bounds of a `legacy::Client` connector.
    // The service has to be `Clone + Send + Sync`, which an HTTP/1
    // `SendRequest` is not, so this stands in for one that is.
    #[allow(dead_code)]
    fn assert_usable_as_connector() {
        #[derive(Clone)]
        struct CloneableService;

        impl Service<Request<Empty<Bytes>>> for CloneableService {
            type Response = Response<Incoming>;
            type Error = hyper::Error;
            type Future =
                std::pin::Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

            fn poll_ready(
                &mut self,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                unreachable!("only the bounds matter")
            }

            fn call(&mut self, _: Request<Empty<Bytes>>) -> Self::Future {
                unreachable!("only the bounds matter")
            }
        }

        fn is_connector<C: crate::client::legacy::connect::Connect>(_: C) {}

        is_connector(HttpConnect::<_, Empty<Bytes>>::new(CloneableService));
    }
}
