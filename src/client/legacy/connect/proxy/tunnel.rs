use std::error::Error as StdError;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{self, Poll, ready};

use http::{HeaderMap, HeaderValue, StatusCode, Uri, header::PROXY_AUTHENTICATE};
use hyper::rt::{Read, Write};
use pin_project_lite::pin_project;
use tower_service::Service;

/// Tunnel Proxy via HTTP CONNECT
///
/// This is a connector that can be used by the `legacy::Client`. It wraps
/// another connector, and after getting an underlying connection, it creates
/// an HTTP CONNECT tunnel over it.
#[derive(Debug, Clone)]
pub struct Tunnel<C> {
    headers: Headers,
    inner: C,
    proxy_dst: Uri,
}

#[derive(Clone, Debug)]
enum Headers {
    Empty,
    Auth(HeaderValue),
    Extra(HeaderMap),
}

/// An error from establishing a [`Tunnel`].
#[derive(Debug)]
pub struct TunnelError {
    kind: Kind,
}

#[derive(Debug)]
enum Kind {
    ConnectFailed(Box<dyn StdError + Send + Sync>),
    Io(std::io::Error),
    MissingHost,
    ProxyAuthRequired {
        proxy_authenticate: Vec<HeaderValue>,
    },
    ProxyHeadersTooLong,
    TunnelUnexpectedEof,
    TunnelUnsuccessful {
        status: Option<StatusCode>,
    },
}

pin_project! {
    // Not publicly exported (so missing_docs doesn't trigger).
    //
    // We return this `Future` instead of the `Pin<Box<dyn Future>>` directly
    // so that users don't rely on it fitting in a `Pin<Box<dyn Future>>` slot
    // (and thus we can change the type in the future).
    #[must_use = "futures do nothing unless polled"]
    #[allow(missing_debug_implementations)]
    pub struct Tunneling<F, T> {
        #[pin]
        fut: BoxTunneling<T>,
        _marker: PhantomData<F>,
    }
}

type BoxTunneling<T> = Pin<Box<dyn Future<Output = Result<T, TunnelError>> + Send>>;

impl<C> Tunnel<C> {
    /// Create a new Tunnel service.
    ///
    /// This wraps an underlying connector, and stores the address of a
    /// tunneling proxy server.
    ///
    /// A `Tunnel` can then be called with any destination. The `dst` passed to
    /// `call` will not be used to create the underlying connection, but will
    /// be used in an HTTP CONNECT request sent to the proxy destination.
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

impl<C> Service<Uri> for Tunnel<C>
where
    C: Service<Uri>,
    C::Future: Send + 'static,
    C::Response: Read + Write + Unpin + Send + 'static,
    C::Error: Into<Box<dyn StdError + Send + Sync>>,
{
    type Response = C::Response;
    type Error = TunnelError;
    type Future = Tunneling<C::Future, C::Response>;

    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.inner.poll_ready(cx)).map_err(|e| Kind::ConnectFailed(e.into()))?;
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        let connecting = self.inner.call(self.proxy_dst.clone());
        let headers = self.headers.clone();

        Tunneling {
            fut: Box::pin(async move {
                let conn = connecting
                    .await
                    .map_err(|e| Kind::ConnectFailed(e.into()))?;
                tunnel(
                    conn,
                    dst.host().ok_or(Kind::MissingHost)?,
                    super::dst_port(&dst),
                    &headers,
                )
                .await
            }),
            _marker: PhantomData,
        }
    }
}

impl<F, T, E> Future for Tunneling<F, T>
where
    F: Future<Output = Result<T, E>>,
{
    type Output = Result<T, TunnelError>;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        self.project().fut.poll(cx)
    }
}

async fn tunnel<T>(mut conn: T, host: &str, port: u16, headers: &Headers) -> Result<T, TunnelError>
where
    T: Read + Write + Unpin,
{
    let mut buf = format!(
        "\
         CONNECT {host}:{port} HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         "
    )
    .into_bytes();

    match headers {
        Headers::Auth(auth) => {
            buf.extend_from_slice(b"Proxy-Authorization: ");
            buf.extend_from_slice(auth.as_bytes());
            buf.extend_from_slice(b"\r\n");
        }
        Headers::Extra(extra) => {
            for (name, value) in extra {
                buf.extend_from_slice(name.as_str().as_bytes());
                buf.extend_from_slice(b": ");
                buf.extend_from_slice(value.as_bytes());
                buf.extend_from_slice(b"\r\n");
            }
        }
        Headers::Empty => (),
    }

    // headers end
    buf.extend_from_slice(b"\r\n");

    crate::rt::write_all(&mut conn, &buf)
        .await
        .map_err(Kind::Io)?;

    let mut buf = [0; 8192];
    let mut pos = 0;

    loop {
        let n = crate::rt::read(&mut conn, &mut buf[pos..])
            .await
            .map_err(Kind::Io)?;

        if n == 0 {
            return Err(Kind::TunnelUnexpectedEof.into());
        }
        pos += n;

        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut res = httparse::Response::new(&mut headers);
        match res.parse(&buf[..pos]) {
            Ok(httparse::Status::Complete(_)) => match res.code {
                Some(200) => return Ok(conn),
                Some(407) => {
                    let proxy_authenticate = res
                        .headers
                        .iter()
                        .filter(|h| h.name.eq_ignore_ascii_case(PROXY_AUTHENTICATE.as_str()))
                        .filter_map(|h| HeaderValue::from_bytes(h.value).ok())
                        .collect();
                    return Err(Kind::ProxyAuthRequired { proxy_authenticate }.into());
                }
                code => {
                    let status = code.and_then(|code| StatusCode::from_u16(code).ok());
                    return Err(Kind::TunnelUnsuccessful { status }.into());
                }
            },
            Ok(httparse::Status::Partial) => {
                if pos == buf.len() {
                    return Err(Kind::ProxyHeadersTooLong.into());
                }
            }
            Err(_) => return Err(Kind::TunnelUnsuccessful { status: None }.into()),
        }
    }
}

