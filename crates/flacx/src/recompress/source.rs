use std::io::{Read, Seek};

use crate::{
    Metadata,
    error::Result,
    input::{EncodePcmStream, PcmSpec},
    read::{DecodePcmStream, FlacReader},
};

use super::verify::VerifyingPcmStream;

/// Reader-to-session handoff for explicit FLAC recompression.
///
/// This type keeps the parsed metadata and the verifying decode stream together
/// until a [`super::session::Recompressor`] consumes them.
pub struct FlacRecompressSource<S> {
    metadata: Metadata,
    stream: VerifyingPcmStream<S>,
}

impl<S> FlacRecompressSource<S>
where
    S: DecodePcmStream,
{
    /// Construct a recompress source directly from shared metadata, a decode stream, and the expected STREAMINFO MD5.
    #[must_use]
    pub fn new(metadata: Metadata, stream: S, expected_streaminfo_md5: [u8; 16]) -> Self {
        Self {
            metadata,
            stream: VerifyingPcmStream::new(stream, expected_streaminfo_md5),
        }
    }

    /// Return the PCM spec that will be fed into the recompress session.
    #[must_use]
    pub fn spec(&self) -> PcmSpec {
        self.stream.spec()
    }

    /// Return the staged encode metadata that will be preserved on recompress.
    #[must_use]
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Replace the staged metadata before recompression begins.
    pub fn set_metadata(&mut self, metadata: Metadata) {
        self.metadata = metadata;
    }

    /// Return a new source with different staged metadata.
    #[must_use]
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Return the total sample count recorded on the input FLAC stream.
    #[must_use]
    pub fn total_samples(&self) -> u64 {
        self.stream.spec().total_samples
    }

    /// Set the worker-thread count used by the decode-side FLAC reader stream.
    pub fn set_threads(&mut self, threads: usize) {
        self.stream.set_threads(threads);
    }

    pub(super) fn into_encode_parts(self) -> (Metadata, VerifyingPcmStream<S>) {
        (self.metadata, self.stream)
    }

    pub(super) fn into_verified_pcm_stream(
        self,
    ) -> Result<(Metadata, crate::input::PcmStream, [u8; 16])> {
        let (pcm_stream, streaminfo_md5) = self.stream.into_verified_pcm_stream()?;
        Ok((self.metadata, pcm_stream, streaminfo_md5))
    }
}

impl<R: Read + Seek> FlacRecompressSource<crate::read::FlacPcmStream<R>> {
    /// Convert an inspected [`FlacReader`] into the single-pass recompress source.
    #[must_use]
    pub(crate) fn from_reader(reader: FlacReader<R>) -> Self {
        let (metadata, stream_info, _spec, stream) = reader.into_session_parts();
        Self::new(metadata, stream, stream_info.md5)
    }
}

impl<S> EncodePcmStream for FlacRecompressSource<S>
where
    S: DecodePcmStream,
{
    fn spec(&self) -> PcmSpec {
        self.stream.spec()
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize> {
        self.stream.read_chunk(max_frames, output)
    }
}
