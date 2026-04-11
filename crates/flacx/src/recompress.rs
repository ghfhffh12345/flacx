//! FLAC-to-FLAC recompression session primitives used by the `flacx` crate.
//!
//! The public recompress flow is reader-driven: parse a [`crate::FlacReader`],
//! inspect its recovered spec/metadata, convert it into a single-pass
//! [`FlacRecompressSource`], bind an output writer through
//! [`RecompressConfig::into_recompressor`], then feed the source into
//! [`Recompressor::recompress`].

use std::io::{Read, Seek, Write};

use crate::{
    config::EncoderConfig,
    encoder::{EncodeSummary, Encoder},
    error::Result,
    input::{EncodePcmStream, WavSpec},
    level::Level,
    md5::{StreaminfoMd5, verify_streaminfo_digest},
    metadata::EncodeMetadata,
    plan::EncodePlan,
    progress::{NoProgress, ProgressSink, ProgressSnapshot},
    read::{FlacPcmStream, FlacReader},
};

/// Mode presets for recompress-side metadata handling and relaxable validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecompressMode {
    /// Ignore recompress-side metadata preservation chunks and relaxable
    /// validations when possible.
    Loose,
    /// Preserve current metadata behavior while keeping relaxable decode checks
    /// disabled.
    #[default]
    Default,
    /// Preserve current metadata behavior and enable the strict decode checks.
    Strict,
}

/// User-facing recompression configuration for FLAC-to-FLAC conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecompressConfig {
    mode: RecompressMode,
    level: Level,
    threads: usize,
    block_size: Option<u16>,
}

impl Default for RecompressConfig {
    fn default() -> Self {
        Self {
            mode: RecompressMode::Default,
            level: Level::Level8,
            threads: std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1),
            block_size: None,
        }
    }
}

impl RecompressConfig {
    /// Create a fluent builder for [`RecompressConfig`].
    #[must_use]
    pub fn builder() -> RecompressBuilder {
        RecompressBuilder::default()
    }

    /// Return the recompress policy mode.
    #[must_use]
    pub fn mode(self) -> RecompressMode {
        self.mode
    }

    /// Return the output compression level preset.
    #[must_use]
    pub fn level(self) -> Level {
        self.level
    }

    /// Return the worker-thread count shared by both decode and encode phases.
    #[must_use]
    pub fn threads(self) -> usize {
        self.threads
    }

    /// Return the optional fixed FLAC block size override.
    #[must_use]
    pub fn block_size(self) -> Option<u16> {
        self.block_size
    }

    /// Set the recompress policy mode.
    #[must_use]
    pub fn with_mode(mut self, mode: RecompressMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set the output compression level preset.
    #[must_use]
    pub fn with_level(mut self, level: Level) -> Self {
        self.level = level;
        self.block_size = None;
        self
    }

    /// Set the worker-thread count used by both recompress phases.
    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads.max(1);
        self
    }

    /// Set a fixed FLAC block-size override for the encode phase.
    #[must_use]
    pub fn with_block_size(mut self, block_size: u16) -> Self {
        self.block_size = Some(block_size);
        self
    }

    /// Bind an output writer and create a writer-owning recompress session.
    pub fn into_recompressor<W>(self, writer: W) -> Recompressor<W>
    where
        W: Write + Seek,
    {
        Recompressor::new(writer, self)
    }

    pub(crate) fn flac_reader_options(self) -> crate::FlacReaderOptions {
        crate::FlacReaderOptions {
            strict_seektable_validation: self.decode_config().strict_seektable_validation,
            strict_channel_mask_provenance: self.decode_config().strict_channel_mask_provenance,
        }
    }

