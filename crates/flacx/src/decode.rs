//! FLAC-to-PCM-container decoding session primitives used by the `flacx` crate.
//!
//! The public decode flow can be reader-driven or direct-stream-driven: parse
//! a FLAC reader or construct a [`crate::FlacPcmStream`] explicitly, stage
//! metadata in a [`DecodeSource`], bind an output writer through
//! [`DecodeConfig::into_decoder`], then feed that source into
//! [`Decoder::decode_source`].

use std::io::{Seek, Write};

use crate::{
    config::{DecodeBuilder, DecodeConfig},
    decode_output::decode_stream_to_container,
    error::Result,
    metadata::Metadata,
    progress::{NoProgress, ProgressSink},
    read::{DecodePcmStream, DecodeSource},
};

#[cfg(feature = "progress")]
use crate::progress::{CallbackProgress, ProgressSnapshot};

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

/// Writer-owning FLAC-to-PCM-container decode session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoder<W> {
    config: DecodeConfig,
    writer: W,
}

impl DecodeConfig {
    /// Bind an output writer and create a writer-owning decode session.
    pub fn into_decoder<W>(self, writer: W) -> Decoder<W>
    where
        W: Write + Seek,
    {
        Decoder::new(writer, self)
    }
}

impl DecodeBuilder {
    /// Finish building the configuration and bind an output writer.
    pub fn into_decoder<W>(self, writer: W) -> Decoder<W>
    where
        W: Write + Seek,
    {
        self.build().into_decoder(writer)
    }
}

impl<W> Decoder<W>
where
    W: Write + Seek,
{
    /// Construct a writer-owning decode session from a writer and config.
    #[must_use]
    pub fn new(writer: W, config: DecodeConfig) -> Self {
        Self { config, writer }
    }

    /// Return the configuration currently stored in the decode session.
    #[must_use]
    pub fn config(&self) -> DecodeConfig {
        self.config
    }

    /// Return a new decoder with a different worker thread count.
    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.config = self.config.with_threads(threads);
        self
    }

    /// Return a new decoder with a different output container policy.
    #[must_use]
    pub fn with_output_container(mut self, output_container: crate::PcmContainer) -> Self {
        self.config = self.config.with_output_container(output_container);
        self
    }

    /// Return the owned output writer by value.
    pub fn into_inner(self) -> W {
        self.writer
    }

    /// Decode a PCM stream into the owned writer.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use flacx::{DecodeConfig, read_flac_reader};
    ///
    /// let reader = read_flac_reader(std::fs::File::open("input.flac").unwrap()).unwrap();
    /// let source = reader.into_decode_source();
    /// let mut decoder = DecodeConfig::default()
    ///     .into_decoder(std::io::Cursor::new(Vec::new()));
    /// decoder.decode_source(source).unwrap();
    /// ```
    pub fn decode<S>(&mut self, mut stream: S) -> Result<DecodeSummary>
    where
        S: DecodePcmStream,
    {
        let mut progress = NoProgress;
        stream.set_threads(self.config.threads());
        self.decode_with_sink(stream, &mut progress)
    }

    /// Decode an owned source into the owned writer.
    pub fn decode_source<S>(&mut self, source: DecodeSource<S>) -> Result<DecodeSummary>
    where
        S: DecodePcmStream,
    {
        let mut progress = NoProgress;
        self.decode_source_with_sink(source, &mut progress)
    }

    #[cfg(feature = "progress")]
    /// Decode a PCM stream into the owned writer while reporting progress.
    pub fn decode_with_progress<S, F>(
        &mut self,
        mut stream: S,
        mut on_progress: F,
    ) -> Result<DecodeSummary>
    where
        S: DecodePcmStream,
        F: FnMut(ProgressSnapshot) -> Result<()>,
    {
        let mut progress = CallbackProgress::new(&mut on_progress);
        stream.set_threads(self.config.threads());
        self.decode_with_sink(stream, &mut progress)
    }

    #[cfg(feature = "progress")]
    /// Decode an owned source into the owned writer while reporting progress.
    pub fn decode_source_with_progress<S, F>(
        &mut self,
        source: DecodeSource<S>,
        mut on_progress: F,
    ) -> Result<DecodeSummary>
    where
        S: DecodePcmStream,
        F: FnMut(ProgressSnapshot) -> Result<()>,
    {
        let mut progress = CallbackProgress::new(&mut on_progress);
        self.decode_source_with_sink(source, &mut progress)
    }

    pub(crate) fn decode_with_sink<S, P>(
        &mut self,
        stream: S,
        progress: &mut P,
    ) -> Result<DecodeSummary>
    where
        S: DecodePcmStream,
        P: ProgressSink,
    {
        self.decode_source_with_sink(DecodeSource::new(Metadata::default(), stream), progress)
    }

    pub(crate) fn decode_source_with_sink<S, P>(
        &mut self,
        source: DecodeSource<S>,
        progress: &mut P,
    ) -> Result<DecodeSummary>
    where
        S: DecodePcmStream,
        P: ProgressSink,
    {
        let (mut metadata, mut stream) = source.into_parts();
        crate::metadata::align_metadata_to_stream_spec(
            &mut metadata,
            stream.spec(),
            self.config.strict_channel_mask_provenance(),
        )?;
        stream.set_threads(self.config.threads());
        decode_stream_to_container(stream, &mut self.writer, metadata, self.config, progress)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use crate::read_flac_reader;

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

        let result = (|| {
            let reader = read_flac_reader(fs::File::open(&input_path)?)?;
            let source = reader.into_decode_source();
            let mut decoder =
                crate::DecodeConfig::default().into_decoder(fs::File::create(&output_path)?);
            decoder.decode_source(source)
        })();
        assert!(result.is_err());
        assert!(!output_path.exists());

        let _ = fs::remove_file(input_path);
        let _ = fs::remove_file(output_path);
    }
}
