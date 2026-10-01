#![cfg(all(feature = "client-legacy", feature = "http1"))]

mod test_utils;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::Poll;
use std::thread;
use std::time::Duration;

use futures_channel::{mpsc, oneshot};
use futures_util::future::{self, FutureExt, TryFutureExt};
use futures_util::stream::StreamExt;
use futures_util::{self, Stream};
use http_body_util::BodyExt;
use http_body_util::{Empty, StreamBody};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use hyper::Request;
use hyper::body::Bytes;
use hyper::body::Frame;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::{HttpConnector, capture_connection};
use hyper_util::rt::TokioExecutor;

use test_utils::{DebugConnector, DebugStream, runtime};

fn s(buf: &[u8]) -> &str {
    std::str::from_utf8(buf).expect("from_utf8")
}

#[cfg(not(miri))]
#[test]
fn drop_body_before_eof_closes_connection() {
    // https://github.com/hyperium/hyper/issues/1353
    let _ = pretty_env_logger::try_init();

    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    let rt = runtime();
    let (closes_tx, closes) = mpsc::channel::<()>(10);
    let client = Client::builder(hyper_util::rt::TokioExecutor::new()).build(
        DebugConnector::with_http_and_closes(HttpConnector::new(), closes_tx),
    );
    let (tx1, rx1) = oneshot::channel();
    thread::spawn(move || {
        let mut sock = server.accept().unwrap().0;
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0; 4096];
        sock.read(&mut buf).expect("read 1");
        let body = vec![b'x'; 1024 * 128];
        write!(
            sock,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .expect("write head");
        let _ = sock.write_all(&body);
        let _ = tx1.send(());
    });

    let req = Request::builder()
        .uri(&*format!("http://{addr}/a"))
        .body(Empty::<Bytes>::new())
        .unwrap();
    let res = client.request(req).map_ok(move |res| {
        assert_eq!(res.status(), hyper::StatusCode::OK);
    });
    let rx = rx1;
    rt.block_on(async move {
        let (res, _) = future::join(res, rx).await;
        res.unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
    });
    rt.block_on(closes.into_future()).0.expect("closes");
}

#[cfg(not(miri))]
#[tokio::test]
async fn drop_client_closes_idle_connections() {
    let _ = pretty_env_logger::try_init();

    let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = server.local_addr().unwrap();
    let (closes_tx, mut closes) = mpsc::channel(10);

    let (tx1, rx1) = oneshot::channel();

    let t1 = tokio::spawn(async move {
        let mut sock = server.accept().await.unwrap().0;
        let mut buf = [0; 4096];
        sock.read(&mut buf).await.expect("read 1");
        let body = [b'x'; 64];
        let headers = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
        sock.write_all(headers.as_bytes())
            .await
            .expect("write head");
        sock.write_all(&body).await.expect("write body");
        let _ = tx1.send(());

        // prevent this thread from closing until end of test, so the connection
        // stays open and idle until Client is dropped
        if let Ok(n) = sock.read(&mut buf).await {
            assert_eq!(n, 0);
        }
    });

    let client = Client::builder(TokioExecutor::new()).build(DebugConnector::with_http_and_closes(
        HttpConnector::new(),
        closes_tx,
    ));

    let req = Request::builder()
        .uri(&*format!("http://{addr}/a"))
        .body(Empty::<Bytes>::new())
        .unwrap();
    let res = client.request(req).map_ok(move |res| {
        assert_eq!(res.status(), hyper::StatusCode::OK);
    });
    let rx = rx1;
    let (res, _) = future::join(res, rx).await;
    res.unwrap();

    // not closed yet, just idle
    std::future::poll_fn(|ctx| {
        assert!(Pin::new(&mut closes).poll_next(ctx).is_pending());
        Poll::Ready(())
    })
    .await;

    // drop to start the connections closing
    drop(client);

    // and wait a few ticks for the connections to close
    let t = pin!(tokio::time::sleep(Duration::from_millis(100)).map(|_| panic!("time out")));
    let close = closes.into_future().map(|(opt, _)| opt.expect("closes"));
    future::select(t, close).await;
    t1.await.unwrap();
}

