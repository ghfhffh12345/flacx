//! High-performance WAV/FLAC conversion for Rust.
//!
//! `flacx` exposes small encode/decode façades over staged audio pipelines.
//!
//! Default builds exclude progress-specific API surface.
//!
//! # Encode
//!
//! ```no_run
//! use flacx::{Encoder, EncoderConfig, level::Level};
//!
//! let config = EncoderConfig::builder()
//!     .level(Level::Level8)
//!     .threads(4)
//!     .build();
//!
//! Encoder::new(config)
//!     .encode_file("input.wav", "output.flac")
//!     .unwrap();
//! ```
//!
//! # Decode
//!
//! ```no_run
//! use flacx::Decoder;
//!
//! Decoder::new()
//!     .decode_file("input.flac", "output.wav")
//!     .unwrap();
//! ```

mod config;
mod crc;
mod decode;
mod encoder;
mod error;
mod input;
mod model;
mod plan;
mod progress;
mod read;
mod reconstruct;
mod stream_info;
mod wav_output;
mod write;

pub mod level;

pub use config::{EncoderBuilder, EncoderConfig};
pub use decode::{DecodeSummary, Decoder, decode_bytes, decode_file};
pub use encoder::{EncodeSummary, Encoder, encode_bytes, encode_file};
pub use error::{Error, Result};

#[cfg(feature = "progress")]
pub use progress::EncodeProgress;

#[cfg(not(feature = "progress"))]
#[doc = r#"```compile_fail
use flacx::EncodeProgress;

fn main() {}
```"#]
#[doc(hidden)]
pub struct _ProgressTypeFeatureDisabledDoc;

#[cfg(not(feature = "progress"))]
#[doc = r#"```compile_fail
use flacx::Encoder;

fn main() {
    let encoder = Encoder::default();
    let input = std::io::Cursor::new(Vec::<u8>::new());
    let mut output = std::io::Cursor::new(Vec::<u8>::new());
    let _ = encoder.encode_with_progress(input, &mut output, |_| Ok(()));
}
```"#]
#[doc(hidden)]
pub struct _ProgressMethodFeatureDisabledDoc;
