use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use http::Uri;
use hyper::rt::{Read, Write};
use tower_service::Service;

use pin_project_lite::pin_project;

/// Tunnel Proxy via SOCKS 5 CONNECT
#[derive(Debug)]
pub struct Socks<C> {
    auth: Option<(String, String)>,
    inner: C,
    proxy_dst: Uri,
}

#[derive(Debug)]
pub enum SocksError<C> {
    Inner(C),
    Io(std::io::Error),

    Parsing(ParsingError),
    Serialize(SerializeError),

    Auth(AuthError),
    Command(Status),

    MissingHost,
    MissingPort,
    HostTooLong,
}

#[derive(Debug)]
pub enum AuthError {
    Unsupported,
    MethodMismatch,
    Failed,
}

pin_project! {
    pub struct Handshaking<F, T, E> {
        #[pin]
        fut: BoxHandshaking<T, E>,
        _marker: std::marker::PhantomData<F>
    }
}

type BoxHandshaking<T, E> = Pin<Box<dyn Future<Output = Result<T, SocksError<E>>> + Send>>;

impl<C> Socks<C> {
    /// Create a new SOCKS CONNECT service
    pub fn new(proxy_dst: Uri, connector: C) -> Self {
        Self {
            auth: None,
            inner: connector,
            proxy_dst,
        }
    }

    /// Use User/Pass authentication method during handshake
    pub fn with_auth(mut self, user: String, pass: String) -> Self {
        self.auth = Some((user, pass));
        self
    }
}

impl<C> Service<Uri> for Socks<C>
where
    C: Service<Uri>,
    C::Future: Send + 'static,
    C::Response: Read + Write + Unpin + Send + 'static,
    C::Error: Send + 'static,
{
    type Response = C::Response;
    type Error = SocksError<C::Error>;
    type Future = Handshaking<C::Future, C::Response, C::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(SocksError::Inner)
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        let conn = self.inner.call(self.proxy_dst.clone());
        let auth = self.auth.clone();

        Handshaking {
            fut: Box::pin(async move {
                handshake(
                    conn.await.map_err(SocksError::Inner)?,
                    dst.host().ok_or(SocksError::MissingHost)?.to_string(),
                    dst.port().ok_or(SocksError::MissingPort)?.as_u16(),
                    auth,
                )
                .await
            }),

            _marker: Default::default(),
        }
    }
}

impl<F, T, E> Future for Handshaking<F, T, E>
where
    F: Future<Output = Result<T, E>>,
{
    type Output = Result<T, SocksError<E>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().fut.poll(cx)
    }
}

async fn handshake<T, E>(
    mut conn: T,
    host: String,
    port: u16,
    auth: Option<(String, String)>,
) -> Result<T, SocksError<E>>
where
    T: Read + Write + Unpin,
{
    let address = match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => Address::V4(v4, port),
        Ok(IpAddr::V6(v6)) => Address::V6(v6, port),
        Err(_) if host.len() <= 255 => Address::Domain(host, port),
        Err(_) => return Err(SocksError::HostTooLong),
    };

    let method = if let Some(_) = auth {
        AuthMethod::UserPass
    } else {
        AuthMethod::NoAuth
    };

    let mut buf: [u8; 513] = [0; 513];

    // Write message
    let req = NegotiationReq(method);
    let n = req.write_to_buf(&mut buf[..])?;
    crate::rt::write_all(&mut conn, &buf[..n]).await?;

    // Read response
    let res: NegotiationRes = read_message(&mut conn, &mut buf).await?;

    if res.0 == AuthMethod::NoneAcceptable {
        return Err(AuthError::Unsupported.into());
    }

    if res.0 != method {
        return Err(AuthError::MethodMismatch.into());
    }

    // Optional authentication flow
    if res.0 == AuthMethod::UserPass {
        // Write message
        let (user, pass) = auth.unwrap();
        let req = AuthenticationReq(&user, &pass);
        let n = req.write_to_buf(&mut buf[..])?;
        crate::rt::write_all(&mut conn, &buf[..n]).await?;

        // Read response
        let res: AuthenticationRes = read_message(&mut conn, &mut buf).await?;

        if !res.0 {
            return Err(AuthError::Failed.into());
        }
    }

    // Send proxy request
    let req = ProxyReq(address);
    let n = req.write_to_buf(&mut buf[..])?;
    crate::rt::write_all(&mut conn, &buf[..n]).await?;

    // Read proxy status
    let res: ProxyRes = read_message(&mut conn, &mut buf).await?;

    if res.0 == Status::Success {
        Ok(conn)
    } else {
        Err(res.0.into())
    }
}

