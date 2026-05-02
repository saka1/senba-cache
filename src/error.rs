use std::fmt;

#[derive(Debug)]
pub enum Error {
    NotFound,
    Expired,
    StorageFull,
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotFound => write!(f, "key not found"),
            Error::Expired => write!(f, "entry expired"),
            Error::StorageFull => write!(f, "cache storage is full"),
            Error::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
