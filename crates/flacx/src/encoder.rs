//! PCM-container-to-FLAC encoding session primitives used by the `flacx` crate.
//!
//! The public encode flow is reader-driven: parse a family reader, inspect its
//! spec/metadata, bind an output writer through [`EncoderConfig::into_encoder`],
//! then feed the resulting single-pass PCM stream into [`Encoder::encode`].

use std::{
    collections::BTreeMap,
    io::{Seek, Write},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

use crate::{
    config::{EncoderBuilder, EncoderConfig},
    encode_pipeline::{EncodedChunk, encode_frame_batch, encode_stream, write_encoded_chunk},
    error::{Error, Result},
    input::{EncodePcmStream, EncodeSource, PcmStream},
    metadata::Metadata,
    plan::{EncodePlan, summary_from_stream_info},
    progress::{NoProgress, ProgressSink},
    write::FlacWriter,
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
    /// Create a builder initialized from [`EncoderConfig::builder`].
    #[must_use]
    pub fn builder() -> EncoderBuilder {
        EncoderConfig::builder()
    }

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
        let (metadata, stream) = source.into_parts();
        encode_stream(&self.config, metadata, stream, &mut self.writer, progress)
    }

    pub(crate) fn encode_buffered_pcm_with_sink<P>(
        &mut self,
        metadata: Metadata,
        pcm: PcmStream,
        streaminfo_md5: [u8; 16],
        progress: &mut P,
    ) -> Result<EncodeSummary>
    where
        P: ProgressSink,
    {
        let PcmStream { spec, samples } = pcm;
        let plan = EncodePlan::new(spec, self.config.clone())?;
        let mut stream_info = plan.stream_info();
        stream_info.md5 = streaminfo_md5;
        let has_preserved_bundle = metadata.has_preserved_bundle();
        let metadata_blocks = metadata.flac_blocks(spec.total_samples);
        let mut writer = FlacWriter::new(
            &mut self.writer,
            stream_info,
            &metadata_blocks,
            plan.total_frames,
            !has_preserved_bundle,
        )?;

        if plan.total_frames == 0 {
            let (_, stream_info) = writer.finalize()?;
            return Ok(summary_from_stream_info(stream_info, 0));
        }

        let total_frames = plan.total_frames;
        encode_buffered_frames(&self.config, plan, samples, &mut writer, progress)?;

        let (_, stream_info) = writer.finalize()?;
        Ok(summary_from_stream_info(stream_info, total_frames))
    }
}

fn encode_buffered_frames<W, P>(
    config: &EncoderConfig,
    plan: EncodePlan,
    samples: Vec<i32>,
    writer: &mut FlacWriter<&mut W>,
    progress: &mut P,
) -> Result<()>
where
    W: Write + Seek,
    P: ProgressSink,
{
    const FRAME_CHUNK_SIZE: usize = 32;

    let worker_count = config.threads.max(1).min(plan.total_frames.max(1));
    if worker_count == 1 || plan.total_frames <= FRAME_CHUNK_SIZE {
        let chunk = encode_frame_batch(&samples, &plan, 0, 0, plan.total_frames, 0)?;
        write_encoded_chunk(
            writer,
            chunk,
            0,
            plan.spec.total_samples,
            0,
            plan.total_frames,
            progress,
        )?;
        return Ok(());
    }

    let next_frame = Arc::new(AtomicUsize::new(0));
    let samples: Arc<[i32]> = Arc::from(samples);

    thread::scope(|scope| -> Result<()> {
        let (sender, receiver) = mpsc::channel();
        for _ in 0..worker_count {
            let sender = sender.clone();
            let next_frame = Arc::clone(&next_frame);
            let samples = Arc::clone(&samples);
            let plan = plan.clone();

            scope.spawn(move || {
                loop {
                    let chunk_start = next_frame.fetch_add(FRAME_CHUNK_SIZE, Ordering::Relaxed);
                    if chunk_start >= plan.total_frames {
                        break;
                    }
                    let chunk_end = (chunk_start + FRAME_CHUNK_SIZE).min(plan.total_frames);
                    if sender
                        .send(encode_frame_batch(
                            &samples,
                            &plan,
                            0,
                            chunk_start,
                            chunk_end,
                            0,
                        ))
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }

        drop(sender);
        let mut next_expected = 0usize;
        let mut processed_samples = 0u64;
        let mut pending: BTreeMap<usize, EncodedChunk> = BTreeMap::new();
        while next_expected < plan.total_frames {
            let encoded_chunk = receiver.recv().map_err(|_| {
                Error::Thread("frame worker channel closed before all frames were encoded".into())
            })??;
            if encoded_chunk.start_frame == next_expected {
                let chunk_start = encoded_chunk.start_frame;
                next_expected = chunk_start + encoded_chunk.frames.len();
                processed_samples = write_encoded_chunk(
                    writer,
                    encoded_chunk,
                    processed_samples,
                    plan.spec.total_samples,
                    chunk_start,
                    plan.total_frames,
                    progress,
                )?;
                while let Some(chunk) = pending.remove(&next_expected) {
                    let chunk_start = chunk.start_frame;
                    next_expected = chunk_start + chunk.frames.len();
                    processed_samples = write_encoded_chunk(
                        writer,
                        chunk,
                        processed_samples,
                        plan.spec.total_samples,
                        chunk_start,
                        plan.total_frames,
                        progress,
                    )?;
                }
            } else {
                pending.insert(encoded_chunk.start_frame, encoded_chunk);
            }
        }

        Ok(())
    })
}
