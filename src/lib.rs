pub mod encoder;
pub mod error;
pub mod level;
pub mod metadata;
pub mod wav;

pub use encoder::{EncodeSummary, Encoder, EncoderConfig};
pub use error::{Error, Result};

pub(crate) mod crc;
mod flac_writer;
mod frame;