    fn decode_config(self) -> crate::DecodeConfig {
        let base = crate::DecodeConfig::default().with_threads(self.threads);
        match self.mode {
            RecompressMode::Loose => base
                .with_emit_fxmd(false)
                .with_strict_channel_mask_provenance(false)
                .with_strict_seektable_validation(false),
            RecompressMode::Default => base
                .with_emit_fxmd(true)
                .with_strict_channel_mask_provenance(false)
                .with_strict_seektable_validation(false),
            RecompressMode::Strict => base
                .with_emit_fxmd(true)
                .with_strict_channel_mask_provenance(true)
                .with_strict_seektable_validation(true),
        }
    }

    fn encode_config(self) -> EncoderConfig {
        let mut base = EncoderConfig::default()
            .with_level(self.level)
            .with_threads(self.threads);
        if let Some(block_size) = self.block_size {
            base = base.with_block_size(block_size);
        }
        match self.mode {
            RecompressMode::Loose => base
                .with_capture_fxmd(false)
                .with_strict_fxmd_validation(false),
            RecompressMode::Default | RecompressMode::Strict => base
                .with_capture_fxmd(true)
                .with_strict_fxmd_validation(true),
        }
    }
}

/// Fluent builder for [`RecompressConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecompressBuilder {
    config: RecompressConfig,
}

impl RecompressBuilder {
    /// Create a new builder starting from [`RecompressConfig::default`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the recompress policy mode.
    #[must_use]
    pub fn mode(mut self, mode: RecompressMode) -> Self {
        self.config = self.config.with_mode(mode);
        self
    }

    /// Set the output compression level preset.
    #[must_use]
    pub fn level(mut self, level: Level) -> Self {
        self.config = self.config.with_level(level);
        self
    }

    /// Set the worker-thread count used by both recompress phases.
    #[must_use]
    pub fn threads(mut self, threads: usize) -> Self {
        self.config = self.config.with_threads(threads);
        self
    }

    /// Set a fixed FLAC block-size override for the encode phase.
    #[must_use]
    pub fn block_size(mut self, block_size: u16) -> Self {
        self.config = self.config.with_block_size(block_size);
        self
    }

    /// Finish building the configuration.
    #[must_use]
    pub fn build(self) -> RecompressConfig {
        self.config
    }

    /// Finish building the configuration and bind an output writer.
    pub fn into_recompressor<W>(self, writer: W) -> Recompressor<W>
    where
        W: Write + Seek,
    {
        self.build().into_recompressor(writer)
    }
}

/// Phase marker for recompress progress reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecompressPhase {
    Decode,
    Encode,
}

#[cfg_attr(not(feature = "progress"), allow(dead_code))]
impl RecompressPhase {
    /// Return the user-facing phase label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Decode => "Decode",
            Self::Encode => "Encode",
        }
    }
}

/// A phase-aware recompress progress snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecompressProgress {
    /// The active recompress phase.
    pub phase: RecompressPhase,
    /// Samples processed so far within the active phase.
    pub phase_processed_samples: u64,
    /// Total samples expected within the active phase.
    pub phase_total_samples: u64,
    /// Samples processed so far across the full decode+encode operation.
    pub overall_processed_samples: u64,
    /// Total samples expected across the full decode+encode operation.
    pub overall_total_samples: u64,
    /// Frames completed so far within the active phase.
    pub completed_frames: usize,
    /// Total frames expected within the active phase when known.
    pub total_frames: usize,
}

/// Summary of the FLAC stream produced by a recompress operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecompressSummary {
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

impl From<EncodeSummary> for RecompressSummary {
    fn from(value: EncodeSummary) -> Self {
        Self {
            frame_count: value.frame_count,
            total_samples: value.total_samples,
            block_size: value.block_size,
            min_frame_size: value.min_frame_size,
            max_frame_size: value.max_frame_size,
            min_block_size: value.min_block_size,
            max_block_size: value.max_block_size,
            sample_rate: value.sample_rate,
            channels: value.channels,
            bits_per_sample: value.bits_per_sample,
        }
    }
}

