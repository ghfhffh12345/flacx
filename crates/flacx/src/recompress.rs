//! FLAC-to-FLAC recompression session primitives used by the `flacx` crate.
//!
//! The public recompress flow can be reader-driven or direct-stream-driven:
//! parse a [`crate::FlacReader`] or construct a [`crate::FlacPcmStream`]
//! explicitly, stage metadata in a [`FlacRecompressSource`], bind an output
//! writer through [`RecompressConfig::into_recompressor`], then feed the source
//! into [`Recompressor::recompress`].

mod config;
mod progress;
mod session;
mod source;
mod verify;

pub use config::{RecompressBuilder, RecompressConfig, RecompressMode};
pub use session::{RecompressSummary, Recompressor};
pub use source::FlacRecompressSource;

#[cfg(feature = "progress")]
pub use progress::{RecompressPhase, RecompressProgress};

pub(crate) use progress::RecompressProgressSink;
