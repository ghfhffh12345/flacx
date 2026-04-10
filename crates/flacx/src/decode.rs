//! FLAC-to-PCM-container decoding primitives used by the `flacx` crate.
//!
//! The main façade is [`Decoder`]. Pair it with [`DecodeConfig`] or
//! [`Decoder::builder`] to choose worker-thread count, output-family, and
//! channel-mask provenance handling before decoding.

use std::{
    io::{Read, Seek, Write},
    path::Path,
};

use crate::{
    config::{DecodeBuilder, DecodeConfig},
    convenience,
    decode_output::decode_with_output_container,
    error::Result,
    pcm::PcmContainer,
    progress::{NoProgress, ProgressSink},
    read::read_flac_for_decode,
};

#[cfg(feature = "progress")]
use crate::progress::CallbackProgress;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Summary of the PCM stream produced by a decode operation.
///
/// The values mirror the stream information recovered from the FLAC input.
pub struct DecodeSummary {
    /// Number of FLAC frames read from the input stream.
    pub frame_count: usize,
    /// Total output samples reconstructed from the FLAC stream.
    pub total_samples: u64,
    /// Maximum block size recorded in the input stream.
    pub block_size: u16,
    /// Smallest decoded frame size in bytes.
    pub min_frame_size: u32,
    /// Largest decoded frame size in bytes.
    pub max_frame_size: u32,
    /// Smallest decoded block size in samples.
    pub min_block_size: u16,
    /// Largest decoded block size in samples.
    pub max_block_size: u16,
    /// Sample rate of the decoded stream.
    pub sample_rate: u32,
    /// Number of channels in the decoded stream.
    pub channels: u8,
    /// Bits per sample recorded in the decoded stream.
    pub bits_per_sample: u8,
}

/// Primary library façade for FLAC-to-PCM-container conversion.
///
/// Construct a decoder from [`DecodeConfig`] and call one of the decode
/// methods depending on your input shape:
///
/// - [`Decoder::decode`] for generic `Read + Seek` sources
/// - [`Decoder::decode_file`] for file paths
/// - [`Decoder::decode_bytes`] for in-memory input
///
/// The decoder itself is cheap to copy and holds only its configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decoder {
    config: DecodeConfig,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new(DecodeConfig::default())
    }
}

impl Decoder {
    /// Create a builder initialized from [`DecodeConfig::builder`].
    #[must_use]
    pub fn builder() -> DecodeBuilder {
        DecodeConfig::builder()
    }

    /// Construct a decoder from a configuration value.
    #[must_use]
    pub fn new(config: DecodeConfig) -> Self {
        Self { config }
    }

    /// Return the configuration currently stored in the decoder.
    #[must_use]
    pub fn config(&self) -> DecodeConfig {
        self.config
    }

    /// Return a new decoder with a different worker thread count.
    #[must_use]
    pub fn with_threads(self, threads: usize) -> Self {
        Self::new(self.config.with_threads(threads))
    }

    /// Decode a FLAC reader into PCM-container output.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::io::Cursor;
    /// use flacx::Decoder;
    ///
    /// let input = Cursor::new(std::fs::read("input.flac").unwrap());
    /// let mut output = Cursor::new(Vec::new());
    /// Decoder::default().decode(input, &mut output).unwrap();
    /// ```
    pub fn decode<R, W>(&self, input: R, output: W) -> Result<DecodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
    {
        let mut progress = NoProgress;
        self.decode_into(input, output, &mut progress)
    }

    /// Decode a FLAC reader into an explicit typed PCM stream.
    pub fn decode_pcm<R>(&self, input: R) -> Result<crate::PcmStream>
    where
        R: Read + Seek,
    {
        let mut progress = NoProgress;
        Ok(read_flac_for_decode(input, self.config, &mut progress)?.wav)
    }

    /// Decode a FLAC reader into an explicit typed PCM stream.
    ///
    /// This alias keeps the public API spelling aligned with
    /// [`crate::read_pcm_stream`] / [`crate::write_pcm_stream`].
    pub fn decode_pcm_stream<R>(&self, input: R) -> Result<crate::PcmStream>
    where
        R: Read + Seek,
    {
        self.decode_pcm(input)
    }

    #[cfg(feature = "progress")]
    /// Decode a FLAC reader into an explicit typed PCM stream while reporting progress.
    pub fn decode_pcm_with_progress<R, F>(
        &self,
        input: R,
        mut on_progress: F,
    ) -> Result<crate::PcmStream>
    where
        R: Read + Seek,
        F: FnMut(crate::progress::ProgressSnapshot) -> Result<()>,
    {
        let mut progress = CallbackProgress::new(&mut on_progress);
        Ok(read_flac_for_decode(input, self.config, &mut progress)?.wav)
    }

