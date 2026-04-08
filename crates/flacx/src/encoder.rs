//! WAV-to-FLAC encoding primitives used by the `flacx` crate.
//!
//! The main façade is [`Encoder`]. Pair it with [`EncoderConfig`] or
//! [`Encoder::builder`] to choose the compression level, thread count, and
//! optional block sizing strategy before encoding.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{Cursor, Read, Seek, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

use crate::{
    config::{EncoderBuilder, EncoderConfig},
    error::{Error, Result},
    input::read_wav_for_encode_with_config,
    model::encode_frame,
    plan::{EncodePlan, FrameCodedNumberKind, summary_from_stream_info},
    progress::{NoProgress, ProgressSink, ProgressSnapshot},
    write::{EncodedFrame, FlacWriter, FrameHeaderNumber},
};

const FRAME_CHUNK_SIZE: usize = 32;

struct EncodedChunk {
    start_frame: usize,
    frames: Vec<EncodedFrame>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Summary of the FLAC stream produced by an encode operation.
///
/// The values mirror the stream information written into the output file.
pub struct EncodeSummary {
    /// Number of FLAC frames written to the output stream.
    pub frame_count: usize,
    /// Total input samples consumed by the encoder.
    pub total_samples: u64,
    /// Maximum block size recorded in the output stream.
    pub block_size: u16,
    /// Smallest encoded frame size in bytes.
    pub min_frame_size: u32,
    /// Largest encoded frame size in bytes.
    pub max_frame_size: u32,
    /// Smallest encoded block size in samples.
    pub min_block_size: u16,
    /// Largest encoded block size in samples.
    pub max_block_size: u16,
    /// Sample rate of the encoded stream.
    pub sample_rate: u32,
    /// Number of channels in the encoded stream.
    pub channels: u8,
    /// Bits per sample recorded in the encoded stream.
    pub bits_per_sample: u8,
}

/// Primary library façade for WAV-to-FLAC conversion.
///
/// Construct an encoder from [`EncoderConfig`] and call one of the encode
/// methods depending on your input shape:
///
/// - [`Encoder::encode`] for generic `Read + Seek` sources
/// - [`Encoder::encode_file`] for file paths
/// - [`Encoder::encode_bytes`] for in-memory input
///
/// The encoder itself is cheap to clone and holds only its configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Encoder {
    config: EncoderConfig,
}

impl Encoder {
    /// Create a builder initialized from [`EncoderConfig::builder`].
    #[must_use]
    pub fn builder() -> EncoderBuilder {
        EncoderConfig::builder()
    }

    /// Construct an encoder from a configuration value.
    #[must_use]
    pub fn new(config: EncoderConfig) -> Self {
        Self { config }
    }

    /// Return a clone of the configuration currently stored in the encoder.
    #[must_use]
    pub fn config(&self) -> EncoderConfig {
        self.config.clone()
    }

    /// Return a new encoder with a different compression level preset.
    #[must_use]
    pub fn with_level(self, level: crate::level::Level) -> Self {
        Self::new(self.config.with_level(level))
    }

    /// Return a new encoder with a different worker thread count.
    #[must_use]
    pub fn with_threads(self, threads: usize) -> Self {
        Self::new(self.config.with_threads(threads))
    }

    /// Return a new encoder with a different fixed block size.
    #[must_use]
    pub fn with_block_size(self, block_size: u16) -> Self {
        Self::new(self.config.with_block_size(block_size))
    }

    /// Encode a WAV reader into FLAC output.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::io::Cursor;
    /// use flacx::Encoder;
    ///
    /// let input = Cursor::new(std::fs::read("input.wav").unwrap());
    /// let mut output = Cursor::new(Vec::new());
    /// Encoder::default().encode(input, &mut output).unwrap();
    /// ```
    pub fn encode<R, W>(&self, input: R, output: W) -> Result<EncodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
    {
        let input = read_wav_for_encode_with_config(input, &self.config)?;
        let mut progress = NoProgress;
        self.encode_wav_data(input, output, &mut progress)
    }

