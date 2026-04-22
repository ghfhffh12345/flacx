use std::io::{Seek, Write};

use crate::{config::EncoderConfig, level::Level};

use super::session::Recompressor;

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
/// Recompression reuses the decode pipeline to verify and materialize PCM, then
/// runs the encode pipeline again with the selected output policy.
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
        let decode = self.decode_config();
        crate::FlacReaderOptions {
            strict_seektable_validation: decode.strict_seektable_validation(),
            strict_channel_mask_provenance: decode.strict_channel_mask_provenance(),
        }
    }

    pub(crate) fn decode_config(self) -> crate::DecodeConfig {
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

    pub(crate) fn encode_config(self) -> EncoderConfig {
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