async fn read_message<T, M, C>(mut conn: &mut T, buf: &mut [u8]) -> Result<M, SocksError<C>>
where
    T: Read + Unpin,
    M: for<'a> TryFrom<&'a [u8], Error = ParsingError>,
{
    let mut n = 0;
    loop {
        let read = crate::rt::read(&mut conn, buf).await?;

        if read == 0 {
            return Err(
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "unexpected eof").into(),
            );
        }

        n += read;
        match M::try_from(&buf[..n]) {
            Err(ParsingError::Incomplete) => continue,
            Err(err) => return Err(err.into()),

            Ok(res) => return Ok(res),
        }
    }
}

use messages::*;
mod messages {
    use super::*;

    #[derive(Debug)]
    pub struct NegotiationReq(pub AuthMethod);
    #[derive(Debug)]
    pub struct NegotiationRes(pub AuthMethod);

    #[derive(Debug)]
    pub struct AuthenticationReq<'a>(pub &'a str, pub &'a str);
    #[derive(Debug)]
    pub struct AuthenticationRes(pub bool);

    #[derive(Debug)]
    pub struct ProxyReq(pub Address);
    #[derive(Debug)]
    pub struct ProxyRes(pub Status);

    #[repr(u8)]
    #[derive(Debug, Copy, Clone, PartialEq)]
    pub enum AuthMethod {
        NoAuth = 0x00,
        UserPass = 0x02,
        NoneAcceptable = 0xFF,
    }

    #[derive(Debug)]
    pub enum Address {
        V4(Ipv4Addr, u16),
        V6(Ipv6Addr, u16),
        Domain(String, u16),
    }

    #[derive(Debug, Copy, Clone, PartialEq)]
    pub enum Status {
        Success,
        GeneralServerFailure,
        ConnectionNotAllowed,
        NetworkUnreachable,
        HostUnreachable,
        ConnectionRefused,
        TtlExpired,
        CommandNotSupported,
        AddressTypeNotSupported,
    }

    #[derive(Debug)]
    pub enum ParsingError {
        Incomplete,
        Other,
    }

    #[derive(Debug)]
    pub enum SerializeError {
        WouldOverflow,
    }

    impl TryFrom<u8> for AuthMethod {
        type Error = ParsingError;

        fn try_from(byte: u8) -> Result<Self, Self::Error> {
            Ok(match byte {
                0x00 => Self::NoAuth,
                0x02 => Self::UserPass,
                0xFF => Self::NoneAcceptable,

                _ => return Err(ParsingError::Other),
            })
        }
    }

    impl From<NegotiationReq> for [u8; 3] {
        fn from(req: NegotiationReq) -> Self {
            [0x05, 0x01, req.0 as u8]
        }
    }

    use bytes::{Buf, BufMut};

    impl NegotiationReq {
        ///  +----+----------+----------+
        /// |VER | NMETHODS | METHODS  |
        /// +----+----------+----------+
        /// | 1  |    1     | 1 to 255 |
        /// +----+----------+----------+
        pub fn write_to_buf<B: BufMut>(&self, mut buf: B) -> Result<usize, SerializeError> {
            if buf.remaining_mut() < 3 {
                return Err(SerializeError::WouldOverflow);
            }

            buf.put_u8(0x05); // Version
            buf.put_u8(0x01); // Number of authentication methods
            buf.put_u8(self.0 as u8); // Authentication method

            Ok(3)
        }
    }

    impl TryFrom<&[u8]> for NegotiationRes {
        type Error = ParsingError;

