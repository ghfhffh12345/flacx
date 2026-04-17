//! High-performance PCM-container/FLAC conversion and recompression for Rust.
//!
//! `flacx` is the reusable library crate in this workspace. The public surface
//! is intentionally layered so callers — and maintainers reading the exported
//! API — can distinguish the explicit codec pipeline from the thin built-in
//! wrappers layered on top of it.
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
//! │  ├─ reader / session façades
//! │  │  ├─ Encoder / EncodeSummary
//! │  │  ├─ FlacReader / DecodePcmStream / Decoder / DecodeSummary
//! │  │  └─ FlacRecompressSource / Recompressor / RecompressSummary / RecompressProgress / RecompressPhase
//! │  ├─ typed PCM boundary
//! │  │  ├─ PcmReader / AnyPcmStream / PcmStream / PcmStreamSpec / PcmContainer
//! │  │  ├─ read_pcm_reader / write_pcm_stream
//! │  │  └─ inspect_pcm_total_samples / inspect_raw_pcm_total_samples
//! │  └─ support surfaces
//! │     ├─ EncodeMetadata / DecodeMetadata / RawPcmDescriptor / RawPcmByteOrder
//! │     └─ level
//! ├─ inspectors
//! │  ├─ inspect_wav_total_samples
//! │  └─ inspect_flac_total_samples
//! ├─ builtin
//! │  ├─ builtin::encode_file / builtin::encode_bytes
//! │  ├─ builtin::decode_file / builtin::decode_bytes
//! │  ├─ builtin::recompress_file / builtin::recompress_bytes
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
//! | Explicit core | [`core`], [`Encoder`], [`FlacReader`], [`Decoder`], [`FlacRecompressSource`], [`Recompressor`], config/builders, reader/session helpers | Owns codec configuration, reader-driven handoff, summary reporting, and explicit encode/decode/recompress entry points. |
//! | Builtin/orchestration | [`builtin`] | Owns one-shot file/byte routing and extension-driven ergonomics without becoming a second policy engine. |
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
//! ├─ convenience.rs         # implementation backing the public `builtin` module
//! ├─ encoder.rs             # encode façade
//! ├─ decode.rs              # decode façade
//! ├─ recompress.rs          # public recompress surface + exports
//! │  ├─ config.rs           # recompress policy + builder
//! │  ├─ source.rs           # reader-to-session handoff
//! │  ├─ session.rs          # writer-owning recompress execution
//! │  ├─ progress.rs         # recompress progress types/adapters
//! │  └─ verify.rs           # recompress MD5 verification glue
//! ├─ pcm.rs                 # typed PCM values shared with decode/write-side APIs
//! ├─ input.rs               # encode-side reader/stream contracts + dispatch
//! ├─ wav_input.rs           # WAV/RF64/Wave64 reader family
//! ├─ wav_output.rs          # WAV-family writer family
//! ├─ decode_output.rs       # decode-side temp output + commit helpers
//! ├─ encode_pipeline.rs     # encode planning helpers
//! ├─ metadata.rs            # public metadata-facing helpers
//! ├─ metadata/
//! │  ├─ blocks.rs           # FLAC metadata block model
//! │  └─ draft.rs            # metadata drafting/translation helpers
//! ├─ read.rs                # FLAC read orchestration
//! │  ├─ frame.rs            # FLAC frame parsing/decoding
//! │  └─ metadata.rs         # FLAC metadata parsing + inspection
//! ├─ write.rs               # FLAC write orchestration
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
//! - Use [`builtin`] when you specifically want the one-shot orchestration
//!   wrappers.
//! - Use [`level`] for compression presets, [`RawPcmDescriptor`] for explicit
//!   raw PCM ingest, and [`FlacReader`] for explicit decode-side control.
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
mod convenience;
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

/// Built-in one-shot orchestration helpers layered on top of the explicit core.
pub mod builtin {
    pub use crate::convenience::{
        decode_bytes, decode_file, encode_bytes, encode_file, inspect_flac_total_samples,
        inspect_pcm_total_samples, inspect_raw_pcm_total_samples, inspect_wav_total_samples,
        recompress_bytes, recompress_file,
    };
}

