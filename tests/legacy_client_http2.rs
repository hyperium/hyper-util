#![cfg(all(feature = "client-legacy", feature = "http2"))]

mod test_utils;

use std::sync::atomic::Ordering;

use bytes::Bytes;
use futures_util::future;
use http::{Request, Response};
use http_body_util::{Empty, Full};
use tokio::net::TcpListener;

use hyper::service::service_fn;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;

use test_utils::{DebugConnector, runtime};

#[cfg(not(miri))]
#[cfg(feature = "server")]
#[test]
fn client_http2_upgrade() {
    use http::{Method, Version};
    use hyper_util::rt::TokioIo;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let _ = pretty_env_logger::try_init();
    let rt = runtime();
    let server = rt
        .block_on(TcpListener::bind(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            0,
        ))))
        .unwrap();
    let addr = server.local_addr().unwrap();
    let mut connector = DebugConnector::new();
    connector.alpn_h2 = true;

    let client = Client::builder(TokioExecutor::new()).build(connector);

    rt.spawn(async move {
        let (stream, _) = server.accept().await.expect("accept");
        let stream = TokioIo::new(stream);
        let mut builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
        // IMPORTANT: This is required to advertise our support for HTTP/2 websockets to the client.
        builder.http2().enable_connect_protocol();
        builder
            .serve_connection_with_upgrades(
                stream,
                service_fn(|req| async move {
                    assert_eq!(req.headers().get("host"), None);
                    assert_eq!(req.version(), Version::HTTP_2);
                    assert_eq!(
                        req.headers().get(http::header::SEC_WEBSOCKET_VERSION),
                        Some(&http::header::HeaderValue::from_static("13"))
                    );
                    assert_eq!(
                        req.extensions().get::<hyper::ext::Protocol>(),
                        Some(&hyper::ext::Protocol::from_static("websocket"))
                    );

                    let on_upgrade = hyper::upgrade::on(req);
                    tokio::spawn(async move {
                        let upgraded = on_upgrade.await.unwrap();
                        let mut io = TokioIo::new(upgraded);

                        let mut vec = vec![];
                        io.read_buf(&mut vec).await.unwrap();
                        assert_eq!(vec, b"foo=bar");
                        io.write_all(b"bar=foo").await.unwrap();
                    });

                    Ok::<_, hyper::Error>(Response::new(Empty::<Bytes>::new()))
                }),
            )
            .await
            .expect("server");
    });

    let req = Request::builder()
        .method(Method::CONNECT)
        .uri(&*format!("http://{addr}/up"))
        .header(http::header::SEC_WEBSOCKET_VERSION, "13")
        .version(Version::HTTP_2)
        .extension(hyper::ext::Protocol::from_static("websocket"))
        .body(Empty::<Bytes>::new())
        .unwrap();

    let res = client.request(req);
    let res = rt.block_on(res).unwrap();

    assert_eq!(res.status(), http::StatusCode::OK);
    assert_eq!(res.version(), Version::HTTP_2);

    let upgraded = rt.block_on(hyper::upgrade::on(res)).expect("on_upgrade");
    let mut io = hyper_util::rt::TokioIo::new(upgraded);

    rt.block_on(io.write_all(b"foo=bar")).unwrap();
    let mut vec = vec![];
    rt.block_on(io.read_to_end(&mut vec)).unwrap();
    assert_eq!(vec, b"bar=foo");
}

#[cfg(not(miri))]
#[test]
fn alpn_h2() {
    let _ = pretty_env_logger::try_init();
    let rt = runtime();
    let listener = rt
        .block_on(TcpListener::bind(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            0,
        ))))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let mut connector = DebugConnector::new();
    connector.alpn_h2 = true;
    let connects = connector.connects.clone();

    let client = Client::builder(TokioExecutor::new()).build(connector);

    rt.spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let stream = hyper_util::rt::TokioIo::new(stream);
        hyper::server::conn::http2::Builder::new(TokioExecutor::new())
            .serve_connection(
                stream,
                service_fn(|req| async move {
                    assert_eq!(req.headers().get("host"), None);
                    Ok::<_, hyper::Error>(Response::new(Full::<Bytes>::from("Hello, world")))
                }),
            )
            .await
            .expect("server");
    });

    assert_eq!(connects.load(Ordering::SeqCst), 0);

    let url = format!("http://{addr}/a").parse::<::hyper::Uri>().unwrap();
    let res1 = client.get(url.clone());
    let res2 = client.get(url.clone());
    let res3 = client.get(url.clone());
    rt.block_on(future::try_join3(res1, res2, res3)).unwrap();

    // Since the client doesn't know it can ALPN at first, it will have
    // started 3 connections. But, the server above will only handle 1,
    // so the unwrapped responses futures show it still worked.
    assert_eq!(connects.load(Ordering::SeqCst), 3);

    let res4 = client.get(url.clone());
    rt.block_on(res4).unwrap();

    // HTTP/2 request allowed
    let res5 = client.request(
        Request::builder()
            .uri(url)
            .version(hyper::Version::HTTP_2)
            .body(Empty::<Bytes>::new())
            .unwrap(),
    );
    rt.block_on(res5).unwrap();

    assert_eq!(
        connects.load(Ordering::SeqCst),
        3,
        "after ALPN, no more connects"
    );
    drop(client);
}