#[cfg(not(miri))]
#[tokio::test]
async fn drop_response_future_closes_in_progress_connection() {
    let _ = pretty_env_logger::try_init();

    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    let (closes_tx, closes) = mpsc::channel(10);

    let (tx1, rx1) = oneshot::channel();
    let (_client_drop_tx, client_drop_rx) = std::sync::mpsc::channel::<()>();

    thread::spawn(move || {
        let mut sock = server.accept().unwrap().0;
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0; 4096];
        sock.read(&mut buf).expect("read 1");
        // we never write a response head
        // simulates a slow server operation
        let _ = tx1.send(());

        // prevent this thread from closing until end of test, so the connection
        // stays open and idle until Client is dropped
        let _ = client_drop_rx.recv();
    });

    let res = {
        let client = Client::builder(TokioExecutor::new()).build(
            DebugConnector::with_http_and_closes(HttpConnector::new(), closes_tx),
        );

        let req = Request::builder()
            .uri(&*format!("http://{addr}/a"))
            .body(Empty::<Bytes>::new())
            .unwrap();
        client.request(req).map(|_| unreachable!())
    };

    future::select(res, rx1).await;

    // res now dropped
    let t = pin!(tokio::time::sleep(Duration::from_millis(100)).map(|_| panic!("time out")));
    let close = closes.into_future().map(|(opt, _)| opt.expect("closes"));
    future::select(t, close).await;
}

#[cfg(not(miri))]
#[tokio::test]
async fn drop_response_body_closes_in_progress_connection() {
    let _ = pretty_env_logger::try_init();

    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    let (closes_tx, closes) = mpsc::channel(10);

    let (tx1, rx1) = oneshot::channel();
    let (_client_drop_tx, client_drop_rx) = std::sync::mpsc::channel::<()>();

    thread::spawn(move || {
        let mut sock = server.accept().unwrap().0;
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0; 4096];
        sock.read(&mut buf).expect("read 1");
        write!(
            sock,
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n"
        )
        .expect("write head");
        let _ = tx1.send(());

        // prevent this thread from closing until end of test, so the connection
        // stays open and idle until Client is dropped
        let _ = client_drop_rx.recv();
    });

    let rx = rx1;
    let res = {
        let client = Client::builder(TokioExecutor::new()).build(
            DebugConnector::with_http_and_closes(HttpConnector::new(), closes_tx),
        );

        let req = Request::builder()
            .uri(&*format!("http://{addr}/a"))
            .body(Empty::<Bytes>::new())
            .unwrap();
        // notably, haven't read body yet
        client.request(req)
    };

    let (res, _) = future::join(res, rx).await;
    // drop the body
    res.unwrap();

    // and wait a few ticks to see the connection drop
    let t = pin!(tokio::time::sleep(Duration::from_millis(100)).map(|_| panic!("time out")));
    let close = closes.into_future().map(|(opt, _)| opt.expect("closes"));
    future::select(t, close).await;
}

#[cfg(not(miri))]
#[tokio::test]
async fn no_keep_alive_closes_connection() {
    // https://github.com/hyperium/hyper/issues/1383
    let _ = pretty_env_logger::try_init();

    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    let (closes_tx, closes) = mpsc::channel(10);

    let (tx1, rx1) = oneshot::channel();
    let (_tx2, rx2) = std::sync::mpsc::channel::<()>();

    thread::spawn(move || {
        let mut sock = server.accept().unwrap().0;
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0; 4096];
        sock.read(&mut buf).expect("read 1");
        sock.write(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        let _ = tx1.send(());

        // prevent this thread from closing until end of test, so the connection
        // stays open and idle until Client is dropped
        let _ = rx2.recv();
    });

    let client = Client::builder(TokioExecutor::new())
        .pool_max_idle_per_host(0)
        .build(DebugConnector::with_http_and_closes(
            HttpConnector::new(),
            closes_tx,
        ));

    let req = Request::builder()
        .uri(&*format!("http://{addr}/a"))
        .body(Empty::<Bytes>::new())
        .unwrap();
    let res = client.request(req).map_ok(move |res| {
        assert_eq!(res.status(), hyper::StatusCode::OK);
    });
    let rx = rx1;
    let (res, _) = future::join(res, rx).await;
    res.unwrap();

    let t = pin!(tokio::time::sleep(Duration::from_millis(100)).map(|_| panic!("time out")));
    let close = closes.into_future().map(|(opt, _)| opt.expect("closes"));
    future::select(close, t).await;
}

