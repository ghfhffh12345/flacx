//! High-performance PCM-container/FLAC conversion and recompression for Rust.
//!
//! `flacx` is the reusable library crate in this workspace. The public surface
//! is intentionally layered so callers — and maintainers reading the exported
//! API — can distinguish the explicit codec pipeline from the thin convenience
//! wrappers built on top of it.
//!
//! This crate-level documentation is architecture-first. It is meant to answer
//! “what does the public API expose, and how is it organized?” rather than to
//! serve as a beginner tutorial.
//!
//! ## Public API interface map
//!
//! ```text
//! flacx
//! ├─ core
//! │  ├─ config/builders
//! │  │  ├─ EncoderConfig / EncoderBuilder
//! │  │  ├─ DecodeConfig / DecodeBuilder
//! │  │  └─ RecompressConfig / RecompressBuilder
//! │  ├─ streaming façades
//! │  │  ├─ Encoder / EncodeSummary
//! │  │  ├─ Decoder / DecodeSummary
//! │  │  └─ Recompressor / RecompressProgress / RecompressPhase
//! │  ├─ typed PCM boundary
//! │  │  ├─ PcmStream / PcmStreamSpec / PcmContainer
//! │  │  ├─ read_pcm_stream / write_pcm_stream
//! │  │  └─ inspect_pcm_total_samples / inspect_raw_pcm_total_samples
//! │  └─ support surfaces
//! │     ├─ RawPcmDescriptor / RawPcmByteOrder
//! │     └─ level
//! ├─ inspectors
//! │  ├─ inspect_wav_total_samples
//! │  └─ inspect_flac_total_samples
//! ├─ convenience
//! │  ├─ encode_file / encode_bytes
//! │  ├─ decode_file / decode_bytes
//! │  ├─ recompress_file / recompress_bytes
//! │  └─ re-exported inspection helpers
//! └─ progress (feature = "progress")
//!    ├─ ProgressSnapshot
//!    ├─ EncodeProgress / DecodeProgress
//!    └─ progress-enabled encode/decode/recompress methods
//! ```
//!
//! ## Layer contract
//!
//! | Layer | Public surface | Responsibility |
//! | --- | --- | --- |
//! | Explicit core | [`core`], [`Encoder`], [`Decoder`], [`Recompressor`], config/builders, typed PCM helpers | Owns codec configuration, typed PCM handoff, summary reporting, and explicit encode/decode/recompress entry points. |
//! | Convenience/orchestration | [`convenience`], top-level `*_file` / `*_bytes` helpers | Owns one-shot file/byte routing and extension-driven ergonomics without becoming a second policy engine. |
//! | Support surfaces | [`level`], raw PCM helpers, inspector helpers, optional progress types | Exposes stable supporting concepts that sit beside the core pipeline. |
//!
//! ## Source structure snapshot
//!
//! The current crate layout is intentionally readable from the public surface
//! inward:
//!
//! ```text
//! crates/flacx/src/
//! ├─ lib.rs                 # public re-exports and crate contract
//! ├─ config.rs              # encode/decode config + builders
//! ├─ convenience.rs         # one-shot file/byte orchestration helpers
//! ├─ encoder.rs             # encode façade
//! ├─ decode.rs              # decode façade
//! ├─ recompress.rs          # subordinate FLAC→FLAC façade
//! ├─ pcm.rs                 # typed PCM boundary (`PcmStream`, `PcmSpec`, `PcmContainer`)
//! ├─ input.rs               # container dispatch for PCM ingest
//! ├─ wav_input.rs           # WAV/RF64/Wave64 reader family
//! ├─ wav_output.rs          # WAV-family writer family
//! ├─ decode_output.rs       # decode-side temp output + commit helpers
//! ├─ encode_pipeline.rs     # encode planning helpers
//! ├─ metadata.rs            # public metadata-facing helpers
//! ├─ metadata/
//! │  ├─ blocks.rs           # FLAC metadata block model
//! │  └─ draft.rs            # metadata drafting/translation helpers
//! ├─ read/
//! │  ├─ mod.rs              # FLAC read orchestration
//! │  ├─ frame.rs            # FLAC frame parsing/decoding
//! │  └─ metadata.rs         # FLAC metadata parsing + inspection
//! ├─ write/
//! │  ├─ mod.rs              # FLAC write orchestration
//! │  └─ frame.rs            # frame/subframe serialization
//! └─ progress.rs            # optional callback-oriented progress reporting
//! ```
//!
//! ## Feature-gated surface
//!
//! Format families are coarse feature gates:
//!
//! - `wav` => RIFF/WAVE, RF64, Wave64
//! - `aiff` => AIFF, AIFC
//! - `caf` => CAF
//! - `progress` => callback-friendly progress reporting
//!
//! ## Reading guide
//!
//! - Start with [`core`] when you want the explicit architecture story.
//! - Use [`convenience`] when you specifically want the one-shot orchestration
//!   wrappers.
//! - Use [`level`] for compression presets and [`RawPcmDescriptor`] when PCM
//!   ingest must be described explicitly instead of inferred from a container.
//! - In the repository, `crates/flacx/README.md` mirrors this crate contract,
//!   and `docs/flacx-public-api-architecture.md` expands the public API into a
//!   maintainer-oriented architecture guide with structural artifacts.
//!
//! ## Scope of this rustdoc
//!
//! The crate docs intentionally stop at the architectural layer map and the
//! exported interface. They do not attempt to narrate the full internal
//! execution path of encode, decode, or recompress operations.
//!
#[cfg(feature = "aiff")]
mod aiff;
#[cfg(feature = "aiff")]
mod aiff_output;
#[cfg(feature = "caf")]
mod caf;
#[cfg(feature = "caf")]
mod caf_output;
mod config;
pub mod convenience;
mod crc;
mod decode;
mod decode_output;
mod encode_pipeline;
mod encoder;
mod error;
mod input;
mod md5;
mod metadata;
mod model;
mod pcm;
mod plan;
mod progress;
mod raw;
mod read;
mod recompress;
mod reconstruct;
mod stream_info;
mod wav_input;
mod wav_output;
mod write;