    #[cfg(feature = "progress")]
    /// Decode a FLAC reader into an explicit typed PCM stream while reporting progress.
    ///
    /// This alias keeps the public API spelling aligned with
    /// [`crate::read_pcm_stream`] / [`crate::write_pcm_stream`].
    pub fn decode_pcm_stream_with_progress<R, F>(
        &self,
        input: R,
        on_progress: F,
    ) -> Result<crate::PcmStream>
    where
        R: Read + Seek,
        F: FnMut(crate::progress::ProgressSnapshot) -> Result<()>,
    {
        self.decode_pcm_with_progress(input, on_progress)
    }

    #[cfg(feature = "progress")]
    /// Decode a FLAC reader into PCM-container output while reporting progress.
    ///
    /// The callback receives a [`crate::progress::ProgressSnapshot`] after
    /// each decoded frame.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # #[cfg(feature = "progress")]
    /// # {
    /// use std::io::Cursor;
    /// use flacx::{Decoder, ProgressSnapshot};
    ///
    /// let input = Cursor::new(std::fs::read("input.flac").unwrap());
    /// let mut output = Cursor::new(Vec::new());
    /// Decoder::default().decode_with_progress(input, &mut output, |snapshot: ProgressSnapshot| {
    ///     println!("{} / {} samples", snapshot.processed_samples, snapshot.total_samples);
    ///     Ok(())
    /// }).unwrap();
    /// # }
    /// ```
    pub fn decode_with_progress<R, W, F>(
        &self,
        input: R,
        output: W,
        mut on_progress: F,
    ) -> Result<DecodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
        F: FnMut(crate::progress::ProgressSnapshot) -> Result<()>,
    {
        let mut progress = CallbackProgress::new(&mut on_progress);
        self.decode_into(input, output, &mut progress)
    }

    /// Decode from one file path to another.
    ///
    /// The output is written through a temporary file and committed on success
    /// so the destination is only updated when decoding completes.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use flacx::Decoder;
    ///
    /// Decoder::default()
    ///     .decode_file("input.flac", "output.wav")
    ///     .unwrap();
    /// ```
    pub fn decode_file<P, Q>(&self, input_path: P, output_path: Q) -> Result<DecodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
    {
        convenience::decode_file_with_decoder(self, input_path, output_path)
    }

    #[cfg(feature = "progress")]
    /// Decode from one file path to another while reporting progress.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # #[cfg(feature = "progress")]
    /// # {
    /// use flacx::{Decoder, ProgressSnapshot};
    ///
    /// Decoder::default()
    ///     .decode_file_with_progress("input.flac", "output.wav", |snapshot: ProgressSnapshot| {
    ///         println!("{} / {} frames", snapshot.completed_frames, snapshot.total_frames);
    ///         Ok(())
    ///     })
    ///     .unwrap();
    /// # }
    /// ```
    pub fn decode_file_with_progress<P, Q, F>(
        &self,
        input_path: P,
        output_path: Q,
        mut on_progress: F,
    ) -> Result<DecodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
        F: FnMut(crate::progress::ProgressSnapshot) -> Result<()>,
    {
        let mut progress = CallbackProgress::new(&mut on_progress);
        convenience::decode_file_with_decoder_and_progress(
            self,
            input_path,
            output_path,
            &mut progress,
        )
    }

    /// Decode an in-memory FLAC buffer and return PCM-container bytes.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use flacx::Decoder;
    ///
    /// let flac_bytes = std::fs::read("input.flac").unwrap();
    /// let wav_bytes = Decoder::default().decode_bytes(&flac_bytes).unwrap();
    /// assert!(!wav_bytes.is_empty());
    /// ```
    pub fn decode_bytes(&self, input: &[u8]) -> Result<Vec<u8>> {
        convenience::decode_bytes_with_decoder(self, input)
    }

    fn decode_into<R, W, P>(
        &self,
        input: R,
        mut output: W,
        progress: &mut P,
    ) -> Result<DecodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
        P: ProgressSink,
    {
        self.decode_with_output_container(
            input,
            &mut output,
            self.config.output_container,
            progress,
        )
    }

    /// Decode a FLAC stream into an explicitly selected PCM container.
    pub fn decode_as<R, W>(
        &self,
        input: R,
        mut output: W,
        output_container: PcmContainer,
    ) -> Result<DecodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
    {
        let mut progress = NoProgress;
        self.decode_with_output_container(input, &mut output, output_container, &mut progress)
    }

    pub(crate) fn decode_with_output_container<R, W, P>(
        &self,
        input: R,
        output: &mut W,
        output_container: PcmContainer,
        progress: &mut P,
    ) -> Result<DecodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
        P: ProgressSink,
    {
        decode_with_output_container(input, output, output_container, self.config, progress)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::Decoder;

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn unique_path(extension: &str) -> std::path::PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "flacx-decode-test-{}-{id}.{extension}",
            std::process::id()
        ))
    }

    #[test]
    fn decode_file_cleans_up_temp_output_on_failure() {
        let input_path = unique_path("flac");
        let output_path = unique_path("wav");
        fs::write(&input_path, b"not a flac file").unwrap();

        let result = Decoder::default().decode_file(&input_path, &output_path);
        assert!(result.is_err());
        assert!(!output_path.exists());

        let _ = fs::remove_file(input_path);
        let _ = fs::remove_file(output_path);
    }
}