#[cfg(not(miri))]
#[tokio::test]
async fn socket_disconnect_closes_idle_conn() {
    // notably when keep-alive is enabled
    let _ = pretty_env_logger::try_init();

    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    let (closes_tx, closes) = mpsc::channel(10);

    let (tx1, rx1) = oneshot::channel();

    thread::spawn(move || {
        let mut sock = server.accept().unwrap().0;
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0; 4096];
        sock.read(&mut buf).expect("read 1");
        sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        let _ = tx1.send(());
    });

    let client = Client::builder(TokioExecutor::new()).build(DebugConnector::with_http_and_closes(
        HttpConnector::new(),
        closes_tx,
    ));

    let req = Request::builder()
        .uri(&*format!("http://{addr}/a"))
        .body(Empty::<Bytes>::new())
        .unwrap();
    let res = client.request(req).map_ok(move |res| {
        assert_eq!(res.status(), hyper::StatusCode::OK);
    });
    let rx = rx1;

    let (res, _) = future::join(res, rx).await;
    res.unwrap();

    let t = pin!(tokio::time::sleep(Duration::from_millis(100)).map(|_| panic!("time out")));
    let close = closes.into_future().map(|(opt, _)| opt.expect("closes"));
    future::select(t, close).await;
}

#[test]
fn connect_call_is_lazy() {
    // We especially don't want connects() triggered if there's
    // idle connections that the Checkout would have found
    let _ = pretty_env_logger::try_init();

    let _rt = runtime();
    let connector = DebugConnector::new();
    let connects = connector.connects.clone();

    let client = Client::builder(TokioExecutor::new()).build(connector);

    assert_eq!(connects.load(Ordering::Relaxed), 0);
    let req = Request::builder()
        .uri("http://hyper.local/a")
        .body(Empty::<Bytes>::new())
        .unwrap();
    let _fut = client.request(req);
    // internal Connect::connect should have been lazy, and not
    // triggered an actual connect yet.
    assert_eq!(connects.load(Ordering::Relaxed), 0);
}

#[cfg(not(miri))]
#[test]
fn client_keep_alive_0() {
    let _ = pretty_env_logger::try_init();
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    let rt = runtime();
    let connector = DebugConnector::new();
    let connects = connector.connects.clone();

    let client = Client::builder(TokioExecutor::new()).build(connector);

    let (tx1, rx1) = oneshot::channel();
    let (tx2, rx2) = oneshot::channel();
    thread::spawn(move || {
        let mut sock = server.accept().unwrap().0;
        //drop(server);
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0; 4096];
        sock.read(&mut buf).expect("read 1");
        sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .expect("write 1");
        let _ = tx1.send(());

        let n2 = sock.read(&mut buf).expect("read 2");
        assert_ne!(n2, 0);
        let second_get = "GET /b HTTP/1.1\r\n";
        assert_eq!(s(&buf[..second_get.len()]), second_get);
        sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .expect("write 2");
        let _ = tx2.send(());
    });

    assert_eq!(connects.load(Ordering::SeqCst), 0);

    let rx = rx1;
    let req = Request::builder()
        .uri(&*format!("http://{addr}/a"))
        .body(Empty::<Bytes>::new())
        .unwrap();
    let res = client.request(req);
    rt.block_on(future::join(res, rx).map(|r| r.0)).unwrap();

    assert_eq!(connects.load(Ordering::SeqCst), 1);

    // sleep real quick to let the threadpool put connection in ready
    // state and back into client pool
    thread::sleep(Duration::from_millis(50));

    let rx = rx2;
    let req = Request::builder()
        .uri(&*format!("http://{addr}/b"))
        .body(Empty::<Bytes>::new())
        .unwrap();
    let res = client.request(req);
    rt.block_on(future::join(res, rx).map(|r| r.0)).unwrap();

    assert_eq!(
        connects.load(Ordering::SeqCst),
        1,
        "second request should still only have 1 connect"
    );
    drop(client);
}

