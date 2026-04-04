//! High-performance WAV-to-FLAC encoding for Rust.
//!
//! `flacx` exposes a small encoder façade over a staged encode pipeline:
//! input loading, encode planning, frame modelling, stream writing, and
//! optional progress reporting.
//!
//! Default builds exclude progress-specific API surface.
//!
//! # Quick start
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

mod config;
mod crc;
mod encoder;
mod error;
mod input;
mod model;
mod plan;
mod progress;
mod stream_info;
mod write;

pub mod level;

pub use config::{EncoderBuilder, EncoderConfig};
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
