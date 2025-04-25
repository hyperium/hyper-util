mod v5;
pub use v5::{SocksV5, SocksV5Error};

mod v4;
pub use v4::{SocksV4, SocksV4Error};

use hyper::rt::Read;

#[derive(Debug)]
pub enum SocksError<C> {
    Inner(C),
    Io(std::io::Error),

    DnsFailure,
    MissingHost,
    MissingPort,

    V4(SocksV4Error),
    V5(SocksV5Error),

    Parsing(ParsingError),
    Serialize(SerializeError),
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

impl<C> From<SerializeError> for SocksError<C> {
    fn from(err: SerializeError) -> Self {
        Self::Serialize(err)
    }
}

impl<C> From<SocksV4Error> for SocksError<C> {
    fn from(err: SocksV4Error) -> Self {
        Self::V4(err)
    }
}

impl<C> From<SocksV5Error> for SocksError<C> {
    fn from(err: SocksV5Error) -> Self {
        Self::V5(err)
    }
}
