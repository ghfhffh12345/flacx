//! FLAC-to-FLAC recompression primitives used by the `flacx` crate.
//!
//! The main façade is [`Recompressor`]. Pair it with [`RecompressConfig`] or
//! [`Recompressor::builder`] to choose the recompress policy, thread count,
//! compression level, and optional block sizing used when transforming an
//! existing FLAC stream into a new FLAC stream.

use std::{
    fs::File,
    io::{Cursor, Read, Seek, SeekFrom, Write},
    path::Path,
};

use crate::{
    decode_output::{commit_temp_output, open_temp_output},
    encoder::EncodeSummary,
    error::Result,
    input::SlicePcmStream,
    level::Level,
    md5::verify_streaminfo_md5,
    plan::EncodePlan,
    progress::{ProgressSink, ProgressSnapshot},
    read::{inspect_flac_total_samples, read_flac_for_decode},
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
///
/// The configuration is intentionally recompress-specific rather than exposing
/// the full nested encode/decode config surfaces directly.
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
    ///
    /// This resets any explicit block-size override so the selected level once
    /// again controls the default block size.
    #[must_use]
    pub fn with_level(mut self, level: Level) -> Self {
        self.level = level;
        self.block_size = None;
        self
    }

    /// Set the worker-thread count used by both recompress phases.
    ///
    /// Values are clamped to at least `1`.
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

    fn encode_config(self) -> crate::EncoderConfig {
        let mut base = crate::EncoderConfig::default()
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

/// Primary library façade for FLAC-to-FLAC recompression.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Recompressor {
    config: RecompressConfig,
}

impl Recompressor {
    /// Create a builder initialized from [`RecompressConfig::builder`].
    #[must_use]
    pub fn builder() -> RecompressBuilder {
        RecompressConfig::builder()
    }

    /// Construct a recompressor from a configuration value.
    #[must_use]
    pub fn new(config: RecompressConfig) -> Self {
        Self { config }
    }

    /// Return the configuration currently stored in the recompressor.
    #[must_use]
    pub fn config(&self) -> RecompressConfig {
        self.config
    }

    /// Return a new recompressor with a different recompress mode.
    #[must_use]
    pub fn with_mode(self, mode: RecompressMode) -> Self {
        Self::new(self.config.with_mode(mode))
    }

    /// Return a new recompressor with a different output compression level.
    #[must_use]
    pub fn with_level(self, level: Level) -> Self {
        Self::new(self.config.with_level(level))
    }

    /// Return a new recompressor with a different shared worker-thread count.
    #[must_use]
    pub fn with_threads(self, threads: usize) -> Self {
        Self::new(self.config.with_threads(threads))
    }

    /// Return a new recompressor with a fixed block-size override.
    #[must_use]
    pub fn with_block_size(self, block_size: u16) -> Self {
        Self::new(self.config.with_block_size(block_size))
    }

    /// Recompress a FLAC reader into FLAC output.
    pub fn recompress<R, W>(&self, input: R, output: W) -> Result<EncodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
    {
        let mut ignore = |_progress: RecompressProgress| Ok(());
        self.recompress_into(input, output, &mut ignore)
    }

    #[cfg(feature = "progress")]
    /// Recompress a FLAC reader into FLAC output while reporting phase-aware progress.
    pub fn recompress_with_progress<R, W, F>(
        &self,
        input: R,
        output: W,
        mut on_progress: F,
    ) -> Result<EncodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
        F: FnMut(RecompressProgress) -> Result<()>,
    {
        self.recompress_into(input, output, &mut on_progress)
    }

    /// Recompress from one file path to another.
    pub fn recompress_file<P, Q>(&self, input_path: P, output_path: Q) -> Result<EncodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
    {
        let mut ignore = |_progress: RecompressProgress| Ok(());
        self.recompress_file_with_sink(input_path, output_path, &mut ignore)
    }

    #[cfg(feature = "progress")]
    /// Recompress from one file path to another while reporting phase-aware progress.
    pub fn recompress_file_with_progress<P, Q, F>(
        &self,
        input_path: P,
        output_path: Q,
        mut on_progress: F,
    ) -> Result<EncodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
        F: FnMut(RecompressProgress) -> Result<()>,
    {
        self.recompress_file_with_sink(input_path, output_path, &mut on_progress)
    }

    /// Recompress an in-memory FLAC buffer and return the FLAC bytes.
    pub fn recompress_bytes(&self, input: &[u8]) -> Result<Vec<u8>> {
        let mut output = Cursor::new(Vec::new());
        self.recompress(Cursor::new(input), &mut output)?;
        Ok(output.into_inner())
    }

    fn recompress_into<R, W, F>(
        &self,
        mut input: R,
        output: W,
        on_progress: &mut F,
    ) -> Result<EncodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
        F: FnMut(RecompressProgress) -> Result<()>,
    {
        let total_samples = inspect_flac_total_samples(&mut input)?;
        input.seek(SeekFrom::Start(0))?;
        let overall_total_samples = overall_total_samples(total_samples);
        on_progress(RecompressProgress {
            phase: RecompressPhase::Decode,
            phase_processed_samples: 0,
            phase_total_samples: total_samples,
            overall_processed_samples: 0,
            overall_total_samples,
            completed_frames: 0,
            total_frames: 0,
        })?;

        let decoded = {
            let mut decode_progress = DecodePhaseProgress {
                callback: on_progress,
            };
            read_flac_for_decode(input, self.config.decode_config(), &mut decode_progress)?
        };

        let encode_config = self.config.encode_config();
        let encode_plan = EncodePlan::new(decoded.wav.spec, encode_config.clone())?;
        on_progress(RecompressProgress {
            phase: RecompressPhase::Encode,
            phase_processed_samples: 0,
            phase_total_samples: total_samples,
            overall_processed_samples: total_samples,
            overall_total_samples,
            completed_frames: 0,
            total_frames: encode_plan.total_frames,
        })?;

        verify_streaminfo_md5(
            decoded.wav.spec,
            &decoded.wav.samples,
            decoded.stream_info.md5,
        )?;
        let metadata = decoded.metadata.into_encode_metadata();
        let stream = SlicePcmStream::from_pcm_stream(decoded.wav);
        let mut encode_progress = EncodePhaseProgress {
            callback: on_progress,
            total_samples,
        };
        let mut encoder = encode_config.into_encoder(output);
        encoder.set_metadata(metadata);
        encoder.encode_with_sink(stream, &mut encode_progress)
    }

    fn recompress_file_with_sink<P, Q, F>(
        &self,
        input_path: P,
        output_path: Q,
        on_progress: &mut F,
    ) -> Result<EncodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
        F: FnMut(RecompressProgress) -> Result<()>,
    {
        let input_path = input_path.as_ref();
        let output_path = output_path.as_ref();
        let (temp_path, temp_file) = open_temp_output(output_path)?;

        let result = (|| {
            let input = File::open(input_path)?;
            self.recompress_into(input, temp_file, on_progress)
        })();
        match result {
            Ok(summary) => {
                if let Err(error) = commit_temp_output(&temp_path, output_path) {
                    let _ = std::fs::remove_file(&temp_path);
                    return Err(error);
                }
                Ok(summary)
            }
            Err(error) => {
                let _ = std::fs::remove_file(&temp_path);
                Err(error)
            }
        }
    }
}

struct DecodePhaseProgress<'a, F> {
    callback: &'a mut F,
}

