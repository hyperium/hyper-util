//! Integration test verifying that `client-legacy` can be compiled and used with
//! a custom connector (e.g. for custom in-memory streams, TLS, or non-TCP platforms
//! like WASM and Fuchsia) without enabling the `tcp` feature or pulling in `mio`.

#![cfg(all(
    feature = "client-legacy",
    feature = "http1",
    feature = "tokio",
    not(feature = "tcp")
))]

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use http::{Request, StatusCode, Uri};
use http_body_util::{BodyExt, Empty};
use hyper::rt::{Read, ReadBufCursor, Write};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tower_service::Service;

struct MockStream(TokioIo<tokio_test::io::Mock>);

impl Read for MockStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl Write for MockStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

impl Connection for MockStream {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

#[derive(Clone)]
struct CustomConnector {
    mock: Arc<Mutex<Option<tokio_test::io::Mock>>>,
}

impl Service<Uri> for CustomConnector {
    type Response = MockStream;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _dst: Uri) -> Self::Future {
        let mock = self
            .mock
            .lock()
            .unwrap()
            .take()
            .expect("mock connection only called once");
        Box::pin(async move { Ok(MockStream(TokioIo::new(mock))) })
    }
}

#[tokio::test]
async fn test_client_legacy_with_custom_connector_without_tcp() {
    let mock_io = tokio_test::io::Builder::new()
        .write(b"GET /test HTTP/1.1\r\nhost: example.com\r\n\r\n")
        .read(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello")
        .build();

    let connector = CustomConnector {
        mock: Arc::new(Mutex::new(Some(mock_io))),
    };

    let client = Client::builder(TokioExecutor::new())
        .pool_max_idle_per_host(0)
        .build(connector);

    let req = Request::builder()
        .uri("http://example.com/test")
        .body(Empty::<Bytes>::new())
        .unwrap();

    let mut resp = client.request(req).await.expect("request succeeds");
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.body_mut().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"hello");
}
