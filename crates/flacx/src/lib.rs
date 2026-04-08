//! High-performance WAV/FLAC conversion and recompression for Rust.
//!
//! The `flacx` crate is the reusable library layer in this workspace. It
//! exposes compact encode and decode façades, builder-backed configuration
//! types, a FLAC recompression façade, byte-oriented helpers, stream
//! inspection helpers, and optional progress reporting.
//!
//! The API is intentionally small:
//!
//! - [`EncoderConfig`] and [`DecodeConfig`] hold user-facing settings.
//! - [`RecompressConfig`] composes the existing decode and encode settings for
//!   FLAC-to-FLAC recompression.
//! - [`Encoder`], [`Decoder`], and [`Recompressor`] provide the main streaming
//!   façades.
//! - [`encode_file`], [`encode_bytes`], [`decode_file`], and [`decode_bytes`]
//!   offer convenience entry points for one-off use.
//! - [`recompress_file`] and [`recompress_bytes`] provide convenience
//!   FLAC-to-FLAC entry points.
//! - [`inspect_wav_total_samples`] and [`inspect_flac_total_samples`] let you
//!   read stream totals without running a full conversion.
//! - [`level`] exposes the compression presets used by the encoder.
//! - The optional `progress` feature adds callback-friendly progress
//!   reporting.
//!
//! ## Quick start
//!
//! Encode a WAV file to FLAC with the default level and a custom thread count:
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
//! Decode a FLAC file back to WAV:
//!
//! ```no_run
//! use flacx::Decoder;
//!
//! Decoder::default()
//!     .decode_file("input.flac", "output.wav")
//!     .unwrap();
//! ```
//!
//! Recompress an existing FLAC with a different encode configuration:
//!
//! ```no_run
//! use flacx::{Decoder, EncoderConfig, RecompressConfig, Recompressor, level::Level};
//!
//! let config = RecompressConfig::builder()
//!     .encode_config(
//!         EncoderConfig::builder()
//!             .level(Level::Level0)
//!             .threads(2)
//!             .build(),
//!     )
//!     .build();
//!
//! Recompressor::new(config)
//!     .recompress_file("input.flac", "input.recompressed.flac")
//!     .unwrap();
//!
//! let original = Decoder::default().decode_bytes(&std::fs::read("input.flac").unwrap()).unwrap();
//! let recompressed = Decoder::default()
//!     .decode_bytes(&std::fs::read("input.recompressed.flac").unwrap())
//!     .unwrap();
//! assert_eq!(original, recompressed);
//! ```
//!
//! ## Helpers and inspectors
//!
//! The convenience functions are useful when you want to stay in memory or
//! inspect a stream before converting it:
//!
//! ```no_run
//! use std::io::Cursor;
//! use flacx::{decode_bytes, encode_bytes, inspect_flac_total_samples, inspect_wav_total_samples};
//!
//! let wav_bytes = std::fs::read("input.wav").unwrap();
//! let total_samples = inspect_wav_total_samples(Cursor::new(&wav_bytes)).unwrap();
//! let flac_bytes = encode_bytes(&wav_bytes).unwrap();
//! let flac_total_samples = inspect_flac_total_samples(Cursor::new(&flac_bytes)).unwrap();
//! let wav_round_trip = decode_bytes(&flac_bytes).unwrap();
//!
//! assert_eq!(total_samples, flac_total_samples);
//! assert!(!wav_round_trip.is_empty());
//! ```
//!
//! ## Progress feature
//!
//! When the `progress` feature is enabled, encode and decode operations can
//! report [`ProgressSnapshot`] updates through callbacks.
//!
//! ```no_run
//! # #[cfg(feature = "progress")]
//! # {
//! use flacx::{Decoder, Encoder, EncoderConfig, ProgressSnapshot};
//!
//! let encoder = Encoder::new(EncoderConfig::default());
//! encoder.encode_file_with_progress("input.wav", "output.flac", |progress: ProgressSnapshot| {
//!     println!("{} / {} samples", progress.processed_samples, progress.total_samples);
//!     Ok(())
//! }).unwrap();
//! # }
//! ```
//!
//! ## Supported scope
//!
//! This crate focuses on the current WAV <-> FLAC conversion surface used by
//! the workspace. The crate documentation intentionally stays aligned with the
//! exported API so that docs.rs readers can use it as the canonical reference.

mod config;
mod crc;
mod decode;
mod encoder;
mod error;
mod input;
mod md5;
mod metadata;
mod model;
mod plan;
mod progress;
mod read;
mod recompress;
mod reconstruct;
mod stream_info;
mod wav_output;
mod write;

/// Compression level presets and tuning profiles used by the encoder.
pub mod level;

pub use config::{DecodeBuilder, DecodeConfig, EncoderBuilder, EncoderConfig};
pub use decode::{DecodeSummary, Decoder, decode_bytes, decode_file};
pub use encoder::{EncodeSummary, Encoder, encode_bytes, encode_file};
pub use error::{Error, Result};
pub use recompress::{
    RecompressBuilder, RecompressConfig, Recompressor, recompress_bytes, recompress_file,
};

/// Inspect a WAV stream and return its total sample count without decoding it.
///
/// This helper is useful when you want to report progress or preflight an
/// encode job before you start writing FLAC output.
pub use input::inspect_wav_total_samples;

/// Inspect a FLAC stream and return the total sample count recorded in its
/// STREAMINFO metadata.
///
/// This is the FLAC counterpart to [`inspect_wav_total_samples`].
pub use read::inspect_flac_total_samples;

#[cfg(feature = "progress")]
pub use progress::{DecodeProgress, EncodeProgress, ProgressSnapshot};

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

#[cfg(not(feature = "progress"))]
#[doc = r#"```compile_fail
use flacx::Decoder;

fn main() {
    let decoder = Decoder::default();
    let input = std::io::Cursor::new(Vec::<u8>::new());
    let mut output = std::io::Cursor::new(Vec::<u8>::new());
    let _ = decoder.decode_with_progress(input, &mut output, |_| Ok(()));
}
```"#]
#[doc(hidden)]
pub struct _ProgressDecodeMethodFeatureDisabledDoc;