impl<F> ProgressSink for DecodePhaseProgress<'_, F>
where
    F: FnMut(RecompressProgress) -> Result<()>,
{
    fn on_frame(&mut self, progress: ProgressSnapshot) -> Result<()> {
        (self.callback)(RecompressProgress {
            phase: RecompressPhase::Decode,
            phase_processed_samples: progress.processed_samples,
            phase_total_samples: progress.total_samples,
            overall_processed_samples: progress.processed_samples,
            overall_total_samples: overall_total_samples(progress.total_samples),
            completed_frames: progress.completed_frames,
            total_frames: progress.total_frames,
        })
    }
}

struct EncodePhaseProgress<'a, F> {
    callback: &'a mut F,
    total_samples: u64,
}

impl<F> ProgressSink for EncodePhaseProgress<'_, F>
where
    F: FnMut(RecompressProgress) -> Result<()>,
{
    fn on_frame(&mut self, progress: ProgressSnapshot) -> Result<()> {
        (self.callback)(RecompressProgress {
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

/// Convenience wrapper around the default [`Recompressor`] for file-path input.
pub fn recompress_file<P, Q>(input_path: P, output_path: Q) -> Result<EncodeSummary>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    Recompressor::default().recompress_file(input_path, output_path)
}

/// Convenience wrapper around the default [`Recompressor`] for in-memory input.
pub fn recompress_bytes(input: &[u8]) -> Result<Vec<u8>> {
    Recompressor::default().recompress_bytes(input)
}
