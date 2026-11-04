//! This example runs a server that responds to any request with "Hello, world!".
//! Unlike it's server.rs counter part, it demonstrates using a !Send executor (i.e. gloomio,
//! monoio).

use bytes::Bytes;
use http::{Request, Response, header::CONTENT_TYPE};
use http_body_util::Full;
use hyper::{body::Incoming, service::service_fn};
use hyper_util::{
    rt::{TokioIo, tokio::TokioLocalExecutor},
    server::conn::auto::Builder,
};
use std::{convert::Infallible, error::Error};
use tokio::net::TcpListener;

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

#[tokio::main(flavor = "local")]
async fn main() -> Result<(), Box<dyn Error + 'static>> {
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

            let result = Builder::new(TokioLocalExecutor::new())
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
