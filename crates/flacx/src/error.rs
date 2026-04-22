use std::{fmt, io};

/// Result alias used by the public `flacx` API.
pub type Result<T> = std::result::Result<T, Error>;

/// Error type used by encode, decode, inspect, and recompress operations.
#[derive(Debug)]
pub enum Error {
    /// Wrapper for I/O failures from callers or owned readers/writers.
    Io(io::Error),
    /// The input PCM container is structurally invalid.
    InvalidPcmContainer(&'static str),
    /// The FLAC input is structurally invalid.
    InvalidFlac(&'static str),
    /// The input PCM container is valid enough to parse but uses unsupported features.
    UnsupportedPcmContainer(String),
    /// The FLAC input is valid enough to parse but uses unsupported features.
    UnsupportedFlac(String),
    /// The encode pipeline failed after input validation.
    Encode(String),
    /// The decode pipeline failed after input validation.
    Decode(String),
    /// A worker-thread or cross-thread coordination error occurred.
    Thread(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::InvalidPcmContainer(message) => write!(f, "invalid pcm container: {message}"),
            Self::InvalidFlac(message) => write!(f, "invalid flac: {message}"),
            Self::UnsupportedPcmContainer(message) => {
                write!(f, "unsupported pcm container: {message}")
            }
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
