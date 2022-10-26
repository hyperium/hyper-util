//! High Level HTTP Server

#![allow(dead_code)]

use http::{Request, Response};
use http_body::Body;
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use parking_lot::Mutex;
use std::{
    error::Error as StdError,
    io::{self, ErrorKind},
    net::{SocketAddr, TcpListener as StdTcpListener},
    sync::Arc,
};
use tokio::net::TcpListener;
use tower::ServiceExt;
use tower_service::Service;

type BoxError = Box<dyn StdError + Send + Sync>;

#[derive(Debug)]
struct Server {
    listener: TcpListener,
}

impl Server {
    fn bind(addr: SocketAddr) -> io::Result<Self> {
        let listener = StdTcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;

        let listener = TcpListener::from_std(listener)?;

        Ok(Self { listener })
    }

    async fn bind_async(addr: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;

        Ok(Self { listener })
    }

    fn from_tcp(listener: StdTcpListener) -> io::Result<Self> {
        listener.set_nonblocking(true)?;
        let listener = TcpListener::from_std(listener)?;

        Ok(Self { listener })
    }

    async fn serve<M, S, B>(self, make_service: M) -> io::Result<()>
    where
        M: Service<SocketAddr, Response = S>,
        M::Error: Into<BoxError>,
        S: Service<Request<Incoming>, Response = Response<B>> + Send + 'static,
        S::Future: Send,
        S::Error: Into<BoxError> + Send,
        B: Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<BoxError>,
    {
        let mut make_service = make_service.map_err(io_other);

        loop {
            let (stream, addr) = self.listener.accept().await?;

            let service = make_service.ready().await?.call(addr).await?;

            tokio::spawn(async move {
                let service = Arc::new(Mutex::new(Some(service)));

                let _ = http1::Builder::new()
                    .serve_connection(
                        stream,
                        service_fn(move |req: Request<Incoming>| {
                            let service_store = service.clone();

                            async move {
                                // Take service. Don't forget to put it back.
                                let mut service = service_store
                                    .lock()
                                    .take()
                                    .expect("second future called before first finished");

                                let resp = service.ready().await?.call(req).await?;

                                // Put the service back.
                                *service_store.lock() = Some(service);

                                Ok::<_, S::Error>(resp)
                            }
                        }),
                    )
                    .await;
            });
        }
    }
}

fn io_other<E>(error: E) -> io::Error
where
    E: Into<BoxError>,
{
    io::Error::new(ErrorKind::Other, error)
}

#[cfg(test)]
mod tests {
    use super::Server;
    use http::{Request, Response};
    use http_body_util::{BodyExt, Empty, Full};
    use hyper::{
        body::{Bytes, Incoming},
        client::conn,
    };
    use std::{convert::Infallible, net::TcpListener as StdTcpListener};
    use tokio::net::TcpStream;
    use tower::make::Shared;

    async fn works() {
        const BODY: &'static [u8] = b"Hello, world!";

        let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            Server::from_tcp(listener)
                .unwrap()
                .serve(Shared::new(tower::service_fn(
                    |_req: Request<Incoming>| async move {
                        Ok::<_, Infallible>(Response::new(Full::<Bytes>::from(BODY)))
                    },
                )))
                .await
                .unwrap();
        });

        let stream = TcpStream::connect(addr).await.unwrap();
        let (mut sender, connection) = conn::http1::handshake(stream).await.unwrap();

        tokio::spawn(connection);

        let request = Request::new(Empty::<Bytes>::new());
        let response = sender.send_request(request).await.unwrap();

        let body = response.into_body().collect().await.unwrap().to_bytes();

        assert_eq!(body, BODY);
    }
}
