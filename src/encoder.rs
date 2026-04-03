use std::{
    collections::BTreeMap,
    io::{Cursor, Read, Seek, Write},
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    pub level: Level,
    pub threads: usize,
    pub block_size: u16,
}

impl Default for EncoderConfig {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Encoder {
    config: EncoderConfig,
}

impl Default for Encoder {
    fn default() -> Self {
        Self {
            config: EncoderConfig::default(),
        }
    }
}

impl Encoder {
    #[must_use]
    pub fn new(config: EncoderConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn with_level(mut self, level: Level) -> Self {
        let profile = level.profile();
        self.config.level = level;
        self.config.block_size = profile.block_size;
        self
    }

    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.config.threads = threads.max(1);
        self
    }

    #[must_use]
    pub fn with_block_size(mut self, block_size: u16) -> Self {
        self.config.block_size = block_size;
        self
    }

    #[must_use]
    pub fn config(&self) -> EncoderConfig {
        self.config
    }

    pub fn encode_wav_to_flac<R, W>(&self, input: R, output: W) -> Result<EncodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
    {
        let wav = read_wav(input)?;
        self.encode_wav_data_to_flac(wav, output)
    }

    pub fn encode_wav_bytes(&self, input: &[u8]) -> Result<Vec<u8>> {
        let mut output = Cursor::new(Vec::new());
        self.encode_wav_to_flac(Cursor::new(input), &mut output)?;
        Ok(output.into_inner())
    }

    fn encode_wav_data_to_flac<W: Write + Seek>(
        &self,
        wav: WavData,
        output: W,
    ) -> Result<EncodeSummary> {
        validate_stream(&wav.spec, self.config.block_size)?;

        let profile = self.config.level.profile();
        let block_size = self.config.block_size;
        let total_frames = if wav.spec.total_samples == 0 {
            0
        } else {
            wav.spec.total_samples.div_ceil(u64::from(block_size)) as usize
        };

        let mut stream_info = StreamInfo::new(
            wav.spec.sample_rate,
            wav.spec.channels,
            wav.spec.bits_per_sample,
            wav.spec.total_samples,
            [0; 16],
        );
        stream_info.min_block_size = block_size;
        stream_info.max_block_size = block_size;

        let mut writer = FlacWriter::new(output, stream_info)?;

        if total_frames == 0 {
            let (_, stream_info) = writer.finalize()?;
            return Ok(summary_from_stream_info(stream_info, 0));
        }

        self.encode_frames_in_parallel(wav.spec, wav.samples, block_size, profile, &mut writer)?;

        let (_, stream_info) = writer.finalize()?;
        Ok(summary_from_stream_info(stream_info, total_frames))
    }

    fn encode_frames_in_parallel<W: Write + Seek>(
        &self,
        spec: WavSpec,
        samples: Vec<i32>,
        block_size: u16,
        profile: LevelProfile,
        writer: &mut FlacWriter<W>,
    ) -> Result<()> {
        let total_frames = spec.total_samples.div_ceil(u64::from(block_size)) as usize;
        let worker_count = self.config.threads.max(1).min(total_frames.max(1));
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
                        writer.write_frame(&frame.bytes)?;
                        next_expected += 1;
                        while let Some(frame) = pending.remove(&next_expected) {
                            writer.write_frame(&frame.bytes)?;
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
