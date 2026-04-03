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
    error::{Error, Result},
    flac_writer::FlacWriter,
    frame::{EncodedFrame, encode_frame, sample_rate_is_representable},
    level::{Level, LevelProfile},
    metadata::StreamInfo,
    wav::{WavData, WavSpec, read_wav},
};

const FRAME_CHUNK_SIZE: usize = 8;

/// User-facing encoder options for WAV-to-FLAC conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeOptions {
    pub level: Level,
    pub threads: usize,
    pub block_size: u16,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        let level = Level::Level8;
        let profile = level.profile();
        Self {
            level,
            threads: std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1),
            block_size: profile.block_size,
        }
    }
}

impl EncodeOptions {
    #[must_use]
    pub fn with_level(mut self, level: Level) -> Self {
        let profile = level.profile();
        self.level = level;
        self.block_size = profile.block_size;
        self
    }

    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads.max(1);
        self
    }

    #[must_use]
    pub fn with_block_size(mut self, block_size: u16) -> Self {
        self.block_size = block_size;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[cfg(feature = "progress")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeProgress {
    pub processed_samples: u64,
    pub total_samples: u64,
    pub completed_frames: usize,
    pub total_frames: usize,
}

trait ProgressSink {
    fn on_frame(
        &mut self,
        processed_samples: u64,
        total_samples: u64,
        completed_frames: usize,
        total_frames: usize,
    ) -> Result<()>;
}

struct NoProgress;

impl ProgressSink for NoProgress {
    fn on_frame(
        &mut self,
        _processed_samples: u64,
        _total_samples: u64,
        _completed_frames: usize,
        _total_frames: usize,
    ) -> Result<()> {
        Ok(())
    }
}

#[cfg(feature = "progress")]
struct CallbackProgress<F> {
    callback: F,
}

#[cfg(feature = "progress")]
impl<F> ProgressSink for CallbackProgress<F>
where
    F: FnMut(EncodeProgress) -> Result<()>,
{
    fn on_frame(
        &mut self,
        processed_samples: u64,
        total_samples: u64,
        completed_frames: usize,
        total_frames: usize,
    ) -> Result<()> {
        (self.callback)(EncodeProgress {
            processed_samples,
            total_samples,
            completed_frames,
            total_frames,
        })
    }
}

/// Primary library entrypoint for WAV-to-FLAC conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlacEncoder {
    options: EncodeOptions,
}

impl Default for FlacEncoder {
    fn default() -> Self {
        Self {
            options: EncodeOptions::default(),
        }
    }
}

impl FlacEncoder {
    #[must_use]
    pub fn new(options: EncodeOptions) -> Self {
        Self { options }
    }

    #[must_use]
    pub fn options(&self) -> EncodeOptions {
        self.options
    }

    #[must_use]
    pub fn with_level(self, level: Level) -> Self {
        Self::new(self.options.with_level(level))
    }

    #[must_use]
    pub fn with_threads(self, threads: usize) -> Self {
        Self::new(self.options.with_threads(threads))
    }

    #[must_use]
    pub fn with_block_size(self, block_size: u16) -> Self {
        Self::new(self.options.with_block_size(block_size))
    }

    pub fn encode<R, W>(&self, input: R, output: W) -> Result<EncodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
    {
        let wav = read_wav(input)?;
        let mut progress = NoProgress;
        self.encode_wav_data_to_flac(wav, output, &mut progress)
    }

    #[cfg(feature = "progress")]
    pub fn encode_with_progress<R, W, F>(
        &self,
        input: R,
        output: W,
        mut on_progress: F,
    ) -> Result<EncodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
        F: FnMut(EncodeProgress) -> Result<()>,
    {
        let wav = read_wav(input)?;
        let mut progress = CallbackProgress {
            callback: &mut on_progress,
        };
        self.encode_wav_data_to_flac(wav, output, &mut progress)
    }

    pub fn encode_file<P, Q>(&self, input_path: P, output_path: Q) -> Result<EncodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
    {
        self.encode(File::open(input_path)?, File::create(output_path)?)
    }

    #[cfg(feature = "progress")]
    pub fn encode_file_with_progress<P, Q, F>(
        &self,
        input_path: P,
        output_path: Q,
        on_progress: F,
    ) -> Result<EncodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
        F: FnMut(EncodeProgress) -> Result<()>,
    {
        self.encode_with_progress(
            File::open(input_path)?,
            File::create(output_path)?,
            on_progress,
        )
    }

    pub fn encode_bytes(&self, input: &[u8]) -> Result<Vec<u8>> {
        let mut output = Cursor::new(Vec::new());
        self.encode(Cursor::new(input), &mut output)?;
        Ok(output.into_inner())
    }

    pub fn encode_wav_to_flac<R, W>(&self, input: R, output: W) -> Result<EncodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
    {
        self.encode(input, output)
    }

    pub fn encode_wav_bytes(&self, input: &[u8]) -> Result<Vec<u8>> {
        self.encode_bytes(input)
    }

    fn encode_wav_data_to_flac<W, P>(
        &self,
        wav: WavData,
        output: W,
        progress: &mut P,
    ) -> Result<EncodeSummary>
    where
        W: Write + Seek,
        P: ProgressSink,
    {
        let profile = self.options.level.profile();
        let block_size = self.options.block_size;
        let total_frames = if wav.spec.total_samples == 0 {
            0
        } else {
            wav.spec.total_samples.div_ceil(u64::from(block_size)) as usize
        };
        let mut writer = self.init_flac_writer(wav.spec, output)?;

        if total_frames == 0 {
            let (_, stream_info) = writer.finalize()?;
            return Ok(summary_from_stream_info(stream_info, 0));
        }

        self.encode_frames_in_parallel(
            wav.spec,
            wav.samples,
            block_size,
            profile,
            &mut writer,
            progress,
        )?;

        let (_, stream_info) = writer.finalize()?;
        Ok(summary_from_stream_info(stream_info, total_frames))
    }

    fn encode_frames_in_parallel<W, P>(
        &self,
        spec: WavSpec,
        samples: Vec<i32>,
        block_size: u16,
        profile: LevelProfile,
        writer: &mut FlacWriter<W>,
        progress: &mut P,
    ) -> Result<()>
    where
        W: Write + Seek,
        P: ProgressSink,
    {
        let total_frames = spec.total_samples.div_ceil(u64::from(block_size)) as usize;
        let worker_count = self.options.threads.max(1).min(total_frames.max(1));
        let next_frame = Arc::new(AtomicUsize::new(0));
        let samples: Arc<[i32]> = Arc::from(samples);
        let channels = usize::from(spec.channels);
        let sample_rate = spec.sample_rate;
        let bits_per_sample = spec.bits_per_sample;
        let total_samples = spec.total_samples;

        thread::scope(|scope| -> Result<()> {
            let (sender, receiver) = mpsc::channel();
            for _ in 0..worker_count {
                let sender = sender.clone();
                let next_frame = Arc::clone(&next_frame);
                let samples = Arc::clone(&samples);
                let channels_in_scope = channels;
                let wav_channels = spec.channels;

                scope.spawn(move || {
                    loop {
                        let chunk_start = next_frame.fetch_add(FRAME_CHUNK_SIZE, Ordering::Relaxed);
                        if chunk_start >= total_frames {
                            break;
                        }
                        let chunk_end = (chunk_start + FRAME_CHUNK_SIZE).min(total_frames);
                        let mut encoded_chunk = Vec::with_capacity(chunk_end - chunk_start);
                        let mut saw_error = false;

                        for frame_index in chunk_start..chunk_end {
                            let start_sample = frame_index as u64 * u64::from(block_size);
                            let frame_samples =
                                (total_samples - start_sample).min(u64::from(block_size)) as usize;
                            let sample_start =
                                frame_index * usize::from(block_size) * channels_in_scope;
                            let sample_end = sample_start + frame_samples * channels_in_scope;
                            let frame = encode_frame(
                                &samples[sample_start..sample_end],
                                wav_channels,
                                bits_per_sample,
                                sample_rate,
                                frame_index as u64,
                                profile,
                            );
                            saw_error |= frame.is_err();
                            encoded_chunk.push((frame_index, frame));
                            if saw_error {
                                break;
                            }
                        }
                        if sender.send(encoded_chunk).is_err() {
                            return;
                        }
                        if saw_error {
                            return;
                        }
                    }
                });
            }

            drop(sender);
            let mut next_expected = 0usize;
            let mut processed_samples = 0u64;
            let mut pending: BTreeMap<usize, EncodedFrame> = BTreeMap::new();
            while next_expected < total_frames {
                let encoded_chunk = receiver.recv().map_err(|_| {
                    Error::Thread(
                        "frame worker channel closed before all frames were encoded".into(),
                    )
                })?;
                for (frame_index, frame) in encoded_chunk {
                    let frame = frame?;
                    if frame_index == next_expected {
                        processed_samples = write_encoded_frame(
                            writer,
                            &frame,
                            processed_samples,
                            spec.total_samples,
                            next_expected + 1,
                            total_frames,
                            progress,
                        )?;
                        next_expected += 1;
                        while let Some(frame) = pending.remove(&next_expected) {
                            processed_samples = write_encoded_frame(
                                writer,
                                &frame,
                                processed_samples,
                                spec.total_samples,
                                next_expected + 1,
                                total_frames,
                                progress,
                            )?;
                            next_expected += 1;
                        }
                    } else {
                        pending.insert(frame_index, frame);
                    }
                }
            }

            Ok(())
        })
    }

    fn init_flac_writer<W: Write + Seek>(&self, spec: WavSpec, output: W) -> Result<FlacWriter<W>> {
        validate_stream(&spec, self.options.block_size)?;

        let mut stream_info = StreamInfo::new(
            spec.sample_rate,
            spec.channels,
            spec.bits_per_sample,
            spec.total_samples,
            [0; 16],
        );
        stream_info.min_block_size = self.options.block_size;
        stream_info.max_block_size = self.options.block_size;

        Ok(FlacWriter::new(output, stream_info)?)
    }
}

pub fn encode_file<P, Q>(input_path: P, output_path: Q) -> Result<EncodeSummary>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    FlacEncoder::default().encode_file(input_path, output_path)
}