#[cfg(not(miri))]
#[test]
fn client_keep_alive_extra_body() {
    let _ = pretty_env_logger::try_init();
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    let rt = runtime();

    let connector = DebugConnector::new();
    let connects = connector.connects.clone();

    let client = Client::builder(TokioExecutor::new()).build(connector);

    let (tx1, rx1) = oneshot::channel();
    let (tx2, rx2) = oneshot::channel();
    thread::spawn(move || {
        let mut sock = server.accept().unwrap().0;
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0; 4096];
        sock.read(&mut buf).expect("read 1");
        sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello")
            .expect("write 1");
        // the body "hello", while ignored because its a HEAD request, should mean the connection
        // cannot be put back in the pool
        let _ = tx1.send(());

        let mut sock2 = server.accept().unwrap().0;
        let n2 = sock2.read(&mut buf).expect("read 2");
        assert_ne!(n2, 0);
        let second_get = "GET /b HTTP/1.1\r\n";
        assert_eq!(s(&buf[..second_get.len()]), second_get);
        sock2
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .expect("write 2");
        let _ = tx2.send(());
    });

    assert_eq!(connects.load(Ordering::Relaxed), 0);

    let rx = rx1;
    let req = Request::builder()
        .method("HEAD")
        .uri(&*format!("http://{addr}/a"))
        .body(Empty::<Bytes>::new())
        .unwrap();
    let res = client.request(req);
    rt.block_on(future::join(res, rx).map(|r| r.0)).unwrap();

    assert_eq!(connects.load(Ordering::Relaxed), 1);

    let rx = rx2;
    let req = Request::builder()
        .uri(&*format!("http://{addr}/b"))
        .body(Empty::<Bytes>::new())
        .unwrap();
    let res = client.request(req);
    rt.block_on(future::join(res, rx).map(|r| r.0)).unwrap();

    assert_eq!(connects.load(Ordering::Relaxed), 2);
}

#[cfg(not(miri))]
#[tokio::test]
async fn client_keep_alive_when_response_before_request_body_ends() {
    let _ = pretty_env_logger::try_init();
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();

    let (closes_tx, mut closes) = mpsc::channel::<()>(10);
    let connector = DebugConnector::with_http_and_closes(HttpConnector::new(), closes_tx);
    let connects = connector.connects.clone();
    let client = Client::builder(TokioExecutor::new()).build(connector.clone());

    let (tx1, rx1) = oneshot::channel();
    let (tx2, rx2) = oneshot::channel();
    let (_tx3, rx3) = std::sync::mpsc::channel::<()>();

    thread::spawn(move || {
        let mut sock = server.accept().unwrap().0;
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0; 4096];
        sock.read(&mut buf).expect("read 1");
        sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .expect("write 1");
        // after writing the response, THEN stream the body
        let _ = tx1.send(());

        sock.read(&mut buf).expect("read 2");
        let _ = tx2.send(());

        // prevent this thread from closing until end of test, so the connection
        // stays open and idle until Client is dropped
        let _ = rx3.recv();
    });

    assert_eq!(connects.load(Ordering::Relaxed), 0);

    let delayed_body = rx1
        .then(|_| Box::pin(tokio::time::sleep(Duration::from_millis(200))))
        .map(|_| Ok::<_, ()>(Frame::data(&b"hello a"[..])))
        .map_err(|_| -> hyper::Error { panic!("rx1") })
        .into_stream();

    let req = Request::builder()
        .method("POST")
        .uri(&*format!("http://{addr}/a"))
        .body(StreamBody::new(delayed_body))
        .unwrap();
    let res = client.request(req).map_ok(move |res| {
        assert_eq!(res.status(), hyper::StatusCode::OK);
    });

    future::join(res, rx2).await.0.unwrap();
    std::future::poll_fn(|ctx| {
        assert!(Pin::new(&mut closes).poll_next(ctx).is_pending());
        Poll::Ready(())
    })
    .await;

    assert_eq!(connects.load(Ordering::Relaxed), 1);

    drop(client);
    let t = pin!(tokio::time::sleep(Duration::from_millis(100)).map(|_| panic!("time out")));
    let close = closes.into_future().map(|(opt, _)| opt.expect("closes"));
    future::select(t, close).await;
}

