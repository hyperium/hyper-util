use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tower_service::Service;

use hyper_util::client::legacy::connect::{proxy::{Socks, Tunnel}, HttpConnector};

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

#[cfg(not(miri))]
#[tokio::test]
async fn test_socks_without_auth_works() {
    let proxy_tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let proxy_addr = proxy_tcp.local_addr().expect("local_addr");
    let proxy_dst = format!("http://{}", proxy_addr).parse().expect("uri");

    let target_tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let target_addr = target_tcp.local_addr().expect("local_addr");
    let target_dst = format!("http://{}", target_addr).parse().expect("uri");

    let mut connector = Socks::new(proxy_dst, HttpConnector::new());

    // Client
    //
    // Will use `Tunnel` to establish proxy tunnel.
    // Will send "Hello World!" to the target and receive "Goodbye!" back.
    let t1 = tokio::spawn(async move {
        let conn = connector.call(target_dst).await.expect("tunnel");
        let mut tcp = conn.into_inner();

        tcp.write_all(b"Hello World!").await.expect("write 1");

        let mut buf = [0u8; 64];
        let n = tcp.read(&mut buf).await.expect("read 1");
        assert_eq!(&buf[..n], b"Goodbye!");
    });

    // Proxy
    //
    // Will receive CONNECT request from client.
    // Will connect to target and send 200 back to client.
    // Will blindly tunnel between client and target.
    let t2 = tokio::spawn(async move {
        let (mut to_client, _) = proxy_tcp.accept().await.expect("accept");
        let mut buf = [0u8; 513];

        // negotiation req/res
        let n = to_client.read(&mut buf).await.expect("read 1");
        assert_eq!(&buf[..n], [0x05, 0x01, 0x00]);

        to_client.write_all(&[0x05, 0x00]).await.expect("write 1");

        // command req/rs
        let [p1, p2] = target_addr.port().to_be_bytes();
        let [ip1, ip2, ip3, ip4] = [0x7f, 0x00, 0x00, 0x01];
        let message = [0x05, 0x01, 0x00, 0x01, ip1, ip2, ip3, ip4, p1, p2];
        let n = to_client.read(&mut buf).await.expect("read 2");
        assert_eq!(&buf[..n], message);

        let mut to_target = TcpStream::connect(target_addr).await.expect("connect");

        let message = [0x05, 0x00, 0x00, 0x01, ip1, ip2, ip3, ip4, p1, p2];
        to_client.write_all(&message).await.expect("write 2");

        let (from_client, from_target) =
            tokio::io::copy_bidirectional(&mut to_client, &mut to_target)
                .await
                .expect("proxy");

        assert_eq!(from_client, 12);
        assert_eq!(from_target, 8)
    });

    // Target server
    //
    // Will accept connection from proxy server
    // Will receive "Hello World!" from the client and return "Goodbye!"
    let t3 = tokio::spawn(async move {
        let (mut io, _) = target_tcp.accept().await.expect("accept");
        let mut buf = [0u8; 64];

        let n = io.read(&mut buf).await.expect("read 1");
        assert_eq!(&buf[..n], b"Hello World!");

        io.write_all(b"Goodbye!").await.expect("write 1");
    });

    t1.await.expect("task - client");
    t2.await.expect("task - proxy");
    t3.await.expect("task - target");
}

