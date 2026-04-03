//! High-performance WAV-to-FLAC encoding for Rust.
//!
//! # Library-first, CLI-backed design
//!
//! `flacx` keeps a single tuned encode pipeline and exposes it through both a
//! Rust API and the sibling `flacx-cli` crate. The CLI remains a thin adapter
//! over the same library entrypoints used by Rust callers.
//!
//! Progress reporting is available through the optional `progress` Cargo
//! feature. Default builds exclude the progress-specific API surface.
//!
//! # Quick start
//!
//! ```no_run
//! use flacx::{EncodeOptions, FlacEncoder, level::Level};
//!
//! let options = EncodeOptions::default()
//!     .with_level(Level::Level8)
//!     .with_threads(4);
//!
//! FlacEncoder::new(options)
//!     .encode_file("input.wav", "output.flac")
//!     .unwrap();
//! ```

pub mod encoder;
pub mod error;
pub mod level;
pub mod metadata;
pub mod wav;

#[cfg(feature = "progress")]
pub use encoder::EncodeProgress;
pub use encoder::{EncodeOptions, EncodeSummary, FlacEncoder, encode_bytes, encode_file};
pub use error::{Error, Result};

#[allow(deprecated)]
pub use encoder::{Encoder, EncoderConfig};

pub(crate) mod crc;
mod flac_writer;
mod frame;

#[cfg(not(feature = "progress"))]
#[doc = r#"```compile_fail
use flacx::EncodeProgress;

fn main() {}
```"#]
#[doc(hidden)]
pub struct _ProgressFeatureDisabledDoc;
