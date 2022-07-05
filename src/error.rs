use std::error::Error;
use std::fmt;

/// Generic Result
pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// Generic Error to replace hyper::Error for now
#[derive(Debug)]
pub struct GenericError;

impl std::error::Error for GenericError {}

impl fmt::Display for GenericError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Generic hyper-util error")
    }
}