#[cfg(not(miri))]
#[tokio::test]
async fn test_socks_with_auth_works() {
    let proxy_tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let proxy_addr = proxy_tcp.local_addr().expect("local_addr");
    let proxy_dst = format!("http://{}", proxy_addr).parse().expect("uri");

    let target_tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let target_addr = target_tcp.local_addr().expect("local_addr");
    let target_dst = format!("http://{}", target_addr).parse().expect("uri");

    let mut connector =
        Socks::new(proxy_dst, HttpConnector::new()).with_auth("user".into(), "pass".into());

    // Client
    //
    // Will use `Tunnel` to establish proxy tunnel.
    // Will send "Hello World!" to the target and receive "Goodbye!" back.
    let t1 = tokio::spawn(async move {
        let conn = connector.call(target_dst).await.expect("tunnel");
        let mut tcp = conn.into_inner();

        tcp.write_all(b"Hello World!").await.expect("write 1");

        let mut buf = [0u8; 64];
        let n = tcp.read(&mut buf).await.expect("read 1");
        assert_eq!(&buf[..n], b"Goodbye!");
    });

    // Proxy
    //
    // Will receive CONNECT request from client.
    // Will connect to target and send 200 back to client.
    // Will blindly tunnel between client and target.
    let t2 = tokio::spawn(async move {
        let (mut to_client, _) = proxy_tcp.accept().await.expect("accept");
        let mut buf = [0u8; 513];

        // negotiation req/res
        let n = to_client.read(&mut buf).await.expect("read 1");
        assert_eq!(&buf[..n], [0x05, 0x01, 0x02]);

        to_client.write_all(&[0x05, 0x02]).await.expect("write 1");

        // auth req/res
        let n = to_client.read(&mut buf).await.expect("read 2");
        let [u1, u2, u3, u4] = b"user";
        let [p1, p2, p3, p4] = b"pass";
        let message = [0x01, 0x04, *u1, *u2, *u3, *u4, 0x04, *p1, *p2, *p3, *p4];
        assert_eq!(&buf[..n], message);

        to_client.write_all(&[0x01, 0x00]).await.expect("write 2");

        // command req/res
        let n = to_client.read(&mut buf).await.expect("read 3");
        let [p1, p2] = target_addr.port().to_be_bytes();
        let [ip1, ip2, ip3, ip4] = [0x7f, 0x00, 0x00, 0x01];
        let message = [0x05, 0x01, 0x00, 0x01, ip1, ip2, ip3, ip4, p1, p2];
        assert_eq!(&buf[..n], message);

        let mut to_target = TcpStream::connect(target_addr).await.expect("connect");

        let message = [0x05, 0x00, 0x00, 0x01, ip1, ip2, ip3, ip4, p1, p2];
        to_client.write_all(&message).await.expect("write 3");

        let (from_client, from_target) =
            tokio::io::copy_bidirectional(&mut to_client, &mut to_target)
                .await
                .expect("proxy");

        assert_eq!(from_client, 12);
        assert_eq!(from_target, 8)
    });

    // Target server
    //
    // Will accept connection from proxy server
    // Will receive "Hello World!" from the client and return "Goodbye!"
    let t3 = tokio::spawn(async move {
        let (mut io, _) = target_tcp.accept().await.expect("accept");
        let mut buf = [0u8; 64];

        let n = io.read(&mut buf).await.expect("read 1");
        assert_eq!(&buf[..n], b"Hello World!");

        io.write_all(b"Goodbye!").await.expect("write 1");
    });

    t1.await.expect("task - client");
    t2.await.expect("task - proxy");
    t3.await.expect("task - target");
}

#[cfg(not(miri))]
#[tokio::test]
async fn test_socks_with_domain_works() {
    let proxy_tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let proxy_addr = proxy_tcp.local_addr().expect("local_addr");
    let proxy_addr = format!("http://{}", proxy_addr).parse().expect("uri");

    let mut connector =
        Socks::new(proxy_addr, HttpConnector::new()).with_auth("user".into(), "pass".into());

    // Client
    //
    // Will use `Tunnel` to establish proxy tunnel.
    // Will send "Hello World!" to the target and receive "Goodbye!" back.
    let t1 = tokio::spawn(async move {
        let _conn = connector
            .call("https://hyper.rs:443".try_into().unwrap())
            .await
            .expect("tunnel");
    });

    // Proxy
    //
    // Will receive CONNECT request from client.
    // Will connect to target and send 200 back to client.
    // Will blindly tunnel between client and target.
    let t2 = tokio::spawn(async move {
        let (mut to_client, _) = proxy_tcp.accept().await.expect("accept");
        let mut buf = [0u8; 513];

        // negotiation req/res
        let n = to_client.read(&mut buf).await.expect("read 1");
        assert_eq!(&buf[..n], [0x05, 0x01, 0x02]);

        to_client.write_all(&[0x05, 0x02]).await.expect("write 1");

        // auth req/res
        let n = to_client.read(&mut buf).await.expect("read 2");
        let [u1, u2, u3, u4] = b"user";
        let [p1, p2, p3, p4] = b"pass";
        let message = [0x01, 0x04, *u1, *u2, *u3, *u4, 0x04, *p1, *p2, *p3, *p4];
        assert_eq!(&buf[..n], message);

        to_client.write_all(&[0x01, 0x00]).await.expect("write 2");

        // command req/res
        let n = to_client.read(&mut buf).await.expect("read 3");

        let host = "hyper.rs";
        let port: u16 = 443;
        let mut message = vec![0x05, 0x01, 0x00, 0x03, host.len() as u8];
        message.extend(host.bytes());
        message.extend(port.to_be_bytes());
        assert_eq!(&buf[..n], message);

        let mut message = vec![0x05, 0x00, 0x00, 0x03, host.len() as u8];
        message.extend(host.bytes());
        message.extend(port.to_be_bytes());
        to_client.write_all(&message).await.expect("write 3");
    });

    t1.await.expect("task - client");
    t2.await.expect("task - proxy");
}