#[cfg(not(miri))]
#[cfg(feature = "http1")]
#[tokio::test]
async fn client_keep_alive_eager_when_chunked() {
    // If a response body has been read to completion, with completion
    // determined by some other factor, like decompression, and thus
    // it is in't polled a final time to clear the final 0-len chunk,
    // try to eagerly clear it so the connection can still be used.

    let _ = pretty_env_logger::try_init();
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    let connector = DebugConnector::new();
    let connects = connector.connects.clone();

    let client = Client::builder(TokioExecutor::new()).build(connector);

    let (tx1, rx1) = oneshot::channel();
    let (tx2, rx2) = oneshot::channel();
    thread::spawn(move || {
        let mut sock = server.accept().unwrap().0;
        //drop(server);
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0; 4096];
        sock.read(&mut buf).expect("read 1");
        sock.write_all(
            b"\
                HTTP/1.1 200 OK\r\n\
                transfer-encoding: chunked\r\n\
                \r\n\
                5\r\n\
                hello\r\n\
                0\r\n\r\n\
            ",
        )
        .expect("write 1");
        let _ = tx1.send(());

        let n2 = sock.read(&mut buf).expect("read 2");
        assert_ne!(n2, 0, "bytes of second request");
        let second_get = "GET /b HTTP/1.1\r\n";
        assert_eq!(s(&buf[..second_get.len()]), second_get);
        sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .expect("write 2");
        let _ = tx2.send(());
    });

    assert_eq!(connects.load(Ordering::SeqCst), 0);

    let rx = rx1;
    let req = Request::builder()
        .uri(&*format!("http://{addr}/a"))
        .body(Empty::<Bytes>::new())
        .unwrap();
    let fut = client.request(req);

    let resp = future::join(fut, rx).map(|r| r.0).await.unwrap();
    assert_eq!(connects.load(Ordering::SeqCst), 1);
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["transfer-encoding"], "chunked");

    // Read the "hello" chunk...
    let chunk = resp.collect().await.unwrap().to_bytes();
    assert_eq!(chunk, "hello");

    // sleep real quick to let the threadpool put connection in ready
    // state and back into client pool
    tokio::time::sleep(Duration::from_millis(50)).await;

    let rx = rx2;
    let req = Request::builder()
        .uri(&*format!("http://{addr}/b"))
        .body(Empty::<Bytes>::new())
        .unwrap();
    let fut = client.request(req);
    future::join(fut, rx).map(|r| r.0).await.unwrap();

    assert_eq!(
        connects.load(Ordering::SeqCst),
        1,
        "second request should still only have 1 connect"
    );
    drop(client);
}

#[cfg(not(miri))]
#[cfg(feature = "http1")]
#[test]
fn connect_proxy_sends_absolute_uri() {
    let _ = pretty_env_logger::try_init();
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    let rt = runtime();
    let connector = DebugConnector::new().proxy();

    let client = Client::builder(TokioExecutor::new()).build(connector);

    let (tx1, rx1) = oneshot::channel();
    thread::spawn(move || {
        let mut sock = server.accept().unwrap().0;
        //drop(server);
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0; 4096];
        let n = sock.read(&mut buf).expect("read 1");
        let expected = format!("GET http://{addr}/foo/bar HTTP/1.1\r\nhost: {addr}\r\n\r\n");
        assert_eq!(s(&buf[..n]), expected);

        sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .expect("write 1");
        let _ = tx1.send(());
    });

    let rx = rx1;
    let req = Request::builder()
        .uri(&*format!("http://{addr}/foo/bar"))
        .body(Empty::<Bytes>::new())
        .unwrap();
    let res = client.request(req);
    rt.block_on(future::join(res, rx).map(|r| r.0)).unwrap();
}

#[cfg(not(miri))]
#[cfg(feature = "http1")]
#[test]
fn connect_proxy_http_connect_sends_authority_form() {
    let _ = pretty_env_logger::try_init();
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    let rt = runtime();
    let connector = DebugConnector::new().proxy();

    let client = Client::builder(TokioExecutor::new()).build(connector);

    let (tx1, rx1) = oneshot::channel();
    thread::spawn(move || {
        let mut sock = server.accept().unwrap().0;
        //drop(server);
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0; 4096];
        let n = sock.read(&mut buf).expect("read 1");
        let expected = format!("CONNECT {addr} HTTP/1.1\r\nhost: {addr}\r\n\r\n");
        assert_eq!(s(&buf[..n]), expected);

        sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .expect("write 1");
        let _ = tx1.send(());
    });

    let rx = rx1;
    let req = Request::builder()
        .method("CONNECT")
        .uri(&*format!("http://{addr}/useless/path"))
        .body(Empty::<Bytes>::new())
        .unwrap();
    let res = client.request(req);
    rt.block_on(future::join(res, rx).map(|r| r.0)).unwrap();
}

