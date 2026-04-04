use std::{fmt, io};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    InvalidWav(&'static str),
    InvalidFlac(&'static str),
    UnsupportedWav(String),
    UnsupportedFlac(String),
    Encode(String),
    Decode(String),
    Thread(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::InvalidWav(message) => write!(f, "invalid wav: {message}"),
            Self::InvalidFlac(message) => write!(f, "invalid flac: {message}"),
            Self::UnsupportedWav(message) => write!(f, "unsupported wav: {message}"),
            Self::UnsupportedFlac(message) => write!(f, "unsupported flac: {message}"),
            Self::Encode(message) => write!(f, "encode error: {message}"),
            Self::Decode(message) => write!(f, "decode error: {message}"),
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
