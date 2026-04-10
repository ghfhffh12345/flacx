//! PCM-container-to-FLAC encoding primitives used by the `flacx` crate.
//!
//! The main façade is [`Encoder`]. Pair it with [`EncoderConfig`] or
//! [`Encoder::builder`] to choose the compression level, thread count, and
//! optional block sizing strategy before encoding.

use std::{
    io::{Read, Seek, Write},
    path::Path,
};

use crate::{
    config::{EncoderBuilder, EncoderConfig},
    convenience,
    encode_pipeline::encode_prepared,
    error::Result,
    input::read_pcm_for_encode_with_config,
    md5::streaminfo_md5,
    metadata::EncodeMetadata,
    progress::{NoProgress, ProgressSink},
    raw::{RawPcmDescriptor, read_raw_for_encode},
};

#[cfg(feature = "progress")]
use crate::progress::ProgressSnapshot;

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

/// Primary library façade for PCM-container-to-FLAC conversion.
///
/// Construct an encoder from [`EncoderConfig`] and call one of the encode
/// methods depending on your input shape:
///
/// - [`Encoder::encode`] for generic `Read + Seek` sources
/// - [`Encoder::encode_file`] for file paths
/// - [`Encoder::encode_bytes`] for in-memory input
/// - [`Encoder::encode_raw`] / [`Encoder::encode_raw_file`] for explicit raw
///   PCM descriptors
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

    /// Encode a supported PCM-container reader into FLAC output.
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
        let mut input = input;
        let input = read_pcm_for_encode_with_config(&mut input, &self.config)?;
        let mut progress = NoProgress;
        self.encode_wav_data(input, output, &mut progress)
    }

    #[cfg(feature = "progress")]
    /// Encode a supported PCM-container reader into FLAC output while reporting progress.
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
        let mut input = input;
        let input = read_pcm_for_encode_with_config(&mut input, &self.config)?;
        let mut progress = crate::progress::CallbackProgress::new(&mut on_progress);
        self.encode_wav_data(input, output, &mut progress)
    }

    /// Encode raw signed-integer PCM from a seekable reader into FLAC output.
    pub fn encode_raw<R, W>(
        &self,
        input: R,
        output: W,
        descriptor: RawPcmDescriptor,
    ) -> Result<EncodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
    {
        let mut input = input;
        let input = read_raw_for_encode(&mut input, descriptor)?;
        let mut progress = NoProgress;
        self.encode_wav_data(input, output, &mut progress)
    }

    /// Encode an explicit typed PCM stream into FLAC output.
    pub fn encode_pcm<W>(&self, input: crate::PcmStream, output: W) -> Result<EncodeSummary>
    where
        W: Write + Seek,
    {
        let streaminfo_md5 = streaminfo_md5(input.spec, &input.samples)?;
        let prepared = crate::input::EncodeWavData {
            wav: input,
            metadata: EncodeMetadata::default(),
            streaminfo_md5,
        };
        let mut progress = NoProgress;
        self.encode_wav_data(prepared, output, &mut progress)
    }

    /// Encode an explicit typed PCM stream into FLAC output.
    ///
    /// This alias keeps the public API spelling aligned with
    /// [`crate::read_pcm_stream`] / [`crate::write_pcm_stream`].
    pub fn encode_pcm_stream<W>(&self, input: &crate::PcmStream, output: W) -> Result<EncodeSummary>
    where
        W: Write + Seek,
    {
        self.encode_pcm(input.clone(), output)
    }

    #[cfg(feature = "progress")]
    /// Encode an explicit typed PCM stream into FLAC output while reporting progress.
    pub fn encode_pcm_with_progress<W, F>(
        &self,
        input: crate::PcmStream,
        output: W,
        mut on_progress: F,
    ) -> Result<EncodeSummary>
    where
        W: Write + Seek,
        F: FnMut(ProgressSnapshot) -> Result<()>,
    {
        let streaminfo_md5 = streaminfo_md5(input.spec, &input.samples)?;
        let prepared = crate::input::EncodeWavData {
            wav: input,
            metadata: EncodeMetadata::default(),
            streaminfo_md5,
        };
        let mut progress = crate::progress::CallbackProgress::new(&mut on_progress);
        self.encode_wav_data(prepared, output, &mut progress)
    }

    #[cfg(feature = "progress")]
    /// Encode an explicit typed PCM stream into FLAC output while reporting progress.
    ///
    /// This alias keeps the public API spelling aligned with
    /// [`crate::read_pcm_stream`] / [`crate::write_pcm_stream`].
    pub fn encode_pcm_stream_with_progress<W, F>(
        &self,
        input: &crate::PcmStream,
        output: W,
        on_progress: F,
    ) -> Result<EncodeSummary>
    where
        W: Write + Seek,
        F: FnMut(ProgressSnapshot) -> Result<()>,
    {
        self.encode_pcm_with_progress(input.clone(), output, on_progress)
    }

    #[cfg(feature = "progress")]
    /// Encode raw signed-integer PCM into FLAC output while reporting progress.
    pub fn encode_raw_with_progress<R, W, F>(
        &self,
        input: R,
        output: W,
        descriptor: RawPcmDescriptor,
        mut on_progress: F,
    ) -> Result<EncodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
        F: FnMut(ProgressSnapshot) -> Result<()>,
    {
        let mut input = input;
        let input = read_raw_for_encode(&mut input, descriptor)?;
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
        convenience::encode_file_with_encoder(self, input_path, output_path)
    }

    /// Encode raw signed-integer PCM from one file path to another.
    pub fn encode_raw_file<P, Q>(
        &self,
        input_path: P,
        output_path: Q,
        descriptor: RawPcmDescriptor,
    ) -> Result<EncodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
    {
        convenience::encode_raw_file_with_encoder(self, input_path, output_path, descriptor)
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
        convenience::encode_file_with_progress(self, input_path, output_path, on_progress)
    }

    #[cfg(feature = "progress")]
    /// Encode raw signed-integer PCM from one file path to another while
    /// reporting progress.
    pub fn encode_raw_file_with_progress<P, Q, F>(
        &self,
        input_path: P,
        output_path: Q,
        descriptor: RawPcmDescriptor,
        on_progress: F,
    ) -> Result<EncodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
        F: FnMut(ProgressSnapshot) -> Result<()>,
    {
        convenience::encode_raw_file_with_progress(
            self,
            input_path,
            output_path,
            descriptor,
            on_progress,
        )
    }

    /// Encode an in-memory supported PCM-container buffer and return the FLAC bytes.
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
        convenience::encode_bytes_with_encoder(self, input)
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
        encode_prepared(&self.config, input, output, progress)
    }
}