#[cfg(not(miri))]
#[cfg(feature = "http1")]
#[test]
fn client_upgrade() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let _ = pretty_env_logger::try_init();
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    let rt = runtime();

    let connector = DebugConnector::new();

    let client = Client::builder(TokioExecutor::new()).build(connector);

    let (tx1, rx1) = oneshot::channel();
    thread::spawn(move || {
        let mut sock = server.accept().unwrap().0;
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0; 4096];
        sock.read(&mut buf).expect("read 1");
        sock.write_all(
            b"\
                HTTP/1.1 101 Switching Protocols\r\n\
                Upgrade: foobar\r\n\
                \r\n\
                foobar=ready\
            ",
        )
        .unwrap();
        let _ = tx1.send(());

        let n = sock.read(&mut buf).expect("read 2");
        assert_eq!(&buf[..n], b"foo=bar");
        sock.write_all(b"bar=foo").expect("write 2");
    });

    let rx = rx1;

    let req = Request::builder()
        .method("GET")
        .uri(&*format!("http://{addr}/up"))
        .body(Empty::<Bytes>::new())
        .unwrap();

    let res = client.request(req);
    let res = rt.block_on(future::join(res, rx).map(|r| r.0)).unwrap();

    assert_eq!(res.status(), 101);
    let upgraded = rt.block_on(hyper::upgrade::on(res)).expect("on_upgrade");

    let parts = upgraded.downcast::<DebugStream>().unwrap();
    assert_eq!(s(&parts.read_buf), "foobar=ready");

    let mut io = parts.io;
    rt.block_on(io.write_all(b"foo=bar")).unwrap();
    let mut vec = vec![];
    rt.block_on(io.read_to_end(&mut vec)).unwrap();
    assert_eq!(vec, b"bar=foo");
}

#[cfg(not(miri))]
#[cfg(feature = "http1")]
#[test]
fn capture_connection_on_client() {
    let _ = pretty_env_logger::try_init();

    let rt = runtime();
    let connector = DebugConnector::new();

    let client = Client::builder(TokioExecutor::new()).build(connector);

    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    thread::spawn(move || {
        let mut sock = server.accept().unwrap().0;
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0; 4096];
        sock.read(&mut buf).expect("read 1");
        sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .expect("write 1");
    });
    let mut req = Request::builder()
        .uri(&*format!("http://{addr}/a"))
        .body(Empty::<Bytes>::new())
        .unwrap();
    let captured_conn = capture_connection(&mut req);
    rt.block_on(client.request(req)).expect("200 OK");
    assert!(captured_conn.connection_metadata().is_some());
}

#[cfg(not(miri))]
#[cfg(feature = "http1")]
#[test]
fn connection_poisoning() {
    use std::sync::atomic::AtomicUsize;

    let _ = pretty_env_logger::try_init();

    let rt = runtime();
    let connector = DebugConnector::new();

    let client = Client::builder(TokioExecutor::new()).build(connector);

    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    let num_conns: Arc<AtomicUsize> = Default::default();
    let num_requests: Arc<AtomicUsize> = Default::default();
    let num_requests_tracker = num_requests.clone();
    let num_conns_tracker = num_conns.clone();
    thread::spawn(move || {
        loop {
            let mut sock = server.accept().unwrap().0;
            num_conns_tracker.fetch_add(1, Ordering::Relaxed);
            let num_requests_tracker = num_requests_tracker.clone();
            thread::spawn(move || {
                sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                sock.set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut buf = [0; 4096];
                loop {
                    if sock.read(&mut buf).expect("read 1") > 0 {
                        num_requests_tracker.fetch_add(1, Ordering::Relaxed);
                        sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                            .expect("write 1");
                    }
                }
            });
        }
    });
    let make_request = || {
        Request::builder()
            .uri(&*format!("http://{addr}/a"))
            .body(Empty::<Bytes>::new())
            .unwrap()
    };
    let mut req = make_request();
    let captured_conn = capture_connection(&mut req);
    rt.block_on(client.request(req)).expect("200 OK");
    assert_eq!(num_conns.load(Ordering::SeqCst), 1);
    assert_eq!(num_requests.load(Ordering::SeqCst), 1);

    rt.block_on(client.request(make_request())).expect("200 OK");
    rt.block_on(client.request(make_request())).expect("200 OK");
    // Before poisoning the connection is reused
    assert_eq!(num_conns.load(Ordering::SeqCst), 1);
    assert_eq!(num_requests.load(Ordering::SeqCst), 3);
    captured_conn
        .connection_metadata()
        .as_ref()
        .unwrap()
        .poison();

    rt.block_on(client.request(make_request())).expect("200 OK");

    // After poisoning, a new connection is established
    assert_eq!(num_conns.load(Ordering::SeqCst), 2);
    assert_eq!(num_requests.load(Ordering::SeqCst), 4);

    rt.block_on(client.request(make_request())).expect("200 OK");
    // another request can still reuse:
    assert_eq!(num_conns.load(Ordering::SeqCst), 2);
    assert_eq!(num_requests.load(Ordering::SeqCst), 5);
}
