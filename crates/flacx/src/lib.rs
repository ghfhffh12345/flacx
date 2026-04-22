//! Convert supported PCM containers to FLAC, decode FLAC back to PCM
//! containers, and recompress existing FLAC streams.
//!
//! `flacx` exposes two complementary ways to work:
//!
//! - the reset API built around staged readers, sources, configs, and
//!   writer-owning sessions
//! - the convenience [`builtin`] helpers for one-shot file and byte workflows
//!
//! Most applications should start with the reset API when they want control
//! over metadata, output configuration, or progress reporting, and use
//! [`builtin`] when a single function call is enough.
//!
//! Advanced callers can also skip the reader façade and directly construct the
//! concrete stream types that feed [`EncodeSource`], [`DecodeSource`], and
//! [`FlacRecompressSource`].
//!
//! ## Getting started
//!
//! Encode a supported PCM container to FLAC:
//!
//! ```no_run
//! use flacx::{EncoderConfig, PcmReader};
//! use std::{
//!     fs::File,
//!     io::{BufReader, BufWriter},
//! };
//!
//! let input = BufReader::new(File::open("input.wav")?);
//! let source = PcmReader::new(input)?.into_source();
//!
//! let output = BufWriter::new(File::create("output.flac")?);
//! let mut encoder = EncoderConfig::default().into_encoder(output);
//! let summary = encoder.encode_source(source)?;
//!
//! println!("encoded {} samples", summary.total_samples);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Or construct a concrete stream directly when you already know the low-level
//! format details:
//!
//! ```no_run
//! use flacx::{EncodeSource, EncoderConfig, Metadata, WavPcmStream};
//! use std::{
//!     fs::File,
//!     io::{BufReader, BufWriter, Seek, SeekFrom},
//! };
//!
//! let mut payload = BufReader::new(File::open("input.wav")?);
//! payload.seek(SeekFrom::Start(44))?; // canonical PCM payload offset
//!
//! let stream = WavPcmStream::builder(payload)
//!     .sample_rate(44_100)
//!     .channels(2)
//!     .valid_bits_per_sample(16)
//!     .total_samples(1_024)
//!     .build()?;
//! let source = EncodeSource::new(Metadata::new(), stream);
//!
//! let output = BufWriter::new(File::create("output.flac")?);
//! let mut encoder = EncoderConfig::default().into_encoder(output);
//! encoder.encode_source(source)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Decode a FLAC stream to a PCM container:
//!
//! ```no_run
//! use flacx::{DecodeConfig, read_flac_reader};
//! use std::{
//!     fs::File,
//!     io::{BufReader, BufWriter},
//! };
//!
//! let input = BufReader::new(File::open("input.flac")?);
//! let source = read_flac_reader(input)?.into_decode_source();
//!
//! let output = BufWriter::new(File::create("output.wav")?);
//! let mut decoder = DecodeConfig::default().into_decoder(output);
//! decoder.decode_source(source)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Recompress an existing FLAC stream:
//!
//! ```no_run
//! use flacx::{RecompressConfig, read_flac_reader};
//! use std::{
//!     fs::File,
//!     io::{BufReader, BufWriter},
//! };
//!
//! let input = BufReader::new(File::open("input.flac")?);
//! let source = read_flac_reader(input)?.into_recompress_source();
//!
//! let output = BufWriter::new(File::create("recompressed.flac")?);
//! let mut recompressor = RecompressConfig::default().into_recompressor(output);
//! recompressor.recompress(source)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! If you prefer a one-shot path, start with [`builtin::encode_file`],
//! [`builtin::decode_file`], or [`builtin::recompress_file`].
//!
//! ## Choosing an API layer
//!
//! | Surface | Use it when | Main entry points |
//! | --- | --- | --- |
//! | Reset API | You want direct control over staged input, direct stream construction, metadata, configs, output containers, or progress callbacks. | [`core`], [`EncoderConfig`], [`DecodeConfig`], [`RecompressConfig`], [`PcmReader`], [`read_flac_reader`], [`WavPcmStream`], [`FlacPcmStream`], [`Encoder`], [`Decoder`], [`Recompressor`] |
//! | Convenience helpers | You want file-path or byte-slice conversions with minimal setup. | [`builtin`] |
//! | Supporting types | You need presets, metadata editing, raw PCM descriptors, or preflight inspection helpers. | [`level`], [`Metadata`], [`RawPcmDescriptor`], [`inspect_pcm_total_samples`], [`inspect_flac_total_samples`] |
//!
//! ## Main building blocks
//!
//! The reset API is organized around a few reusable concepts:
//!
//! - **Reset entry points** parse an input format and hand it off into an
//!   owned source, such as [`PcmReader`] for PCM-container inputs and
//!   [`read_flac_reader`] for FLAC inputs.
//! - **Concrete streams** can be reader-produced or directly constructed when
//!   you already know the payload layout, such as [`WavPcmStream`],
//!   [`AiffPcmStream`], [`CafPcmStream`], [`RawPcmStream`], and
//!   [`FlacPcmStream`].
//! - **Sources** carry parsed metadata and a single-pass PCM stream into the
//!   next stage, such as [`EncodeSource`], [`DecodeSource`], and
//!   [`FlacRecompressSource`].
//! - **Configs and builders** choose output policy and codec tuning, such as
//!   [`EncoderConfig`], [`DecodeConfig`], and [`RecompressConfig`].
//! - **Sessions** own the output writer and perform the actual encode, decode,
//!   or recompress operation through [`Encoder`], [`Decoder`], and
//!   [`Recompressor`].
//!
//! The [`core`] module re-exports the reset API in one place if you
//! prefer a narrower import surface.
//!
//! ## Feature flags
//!
//! `flacx` uses coarse feature flags for container families and optional
//! progress callbacks:
//!
//! - `wav` enables WAV, RF64, and Wave64 support
//! - `aiff` enables AIFF and AIFC support
//! - `caf` enables CAF support
//! - `progress` enables callback-based progress reporting via
//!   [`ProgressSnapshot`], [`EncodeProgress`], [`DecodeProgress`], and
//!   recompress progress helpers with explicit input-read and output-write
//!   counters
//!
//! ## Navigating the docs
//!
//! - Start with [`core`] for the reset API.
//! - Use [`builtin`] for the shortest file/byte workflows.
//! - Visit [`level`] for compression presets and [`PcmContainer`] for decode
//!   output-family selection.
//! - Visit [`Metadata`] and [`RawPcmDescriptor`] when you need to control
//!   preservation or raw PCM ingest.
//!
//! The repository `README.md` gives a short workspace overview, while this
//! rustdoc is the authoritative library reference.
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
        inspect_pcm_total_samples, inspect_raw_pcm_total_samples, recompress_bytes, recompress_file,
    };
}

