use std::io::{Seek, Write};
#[cfg(feature = "progress")]
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[cfg(feature = "progress")]
use crate::input::EncodePcmStream;
#[cfg(not(feature = "progress"))]
use crate::input::counted_encode_pcm_stream;
#[cfg(feature = "progress")]
use crate::progress::{ProgressSink, ProgressSnapshot};
use crate::{
    encoder::{EncodeSummary, Encoder},
    error::Result,
    input::EncodeSource,
    level::Level,
    progress::NoProgress,
};

#[cfg(not(feature = "progress"))]
use super::progress::EncodePhaseProgress;
use super::{
    config::{RecompressConfig, RecompressMode},
    progress::{RecompressProgressSink, emit_recompress_progress},
    source::FlacRecompressSource,
};

#[cfg(feature = "progress")]
use super::progress::{RecompressPhase, RecompressProgress};

#[cfg(feature = "progress")]
struct RecompressEncodePcmStream<S> {
    inner: S,
    phase_input_bytes_read: u64,
    decode_input_bytes_read: Arc<AtomicU64>,
}

#[cfg(feature = "progress")]
impl<S> RecompressEncodePcmStream<S>
where
    S: EncodePcmStream,
{
    fn new(inner: S) -> (Self, Arc<AtomicU64>) {
        let decode_input_bytes_read = Arc::new(AtomicU64::new(
            EncodePcmStream::input_bytes_processed(&inner),
        ));
        (
            Self {
                inner,
                phase_input_bytes_read: 0,
                decode_input_bytes_read: Arc::clone(&decode_input_bytes_read),
            },
            decode_input_bytes_read,
        )
    }
}

#[cfg(feature = "progress")]
impl<S> EncodePcmStream for RecompressEncodePcmStream<S>
where
    S: EncodePcmStream,
{
    fn spec(&self) -> crate::PcmSpec {
        self.inner.spec()
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize> {
        let frames = self.inner.read_chunk(max_frames, output)?;
        self.phase_input_bytes_read = self
            .phase_input_bytes_read
            .saturating_add(pcm_bytes_for_frames(self.inner.spec(), frames));
        self.decode_input_bytes_read.store(
            EncodePcmStream::input_bytes_processed(&self.inner),
            Ordering::Relaxed,
        );
        Ok(frames)
    }

    fn input_bytes_processed(&self) -> u64 {
        self.phase_input_bytes_read
    }

    fn update_streaminfo_md5(
        &mut self,
        md5: &mut crate::md5::StreaminfoMd5,
        samples: &[i32],
    ) -> Result<()> {
        self.inner.update_streaminfo_md5(md5, samples)
    }

    fn finish_streaminfo_md5(&mut self, md5: crate::md5::StreaminfoMd5) -> Result<[u8; 16]> {
        self.inner.finish_streaminfo_md5(md5)
    }

    fn preferred_encode_chunk_max_frames(&self) -> Option<usize> {
        self.inner.preferred_encode_chunk_max_frames()
    }

    fn preferred_encode_chunk_target_pcm_frames(&self) -> Option<usize> {
        self.inner.preferred_encode_chunk_target_pcm_frames()
    }
}

#[cfg(feature = "progress")]
struct StreamingEncodePhaseProgress<'a, P> {
    sink: &'a mut P,
    total_samples: u64,
    decode_input_bytes_read: Arc<AtomicU64>,
}

#[cfg(feature = "progress")]
impl<'a, P> StreamingEncodePhaseProgress<'a, P> {
    fn new(sink: &'a mut P, total_samples: u64, decode_input_bytes_read: Arc<AtomicU64>) -> Self {
        Self {
            sink,
            total_samples,
            decode_input_bytes_read,
        }
    }
}

#[cfg(feature = "progress")]
impl<P> ProgressSink for StreamingEncodePhaseProgress<'_, P>
where
    P: RecompressProgressSink,
{
    fn on_frame(&mut self, progress: ProgressSnapshot) -> Result<()> {
        let decode_input_bytes_read = self.decode_input_bytes_read.load(Ordering::Relaxed);
        self.sink.on_progress(RecompressProgress {
            phase: RecompressPhase::Encode,
            phase_processed_samples: progress.processed_samples,
            phase_total_samples: progress.total_samples,
            overall_processed_samples: self
                .total_samples
                .saturating_add(progress.processed_samples),
            overall_total_samples: super::progress::overall_total_samples(self.total_samples),
            completed_frames: progress.completed_frames,
            total_frames: progress.total_frames,
            phase_input_bytes_read: progress.input_bytes_read,
            phase_output_bytes_written: progress.output_bytes_written,
            overall_input_bytes_read: decode_input_bytes_read
                .saturating_add(progress.input_bytes_read),
            overall_output_bytes_written: progress.output_bytes_written,
        })
    }
}

