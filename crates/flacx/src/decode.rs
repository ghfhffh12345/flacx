//! FLAC-to-WAV decoding primitives used by the `flacx` crate.
//!
//! The main façade is [`Decoder`]. Pair it with [`DecodeConfig`] or
//! [`Decoder::builder`] to choose worker-thread count and channel-mask
//! provenance handling before decoding.

use std::{
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{
    config::{DecodeBuilder, DecodeConfig},
    error::{Error, Result},
    md5::verify_streaminfo_digest,
    pcm::PcmContainer,
    progress::{NoProgress, ProgressSink},
    read::read_flac_for_decode,
    stream_info::StreamInfo,
    wav_output::{WavMetadataWriteOptions, write_wav_with_metadata_and_md5_with_options},
};

#[cfg(feature = "progress")]
use crate::progress::CallbackProgress;

static TEMP_OUTPUT_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Summary of the WAV stream produced by a decode operation.
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

/// Primary library façade for FLAC-to-WAV conversion.
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

    /// Decode a FLAC reader into WAV output.
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

    #[cfg(feature = "progress")]
    /// Decode a FLAC reader into WAV output while reporting progress.
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
        let mut progress = NoProgress;
        self.decode_file_with_sink(input_path, output_path, &mut progress)
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
        self.decode_file_with_sink(input_path, output_path, &mut progress)
    }

    /// Decode an in-memory FLAC buffer and return the WAV bytes.
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
        let mut output = Cursor::new(Vec::new());
        self.decode(Cursor::new(input), &mut output)?;
        Ok(output.into_inner())
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
        self.decode_into_with_container(input, &mut output, progress, self.config.output_container)
    }

    fn decode_into_with_container<R, W, P>(
        &self,
        input: R,
        output: &mut W,
        progress: &mut P,
        output_container: PcmContainer,
    ) -> Result<DecodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
        P: ProgressSink,
    {
        let decoded = read_flac_for_decode(input, self.config, progress)?;
        let streaminfo_md5 = write_wav_with_metadata_and_md5_with_options(
            output,
            decoded.wav.spec,
            &decoded.wav.samples,
            &decoded.metadata,
            WavMetadataWriteOptions {
                emit_fxmd: self.config.emit_fxmd,
                container: output_container,
            },
        )?;
        verify_streaminfo_digest(streaminfo_md5, decoded.stream_info.md5)?;
        output.flush()?;
        Ok(summary_from_stream_info(
            decoded.stream_info,
            decoded.frame_count,
        ))
    }

    fn decode_file_with_sink<P, Q, R>(
        &self,
        input_path: P,
        output_path: Q,
        progress: &mut R,
    ) -> Result<DecodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
        R: ProgressSink,
    {
        let input_path = input_path.as_ref();
        let output_path = output_path.as_ref();
        let (temp_path, temp_file) = open_temp_output(output_path)?;
        let output_container = output_container_from_path(output_path)
            .unwrap_or(self.config.output_container);

        let result = (|| {
            let input = File::open(input_path)?;
            let mut temp_file = temp_file;
            self.decode_into_with_container(input, &mut temp_file, progress, output_container)
        })();
        match result {
            Ok(summary) => {
                if let Err(error) = commit_temp_output(&temp_path, output_path) {
                    let _ = fs::remove_file(&temp_path);
                    return Err(error);
                }
                Ok(summary)
            }
            Err(error) => {
                let _ = fs::remove_file(&temp_path);
                Err(error)
            }
        }
    }
}

fn output_container_from_path(path: &Path) -> Option<PcmContainer> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("rf64") => Some(PcmContainer::Rf64),
        Some(ext) if ext.eq_ignore_ascii_case("w64") => Some(PcmContainer::Wave64),
        Some(ext) if ext.eq_ignore_ascii_case("wav") => Some(PcmContainer::Wave),
        _ => None,
    }
}

/// Convenience wrapper around the default [`Decoder`] for file-path input.
///
/// # Example
///
/// ```no_run
/// use flacx::decode_file;
///
/// decode_file("input.flac", "output.wav").unwrap();
/// ```
pub fn decode_file<P, Q>(input_path: P, output_path: Q) -> Result<DecodeSummary>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    Decoder::default().decode_file(input_path, output_path)
}

/// Convenience wrapper around the default [`Decoder`] for in-memory input.
///
/// # Example
///
/// ```no_run
/// use flacx::decode_bytes;
///
/// let flac_bytes = std::fs::read("input.flac").unwrap();
/// let wav_bytes = decode_bytes(&flac_bytes).unwrap();
/// assert!(!wav_bytes.is_empty());
/// ```
pub fn decode_bytes(input: &[u8]) -> Result<Vec<u8>> {
    Decoder::default().decode_bytes(input)
}

pub(crate) fn open_temp_output(output_path: &Path) -> Result<(PathBuf, File)> {
    let parent = output_path.parent().unwrap_or_else(|| Path::new("."));
    let pid = std::process::id();

    for _ in 0..1_024 {
        let suffix = TEMP_OUTPUT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_name = format!(".flacx-{pid}-{suffix}.tmp");
        let temp_path = parent.join(temp_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }

    Err(Error::Thread(
        "failed to allocate a temporary output path".into(),
    ))
}

pub(crate) fn commit_temp_output(temp_path: &Path, output_path: &Path) -> Result<()> {
    match fs::rename(temp_path, output_path) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            let _ = fs::remove_file(output_path);
            fs::rename(temp_path, output_path)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn summary_from_stream_info(stream_info: StreamInfo, frame_count: usize) -> DecodeSummary {
    DecodeSummary {
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