#[cfg(feature = "aiff")]
pub use aiff::{AiffPcmDescriptor, AiffPcmStream, AiffReader};
#[cfg(feature = "caf")]
pub use caf::{CafPcmStream, CafReader};
pub use config::{DecodeBuilder, DecodeConfig, EncoderBuilder, EncoderConfig};
pub use decode::{DecodeSummary, Decoder};
pub use encoder::{EncodeSummary, Encoder};
pub use error::{Error, Result};
pub use input::{
    EncodePcmStream, EncodeSource, PcmReader, PcmSpec, PcmSpec as PcmStreamSpec, PcmStream,
};
pub use metadata::Metadata;
pub use pcm::PcmContainer;
pub use raw::{
    RawPcmByteOrder, RawPcmDescriptor, RawPcmReader, RawPcmStream, inspect_raw_pcm_total_samples,
};
pub use read::{
    DecodePcmStream, DecodeSource, FlacPcmStream, FlacPcmStreamBuilder, FlacReader,
    FlacReaderOptions, read_flac_reader, read_flac_reader_with_options,
};
pub use recompress::{
    FlacRecompressSource, RecompressBuilder, RecompressConfig, RecompressMode, RecompressSummary,
    Recompressor,
};
pub use stream_info::StreamInfo;
pub use wav_input::{WavPcmStream, WavPcmStreamBuilder, WavReader, WavReaderOptions};

