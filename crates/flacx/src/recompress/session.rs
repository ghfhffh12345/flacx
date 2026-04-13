use std::io::{Read, Seek, Write};

use crate::{
    encoder::{EncodeSummary, Encoder},
    error::Result,
    level::Level,
    plan::EncodePlan,
    progress::NoProgress,
};

use super::{
    config::{RecompressConfig, RecompressMode},
    progress::{
        EncodePhaseProgress, RecompressPhase, RecompressProgress, RecompressProgressSink,
        overall_total_samples,
    },
    source::FlacRecompressSource,
};

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
        mut source: FlacRecompressSource<R>,
        progress: &mut P,
    ) -> Result<RecompressSummary>
    where
        R: Read + Seek,
        P: RecompressProgressSink,
    {
        source.set_threads(self.config.threads());
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

        let (metadata, pcm_stream, streaminfo_md5) = source.into_verified_pcm_stream()?;
        let encode_config = self.config.encode_config();
        let encode_plan = EncodePlan::new(pcm_stream.spec, encode_config.clone())?;
        progress.on_progress(RecompressProgress {
            phase: RecompressPhase::Encode,
            phase_processed_samples: 0,
            phase_total_samples: total_samples,
            overall_processed_samples: total_samples,
            overall_total_samples: overall_total_samples(total_samples),
            completed_frames: 0,
            total_frames: encode_plan.total_frames,
        })?;

        let mut encode_progress = EncodePhaseProgress {
            sink: progress,
            total_samples,
        };
        let mut encoder: Encoder<&mut W> = encode_config.into_encoder(&mut self.writer);
        encoder.set_metadata(metadata);
        let summary = encoder.encode_buffered_pcm_with_sink(
            pcm_stream,
            streaminfo_md5,
            &mut encode_progress,
        )?;
        Ok(summary.into())
    }
}
