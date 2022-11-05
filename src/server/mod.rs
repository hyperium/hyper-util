//! Server utilities.

use crate::rt::tokio_executor::TokioExecutor;
use http::{Request, Response};
use http_body::Body;
use hyper::{
    body::Incoming,
    server::conn::{http1, http2},
    service::Service,
};
use pin_project_lite::pin_project;
use std::{
    error::Error as StdError,
    io::Cursor,
    marker::Unpin,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};

const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// Http1 or Http2 connection.
pub struct AutoConn {
    h1: http1::Builder,
    h2: http2::Builder<TokioExecutor>,
}

impl AutoConn {
    /// Create a new AutoConn.
    pub fn new() -> Self {
        Self {
            h1: http1::Builder::new(),
            h2: http2::Builder::new(TokioExecutor::new()),
        }
    }

    /// Http1 configuration.
    pub fn h1(&mut self) -> &mut http1::Builder {
        &mut self.h1
    }

    /// Http2 configuration.
    pub fn h2(&mut self) -> &mut http2::Builder<TokioExecutor> {
        &mut self.h2
    }

    /// Bind a connection together with a [`Service`].
    pub async fn serve_connection<I, S, B>(&self, mut io: I, service: S) -> crate::Result<()>
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

        let io = PrependAsyncRead::new(Cursor::new(buf), io);

        match protocol {
            Protocol::H1 => self.h1.serve_connection(io, service).await?,
            Protocol::H2 => self.h2.serve_connection(io, service).await?,
        }

        Ok(())
    }
}

enum Protocol {
    H1,
    H2,
}

pin_project! {
    struct PrependAsyncRead<T1, T2> {
        #[pin]
        first: T1,
        #[pin]
        second: T2,
        state: State,
    }
}

impl<T1, T2> PrependAsyncRead<T1, T2> {
    fn new(first: T1, second: T2) -> Self {
        Self {
            first,
            second,
            state: State::First,
        }
    }
}

enum State {
    First,
    Second,
}

impl<T1: AsyncRead, T2: AsyncRead> AsyncRead for PrependAsyncRead<T1, T2> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let mut this = self.project();

        loop {
            match &this.state {
                State::First => {
                    let old_len = buf.filled().len();

                    match this.first.as_mut().poll_read(cx, buf) {
                        Poll::Ready(result) => match result {
                            Ok(()) => {
                                if buf.filled().len() == old_len {
                                    *this.state = State::Second;
                                } else {
                                    return Poll::Ready(Ok(()));
                                }
                            }
                            Err(e) => return Poll::Ready(Err(e)),
                        },
                        Poll::Pending => return Poll::Pending,
                    }
                }
                State::Second => return this.second.as_mut().poll_read(cx, buf),
            }
        }
    }
}

impl<T1, T2: AsyncWrite> AsyncWrite for PrependAsyncRead<T1, T2> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        self.project().second.poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        self.project().second.poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        self.project().second.poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::AutoConn;
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
                    let _ = AutoConn::new()
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
