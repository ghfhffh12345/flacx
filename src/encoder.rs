use std::{
    io::{Cursor, Read, Seek, Write},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
        Arc,
    },
    thread,
};

use crate::{
    error::{Error, Result},
    flac_writer::FlacWriter,
    frame::{encode_frame, sample_rate_is_representable, EncodedFrame},
    level::{Level, LevelProfile},
    metadata::StreamInfo,
    wav::{read_wav, WavData, WavSpec},
};

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

    fn encode_wav_data_to_flac<W: Write + Seek>(&self, wav: WavData, output: W) -> Result<EncodeSummary> {
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

        let encoded_frames = self.encode_frames_in_parallel(&wav, block_size, profile)?;
        for frame in encoded_frames {
            writer.write_frame(&frame.bytes)?;
        }

        let (_, stream_info) = writer.finalize()?;
        Ok(summary_from_stream_info(stream_info, total_frames))
    }

    fn encode_frames_in_parallel(
        &self,
        wav: &WavData,
        block_size: u16,
        profile: LevelProfile,
    ) -> Result<Vec<EncodedFrame>> {
        let total_frames = wav.spec.total_samples.div_ceil(u64::from(block_size)) as usize;
        let worker_count = self.config.threads.max(1).min(total_frames.max(1));
        let next_frame = Arc::new(AtomicUsize::new(0));
        let samples = Arc::new(wav.samples.clone());
        let channels = usize::from(wav.spec.channels);
        let sample_rate = wav.spec.sample_rate;
        let bits_per_sample = wav.spec.bits_per_sample;
        let total_samples = wav.spec.total_samples;
        let max_fixed_order = profile.max_fixed_order.min(4);

        let mut results: Vec<Option<Result<EncodedFrame>>> = (0..total_frames).map(|_| None).collect();

        thread::scope(|scope| -> Result<()> {
            let (sender, receiver) = mpsc::channel();
            for _ in 0..worker_count {
                let sender = sender.clone();
                let next_frame = Arc::clone(&next_frame);
                let samples = Arc::clone(&samples);
                let channels_in_scope = channels;
                let wav_channels = wav.spec.channels;

                scope.spawn(move || {
                    loop {
                        let frame_index = next_frame.fetch_add(1, Ordering::Relaxed);
                        if frame_index >= total_frames {
                            break;
                        }

                        let start_sample = frame_index as u64 * u64::from(block_size);
                        let frame_samples = (total_samples - start_sample).min(u64::from(block_size)) as usize;
                        let sample_start = frame_index * usize::from(block_size) * channels_in_scope;
                        let sample_end = sample_start + frame_samples * channels_in_scope;
                        let frame = encode_frame(
                            &samples[sample_start..sample_end],
                            wav_channels,
                            bits_per_sample,
                            sample_rate,
                            frame_index as u64,
                            max_fixed_order,
                        );
                        if sender.send((frame_index, frame)).is_err() {
                            break;
                        }
                    }
                });
            }

            drop(sender);
            for _ in 0..total_frames {
                let (frame_index, frame) = receiver.recv().map_err(|_| {
                    Error::Thread("frame worker channel closed before all frames were encoded".into())
                })?;
                results[frame_index] = Some(frame);
            }

            Ok(())
        })?;

        results
            .into_iter()
            .map(|frame| frame.ok_or_else(|| Error::Thread("missing encoded frame".into()))?)
            .collect()
    }
}

fn validate_stream(spec: &WavSpec, block_size: u16) -> Result<()> {
    if spec.sample_rate == 0 {
        return Err(Error::UnsupportedFlac("sample rate 0 is not allowed".into()));
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