#[cfg(feature = "progress")]
pub use recompress::{RecompressPhase, RecompressProgress};

/// Inspect a supported PCM-container stream and return its total sample count
/// without decoding it.
pub use input::inspect_wav_total_samples as inspect_pcm_total_samples;

/// Inspect a FLAC stream and return the total sample count recorded in its
/// STREAMINFO metadata.
///
/// This is the FLAC counterpart to [`inspect_pcm_total_samples`].
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
        &metadata::Metadata::default(),
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
    pub use crate::{AiffPcmDescriptor, AiffPcmStream, AiffReader};
    #[cfg(feature = "caf")]
    pub use crate::{CafPcmStream, CafReader};
    pub use crate::{
        DecodeBuilder, DecodeConfig, DecodePcmStream, DecodeSource, DecodeSummary, Decoder,
        EncodePcmStream, EncodeSource, EncodeSummary, Encoder, EncoderBuilder, EncoderConfig,
        FlacPcmStream, FlacPcmStreamBuilder, FlacReader, FlacReaderOptions, Metadata, PcmContainer,
        PcmReader, PcmStream, PcmStreamSpec, RawPcmByteOrder, RawPcmDescriptor, RawPcmReader,
        RawPcmStream, RecompressBuilder, RecompressConfig, RecompressMode, Recompressor,
        StreamInfo, inspect_pcm_total_samples, inspect_raw_pcm_total_samples, read_flac_reader,
        read_flac_reader_with_options, write_pcm_stream,
    };
    pub use crate::{WavPcmStream, WavPcmStreamBuilder, WavReader, WavReaderOptions};

    #[cfg(feature = "progress")]
    pub use crate::{RecompressPhase, RecompressProgress};

    #[cfg(feature = "progress")]
    pub use crate::{DecodeProgress, EncodeProgress, ProgressSnapshot};
}

#[cfg(feature = "progress")]
pub use progress::{DecodeProgress, EncodeProgress, ProgressSnapshot};

#[doc(hidden)]
pub fn __set_encode_profile_path_for_current_thread(path: Option<std::path::PathBuf>) {
    encode_pipeline::set_encode_profile_path_for_current_thread(path);
}

#[doc(hidden)]
pub fn __set_decode_profile_path_for_current_thread(path: Option<std::path::PathBuf>) {
    read::set_decode_profile_path_for_current_thread(path);
}

#[cfg(not(feature = "progress"))]
#[doc = r#"```compile_fail
use flacx::EncodeProgress;

fn main() {}
```"#]
#[doc(hidden)]
pub struct _ProgressTypeFeatureDisabledDoc;

#[cfg(not(feature = "progress"))]
#[doc = r#"```compile_fail
use flacx::{EncoderConfig, PcmReader};

fn main() {
    let input = std::io::Cursor::new(Vec::<u8>::new());
    let reader = PcmReader::new(input).unwrap();
    let source = reader.into_source();
    let output = std::io::Cursor::new(Vec::<u8>::new());
    let mut encoder = EncoderConfig::default().into_encoder(output);
    let _ = encoder.encode_source_with_progress(source, |_| Ok(()));
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
    let source = reader.into_decode_source();
    let output = std::io::Cursor::new(Vec::<u8>::new());
    let mut decoder = DecodeConfig::default().into_decoder(output);
    let _ = decoder.decode_source_with_progress(source, |_| Ok(()));
}
```"#]
#[doc(hidden)]
pub struct _ProgressDecodeMethodFeatureDisabledDoc;