const MAX_HEADERS: usize = 100;

impl TunnelError {
    /// Returns the status of the proxy's response to the `CONNECT`, if the
    /// proxy refused it.
    ///
    /// Returns `None` if the error did not come from a response with a valid
    /// status code.
    pub fn status(&self) -> Option<StatusCode> {
        match self.kind {
            Kind::ProxyAuthRequired { .. } => Some(StatusCode::PROXY_AUTHENTICATION_REQUIRED),
            Kind::TunnelUnsuccessful { status } => status,
            _ => None,
        }
    }

    /// Returns the values of the `Proxy-Authenticate` headers of a
    /// `407 Proxy Authentication Required` response.
    ///
    /// Returns an empty iterator for any other error.
    pub fn proxy_authenticate(&self) -> impl Iterator<Item = &HeaderValue> {
        match &self.kind {
            Kind::ProxyAuthRequired { proxy_authenticate } => proxy_authenticate.iter(),
            _ => [].iter(),
        }
    }
}

impl From<Kind> for TunnelError {
    fn from(kind: Kind) -> Self {
        TunnelError { kind }
    }
}

impl std::fmt::Display for TunnelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("tunnel error: ")?;

        f.write_str(match self.kind {
            Kind::MissingHost => "missing destination host",
            Kind::ProxyAuthRequired { .. } => "proxy authorization required",
            Kind::ProxyHeadersTooLong => "proxy response headers too long",
            Kind::TunnelUnexpectedEof => "unexpected end of file",
            Kind::TunnelUnsuccessful { .. } => "unsuccessful",
            Kind::ConnectFailed(_) => "failed to create underlying connection",
            Kind::Io(_) => "io error establishing tunnel",
        })
    }
}

impl std::error::Error for TunnelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            Kind::Io(e) => Some(e),
            Kind::ConnectFailed(e) => Some(&**e),
            _ => None,
        }
    }
}

#[cfg(all(test, feature = "tokio"))]
mod tests {
    use std::time::Duration;

    use http::StatusCode;
    use tokio::io::AsyncWriteExt;

    use super::{Headers, TunnelError, tunnel};
    use crate::rt::TokioIo;

    async fn handshake(response: &'static [u8]) -> Result<(), TunnelError> {
        let (client, mut server) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            server.write_all(response).await.unwrap();
        });

        tokio::time::timeout(
            Duration::from_secs(1),
            tunnel(TokioIo::new(client), "example.com", 443, &Headers::Empty),
        )
        .await
        .expect("handshake should not hang")
        .map(drop)
    }

    #[tokio::test]
    async fn established() {
        handshake(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await
            .expect("200 response should establish the tunnel");
    }

    #[tokio::test]
    async fn established_with_early_data() {
        handshake(b"HTTP/1.1 200 OK\r\n\r\nHELLO")
            .await
            .expect("early data must not prevent establishing the tunnel");
    }

    #[tokio::test]
    async fn proxy_auth_required() {
        let err = handshake(
            b"HTTP/1.1 407 Proxy Authentication Required\r\n\
              Proxy-Authenticate: Negotiate\r\n\
              Proxy-Connection: close\r\n\
              Proxy-Authenticate: Basic realm=\"proxy\"\r\n\r\n",
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.status(),
            Some(StatusCode::PROXY_AUTHENTICATION_REQUIRED)
        );
        let challenges: Vec<_> = err.proxy_authenticate().collect();
        assert_eq!(challenges, ["Negotiate", "Basic realm=\"proxy\""]);
        assert_eq!(
            err.to_string(),
            "tunnel error: proxy authorization required"
        );
    }

    #[tokio::test]
    async fn non_200_is_unsuccessful() {
        for (response, expected) in [
            (
                &b"HTTP/1.1 403 Forbidden\r\n\r\n"[..],
                StatusCode::FORBIDDEN,
            ),
            (b"HTTP/1.1 502 Bad Gateway\r\n\r\n", StatusCode::BAD_GATEWAY),
        ] {
            let err = handshake(response).await.unwrap_err();
            assert_eq!(err.status(), Some(expected));
            assert_eq!(err.to_string(), "tunnel error: unsuccessful");
        }
    }

    #[tokio::test]
    async fn malformed_status_is_rejected() {
        let err = handshake(b"HTTP/1.1 2000 OK\r\n\r\n").await.unwrap_err();
        assert_eq!(err.status(), None);
        assert_eq!(err.to_string(), "tunnel error: unsuccessful");
    }

    #[tokio::test]
    async fn invalid_status_code_has_no_status() {
        let err = handshake(b"HTTP/1.1 099 Odd\r\n\r\n").await.unwrap_err();
        assert_eq!(err.status(), None);
        assert_eq!(err.to_string(), "tunnel error: unsuccessful");
    }
}