    #[cfg(feature = "progress")]
    /// Encode a WAV reader into FLAC output while reporting progress.
    ///
    /// The callback receives a [`ProgressSnapshot`] after each frame is
    /// written.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # #[cfg(feature = "progress")]
    /// # {
    /// use std::io::Cursor;
    /// use flacx::{Encoder, ProgressSnapshot};
    ///
    /// let input = Cursor::new(std::fs::read("input.wav").unwrap());
    /// let mut output = Cursor::new(Vec::new());
    /// Encoder::default().encode_with_progress(input, &mut output, |snapshot: ProgressSnapshot| {
    ///     println!("{} / {}", snapshot.processed_samples, snapshot.total_samples);
    ///     Ok(())
    /// }).unwrap();
    /// # }
    /// ```
    pub fn encode_with_progress<R, W, F>(
        &self,
        input: R,
        output: W,
        mut on_progress: F,
    ) -> Result<EncodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
        F: FnMut(ProgressSnapshot) -> Result<()>,
    {
        let input = read_wav_for_encode_with_config(input, &self.config)?;
        let mut progress = crate::progress::CallbackProgress::new(&mut on_progress);
        self.encode_wav_data(input, output, &mut progress)
    }

    /// Encode from one file path to another.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use flacx::Encoder;
    ///
    /// Encoder::default()
    ///     .encode_file("input.wav", "output.flac")
    ///     .unwrap();
    /// ```
    pub fn encode_file<P, Q>(&self, input_path: P, output_path: Q) -> Result<EncodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
    {
        self.encode(File::open(input_path)?, File::create(output_path)?)
    }

    #[cfg(feature = "progress")]
    /// Encode from one file path to another while reporting progress.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # #[cfg(feature = "progress")]
    /// # {
    /// use flacx::{Encoder, ProgressSnapshot};
    ///
    /// Encoder::default()
    ///     .encode_file_with_progress("input.wav", "output.flac", |snapshot: ProgressSnapshot| {
    ///         println!("{} / {} frames", snapshot.completed_frames, snapshot.total_frames);
    ///         Ok(())
    ///     })
    ///     .unwrap();
    /// # }
    /// ```
    pub fn encode_file_with_progress<P, Q, F>(
        &self,
        input_path: P,
        output_path: Q,
        on_progress: F,
    ) -> Result<EncodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
        F: FnMut(ProgressSnapshot) -> Result<()>,
    {
        self.encode_with_progress(
            File::open(input_path)?,
            File::create(output_path)?,
            on_progress,
        )
    }

    /// Encode an in-memory WAV buffer and return the FLAC bytes.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use flacx::Encoder;
    ///
    /// let wav_bytes = std::fs::read("input.wav").unwrap();
    /// let flac_bytes = Encoder::default().encode_bytes(&wav_bytes).unwrap();
    /// assert!(!flac_bytes.is_empty());
    /// ```
    pub fn encode_bytes(&self, input: &[u8]) -> Result<Vec<u8>> {
        let mut output = Cursor::new(Vec::new());
        self.encode(Cursor::new(input), &mut output)?;
        Ok(output.into_inner())
    }