pub fn encode_bytes(input: &[u8]) -> Result<Vec<u8>> {
    FlacEncoder::default().encode_bytes(input)
}

#[deprecated(note = "Use EncodeOptions instead.")]
pub type EncoderConfig = EncodeOptions;

#[deprecated(note = "Use FlacEncoder instead.")]
pub type Encoder = FlacEncoder;

fn write_encoded_frame<W, P>(
    writer: &mut FlacWriter<W>,
    frame: &EncodedFrame,
    processed_samples: u64,
    total_samples: u64,
    completed_frames: usize,
    total_frames: usize,
    progress: &mut P,
) -> Result<u64>
where
    W: Write + Seek,
    P: ProgressSink,
{
    writer.write_frame(&frame.bytes)?;
    let processed_samples = processed_samples + u64::from(frame.sample_count);
    progress.on_frame(
        processed_samples,
        total_samples,
        completed_frames,
        total_frames,
    )?;
    Ok(processed_samples)
}

fn validate_stream(spec: &WavSpec, block_size: u16) -> Result<()> {
    if spec.sample_rate == 0 {
        return Err(Error::UnsupportedFlac(
            "sample rate 0 is not allowed".into(),
        ));
    }

    if block_size < 16 {
        return Err(Error::UnsupportedFlac(
            "block size must be at least 16 to satisfy STREAMINFO bounds".into(),
        ));
    }

    if block_size > 16_384 {
        return Err(Error::UnsupportedFlac(
            "streamable subset requires block sizes <= 16384".into(),
        ));
    }

    if spec.sample_rate <= 48_000 && block_size > 4_608 {
        return Err(Error::UnsupportedFlac(
            "sample rates <= 48000 Hz require block sizes <= 4608 in the streamable subset".into(),
        ));
    }

    if !sample_rate_is_representable(spec.sample_rate) {
        return Err(Error::UnsupportedFlac(format!(
            "sample rate {} cannot be represented in a FLAC frame header without referring to STREAMINFO",
            spec.sample_rate
        )));
    }

    Ok(())
}

fn summary_from_stream_info(stream_info: StreamInfo, frame_count: usize) -> EncodeSummary {
    EncodeSummary {
        frame_count,
        total_samples: stream_info.total_samples,
        block_size: stream_info.max_block_size,
        min_frame_size: stream_info.min_frame_size,
        max_frame_size: stream_info.max_frame_size,
        min_block_size: stream_info.min_block_size,
        max_block_size: stream_info.max_block_size,
        sample_rate: stream_info.sample_rate,
        channels: stream_info.channels,
        bits_per_sample: stream_info.bits_per_sample,
    }
}

#[cfg(test)]
mod tests {
    use super::EncodeOptions;
    use crate::level::Level;

    #[test]
    fn with_threads_clamps_to_one() {
        assert_eq!(EncodeOptions::default().with_threads(0).threads, 1);
    }

    #[test]
    fn with_level_resets_block_size_to_level_default() {
        let options = EncodeOptions::default()
            .with_block_size(576)
            .with_level(Level::Level6);
        assert_eq!(options.block_size, Level::Level6.profile().block_size);
    }
}