/// Reader-to-session handoff for explicit FLAC recompression.
pub struct FlacRecompressSource<R> {
    metadata: EncodeMetadata,
    total_samples: u64,
    stream: VerifyingPcmStream<R>,
}

impl<R: Read + Seek> FlacRecompressSource<R> {
    /// Convert an inspected [`FlacReader`] into the single-pass recompress source.
    #[must_use]
    pub fn from_reader(reader: FlacReader<R>) -> Self {
        let metadata = reader.metadata().clone().into_encode_metadata();
        let total_samples = reader.spec().total_samples;
        let stream_info = reader.stream_info();
        let stream = VerifyingPcmStream::new(reader.into_pcm_stream(), stream_info.md5);
        Self {
            metadata,
            total_samples,
            stream,
        }
    }

    /// Return the PCM spec that will be fed into the recompress session.
    #[must_use]
    pub fn spec(&self) -> WavSpec {
        self.stream.spec()
    }

    /// Return the staged encode metadata that will be preserved on recompress.
    #[must_use]
    pub fn metadata(&self) -> &EncodeMetadata {
        &self.metadata
    }

    /// Replace the staged metadata before recompression begins.
    pub fn set_metadata(&mut self, metadata: EncodeMetadata) {
        self.metadata = metadata;
    }

    /// Return a new source with different staged metadata.
    #[must_use]
    pub fn with_metadata(mut self, metadata: EncodeMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Return the total sample count recorded on the input FLAC stream.
    #[must_use]
    pub fn total_samples(&self) -> u64 {
        self.total_samples
    }
}

impl<R: Read + Seek> EncodePcmStream for FlacRecompressSource<R> {
    fn spec(&self) -> WavSpec {
        self.stream.spec()
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize> {
        self.stream.read_chunk(max_frames, output)
    }
}

/// Writer-owning FLAC-to-FLAC recompress session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recompressor<W> {
    config: RecompressConfig,
    writer: W,
}

impl<W> Recompressor<W>
where
    W: Write + Seek,
{
    /// Construct a writer-owning recompress session from a writer and config.
    #[must_use]
    pub fn new(writer: W, config: RecompressConfig) -> Self {
        Self { config, writer }
    }

    /// Return the configuration currently stored in the recompress session.
    #[must_use]
    pub fn config(&self) -> RecompressConfig {
        self.config
    }

    /// Return a new recompressor with a different recompress mode.
    #[must_use]
    pub fn with_mode(mut self, mode: RecompressMode) -> Self {
        self.config = self.config.with_mode(mode);
        self
    }

    /// Return a new recompressor with a different output compression level.
    #[must_use]
    pub fn with_level(mut self, level: Level) -> Self {
        self.config = self.config.with_level(level);
        self
    }

    /// Return a new recompressor with a different shared worker-thread count.
    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.config = self.config.with_threads(threads);
        self
    }

    /// Return a new recompressor with a fixed block-size override.
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

    /// Recompress a single-pass FLAC source into the owned writer.
    pub fn recompress<R>(&mut self, source: FlacRecompressSource<R>) -> Result<RecompressSummary>
    where
        R: Read + Seek,
    {
        let mut progress = NoProgress;
        self.recompress_with_sink(source, &mut progress)
    }

    #[cfg(feature = "progress")]
    /// Recompress a single-pass FLAC source while reporting phase-aware progress.
    pub fn recompress_with_progress<R, F>(
        &mut self,
        source: FlacRecompressSource<R>,
        mut on_progress: F,
    ) -> Result<RecompressSummary>
    where
        R: Read + Seek,
        F: FnMut(RecompressProgress) -> Result<()>,
    {
        self.recompress_with_sink(source, &mut on_progress)
    }

