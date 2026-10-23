#![cfg(all(
    feature = "client-legacy",
    feature = "server",
    feature = "http2",
    feature = "tokio"
))]

use std::convert::Infallible;
use std::future::{Ready, ready};
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures_util::task::AtomicWaker;
use http_body_util::{BodyExt, Empty, Full, StreamBody};
use hyper::body::Frame;
use hyper::service::service_fn;
use hyper::{Request, Response, Uri};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tokio::sync::{Notify, mpsc};
use tower_service::Service;

// Pause both directions without EOF, reset, or discarding buffered bytes.
#[derive(Default)]
struct Gate {
    paused: AtomicBool,
    read: AtomicWaker,
    write: AtomicWaker,
}

impl Gate {
    fn blocked(&self, cx: &Context<'_>, waker: &AtomicWaker) -> bool {
        waker.register(cx.waker());
        self.paused.load(Ordering::SeqCst)
    }

    fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
        self.read.wake();
        self.write.wake();
    }
}

struct GatedIo {
    io: DuplexStream,
    gate: Arc<Gate>,
}

impl AsyncRead for GatedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.gate.blocked(cx, &self.gate.read) {
            return Poll::Pending;
        }
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for GatedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.gate.blocked(cx, &self.gate.write) {
            return Poll::Pending;
        }
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.gate.blocked(cx, &self.gate.write) {
            return Poll::Pending;
        }
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

impl Connection for GatedIo {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

#[derive(Clone)]
struct Connector {
    count: Arc<AtomicUsize>,
    gates: Arc<Mutex<Vec<Arc<Gate>>>>,
    seen: mpsc::UnboundedSender<(usize, String)>,
    headers: Arc<Notify>,
    body: Arc<Notify>,
}

impl Service<Uri> for Connector {
    type Response = TokioIo<GatedIo>;
    type Error = io::Error;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: Uri) -> Self::Future {
        let id = self.count.fetch_add(1, Ordering::SeqCst);
        let gate = Arc::new(Gate::default());
        self.gates.lock().unwrap().push(gate.clone());
        let (client, server) = tokio::io::duplex(64 * 1024);
        let seen = self.seen.clone();
        let headers = self.headers.clone();
        let body = self.body.clone();
        tokio::spawn(async move {
            let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                let seen = seen.clone();
                let headers = headers.clone();
                let body = body.clone();
                async move {
                    let path = req.uri().path().to_owned();
                    let _ = seen.send((id, path.clone()));
                    if path == "/pending" {
                        headers.notified().await;
                    }
                    let payload = if path == "/stream" {
                        StreamBody::new(futures_util::stream::once(async move {
                            body.notified().await;
                            Ok::<_, Infallible>(Frame::data(Bytes::from_static(b"complete")))
                        }))
                        .boxed_unsync()
                    } else {
                        Full::new(Bytes::from_static(b"complete"))
                            .map_err(|never| match never {})
                            .boxed_unsync()
                    };
                    Ok::<_, Infallible>(
                        Response::builder()
                            .header("connection-id", id.to_string())
                            .body(payload)
                            .unwrap(),
                    )
                }
            });
            let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(GatedIo { io: server, gate }), service)
                .await;
        });
        ready(Ok(TokioIo::new(GatedIo {
            io: client,
            gate: Arc::new(Gate::default()),
        })))
    }
}

type TestClient = Client<Connector, Empty<Bytes>>;

fn setup(
    reuse: Option<Duration>,
    adaptive: bool,
) -> (
    TestClient,
    Connector,
    mpsc::UnboundedReceiver<(usize, String)>,
) {
    let (seen, rx) = mpsc::unbounded_channel();
    let connector = Connector {
        count: Arc::new(AtomicUsize::new(0)),
        gates: Arc::new(Mutex::new(Vec::new())),
        seen,
        headers: Arc::new(Notify::new()),
        body: Arc::new(Notify::new()),
    };
    let client = Client::builder(TokioExecutor::new())
        .http2_only(true)
        .timer(TokioTimer::new())
        .http2_adaptive_window(adaptive)
        .http2_keep_alive_interval(Some(Duration::from_secs(10)))
        .http2_keep_alive_reuse_timeout(reuse)
        .http2_keep_alive_timeout(Duration::from_secs(60))
        .http2_keep_alive_while_idle(true)
        .build(connector.clone());
    (client, connector, rx)
}

fn request(path: &str) -> Request<Empty<Bytes>> {
    Request::builder()
        .uri(format!("http://test{path}"))
        .body(Empty::new())
        .unwrap()
}

async fn settle() {
    // Keep this task runnable so paused time cannot auto-advance while flushing IO.
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
}