        /// +----+--------+
        /// |VER | METHOD |
        /// +----+--------+
        /// | 1  |   1    |
        /// +----+--------+
        fn try_from(mut buf: &[u8]) -> Result<Self, ParsingError> {
            use bytes::Buf;

            if buf.remaining() < 2 {
                return Err(ParsingError::Incomplete);
            }

            if buf.get_u8() != 0x05 {
                return Err(ParsingError::Other);
            }

            let method = buf.get_u8().try_into()?;
            Ok(Self(method))
        }
    }

    impl AuthenticationReq<'_> {
        /// +----+------+----------+------+----------+
        /// |VER | ULEN |  UNAME   | PLEN |  PASSWD  |
        /// +----+------+----------+------+----------+
        /// | 1  |  1   | 1 to 255 |  1   | 1 to 255 |
        /// +----+------+----------+------+----------+
        pub fn write_to_buf<B: BufMut>(&self, mut buf: B) -> Result<usize, SerializeError> {
            if buf.remaining_mut() < 3 + self.0.len() + self.1.len() {
                return Err(SerializeError::WouldOverflow);
            }

            buf.put_u8(0x01); // Version

            buf.put_u8(self.0.len() as u8); // Username length (guarenteed to be 255 or less)
            buf.put_slice(self.0.as_bytes()); // Username

            buf.put_u8(self.1.len() as u8); // Password length (guarenteed to be 255 or less)
            buf.put_slice(self.1.as_bytes()); // Password

            Ok(3 + self.0.len() + self.1.len())
        }
    }

    impl TryFrom<&[u8]> for AuthenticationRes {
        type Error = ParsingError;

        /// +----+--------+
        /// |VER | STATUS |
        /// +----+--------+
        /// | 1  |   1    |
        /// +----+--------+
        fn try_from(mut buf: &[u8]) -> Result<Self, ParsingError> {
            use bytes::Buf;

            if buf.remaining() < 2 {
                return Err(ParsingError::Incomplete);
            }

            if buf.get_u8() != 0x01 {
                return Err(ParsingError::Other);
            }

            if buf.get_u8() == 0 {
                Ok(Self(true))
            } else {
                Ok(Self(false))
            }
        }
    }

    impl ProxyReq {
        /// +----+-----+-------+------+----------+----------+
        /// |VER | CMD |  RSV  | ATYP | DST.ADDR | DST.PORT |
        /// +----+-----+-------+------+----------+----------+
        /// | 1  |  1  | X'00' |  1   | Variable |    2     |
        /// +----+-----+-------+------+----------+----------+
        pub fn write_to_buf<B: BufMut>(&self, mut buf: B) -> Result<usize, SerializeError> {
            let addr_len = match self.0 {
                Address::V4(_, _) => 1 + 4 + 2,
                Address::V6(_, _) => 1 + 16 + 2,
                Address::Domain(ref domain, _) => 1 + 1 + domain.len() + 2,
            };

            if buf.remaining_mut() < 3 + addr_len {
                return Err(SerializeError::WouldOverflow);
            }

            buf.put_u8(0x05); // Version
            buf.put_u8(0x01); // TCP tunneling command
            buf.put_u8(0x00); // Reserved
            let _ = self.0.write_to_buf(buf); // Address

            Ok(3 + addr_len)
        }
    }

    impl TryFrom<&[u8]> for ProxyRes {
        type Error = ParsingError;

        /// +----+-----+-------+------+----------+----------+
        /// |VER | REP |  RSV  | ATYP | BND.ADDR | BND.PORT |
        /// +----+-----+-------+------+----------+----------+
        /// | 1  |  1  | X'00' |  1   | Variable |    2     |
        /// +----+-----+-------+------+----------+----------+
        fn try_from(mut buf: &[u8]) -> Result<Self, ParsingError> {
            if buf.remaining() < 2 {
                return Err(ParsingError::Incomplete);
            }

            // VER
            if buf.get_u8() != 0x05 {
                return Err(ParsingError::Other);
            }

            // REP
            let status = buf.get_u8().try_into()?;

            // RSV
            if buf.get_u8() != 0x00 {
                return Err(ParsingError::Other);
            }

            // ATYP + ADDR
            Address::try_from(buf)?;

            Ok(Self(status))
        }
    }

    impl Address {
        pub fn write_to_buf<B: BufMut>(&self, mut buf: B) -> Result<usize, SerializeError> {
            match self {
                Self::V4(ip, port) => {
                    if buf.remaining_mut() < 1 + 4 + 2 {
                        return Err(SerializeError::WouldOverflow);
                    }

                    buf.put_u8(0x01);
                    buf.put_slice(&ip.octets());
                    buf.put_u16(*port); // Network Order/BigEndian for port

                    Ok(7)
                }

                Self::V6(ip, port) => {
                    if buf.remaining_mut() < 1 + 16 + 2 {
                        return Err(SerializeError::WouldOverflow);
                    }

                    buf.put_u8(0x04);
                    buf.put_slice(&ip.octets());
                    buf.put_u16(*port); // Network Order/BigEndian for port

                    Ok(19)
                }

                Self::Domain(domain, port) => {
                    if buf.remaining_mut() < 1 + 1 + domain.len() + 2 {
                        return Err(SerializeError::WouldOverflow);
                    }

                    buf.put_u8(0x03);
                    buf.put_u8(domain.len() as u8); // Guarenteed to be less than 255
                    buf.put_slice(domain.as_bytes());
                    buf.put_u16(*port);

                    Ok(4 + domain.len())
                }
            }
        }
    }

    impl TryFrom<&[u8]> for Address {
        type Error = ParsingError;

        fn try_from(mut buf: &[u8]) -> Result<Self, Self::Error> {
            use bytes::Buf;

            if buf.remaining() < 2 {
                return Err(ParsingError::Incomplete);
            }

            Ok(match buf.get_u8() {
                0x01 => {
                    let mut ip = [0; 4];

                    if buf.remaining() < 6 {
                        return Err(ParsingError::Incomplete);
                    }

                    buf.copy_to_slice(&mut ip);
                    let port = buf.get_u16();

                    Self::V4(ip.into(), port)
                }

                0x03 => {
                    let len = buf.get_u8();

                    if len == 0 {
                        return Err(ParsingError::Other);
                    } else if buf.remaining() < (len as usize) + 2 {
                        return Err(ParsingError::Incomplete);
                    }

                    let domain = std::str::from_utf8(&buf[..len as usize])
                        .map_err(|_| ParsingError::Other)?
                        .to_string();

                    let port = buf.get_u16();

                    Self::Domain(domain, port)
                }

                0x04 => {
                    let mut ip = [0; 16];

                    if buf.remaining() < 6 {
                        return Err(ParsingError::Incomplete);
                    }
                    buf.copy_to_slice(&mut ip);
                    let port = buf.get_u16();

                    Self::V6(ip.into(), port)
                }

                _ => return Err(ParsingError::Other),
            })
        }
    }

    impl TryFrom<u8> for Status {
        type Error = ParsingError;

        fn try_from(byte: u8) -> Result<Self, Self::Error> {
            Ok(match byte {
                0x00 => Status::Success,

                0x01 => Status::GeneralServerFailure,
                0x02 => Status::ConnectionNotAllowed,
                0x03 => Status::NetworkUnreachable,
                0x04 => Status::HostUnreachable,
                0x05 => Status::ConnectionRefused,
                0x06 => Status::TtlExpired,
                0x07 => Status::CommandNotSupported,
                0x08 => Status::AddressTypeNotSupported,
                _ => return Err(ParsingError::Other),
            })
        }
    }
}

impl<C> From<std::io::Error> for SocksError<C> {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl<C> From<ParsingError> for SocksError<C> {
    fn from(err: ParsingError) -> Self {
        Self::Parsing(err)
    }
}

impl<C> From<AuthError> for SocksError<C> {
    fn from(err: AuthError) -> Self {
        Self::Auth(err)
    }
}

impl<C> From<Status> for SocksError<C> {
    fn from(err: Status) -> Self {
        Self::Command(err)
    }
}

impl<C> From<SerializeError> for SocksError<C> {
    fn from(err: SerializeError) -> Self {
        Self::Serialize(err)
    }
}