    pub(crate) fn recompress_with_sink<R, P>(
        &mut self,
        source: FlacRecompressSource<R>,
        progress: &mut P,
    ) -> Result<RecompressSummary>
    where
        R: Read + Seek,
        P: RecompressProgressSink,
    {
        let total_samples = source.total_samples();
        progress.on_progress(RecompressProgress {
            phase: RecompressPhase::Decode,
            phase_processed_samples: 0,
            phase_total_samples: total_samples,
            overall_processed_samples: 0,
            overall_total_samples: overall_total_samples(total_samples),
            completed_frames: 0,
            total_frames: 0,
        })?;

        let encode_config = self.config.encode_config();
        let encode_plan = EncodePlan::new(source.spec(), encode_config.clone())?;
        progress.on_progress(RecompressProgress {
            phase: RecompressPhase::Encode,
            phase_processed_samples: 0,
            phase_total_samples: total_samples,
            overall_processed_samples: total_samples,
            overall_total_samples: overall_total_samples(total_samples),
            completed_frames: 0,
            total_frames: encode_plan.total_frames,
        })?;

        let metadata = source.metadata().clone();
        let mut encode_progress = EncodePhaseProgress {
            sink: progress,
            total_samples,
        };
        let mut encoder: Encoder<&mut W> = encode_config.into_encoder(&mut self.writer);
        encoder.set_metadata(metadata);
        let summary = encoder.encode_with_sink(source, &mut encode_progress)?;
        Ok(summary.into())
    }
}

pub(crate) trait RecompressProgressSink {
    fn on_progress(&mut self, progress: RecompressProgress) -> Result<()>;
}

impl RecompressProgressSink for crate::progress::NoProgress {
    fn on_progress(&mut self, _progress: RecompressProgress) -> Result<()> {
        Ok(())
    }
}

#[cfg(feature = "progress")]
impl<F> RecompressProgressSink for F
where
    F: FnMut(RecompressProgress) -> Result<()>,
{
    fn on_progress(&mut self, progress: RecompressProgress) -> Result<()> {
        self(progress)
    }
}

struct VerifyingPcmStream<R> {
    inner: FlacPcmStream<R>,
    expected_md5: [u8; 16],
    md5: Option<StreaminfoMd5>,
    verified: bool,
}

impl<R> VerifyingPcmStream<R> {
    fn new(inner: FlacPcmStream<R>, expected_md5: [u8; 16]) -> Self {
        Self {
            md5: Some(StreaminfoMd5::new(inner.spec())),
            expected_md5,
            inner,
            verified: false,
        }
    }

    fn spec(&self) -> WavSpec {
        self.inner.spec()
    }
}

impl<R: Read + Seek> EncodePcmStream for VerifyingPcmStream<R> {
    fn spec(&self) -> WavSpec {
        self.spec()
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize> {
        let mut chunk = Vec::new();
        let frames = self.inner.read_chunk(max_frames, &mut chunk)?;
        if frames == 0 {
            if !self.verified {
                verify_streaminfo_digest(
                    self.md5.take().expect("md5 state present").finalize()?,
                    self.expected_md5,
                )?;
                self.verified = true;
            }
            return Ok(0);
        }
        self.md5
            .as_mut()
            .expect("md5 state present")
            .update_samples(&chunk)?;
        output.extend(chunk);
        Ok(frames)
    }
}

struct EncodePhaseProgress<'a, P> {
    sink: &'a mut P,
    total_samples: u64,
}

impl<P> ProgressSink for EncodePhaseProgress<'_, P>
where
    P: RecompressProgressSink,
{
    fn on_frame(&mut self, progress: ProgressSnapshot) -> Result<()> {
        self.sink.on_progress(RecompressProgress {
            phase: RecompressPhase::Encode,
            phase_processed_samples: progress.processed_samples,
            phase_total_samples: progress.total_samples,
            overall_processed_samples: self
                .total_samples
                .saturating_add(progress.processed_samples),
            overall_total_samples: overall_total_samples(self.total_samples),
            completed_frames: progress.completed_frames,
            total_frames: progress.total_frames,
        })
    }
}

const fn overall_total_samples(total_samples: u64) -> u64 {
    total_samples.saturating_mul(2)
}