/// Compression level presets and tuning profiles used by the encoder.
pub mod level;

pub use config::{DecodeBuilder, DecodeConfig, EncoderBuilder, EncoderConfig};
pub use convenience::{decode_bytes, decode_file, encode_bytes, encode_file};
pub use decode::{DecodeSummary, Decoder};
pub use encoder::{EncodeSummary, Encoder};
pub use error::{Error, Result};
pub use input::{
    PcmSpec, PcmSpec as PcmStreamSpec, PcmStream,
    inspect_wav_total_samples as inspect_pcm_total_samples, read_wav as read_pcm_stream,
};
pub use pcm::PcmContainer;
pub use raw::{RawPcmByteOrder, RawPcmDescriptor, inspect_raw_pcm_total_samples};
pub use recompress::{
    RecompressBuilder, RecompressConfig, RecompressMode, RecompressPhase, RecompressProgress,
    Recompressor, recompress_bytes, recompress_file,
};

/// Inspect a supported PCM-container stream and return its total sample count without decoding it.
///
/// This WAV-named helper remains the stable public inspection API for encode
/// preflight and currently accepts RIFF/WAVE, RF64, Wave64, AIFF, the Stage 2
/// AIFC allowlist, and the Stage 3 CAF allowlist.
pub use input::inspect_wav_total_samples;

/// Inspect a FLAC stream and return the total sample count recorded in its
/// STREAMINFO metadata.
///
/// This is the FLAC counterpart to [`inspect_wav_total_samples`].
pub use read::inspect_flac_total_samples;

/// Write a typed [`PcmStream`] out to a supported PCM-container family without
/// invoking convenience-layer file routing or extension inference.
pub fn write_pcm_stream<W: std::io::Write>(
    writer: &mut W,
    stream: &PcmStream,
    container: PcmContainer,
) -> Result<()> {
    wav_output::write_wav_with_metadata_and_md5_with_options(
        writer,
        stream.spec,
        &stream.samples,
        &metadata::WavMetadata::default(),
        wav_output::WavMetadataWriteOptions {
            emit_fxmd: false,
            container,
        },
    )?;
    Ok(())
}

/// Explicit core surface for callers that want the typed/configured pipeline
/// without the one-shot convenience wrappers.
pub mod core {
    pub use crate::{
        DecodeBuilder, DecodeConfig, DecodeSummary, Decoder, EncodeSummary, Encoder,
        EncoderBuilder, EncoderConfig, PcmContainer, PcmStream, PcmStreamSpec, RawPcmByteOrder,
        RawPcmDescriptor, RecompressBuilder, RecompressConfig, RecompressMode, RecompressPhase,
        RecompressProgress, Recompressor, inspect_pcm_total_samples, inspect_raw_pcm_total_samples,
        read_pcm_stream, write_pcm_stream,
    };

    #[cfg(feature = "progress")]
    pub use crate::{DecodeProgress, EncodeProgress, ProgressSnapshot};
}

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
