//! FLAC-to-FLAC recompression session primitives used by the `flacx` crate.
//!
//! The public recompress flow is reader-driven: parse a [`crate::FlacReader`],
//! inspect its recovered spec/metadata, convert it into a single-pass
//! [`FlacRecompressSource`], bind an output writer through
//! [`RecompressConfig::into_recompressor`], then feed the source into
//! [`Recompressor::recompress`].

mod config;
mod progress;
mod session;
mod source;
mod verify;

pub use config::{RecompressBuilder, RecompressConfig, RecompressMode};
pub use progress::{RecompressPhase, RecompressProgress};
pub use session::{RecompressSummary, Recompressor};
pub use source::FlacRecompressSource;

pub(crate) use progress::RecompressProgressSink;
