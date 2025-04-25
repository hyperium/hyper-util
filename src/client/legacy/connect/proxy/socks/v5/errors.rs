use super::Status;

#[derive(Debug)]
pub enum SocksV5Error {
    HostTooLong,
    Auth(AuthError),
    Command(Status),
}

#[derive(Debug)]
pub enum AuthError {
    Unsupported,
    MethodMismatch,
    Failed,
}

impl From<Status> for SocksV5Error {
    fn from(err: Status) -> Self {
        Self::Command(err)
    }
}

impl From<AuthError> for SocksV5Error {
    fn from(err: AuthError) -> Self {
        Self::Auth(err)
    }
}
