//! FLAC-to-FLAC recompression primitives used by the `flacx` crate.
//!
//! The main façade is [`Recompressor`]. Pair it with [`RecompressConfig`] or
//! [`Recompressor::builder`] to choose the decode policy and encode settings
//! used when recompressing an existing FLAC stream into a new FLAC stream.

use std::{
    fs::File,
    io::{Cursor, Read, Seek, Write},
    path::Path,
};

use crate::{
    config::{DecodeConfig, EncoderConfig},
    decode::{commit_temp_output, open_temp_output},
    encoder::{EncodeSummary, Encoder},
    error::Result,
    input::EncodeWavData,
    md5::{streaminfo_md5, verify_streaminfo_digest},
    progress::{NoProgress, ProgressSink},
    read::read_flac_for_decode,
};

#[cfg(feature = "progress")]
use crate::progress::CallbackProgress;

/// User-facing recompression configuration for FLAC-to-FLAC conversion.
///
/// This is a shallow composition of the existing decode and encode config
/// types so recompression does not introduce a third bespoke settings surface.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecompressConfig {
    /// Decode-side FLAC policy controls.
    pub decode: DecodeConfig,
    /// Encode-side FLAC output controls.
    pub encode: EncoderConfig,
}

impl RecompressConfig {
    /// Create a fluent builder for [`RecompressConfig`].
    #[must_use]
    pub fn builder() -> RecompressBuilder {
        RecompressBuilder::default()
    }

    /// Replace the nested decode configuration.
    #[must_use]
    pub fn with_decode_config(mut self, decode: DecodeConfig) -> Self {
        self.decode = decode;
        self
    }

    /// Replace the nested encode configuration.
    #[must_use]
    pub fn with_encode_config(mut self, encode: EncoderConfig) -> Self {
        self.encode = encode;
        self
    }
}

/// Fluent builder for [`RecompressConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecompressBuilder {
    config: RecompressConfig,
}

impl RecompressBuilder {
    /// Create a new builder starting from [`RecompressConfig::default`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the nested decode configuration.
    #[must_use]
    pub fn decode_config(mut self, decode: DecodeConfig) -> Self {
        self.config = self.config.with_decode_config(decode);
        self
    }

    /// Replace the nested encode configuration.
    #[must_use]
    pub fn encode_config(mut self, encode: EncoderConfig) -> Self {
        self.config = self.config.with_encode_config(encode);
        self
    }

    /// Finish building the configuration.
    #[must_use]
    pub fn build(self) -> RecompressConfig {
        self.config
    }
}

/// Primary library façade for FLAC-to-FLAC recompression.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Recompressor {
    config: RecompressConfig,
}

impl Recompressor {
    /// Create a builder initialized from [`RecompressConfig::builder`].
    #[must_use]
    pub fn builder() -> RecompressBuilder {
        RecompressConfig::builder()
    }

    /// Construct a recompressor from a configuration value.
    #[must_use]
    pub fn new(config: RecompressConfig) -> Self {
        Self { config }
    }

    /// Return a clone of the configuration currently stored in the recompressor.
    #[must_use]
    pub fn config(&self) -> RecompressConfig {
        self.config.clone()
    }

    /// Return a new recompressor with a different decode configuration.
    #[must_use]
    pub fn with_decode_config(self, decode: DecodeConfig) -> Self {
        Self::new(self.config.with_decode_config(decode))
    }

    /// Return a new recompressor with a different encode configuration.
    #[must_use]
    pub fn with_encode_config(self, encode: EncoderConfig) -> Self {
        Self::new(self.config.with_encode_config(encode))
    }

    /// Recompress a FLAC reader into FLAC output.
    pub fn recompress<R, W>(&self, input: R, output: W) -> Result<EncodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
    {
        let mut progress = NoProgress;
        self.recompress_into(input, output, &mut progress)
    }

    #[cfg(feature = "progress")]
    /// Recompress a FLAC reader into FLAC output while reporting progress.
    pub fn recompress_with_progress<R, W, F>(
        &self,
        input: R,
        output: W,
        mut on_progress: F,
    ) -> Result<EncodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
        F: FnMut(crate::progress::ProgressSnapshot) -> Result<()>,
    {
        let mut progress = CallbackProgress::new(&mut on_progress);
        self.recompress_into(input, output, &mut progress)
    }

    /// Recompress from one file path to another.
    pub fn recompress_file<P, Q>(&self, input_path: P, output_path: Q) -> Result<EncodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
    {
        let mut progress = NoProgress;
        self.recompress_file_with_sink(input_path, output_path, &mut progress)
    }

    #[cfg(feature = "progress")]
    /// Recompress from one file path to another while reporting progress.
    pub fn recompress_file_with_progress<P, Q, F>(
        &self,
        input_path: P,
        output_path: Q,
        mut on_progress: F,
    ) -> Result<EncodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
        F: FnMut(crate::progress::ProgressSnapshot) -> Result<()>,
    {
        let mut progress = CallbackProgress::new(&mut on_progress);
        self.recompress_file_with_sink(input_path, output_path, &mut progress)
    }

    /// Recompress an in-memory FLAC buffer and return the FLAC bytes.
    pub fn recompress_bytes(&self, input: &[u8]) -> Result<Vec<u8>> {
        let mut output = Cursor::new(Vec::new());
        self.recompress(Cursor::new(input), &mut output)?;
        Ok(output.into_inner())
    }

    pub(crate) fn recompress_into<R, W, P>(
        &self,
        input: R,
        output: W,
        progress: &mut P,
    ) -> Result<EncodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
        P: ProgressSink,
    {
        let decoded = read_flac_for_decode(input, self.config.decode, &mut NoProgress)?;
        let streaminfo_md5 = streaminfo_md5(decoded.wav.spec, &decoded.wav.samples)?;
        verify_streaminfo_digest(streaminfo_md5, decoded.stream_info.md5)?;
        let recompress_input = EncodeWavData {
            streaminfo_md5,
            wav: decoded.wav,
            metadata: decoded.metadata.into_encode_metadata(),
        };
        Encoder::new(self.config.encode.clone()).encode_wav_data(recompress_input, output, progress)
    }

    fn recompress_file_with_sink<P, Q, R>(
        &self,
        input_path: P,
        output_path: Q,
        progress: &mut R,
    ) -> Result<EncodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
        R: ProgressSink,
    {
        let input_path = input_path.as_ref();
        let output_path = output_path.as_ref();
        let (temp_path, temp_file) = open_temp_output(output_path)?;

        let result = (|| {
            let input = File::open(input_path)?;
            self.recompress_into(input, temp_file, progress)
        })();
        match result {
            Ok(summary) => {
                if let Err(error) = commit_temp_output(&temp_path, output_path) {
                    let _ = std::fs::remove_file(&temp_path);
                    return Err(error);
                }
                Ok(summary)
            }
            Err(error) => {
                let _ = std::fs::remove_file(&temp_path);
                Err(error)
            }
        }
    }
}

/// Convenience wrapper around the default [`Recompressor`] for file-path input.
pub fn recompress_file<P, Q>(input_path: P, output_path: Q) -> Result<EncodeSummary>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    Recompressor::default().recompress_file(input_path, output_path)
}

/// Convenience wrapper around the default [`Recompressor`] for in-memory input.
pub fn recompress_bytes(input: &[u8]) -> Result<Vec<u8>> {
    Recompressor::default().recompress_bytes(input)
}
