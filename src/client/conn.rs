//! Tower layers and services for HTTP/1 and HTTP/2 client connections.
//!
//! This module provides Tower-compatible layers that wrap Hyper's low-level
//! HTTP client connection types, making them easier to compose with other
//! middleware and connection pooling strategies.

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

use http::{Request, Response};
use tower_service::Service;

use crate::common::future::poll_fn;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A Tower [`Layer`](tower_layer::Layer) for creating HTTP/1 client connections.
///
/// This layer wraps a connection service (typically a TCP or TLS connector) and
/// performs the HTTP/1 handshake, producing an [`Http1ClientService`] that can
/// send requests.
///
/// Use [`http1()`] to create a layer with default settings, or construct from
/// a [`hyper::client::conn::http1::Builder`] for custom configuration.
///
/// # Example
///
/// ```ignore
/// use hyper_util::client::conn::http1;
/// use hyper::{client::connect::HttpConnector, body::Bytes};
/// use tower:: ServiceBuilder;
/// use http_body_util::Empty;
///
/// let connector = HttpConnector::new();
/// let layer: Http1Layer<Empty<Bytes>> = http1();
/// let client = ServiceBuilder::new()
///     .layer(layer)
///     .service(connector);
/// ```
#[cfg(feature = "http1")]
pub struct Http1Layer<B> {
    builder: hyper::client::conn::http1::Builder,
    _body: PhantomData<fn(B)>,
}

/// Creates an [`Http1Layer`] with default HTTP/1 settings.
///
/// For custom settings, construct an [`Http1Layer`] from a
/// [`hyper::client::conn::http1::Builder`] using `.into()`.
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
            _body: self._body,
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

/// A Tower [`Service`] that establishes HTTP/1 connections.
///
/// This service wraps an underlying connection service (e.g., TCP or TLS) and
/// performs the HTTP/1 handshake when called. The resulting service can be used
/// to send HTTP requests over the established connection.
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

/// A Tower [`Layer`](tower_layer::Layer) for creating HTTP/2 client connections.
///
/// This layer wraps a connection service (typically a TCP or TLS connector) and
/// performs the HTTP/2 handshake, producing an [`Http2ClientService`] that can
/// send requests.
///
/// Use [`http2()`] to create a layer with a specific executor, or construct from
/// a [`hyper::client::conn::http2::Builder`] for custom configuration.
///
/// # Example
///
/// ```ignore
/// use hyper_util::client::conn::http2;
/// use hyper::{client::connect::HttpConnector, body::Bytes};
/// use tower:: ServiceBuilder;
/// use http_body_util::Empty;
///
/// let connector = HttpConnector::new();
/// let layer: Http2Layer<Empty<Bytes>> = http2();
/// let client = ServiceBuilder::new()
///     .layer(layer)
///     .service(connector);
/// ```
#[cfg(feature = "http2")]
pub struct Http2Layer<B, E> {
    builder: hyper::client::conn::http2::Builder<E>,
    _body: PhantomData<fn(B)>,
}

/// Creates an [`Http2Layer`] with default HTTP/1 settings.
///
/// For custom settings, construct an [`Http2Layer`] from a
/// [`hyper::client::conn::http2::Builder`] using `.into()`.
#[cfg(feature = "http2")]
pub fn http2<B, E>(executor: E) -> Http2Layer<B, E>
where
    E: Clone,
{
    Http2Layer {
        builder: hyper::client::conn::http2::Builder::new(executor),
        _body: PhantomData,
    }
}

#[cfg(feature = "http2")]
impl<M, B, E> tower_layer::Layer<M> for Http2Layer<B, E>
where
    E: Clone,
{
    type Service = Http2Connect<M, B, E>;
    fn layer(&self, inner: M) -> Self::Service {
        Http2Connect {
            inner,
            builder: hyper::client::conn::http2::Builder::new(crate::rt::TokioExecutor::new()),
            _body: self._body,
        }
    }
}

#[cfg(feature = "http2")]
impl<B, E: Clone> Clone for Http2Layer<B, E> {
    fn clone(&self) -> Self {
        Self {
            builder: self.builder.clone(),
            _body: self._body.clone(),
        }
    }
}

#[cfg(feature = "http2")]
impl<B, E> From<hyper::client::conn::http2::Builder<E>> for Http2Layer<B, E> {
    fn from(builder: hyper::client::conn::http2::Builder<E>) -> Self {
        Self {
            builder,
            _body: PhantomData,
        }
    }
}

/// A Tower [`Service`] that establishes HTTP/2 connections.
///
/// This service wraps an underlying connection service (e.g., TCP or TLS) and
/// performs the HTTP/2 handshake when called.  The resulting service can be used
/// to send HTTP requests over the established connection.
#[cfg(feature = "http2")]
#[derive(Debug)]
pub struct Http2Connect<M, B, E> {
    inner: M,
    builder: hyper::client::conn::http2::Builder<E>,
    _body: PhantomData<fn(B)>,
}

#[cfg(feature = "http2")]
impl<M, Dst, B, E> Service<Dst> for Http2Connect<M, B, E>
where
    M: Service<Dst>,
    M::Future: Send + 'static,
    M::Response: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
    M::Error: Into<BoxError>,
    B: hyper::body::Body + Unpin + Send + 'static,
    B::Data: Send + 'static,
    B::Error: Into<BoxError>,
    E: hyper::rt::bounds::Http2ClientConnExec<B, M::Response> + Unpin + Clone + Send + 'static,
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
impl<M: Clone, B, E: Clone> Clone for Http2Connect<M, B, E> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            builder: self.builder.clone(),
            _body: self._body.clone(),
        }
    }
}

/// A Tower [`Service`] that sends HTTP/1 requests over an established connection.
///
/// This is a thin wrapper around [`hyper::client::conn::http1::SendRequest`] that implements
/// the Tower `Service` trait, making it composable with other Tower middleware.
///
/// The service maintains a single HTTP/1 connection and can be used to send multiple
/// sequential requests.  For concurrent requests or connection pooling, wrap this service
/// with appropriate middleware.
#[cfg(feature = "http1")]
#[derive(Debug)]
pub struct Http1ClientService<B> {
    tx: hyper::client::conn::http1::SendRequest<B>,
}

#[cfg(feature = "http1")]
impl<B> Http1ClientService<B> {
    /// Constructs a new HTTP/1 client service from a Hyper `SendRequest`.
    ///
    /// Typically you won't call this directly; instead, use [`Http1Connect`] to
    /// establish connections and produce this service.
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

/// A Tower [`Service`] that sends HTTP/2 requests over an established connection.
///
/// This is a thin wrapper around [`hyper::client::conn::http2::SendRequest`] that implements
/// the Tower `Service` trait, making it composable with other Tower middleware.
///
/// The service maintains a single HTTP/2 connection and supports multiplexing multiple
/// concurrent requests over that connection. The service can be cloned to send requests
/// concurrently, or used with the [`Singleton`](crate::client::pool::singleton::Singleton) pool service.
#[cfg(feature = "http2")]
#[derive(Debug)]
pub struct Http2ClientService<B> {
    tx: hyper::client::conn::http2::SendRequest<B>,
}

#[cfg(feature = "http2")]
impl<B> Http2ClientService<B> {
    /// Constructs a new HTTP/2 client service from a Hyper `SendRequest`.
    ///
    /// Typically you won't call this directly; instead, use [`Http2Connect`] to
    /// establish connections and produce this service.
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
