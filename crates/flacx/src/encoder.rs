//! PCM-container-to-FLAC encoding session primitives used by the `flacx` crate.
//!
//! The public encode flow can be reader-driven or direct-stream-driven: parse a
//! family reader or construct a concrete stream explicitly, stage metadata in
//! an [`EncodeSource`], bind an output writer through
//! [`EncoderConfig::into_encoder`], then feed that source into
//! [`Encoder::encode_source`].

use std::io::{Seek, Write};

use crate::{
    config::{EncoderBuilder, EncoderConfig},
    encode_pipeline::encode_stream,
    error::Result,
    input::{EncodePcmStream, EncodeSource},
    metadata::Metadata,
    progress::{NoProgress, ProgressSink},
};

#[cfg(feature = "progress")]
use crate::progress::ProgressSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Summary of the FLAC stream produced by an encode operation.
pub struct EncodeSummary {
    pub frame_count: usize,
    pub total_samples: u64,
    pub block_size: u16,
    pub min_frame_size: u32,
    pub max_frame_size: u32,
    pub min_block_size: u16,
    pub max_block_size: u16,
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
}

/// Writer-owning PCM-container-to-FLAC encode session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoder<W> {
    config: EncoderConfig,
    writer: W,
}

impl EncoderConfig {
    /// Bind an output writer and create a writer-owning encode session.
    pub fn into_encoder<W>(self, writer: W) -> Encoder<W>
    where
        W: Write + Seek,
    {
        Encoder::new(writer, self)
    }
}

impl EncoderBuilder {
    /// Finish building the configuration and bind an output writer.
    pub fn into_encoder<W>(self, writer: W) -> Encoder<W>
    where
        W: Write + Seek,
    {
        self.build().into_encoder(writer)
    }
}

impl<W> Encoder<W>
where
    W: Write + Seek,
{
    /// Construct a writer-owning encode session from a writer and config.
    #[must_use]
    pub fn new(writer: W, config: EncoderConfig) -> Self {
        Self { config, writer }
    }

    /// Return a clone of the session configuration.
    #[must_use]
    pub fn config(&self) -> EncoderConfig {
        self.config.clone()
    }

    /// Return a new session with a different compression level preset.
    #[must_use]
    pub fn with_level(mut self, level: crate::level::Level) -> Self {
        self.config = self.config.with_level(level);
        self
    }

    /// Return a new session with a different worker thread count.
    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.config = self.config.with_threads(threads);
        self
    }

    /// Return a new session with a different fixed block size.
    #[must_use]
    pub fn with_block_size(mut self, block_size: u16) -> Self {
        self.config = self.config.with_block_size(block_size);
        self
    }

    /// Return a shared reference to the owned output writer.
    #[must_use]
    pub fn writer(&self) -> &W {
        &self.writer
    }

    /// Return a mutable reference to the owned output writer.
    pub fn writer_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    /// Consume the session and return the owned writer.
    pub fn into_inner(self) -> W {
        self.writer
    }

    /// Encode a single-pass PCM stream into the owned writer.
    pub fn encode<S>(&mut self, stream: S) -> Result<EncodeSummary>
    where
        S: EncodePcmStream,
    {
        let mut progress = NoProgress;
        self.encode_with_sink(stream, &mut progress)
    }

    /// Encode an owned source that keeps metadata and the PCM stream together.
    pub fn encode_source<S>(&mut self, source: EncodeSource<S>) -> Result<EncodeSummary>
    where
        S: EncodePcmStream,
    {
        let mut progress = NoProgress;
        self.encode_source_with_sink(source, &mut progress)
    }

    #[cfg(feature = "progress")]
    /// Encode a single-pass PCM stream while reporting frame-level progress.
    pub fn encode_with_progress<S, F>(
        &mut self,
        stream: S,
        mut on_progress: F,
    ) -> Result<EncodeSummary>
    where
        S: EncodePcmStream,
        F: FnMut(ProgressSnapshot) -> Result<()>,
    {
        let mut progress = crate::progress::CallbackProgress::new(&mut on_progress);
        self.encode_with_sink(stream, &mut progress)
    }

    #[cfg(feature = "progress")]
    /// Encode an owned source while reporting frame-level progress.
    pub fn encode_source_with_progress<S, F>(
        &mut self,
        source: EncodeSource<S>,
        mut on_progress: F,
    ) -> Result<EncodeSummary>
    where
        S: EncodePcmStream,
        F: FnMut(ProgressSnapshot) -> Result<()>,
    {
        let mut progress = crate::progress::CallbackProgress::new(&mut on_progress);
        self.encode_source_with_sink(source, &mut progress)
    }

    pub(crate) fn encode_with_sink<S, P>(
        &mut self,
        stream: S,
        progress: &mut P,
    ) -> Result<EncodeSummary>
    where
        S: EncodePcmStream,
        P: ProgressSink,
    {
        self.encode_source_with_sink(EncodeSource::new(Metadata::default(), stream), progress)
    }

    pub(crate) fn encode_source_with_sink<S, P>(
        &mut self,
        source: EncodeSource<S>,
        progress: &mut P,
    ) -> Result<EncodeSummary>
    where
        S: EncodePcmStream,
        P: ProgressSink,
    {
        let (mut metadata, stream) = source.into_parts();
        crate::metadata::align_metadata_to_stream_spec(&mut metadata, stream.spec(), false)?;
        encode_stream(&self.config, metadata, stream, &mut self.writer, progress)
    }
}
