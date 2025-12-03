//! todo

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

use http::{Request, Response};
use tower_service::Service;

use crate::common::future::poll_fn;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// todo
#[cfg(feature = "http1")]
pub struct Http1Layer<B> {
    builder: hyper::client::conn::http1::Builder,
    _body: PhantomData<fn(B)>,
}

/// todo
#[cfg(feature = "http1")]
pub fn http1<B>() -> Http1Layer<B> {
    Http1Layer {
        builder: hyper::client::conn::http1::Builder::new(),
        _body: PhantomData,
    }
}

#[cfg(feature = "http1")]
impl<M, B> tower_layer::Layer<M> for Http1Layer<B> {
    type Service = Http1Connect<M, B>;
    fn layer(&self, inner: M) -> Self::Service {
        Http1Connect {
            inner,
            builder: self.builder.clone(),
            _body: self._body,
        }
    }
}

#[cfg(feature = "http1")]
impl<B> Clone for Http1Layer<B> {
    fn clone(&self) -> Self {
        Self {
            builder: self.builder.clone(),
            _body: self._body.clone(),
        }
    }
}

#[cfg(feature = "http1")]
impl<B> From<hyper::client::conn::http1::Builder> for Http1Layer<B> {
    fn from(builder: hyper::client::conn::http1::Builder) -> Self {
        Self {
            builder,
            _body: PhantomData,
        }
    }
}

/// todo
#[cfg(feature = "http1")]
pub struct Http1Connect<M, B> {
    inner: M,
    builder: hyper::client::conn::http1::Builder,
    _body: PhantomData<fn(B)>,
}

#[cfg(feature = "http1")]
impl<M, Dst, B> Service<Dst> for Http1Connect<M, B>
where
    M: Service<Dst>,
    M::Future: Send + 'static,
    M::Response: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
    M::Error: Into<BoxError>,
    B: hyper::body::Body + Send + 'static,
    B::Data: Send + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Response = Http1ClientService<B>;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, dst: Dst) -> Self::Future {
        let fut = self.inner.call(dst);
        let builder = self.builder.clone();
        Box::pin(async move {
            let io = fut.await.map_err(Into::into)?;
            let (mut tx, conn) = builder.handshake(io).await?;
            //todo: pass in Executor
            tokio::spawn(async move {
                if let Err(e) = conn.await {
                    eprintln!("connection error: {:?}", e);
                }
            });
            // todo: wait for ready? or factor out to other middleware?
            poll_fn(|cx| tx.poll_ready(cx)).await?;

            Ok(Http1ClientService::new(tx))
        })
    }
}

#[cfg(feature = "http1")]
impl<M: Clone, B> Clone for Http1Connect<M, B> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            builder: self.builder.clone(),
            _body: self._body.clone(),
        }
    }
}

/// todo
#[cfg(feature = "http2")]
pub struct Http2Layer<B> {
    _body: PhantomData<fn(B)>,
}

/// todo
#[cfg(feature = "http2")]
pub fn http2<B>() -> Http2Layer<B> {
    Http2Layer { _body: PhantomData }
}

#[cfg(feature = "http2")]
impl<M, B> tower_layer::Layer<M> for Http2Layer<B> {
    type Service = Http2Connect<M, B>;
    fn layer(&self, inner: M) -> Self::Service {
        Http2Connect {
            inner,
            builder: hyper::client::conn::http2::Builder::new(crate::rt::TokioExecutor::new()),
            _body: self._body,
        }
    }
}

#[cfg(feature = "http2")]
impl<B> Clone for Http2Layer<B> {
    fn clone(&self) -> Self {
        Self {
            _body: self._body.clone(),
        }
    }
}

/// todo
#[cfg(feature = "http2")]
#[derive(Debug)]
pub struct Http2Connect<M, B> {
    inner: M,
    builder: hyper::client::conn::http2::Builder<crate::rt::TokioExecutor>,
    _body: PhantomData<fn(B)>,
}

#[cfg(feature = "http2")]
impl<M, Dst, B> Service<Dst> for Http2Connect<M, B>
where
    M: Service<Dst>,
    M::Future: Send + 'static,
    M::Response: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
    M::Error: Into<BoxError>,
    B: hyper::body::Body + Unpin + Send + 'static,
    B::Data: Send + 'static,
    B::Error: Into<BoxError>,
{
    type Response = Http2ClientService<B>;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, dst: Dst) -> Self::Future {
        let fut = self.inner.call(dst);
        let builder = self.builder.clone();
        Box::pin(async move {
            let io = fut.await.map_err(Into::into)?;
            let (mut tx, conn) = builder.handshake(io).await?;
            tokio::spawn(async move {
                if let Err(e) = conn.await {
                    eprintln!("connection error: {:?}", e);
                }
            });

            // todo: wait for ready? or factor out to other middleware?
            poll_fn(|cx| tx.poll_ready(cx)).await?;
            Ok(Http2ClientService::new(tx))
        })
    }
}

#[cfg(feature = "http2")]
impl<M: Clone, B> Clone for Http2Connect<M, B> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            builder: self.builder.clone(),
            _body: self._body.clone(),
        }
    }
}

/// A thin adapter over hyper HTTP/1 client SendRequest.
#[cfg(feature = "http1")]
#[derive(Debug)]
pub struct Http1ClientService<B> {
    tx: hyper::client::conn::http1::SendRequest<B>,
}

#[cfg(feature = "http1")]
impl<B> Http1ClientService<B> {
    /// todo
    pub fn new(tx: hyper::client::conn::http1::SendRequest<B>) -> Self {
        Self { tx }
    }

    /// Checks if the connection side has been closed.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

#[cfg(feature = "http1")]
impl<B> Service<Request<B>> for Http1ClientService<B>
where
    B: hyper::body::Body + Send + 'static,
{
    type Response = Response<hyper::body::Incoming>;
    type Error = hyper::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.tx.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let fut = self.tx.send_request(req);
        Box::pin(fut)
    }
}

/// todo
#[cfg(feature = "http2")]
#[derive(Debug)]
pub struct Http2ClientService<B> {
    tx: hyper::client::conn::http2::SendRequest<B>,
}

#[cfg(feature = "http2")]
impl<B> Http2ClientService<B> {
    /// todo
    pub fn new(tx: hyper::client::conn::http2::SendRequest<B>) -> Self {
        Self { tx }
    }

    /// Checks if the connection side has been closed.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

#[cfg(feature = "http2")]
impl<B> Service<Request<B>> for Http2ClientService<B>
where
    B: hyper::body::Body + Send + 'static,
{
    type Response = Response<hyper::body::Incoming>;
    type Error = hyper::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.tx.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let fut = self.tx.send_request(req);
        Box::pin(fut)
    }
}

#[cfg(feature = "http2")]
impl<B> Clone for Http2ClientService<B> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}