async fn advance(seconds: u64) {
    tokio::time::advance(Duration::from_secs(seconds)).await;
    settle().await;
}

async fn get(client: &TestClient) -> usize {
    let response = client.request(request("/ok")).await.unwrap();
    let id = response.headers()["connection-id"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(response.collect().await.unwrap().to_bytes(), "complete");
    id
}

async fn recovery(adaptive: bool) {
    let (client, connector, mut seen) = setup(Some(Duration::from_secs(5)), adaptive);
    assert_eq!(get(&client).await, 0);
    seen.recv().await.unwrap();
    let stream = client.request(request("/stream")).await.unwrap();
    assert_eq!(seen.recv().await.unwrap(), (0, "/stream".into()));
    let stream = tokio::spawn(async move { stream.collect().await });
    let pending = tokio::spawn(client.request(request("/pending")));
    assert_eq!(seen.recv().await.unwrap(), (0, "/pending".into()));
    settle().await;
    let old = connector.gates.lock().unwrap()[0].clone();
    old.pause();
    advance(10).await; // The original keep-alive PING enters its timeout phase.
    advance(5).await; // No IO event is needed to retire the connection.
    assert!(!pending.is_finished());
    assert!(!stream.is_finished());

    let ids = futures_util::future::join_all((0..12).map(|_| get(&client))).await;
    assert!(ids.iter().all(|id| *id == 1));
    assert_eq!(connector.count.load(Ordering::SeqCst), 2);
    assert!(!pending.is_finished());
    assert!(!stream.is_finished());

    connector.headers.notify_one();
    connector.body.notify_one();
    old.resume();
    settle().await;
    let response = pending.await.unwrap().unwrap();
    assert_eq!(response.headers()["connection-id"], "0");
    assert_eq!(response.collect().await.unwrap().to_bytes(), "complete");
    assert_eq!(stream.await.unwrap().unwrap().to_bytes(), "complete");
    assert_eq!(get(&client).await, 1); // Late ACK never restores the old pool entry.
    assert_eq!(connector.count.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn soft_timeout_replaces_connection_and_preserves_headers_and_body() {
    recovery(false).await;
}

#[tokio::test(start_paused = true)]
async fn adaptive_window_preserves_recovery_behavior() {
    recovery(true).await;
}

#[tokio::test(start_paused = true)]
async fn original_sixty_second_hard_deadline_survives_retirement() {
    let (client, connector, mut seen) = setup(Some(Duration::from_secs(5)), false);
    assert_eq!(get(&client).await, 0);
    seen.recv().await.unwrap();
    let pending = tokio::spawn(client.request(request("/pending")));
    seen.recv().await.unwrap();
    settle().await;
    connector.gates.lock().unwrap()[0].pause();
    advance(10).await;
    advance(5).await;
    assert_eq!(get(&client).await, 1);
    advance(54).await;
    assert!(
        !pending.is_finished(),
        "soft timeout must not shorten the hard deadline"
    );
    advance(1).await;
    assert!(
        pending.is_finished(),
        "hard timer must still wake at PING + 60s"
    );
    let error = pending.await.unwrap().unwrap_err();
    let mut cause: &(dyn std::error::Error + 'static) = &error;
    let mut timed_out = false;
    loop {
        if let Some(error) = cause.downcast_ref::<hyper::Error>() {
            timed_out |= error.is_timeout();
        }
        match cause.source() {
            Some(source) => cause = source,
            None => break,
        }
    }
    assert!(timed_out, "expected original keep-alive timeout: {error:?}");
}

#[tokio::test(start_paused = true)]
async fn ack_before_soft_deadline_keeps_connection_reusable() {
    let (client, connector, _) = setup(Some(Duration::from_secs(5)), false);
    assert_eq!(get(&client).await, 0);
    settle().await;
    let old = connector.gates.lock().unwrap()[0].clone();
    old.pause();
    advance(10).await;
    advance(4).await;
    old.resume();
    settle().await;
    advance(1).await;
    assert_eq!(get(&client).await, 0);
    assert_eq!(connector.count.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn disabled_soft_timeout_preserves_pool_behavior() {
    let (client, connector, _) = setup(None, false);
    assert_eq!(get(&client).await, 0);
    settle().await;
    let old = connector.gates.lock().unwrap()[0].clone();
    old.pause();
    advance(10).await;
    advance(5).await;
    let request = tokio::spawn(client.request(request("/ok")));
    settle().await;
    assert!(!request.is_finished());
    assert_eq!(connector.count.load(Ordering::SeqCst), 1);
    old.resume();
    let response = request.await.unwrap().unwrap();
    assert_eq!(response.headers()["connection-id"], "0");
}
