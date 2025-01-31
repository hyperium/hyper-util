use std::future::Future;
use std::marker::{PhantomData, Unpin};
use std::pin::Pin;
use std::task::{self, Poll};

use http::{HeaderValue, Uri};
use hyper::rt::{Read, Write};
use pin_project_lite::pin_project;
use tower_service::Service;

/// Tunnel Proxy via HTTP CONNECT
#[derive(Debug)]
pub struct Tunnel<C> {
    auth: Option<HeaderValue>,
    inner: C,
    proxy_dst: Uri,
}

#[derive(Debug)]
pub enum TunnelError<C> {
    Inner(C),
    Io(std::io::Error),
    MissingHost,
    ProxyAuthRequired,
    ProxyHeadersTooLong,
    TunnelUnexpectedEof,
    TunnelUnsuccessful,
}

pin_project! {
    // Not publicly exported (so missing_docs doesn't trigger).
    //
    // We return this `Future` instead of the `Pin<Box<dyn Future>>` directly
    // so that users don't rely on it fitting in a `Pin<Box<dyn Future>>` slot
    // (and thus we can change the type in the future).
    #[must_use = "futures do nothing unless polled"]
    #[allow(missing_debug_implementations)]
    pub struct Tunneling<F, T, E> {
        #[pin]
        fut: BoxTunneling<T, E>,
        _marker: PhantomData<F>,
    }
}

type BoxTunneling<T, E> = Pin<Box<dyn Future<Output = Result<T, TunnelError<E>>> + Send>>;

impl<C> Tunnel<C> {
    /// Create a new Tunnel service.
    pub fn new(proxy_dst: Uri, connector: C) -> Self {
        Self {
            auth: None,
            inner: connector,
            proxy_dst,
        }
    }

    /// Add `proxy-authorization` header value to the CONNECT request.
    pub fn with_auth(mut self, mut auth: HeaderValue) -> Self {
        // just in case the user forgot
        auth.set_sensitive(true);
        self.auth = Some(auth);
        self
    }
}

impl<C> Service<Uri> for Tunnel<C>
where
    C: Service<Uri>,
    C::Future: Send + 'static,
    C::Response: Read + Write + Unpin + Send + 'static,
    C::Error: Send + 'static,
{
    type Response = C::Response;
    type Error = TunnelError<C::Error>;
    type Future = Tunneling<C::Future, C::Response, C::Error>;

    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        futures_util::ready!(self.inner.poll_ready(cx)).map_err(TunnelError::Inner)?;
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        let connecting = self.inner.call(self.proxy_dst.clone());

        Tunneling {
            fut: Box::pin(async move {
                let conn = connecting.await.map_err(TunnelError::Inner)?;
                tunnel(
                    conn,
                    dst.host().ok_or(TunnelError::MissingHost)?,
                    dst.port().map(|p| p.as_u16()).unwrap_or(443),
                    None,
                    None,
                )
                .await
            }),
            _marker: PhantomData,
        }
    }
}

impl<F, T, E> Future for Tunneling<F, T, E>
where
    F: Future<Output = Result<T, E>>,
{
    type Output = Result<T, TunnelError<E>>;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        self.project().fut.poll(cx)
    }
}

async fn tunnel<T, E>(
    mut conn: T,
    host: &str,
    port: u16,
    user_agent: Option<HeaderValue>,
    auth: Option<HeaderValue>,
) -> Result<T, TunnelError<E>>
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

    // user-agent
    if let Some(user_agent) = user_agent {
        buf.extend_from_slice(b"User-Agent: ");
        buf.extend_from_slice(user_agent.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }

    // proxy-authorization
    if let Some(value) = auth {
        //log::debug!("tunnel to {host}:{port} using basic auth");
        buf.extend_from_slice(b"Proxy-Authorization: ");
        buf.extend_from_slice(value.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }

    // headers end
    buf.extend_from_slice(b"\r\n");

    crate::rt::write_all(&mut conn, &buf)
        .await
        .map_err(TunnelError::Io)?;

    let mut buf = [0; 8192];
    let mut pos = 0;

    loop {
        let n = crate::rt::read(&mut conn, &mut buf[pos..])
            .await
            .map_err(TunnelError::Io)?;

        if n == 0 {
            return Err(TunnelError::TunnelUnexpectedEof);
        }
        pos += n;

        let recvd = &buf[..pos];
        if recvd.starts_with(b"HTTP/1.1 200") || recvd.starts_with(b"HTTP/1.0 200") {
            if recvd.ends_with(b"\r\n\r\n") {
                return Ok(conn);
            }
            if pos == buf.len() {
                return Err(TunnelError::ProxyHeadersTooLong);
            }
        // else read more
        } else if recvd.starts_with(b"HTTP/1.1 407") {
            return Err(TunnelError::ProxyAuthRequired);
        } else {
            return Err(TunnelError::TunnelUnsuccessful);
        }
    }
}
