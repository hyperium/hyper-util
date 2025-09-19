use tower::ServiceExt;
use tower_service::Service;

use hyper_util::client::pool;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    send_nego().await
}

async fn send_h1() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let tcp = hyper_util::client::legacy::connect::HttpConnector::new();

    let http1 = tcp.and_then(|conn| {
        Box::pin(async move {
            let (mut tx, c) = hyper::client::conn::http1::handshake::<
                _,
                http_body_util::Empty<hyper::body::Bytes>,
            >(conn)
            .await?;
            tokio::spawn(async move {
                if let Err(e) = c.await {
                    eprintln!("connection error: {:?}", e);
                }
            });
            let svc = tower::service_fn(move |req| tx.send_request(req));
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(svc)
        })
    });

    let mut p = pool::Cache::new(http1).build();

    let mut c = p.call(http::Uri::from_static("http://hyper.rs")).await?;
    eprintln!("{:?}", c);

    let req = http::Request::builder()
        .header("host", "hyper.rs")
        .body(http_body_util::Empty::new())
        .unwrap();

    c.ready().await?;
    let resp = c.call(req).await?;
    eprintln!("{:?}", resp);

    Ok(())
}

async fn send_h2() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let tcp = hyper_util::client::legacy::connect::HttpConnector::new();

    let http2 = tcp.and_then(|conn| {
        Box::pin(async move {
            let (mut tx, c) = hyper::client::conn::http2::handshake::<
                _,
                _,
                http_body_util::Empty<hyper::body::Bytes>,
            >(hyper_util::rt::TokioExecutor::new(), conn)
            .await?;
            println!("connected");
            tokio::spawn(async move {
                if let Err(e) = c.await {
                    eprintln!("connection error: {:?}", e);
                }
            });
            let svc = tower::service_fn(move |req| tx.send_request(req));
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(svc)
        })
    });

    let mut p = pool::Singleton::new(http2);

    for _ in 0..5 {
        let mut c = p
            .call(http::Uri::from_static("http://localhost:3000"))
            .await?;
        eprintln!("{:?}", c);

        let req = http::Request::builder()
            .header("host", "hyper.rs")
            .body(http_body_util::Empty::new())
            .unwrap();

        c.ready().await?;
        let resp = c.call(req).await?;
        eprintln!("{:?}", resp);
    }

    Ok(())
}

async fn send_nego() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let tcp = hyper_util::client::legacy::connect::HttpConnector::new();

    let http1 = tower::layer::layer_fn(|tcp| {
        tower::service_fn(move |dst| {
            let inner = tcp.call(dst);
            async move {
                let conn = inner.await?;
                let (mut tx, c) = hyper::client::conn::http1::handshake::<
                    _,
                    http_body_util::Empty<hyper::body::Bytes>,
                >(conn)
                .await?;
                tokio::spawn(async move {
                    if let Err(e) = c.await {
                        eprintln!("connection error: {:?}", e);
                    }
                });
                let svc = tower::service_fn(move |req| tx.send_request(req));
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(svc)
            }
        })
    });

    let http2 = tower::layer::layer_fn(|tcp| {
        tower::service_fn(move |dst| {
            let inner = tcp.call(dst);
            async move {
                let conn = inner.await?;
                let (mut tx, c) = hyper::client::conn::http2::handshake::<
                    _,
                    _,
                    http_body_util::Empty<hyper::body::Bytes>,
                >(hyper_util::rt::TokioExecutor::new(), conn)
                .await?;
                println!("connected");
                tokio::spawn(async move {
                    if let Err(e) = c.await {
                        eprintln!("connection error: {:?}", e);
                    }
                });
                let svc = tower::service_fn(move |req| tx.send_request(req));
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(svc)
            }
        })
    });

    let mut svc = pool::negotiate(tcp, |_| false, http1, http2);

    for _ in 0..5 {
        let mut c = svc
            .call(http::Uri::from_static("http://localhost:3000"))
            .await?;
        eprintln!("{:?}", c);

        let req = http::Request::builder()
            .header("host", "hyper.rs")
            .body(http_body_util::Empty::new())
            .unwrap();

        c.ready().await?;
        let resp = c.call(req).await?;
        eprintln!("{:?}", resp);
    }

    Ok(())
}
