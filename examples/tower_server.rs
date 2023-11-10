use bytes::Bytes;
use http::{Request, Response};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper_util::{rt::TokioExecutor, server::conn::auto::Builder};
use tokio::net::TcpListener;
use tower::BoxError;

async fn handle(_request: Request<Incoming>) -> Result<Response<Full<Bytes>>, BoxError> {
    Ok(Response::new(Full::from("Hello, World!")))
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let tower_service = tower::service_fn(handle);

    let listener = TcpListener::bind("0.0.0.0:3000").await.unwrap();
    loop {
        let (tcp_stream, _remote_addr) = listener.accept().await.unwrap();
        if let Err(err) = Builder::new(TokioExecutor::new())
            .serve_connection_with_upgrades_tower(tcp_stream, tower_service)
            .await
        {
            eprintln!("failed to serve connection: {err:#}");
        }
    }
}