#[cfg(feature = "aiff")]
pub use aiff::{AiffPcmStream, AiffReader};
#[cfg(feature = "caf")]
pub use caf::{CafPcmStream, CafReader};
pub use config::{DecodeBuilder, DecodeConfig, EncoderBuilder, EncoderConfig};
pub use decode::{DecodeSummary, Decoder};
pub use encoder::{EncodeSummary, Encoder};
pub use error::{Error, Result};
pub use input::{
    AnyPcmStream, EncodePcmStream, PcmReader, PcmReaderOptions, PcmSpec, PcmSpec as PcmStreamSpec,
    PcmStream, inspect_wav_total_samples as inspect_pcm_total_samples, read_pcm_reader,
    read_pcm_reader_with_options,
};
pub use metadata::{DecodeMetadata, EncodeMetadata, WavMetadata};
pub use pcm::PcmContainer;
pub use raw::{
    RawPcmByteOrder, RawPcmDescriptor, RawPcmReader, RawPcmStream, inspect_raw_pcm_total_samples,
};
pub use read::{
    DecodePcmStream, FlacReader, FlacReaderOptions, read_flac_reader, read_flac_reader_with_options,
};
pub use recompress::{
    FlacRecompressSource, RecompressBuilder, RecompressConfig, RecompressMode, RecompressPhase,
    RecompressProgress, RecompressSummary, Recompressor,
};
pub use wav_input::{WavPcmStream, WavReader, WavReaderOptions};

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
    #[cfg(feature = "aiff")]
    pub use crate::{AiffPcmStream, AiffReader};
    #[cfg(feature = "caf")]
    pub use crate::{CafPcmStream, CafReader};
    pub use crate::{
        DecodeBuilder, DecodeConfig, DecodeMetadata, DecodePcmStream, DecodeSummary, Decoder,
        EncodeMetadata, EncodePcmStream, EncodeSummary, Encoder, EncoderBuilder, EncoderConfig,
        FlacReader, FlacReaderOptions, PcmContainer, PcmReader, PcmReaderOptions, PcmStream,
        PcmStreamSpec, RawPcmByteOrder, RawPcmDescriptor, RawPcmReader, RawPcmStream,
        RecompressBuilder, RecompressConfig, RecompressMode, RecompressPhase, RecompressProgress,
        Recompressor, inspect_pcm_total_samples, inspect_raw_pcm_total_samples, read_flac_reader,
        read_flac_reader_with_options, read_pcm_reader, read_pcm_reader_with_options,
        write_pcm_stream,
    };
    pub use crate::{WavPcmStream, WavReader, WavReaderOptions};

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
use flacx::{EncoderConfig, read_pcm_reader};

fn main() {
    let input = std::io::Cursor::new(Vec::<u8>::new());
    let reader = read_pcm_reader(input).unwrap();
    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let output = std::io::Cursor::new(Vec::<u8>::new());
    let mut encoder = EncoderConfig::default().into_encoder(output);
    encoder.set_metadata(metadata);
    let _ = encoder.encode_with_progress(stream, |_| Ok(()));
}
```"#]
#[doc(hidden)]
pub struct _ProgressMethodFeatureDisabledDoc;

#[cfg(not(feature = "progress"))]
#[doc = r#"```compile_fail
use flacx::{DecodeConfig, read_flac_reader};

fn main() {
    let input = std::io::Cursor::new(Vec::<u8>::new());
    let reader = read_flac_reader(input).unwrap();
    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let output = std::io::Cursor::new(Vec::<u8>::new());
    let mut decoder = DecodeConfig::default().into_decoder(output);
    decoder.set_metadata(metadata);
    let _ = decoder.decode_with_progress(stream, |_| Ok(()));
}
```"#]
#[doc(hidden)]
pub struct _ProgressDecodeMethodFeatureDisabledDoc;
