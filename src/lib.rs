//! High-performance WAV-to-FLAC encoding for Rust.
//!
//! # Library-first, CLI-backed design
//!
//! `flacx` keeps a single tuned encode pipeline and exposes it through both a
//! Rust API and the `flacx` CLI. The CLI is a thin adapter over the same
//! library entrypoints used by Rust callers.
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

pub use encoder::{EncodeOptions, EncodeSummary, FlacEncoder, encode_bytes, encode_file};
pub use error::{Error, Result};

#[allow(deprecated)]
pub use encoder::{Encoder, EncoderConfig};

pub(crate) mod crc;
mod flac_writer;
mod frame;