    pub(crate) fn encode_wav_data<W, P>(
        &self,
        input: crate::input::EncodeWavData,
        output: W,
        progress: &mut P,
    ) -> Result<EncodeSummary>
    where
        W: Write + Seek,
        P: ProgressSink,
    {
        let crate::input::EncodeWavData {
            wav,
            metadata,
            streaminfo_md5,
        } = input;
        let plan = EncodePlan::new(wav.spec, self.config.clone())?;
        let mut stream_info = plan.stream_info();
        stream_info.md5 = streaminfo_md5;
        let has_preserved_bundle = metadata.has_preserved_bundle();
        let metadata_blocks = metadata.flac_blocks();
        let mut writer = FlacWriter::new(
            output,
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
        self.encode_frames(plan, wav.samples, &mut writer, progress)?;

        let (_, stream_info) = writer.finalize()?;
        Ok(summary_from_stream_info(stream_info, total_frames))
    }

    fn encode_frames<W, P>(
        &self,
        plan: EncodePlan,
        samples: Vec<i32>,
        writer: &mut FlacWriter<W>,
        progress: &mut P,
    ) -> Result<()>
    where
        W: Write + Seek,
        P: ProgressSink,
    {
        let worker_count = self.config.threads.max(1).min(plan.total_frames.max(1));
        let next_frame = Arc::new(AtomicUsize::new(0));
        let samples: Arc<[i32]> = Arc::from(samples);
        let channels = usize::from(plan.spec.channels);

        thread::scope(|scope| -> Result<()> {
            let (sender, receiver) = mpsc::channel();
            for _ in 0..worker_count {
                let sender = sender.clone();
                let next_frame = Arc::clone(&next_frame);
                let samples = Arc::clone(&samples);
                let channels_in_scope = channels;
                let plan = plan.clone();

                scope.spawn(move || {
                    loop {
                        let chunk_start = next_frame.fetch_add(FRAME_CHUNK_SIZE, Ordering::Relaxed);
                        if chunk_start >= plan.total_frames {
                            break;
                        }
                        let chunk_end = (chunk_start + FRAME_CHUNK_SIZE).min(plan.total_frames);
                        let mut encoded_frames = Vec::with_capacity(chunk_end - chunk_start);

                        for frame_index in chunk_start..chunk_end {
                            let frame_plan = plan.frame(frame_index);
                            let frame_samples = usize::from(frame_plan.block_size);
                            let sample_start = usize::try_from(frame_plan.sample_offset)
                                .expect("sample offset fits in usize")
                                * channels_in_scope;
                            let sample_end = sample_start + frame_samples * channels_in_scope;
                            let frame = encode_frame(
                                &samples[sample_start..sample_end],
                                plan.spec.channels,
                                plan.spec.bits_per_sample,
                                plan.spec.sample_rate,
                                match frame_plan.coded_number_kind {
                                    FrameCodedNumberKind::FrameNumber => {
                                        FrameHeaderNumber::Frame(frame_plan.coded_number)
                                    }
                                    FrameCodedNumberKind::SampleNumber => {
                                        FrameHeaderNumber::Sample(frame_plan.coded_number)
                                    }
                                },
                                plan.profile,
                            );
                            match frame {
                                Ok(frame) => encoded_frames.push(frame),
                                Err(error) => {
                                    let _ = sender.send(Err(error));
                                    return;
                                }
                            }
                        }

                        if sender
                            .send(Ok(EncodedChunk {
                                start_frame: chunk_start,
                                frames: encoded_frames,
                            }))
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
                    Error::Thread(
                        "frame worker channel closed before all frames were encoded".into(),
                    )
                })??;
                if encoded_chunk.start_frame == next_expected {
                    processed_samples = write_encoded_chunk(
                        writer,
                        encoded_chunk,
                        processed_samples,
                        plan.spec.total_samples,
                        &mut next_expected,
                        plan.total_frames,
                        progress,
                    )?;
                    while let Some(chunk) = pending.remove(&next_expected) {
                        processed_samples = write_encoded_chunk(
                            writer,
                            chunk,
                            processed_samples,
                            plan.spec.total_samples,
                            &mut next_expected,
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
}

/// Convenience wrapper around the default [`Encoder`] for file-path input.
///
/// # Example
///
/// ```no_run
/// use flacx::encode_file;
///
/// encode_file("input.wav", "output.flac").unwrap();
/// ```
pub fn encode_file<P, Q>(input_path: P, output_path: Q) -> Result<EncodeSummary>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    Encoder::default().encode_file(input_path, output_path)
}

/// Convenience wrapper around the default [`Encoder`] for in-memory input.
///
/// # Example
///
/// ```no_run
/// use flacx::encode_bytes;
///
/// let wav_bytes = std::fs::read("input.wav").unwrap();
/// let flac_bytes = encode_bytes(&wav_bytes).unwrap();
/// assert!(!flac_bytes.is_empty());
/// ```
pub fn encode_bytes(input: &[u8]) -> Result<Vec<u8>> {
    Encoder::default().encode_bytes(input)
}

fn write_encoded_frame<W, P>(
    writer: &mut FlacWriter<W>,
    frame: &EncodedFrame,
    frame_index: usize,
    processed_samples: u64,
    total_samples: u64,
    total_frames: usize,
    progress: &mut P,
) -> Result<u64>
where
    W: Write + Seek,
    P: ProgressSink,
{
    writer.write_frame(
        frame_index,
        processed_samples,
        frame.sample_count,
        &frame.bytes,
    )?;
    let processed_samples = processed_samples + u64::from(frame.sample_count);
    progress.on_frame(ProgressSnapshot {
        processed_samples,
        total_samples,
        completed_frames: frame_index + 1,
        total_frames,
    })?;
    Ok(processed_samples)
}

fn write_encoded_chunk<W, P>(
    writer: &mut FlacWriter<W>,
    chunk: EncodedChunk,
    mut processed_samples: u64,
    total_samples: u64,
    next_expected: &mut usize,
    total_frames: usize,
    progress: &mut P,
) -> Result<u64>
where
    W: Write + Seek,
    P: ProgressSink,
{
    for frame in chunk.frames {
        processed_samples = write_encoded_frame(
            writer,
            &frame,
            *next_expected,
            processed_samples,
            total_samples,
            total_frames,
            progress,
        )?;
        *next_expected += 1;
    }
    Ok(processed_samples)
}
