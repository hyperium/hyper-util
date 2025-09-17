//! This example runs a server that responds to any request with "Hello, world!".
//! Unlike it's server.rs counter part, it demonstrates using a !Send executor (i.e. gloomio,
//! monoio).

use std::{convert::Infallible, error::Error};

use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::thread;

use bytes::Bytes;
use http::{header::CONTENT_TYPE, Request, Response};
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::{body::Incoming, service::service_fn};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
};
use tokio::{net::TcpListener, net::TcpStream, task::JoinSet};

/// Function from an incoming request to an outgoing response
///
/// This function gets turned into a [`hyper::service::Service`] later via
/// [`service_fn`]. Instead of doing this, you could also write a type that
/// implements [`hyper::service::Service`] directly and pass that in place of
/// writing a function like this and calling [`service_fn`].
async fn handle_request(_request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let response = Response::builder()
        .header(CONTENT_TYPE, "text/plain")
        .body(Full::new(Bytes::from("Hello, world!\n")))
        .expect("values provided to the builder should be valid");

    Ok(response)
}

async fn upgradable_server() -> Result<(), Box<dyn Error + 'static>> {
    let listen_addr = "127.0.0.1:8000";
    let tcp_listener = TcpListener::bind(listen_addr).await?;
    println!("listening on http://{listen_addr}");

    loop {
        let (stream, addr) = match tcp_listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                eprintln!("failed to accept connection: {e}");
                continue;
            }
        };

        let serve_connection = async move {
            println!("handling a request from {addr}");

            let result = Builder::new(LocalExec)
                .serve_connection(TokioIo::new(stream), service_fn(handle_request))
                .await;

            if let Err(e) = result {
                eprintln!("error serving {addr}: {e}");
            }

            println!("handled a request from {addr}");
        };

        tokio::task::spawn_local(serve_connection);
    }
}

fn main() {
    let server = thread::spawn(move || {
        // Configure a runtime for the server that runs everything on the current thread
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build runtime");

        // Combine it with a `LocalSet,  which means it can spawn !Send futures...
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, upgradable_server()).unwrap();
    });

    server.join().unwrap()
}

// NOTE: This part is only needed for HTTP/2. HTTP/1 doesn't need an executor.
//
// Since the Server needs to spawn some background tasks, we needed
// to configure an Executor that can spawn !Send futures...
#[derive(Clone, Copy, Debug)]
struct LocalExec;

impl<F> hyper::rt::Executor<F> for LocalExec
where
    F: std::future::Future + 'static, // not requiring `Send`
{
    fn execute(&self, fut: F) {
        // This will spawn into the currently running `LocalSet`.
        tokio::task::spawn_local(fut);
    }
}

struct IOTypeNotSend {
    _marker: PhantomData<*const ()>,
    stream: TokioIo<TcpStream>,
}

impl IOTypeNotSend {
    fn new(stream: TokioIo<TcpStream>) -> Self {
        Self {
            _marker: PhantomData,
            stream,
        }
    }
}

impl hyper::rt::Write for IOTypeNotSend {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

impl hyper::rt::Read for IOTypeNotSend {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}
