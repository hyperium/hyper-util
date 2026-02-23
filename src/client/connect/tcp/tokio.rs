use crate::client::connect::{Connected, Connection};
use crate::rt::TokioIo;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;
use tokio::net::{TcpSocket, TcpStream};

/// A connector that uses `tokio` for TCP connections.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioTcpConnector {
    _priv: (),
}

impl TokioTcpConnector {
    /// Create a new `TokioTcpConnector`.
    pub fn new() -> Self {
        Self { _priv: () }
    }
}

impl super::TcpConnector for TokioTcpConnector {
    type TcpStream = std::net::TcpStream;
    type Connection = TokioIo<TcpStream>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Connection, Self::Error>> + Send>>;
    type Sleep = tokio::time::Sleep;

    fn connect(&self, socket: std::net::TcpStream, addr: SocketAddr) -> Self::Future {
        let socket = TcpSocket::from_std_stream(socket);
        let connect = socket.connect(addr);
        Box::pin(async move {
            let stream = connect.await?;
            Ok(TokioIo::new(stream))
        })
    }

    fn sleep(&self, duration: Duration) -> Self::Sleep {
        tokio::time::sleep(duration)
    }
}

impl Connection for tokio::net::TcpStream {
    fn connected(&self) -> Connected {
        let connected = Connected::new();
        if let (Ok(remote_addr), Ok(local_addr)) = (self.peer_addr(), self.local_addr()) {
            connected.extra(crate::client::connect::http::HttpInfo {
                remote_addr,
                local_addr,
            })
        } else {
            connected
        }
    }
}

impl<T: Connection> Connection for crate::rt::TokioIo<T> {
    fn connected(&self) -> Connected {
        self.inner().connected()
    }
}

#[cfg(unix)]
impl Connection for std::os::unix::net::UnixStream {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

#[cfg(windows)]
impl Connection for ::tokio::net::windows::named_pipe::NamedPipeClient {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}