#[cfg(feature = "progress")]
fn pcm_bytes_for_frames(spec: crate::PcmSpec, frames: usize) -> u64 {
    (frames as u64)
        .saturating_mul(u64::from(spec.channels))
        .saturating_mul(u64::from(spec.bytes_per_sample))
}

/// Summary of the FLAC stream produced by a recompress operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecompressSummary {
    /// Number of FLAC frames written to the recompressed output.
    pub frame_count: usize,
    /// Total samples per channel written to the recompressed output.
    pub total_samples: u64,
    /// Block size used by the recompressed stream.
    pub block_size: u16,
    /// Minimum encoded frame size in bytes.
    pub min_frame_size: u32,
    /// Maximum encoded frame size in bytes.
    pub max_frame_size: u32,
    /// Minimum encoded block size in samples.
    pub min_block_size: u16,
    /// Maximum encoded block size in samples.
    pub max_block_size: u16,
    /// Output sample rate.
    pub sample_rate: u32,
    /// Output channel count.
    pub channels: u8,
    /// Output bits per sample.
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
    pub fn recompress<S>(&mut self, source: FlacRecompressSource<S>) -> Result<RecompressSummary>
    where
        S: crate::read::DecodePcmStream,
    {
        let mut progress = NoProgress;
        self.recompress_with_sink(source, &mut progress)
    }

    #[cfg(feature = "progress")]
    /// Recompress a single-pass FLAC source while reporting phase-aware progress.
    pub fn recompress_with_progress<S, F>(
        &mut self,
        source: FlacRecompressSource<S>,
        mut on_progress: F,
    ) -> Result<RecompressSummary>
    where
        S: crate::read::DecodePcmStream,
        F: FnMut(RecompressProgress) -> Result<()>,
    {
        self.recompress_with_sink(source, &mut on_progress)
    }

    pub(crate) fn recompress_with_sink<S, P>(
        &mut self,
        mut source: FlacRecompressSource<S>,
        progress: &mut P,
    ) -> Result<RecompressSummary>
    where
        S: crate::read::DecodePcmStream,
        P: RecompressProgressSink,
    {
        let mut metadata = source.metadata().clone();
        crate::metadata::align_metadata_to_stream_spec(
            &mut metadata,
            source.spec(),
            self.config.decode_config().strict_channel_mask_provenance(),
        )?;
        source.set_metadata(metadata);
        source.set_threads(self.config.threads());
        let total_samples = source.total_samples();
        emit_recompress_progress!(
            progress,
            super::progress::RecompressProgress {
                phase: super::progress::RecompressPhase::Decode,
                phase_processed_samples: 0,
                phase_total_samples: total_samples,
                overall_processed_samples: 0,
                overall_total_samples: super::progress::overall_total_samples(total_samples),
                completed_frames: 0,
                total_frames: 0,
                phase_input_bytes_read: 0,
                phase_output_bytes_written: 0,
                overall_input_bytes_read: 0,
                overall_output_bytes_written: 0,
            }
        )?;

        let (metadata, mut stream) = source.into_encode_parts();
        let encode_config = self.config.encode_config();
        #[cfg(feature = "progress")]
        let encode_plan = crate::plan::EncodePlan::new(stream.spec(), encode_config.clone())?;
        if total_samples == 0 {
            stream.finish_verification()?;
        }
        #[cfg(feature = "progress")]
        let (stream, decode_input_bytes_read) = RecompressEncodePcmStream::new(stream);
        #[cfg(feature = "progress")]
        let decode_input_bytes_read_for_transition =
            decode_input_bytes_read.load(Ordering::Relaxed);
        #[cfg(not(feature = "progress"))]
        let decode_input_bytes_read = 0;
        emit_recompress_progress!(
            progress,
            super::progress::encode_phase_transition_progress(
                total_samples,
                encode_plan.total_frames,
                decode_input_bytes_read_for_transition,
            )
        )?;

        #[cfg(feature = "progress")]
        let mut encode_progress =
            StreamingEncodePhaseProgress::new(progress, total_samples, decode_input_bytes_read);
        #[cfg(not(feature = "progress"))]
        let mut encode_progress =
            EncodePhaseProgress::new(progress, total_samples, decode_input_bytes_read);
        #[cfg(not(feature = "progress"))]
        let stream = counted_encode_pcm_stream(stream);
        let mut encoder: Encoder<&mut W> = encode_config.into_encoder(&mut self.writer);
        let summary = encoder
            .encode_source_with_sink(EncodeSource::new(metadata, stream), &mut encode_progress)?;
        Ok(summary.into())
    }
}
