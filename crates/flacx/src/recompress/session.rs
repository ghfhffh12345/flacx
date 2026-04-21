use std::io::{Seek, Write};

use crate::{
    encoder::{EncodeSummary, Encoder},
    error::Result,
    input::{CountedEncodePcmStream, EncodePcmStream, EncodeSource, PcmSpec, PcmStream},
    level::Level,
    progress::NoProgress,
};

use super::{
    config::{RecompressConfig, RecompressMode},
    progress::{
        EncodePhaseProgress, RecompressPhase, RecompressProgress, RecompressProgressSink,
        encode_phase_transition_progress, overall_total_samples,
    },
    source::FlacRecompressSource,
};

const EAGER_RECOMPRESS_TOTAL_SAMPLES_THRESHOLD: u64 = 8 * 1024 * 1024;

struct BufferedRecompressPcmStream {
    spec: PcmSpec,
    samples: Vec<i32>,
    cursor: usize,
    expected_md5: [u8; 16],
}

impl BufferedRecompressPcmStream {
    fn new(pcm_stream: PcmStream, expected_md5: [u8; 16]) -> Self {
        Self {
            spec: pcm_stream.spec,
            samples: pcm_stream.samples,
            cursor: 0,
            expected_md5,
        }
    }
}

impl EncodePcmStream for BufferedRecompressPcmStream {
    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize> {
        if max_frames == 0 {
            return Ok(0);
        }

        let channels = usize::from(self.spec.channels);
        let remaining_frames = (self.samples.len().saturating_sub(self.cursor)) / channels;
        let frames = remaining_frames.min(max_frames);
        let sample_count = frames.saturating_mul(channels);
        let next = self.cursor.saturating_add(sample_count);
        output.extend_from_slice(&self.samples[self.cursor..next]);
        self.cursor = next;
        Ok(frames)
    }

    fn update_streaminfo_md5(
        &mut self,
        _md5: &mut crate::md5::StreaminfoMd5,
        _samples: &[i32],
    ) -> Result<()> {
        Ok(())
    }

    fn finish_streaminfo_md5(&mut self, _md5: crate::md5::StreaminfoMd5) -> Result<[u8; 16]> {
        Ok(self.expected_md5)
    }
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
            self.config.decode_config().strict_channel_mask_provenance,
        )?;
        source.set_metadata(metadata);
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
            phase_input_bytes_read: 0,
            phase_output_bytes_written: 0,
            overall_input_bytes_read: 0,
            overall_output_bytes_written: 0,
        })?;

        if total_samples <= EAGER_RECOMPRESS_TOTAL_SAMPLES_THRESHOLD {
            let (metadata, pcm_stream, streaminfo_md5, decode_input_bytes_read) =
                source.into_verified_pcm_stream()?;
            let encode_config = self.config.encode_config();
            let encode_plan = crate::plan::EncodePlan::new(pcm_stream.spec, encode_config.clone())?;
            progress.on_progress(encode_phase_transition_progress(
                total_samples,
                encode_plan.total_frames,
                decode_input_bytes_read,
            ))?;

            let mut encode_progress = EncodePhaseProgress {
                sink: progress,
                total_samples,
                decode_input_bytes_read,
            };
            let mut encoder: Encoder<&mut W> = encode_config.into_encoder(&mut self.writer);
            let summary = encoder.encode_source_with_sink(
                EncodeSource::new(
                    metadata,
                    CountedEncodePcmStream::new(BufferedRecompressPcmStream::new(
                        pcm_stream,
                        streaminfo_md5,
                    )),
                ),
                &mut encode_progress,
            )?;
            return Ok(summary.into());
        }

        // The previous buffered handoff was `source.into_verified_pcm_stream()?`;
        // keep verification incremental and stream the verified PCM into encode instead.
        let (metadata, mut stream) = source.into_encode_parts();
        let encode_config = self.config.encode_config();
        let decode_input_bytes_read = stream.input_bytes_processed();
        let encode_plan = crate::plan::EncodePlan::new(stream.spec(), encode_config.clone())?;
        if total_samples == 0 {
            stream.finish_verification()?;
        }
        progress.on_progress(encode_phase_transition_progress(
            total_samples,
            encode_plan.total_frames,
            decode_input_bytes_read,
        ))?;

        let mut encode_progress = EncodePhaseProgress {
            sink: progress,
            total_samples,
            decode_input_bytes_read,
        };
        let stream = CountedEncodePcmStream::new(stream);
        let mut encoder: Encoder<&mut W> = encode_config.into_encoder(&mut self.writer);
        let summary = encoder
            .encode_source_with_sink(EncodeSource::new(metadata, stream), &mut encode_progress)?;
        Ok(summary.into())
    }
}
