use std::io::{Read, Seek};

use crate::{
    EncodeMetadata,
    error::Result,
    input::{EncodePcmStream, PcmStream, WavSpec},
    read::FlacReader,
};

use super::verify::VerifyingPcmStream;

/// Reader-to-session handoff for explicit FLAC recompression.
pub struct FlacRecompressSource<R> {
    metadata: EncodeMetadata,
    total_samples: u64,
    stream: VerifyingPcmStream<R>,
}

impl<R: Read + Seek> FlacRecompressSource<R> {
    /// Convert an inspected [`FlacReader`] into the single-pass recompress source.
    #[must_use]
    pub fn from_reader(reader: FlacReader<R>) -> Self {
        let (metadata, stream_info, spec, stream) = reader.into_session_parts();
        Self {
            metadata: metadata.into_encode_metadata(),
            total_samples: spec.total_samples,
            stream: VerifyingPcmStream::new(stream, stream_info.md5),
        }
    }

    /// Return the PCM spec that will be fed into the recompress session.
    #[must_use]
    pub fn spec(&self) -> WavSpec {
        self.stream.spec()
    }

    /// Return the staged encode metadata that will be preserved on recompress.
    #[must_use]
    pub fn metadata(&self) -> &EncodeMetadata {
        &self.metadata
    }

    /// Replace the staged metadata before recompression begins.
    pub fn set_metadata(&mut self, metadata: EncodeMetadata) {
        self.metadata = metadata;
    }

    /// Return a new source with different staged metadata.
    #[must_use]
    pub fn with_metadata(mut self, metadata: EncodeMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Return the total sample count recorded on the input FLAC stream.
    #[must_use]
    pub fn total_samples(&self) -> u64 {
        self.total_samples
    }

    /// Set the worker-thread count used by the decode-side FLAC reader stream.
    pub fn set_threads(&mut self, threads: usize) {
        self.stream.set_threads(threads);
    }

    pub(super) fn into_verified_pcm_stream(self) -> Result<(EncodeMetadata, PcmStream, [u8; 16])> {
        let (pcm_stream, streaminfo_md5) = self.stream.into_verified_pcm_stream()?;
        Ok((self.metadata, pcm_stream, streaminfo_md5))
    }
}

impl<R: Read + Seek> EncodePcmStream for FlacRecompressSource<R> {
    fn spec(&self) -> WavSpec {
        self.stream.spec()
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize> {
        self.stream.read_chunk(max_frames, output)
    }
}
