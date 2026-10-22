//! This example demonstrates request -> response flow of the `Expect: 100-continue` header

use std::{convert::Infallible, error::Error};

use bytes::Bytes;
use http::{Method, Request, Response};
use http_body_util::{BodyExt, Full};
use hyper::{client::conn::http1::handshake, server::conn::http1::Builder, service::service_fn};
use hyper_util::{client::expect_continue::wrap, rt::TokioIo};
use tokio::net::{TcpListener, TcpStream};

async fn echo(req: Request<hyper::body::Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let body = req.into_body().collect().await.unwrap().to_bytes();

    println!(
        "server read {} body bytes {:?}",
        body.len(),
        String::from_utf8_lossy(&body)
    );

    Ok(Response::new(Full::new(body)))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
    let addr = "127.0.0.1:3000";

    let listener = TcpListener::bind(addr).await?;
    tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let io = TokioIo::new(tcp);

        if let Err(e) = Builder::new().serve_connection(io, service_fn(echo)).await {
            eprintln!("server error {e:?}");
        }
    });

    let stream = TcpStream::connect(addr).await?;
    let io = TokioIo::new(stream);
    let (mut sender, conn) = handshake(io).await?;
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("client connection error: {e:?}");
        }
    });

    let req = Request::builder()
        .method(Method::POST)
        .uri("/")
        .header(hyper::header::HOST, addr)
        .body(Full::new(Bytes::from("hi")))?;

    let resp = sender.send_request(wrap(req, None)).await?;

    println!("{:?} {:?}", resp.version(), resp.status());
    println!("{:#?}", resp.headers());

    Ok(())
}
