use super::Status;

#[derive(Debug)]
pub enum SocksV4Error {
    IpV6,
    Command(Status),
}

impl From<Status> for SocksV4Error {
    fn from(err: Status) -> Self {
        Self::Command(err)
    }
}
