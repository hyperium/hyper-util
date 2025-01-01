use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tower_service::Service;

use hyper_util::client::legacy::connect::{proxy::Tunnel, HttpConnector};

#[cfg(not(miri))]
#[tokio::test]
async fn test_tunnel_works() {
    let tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = tcp.local_addr().expect("local_addr");

    let proxy_dst = format!("http://{}", addr).parse().expect("uri");
    let mut connector = Tunnel::new(proxy_dst, HttpConnector::new());
    let t1 = tokio::spawn(async move {
        let _conn = connector
            .call("https://hyper.rs".parse().unwrap())
            .await
            .expect("tunnel");
    });

    let t2 = tokio::spawn(async move {
        let (mut io, _) = tcp.accept().await.expect("accept");
        let mut buf = [0u8; 64];
        let n = io.read(&mut buf).await.expect("read 1");
        assert_eq!(
            &buf[..n],
            b"CONNECT hyper.rs:443 HTTP/1.1\r\nHost: hyper.rs:443\r\n\r\n"
        );
        io.write_all(b"HTTP/1.1 200 OK\r\n\r\n")
            .await
            .expect("write 1");
    });

    t1.await.expect("task 1");
    t2.await.expect("task 2");
}
