use std::{fmt, io};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    InvalidWav(&'static str),
    UnsupportedWav(String),
    UnsupportedFlac(String),
    Encode(String),
    Thread(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::InvalidWav(message) => write!(f, "invalid wav: {message}"),
            Self::UnsupportedWav(message) => write!(f, "unsupported wav: {message}"),
            Self::UnsupportedFlac(message) => write!(f, "unsupported flac target: {message}"),
            Self::Encode(message) => write!(f, "encode error: {message}"),
            Self::Thread(message) => write!(f, "thread error: {message}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
