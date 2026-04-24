use std::io::{Read, Seek, SeekFrom};

use crate::error::{Error, Result};
use crate::metadata::Metadata;

pub(crate) use crate::pcm::{
    PcmEnvelope, append_encoded_sample, container_bits_from_valid_bits, ordinary_channel_mask,
};
pub use crate::pcm::{PcmSpec, PcmStream};
pub(crate) type PcmReaderOptions = crate::wav_input::WavReaderOptions;

pub(crate) enum EncodeChunkPayload {
    DecodedSamples(Vec<i32>),
    PackedPcm {
        bytes: Vec<u8>,
        frame_count: usize,
        envelope: PcmEnvelope,
    },
}

impl EncodeChunkPayload {
    pub(crate) fn pcm_frames(&self, channels: usize) -> usize {
        match self {
            Self::DecodedSamples(samples) => samples.len() / channels,
            Self::PackedPcm { frame_count, .. } => *frame_count,
        }
    }

    pub(crate) fn decoded_samples_len(&self) -> Option<usize> {
        match self {
            Self::DecodedSamples(samples) => Some(samples.len()),
            Self::PackedPcm { .. } => None,
        }
    }

    pub(crate) fn clear_for_reuse(self) -> Self {
        match self {
            Self::DecodedSamples(mut samples) => {
                samples.clear();
                Self::DecodedSamples(samples)
            }
            Self::PackedPcm {
                mut bytes,
                envelope,
                ..
            } => {
                bytes.clear();
                Self::PackedPcm {
                    bytes,
                    frame_count: 0,
                    envelope,
                }
            }
        }
    }
}

/// Single-pass PCM sample source consumed by the encode session.
pub trait EncodePcmStream {
    /// Return the stream specification that drives encode planning.
    fn spec(&self) -> PcmSpec;

    /// Read up to `max_frames` interleaved PCM frames into `output`.
    ///
    /// Returns the number of frames appended to `output`.
    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize>;

    #[doc(hidden)]
    #[allow(private_interfaces)]
    fn read_chunk_payload(
        &mut self,
        max_frames: usize,
        reuse: Option<EncodeChunkPayload>,
    ) -> Result<EncodeChunkPayload> {
        let mut output = match reuse {
            Some(EncodeChunkPayload::DecodedSamples(samples)) => samples,
            Some(_) | None => Vec::new(),
        };
        output.clear();
        let frames = self.read_chunk(max_frames, &mut output)?;
        debug_assert_eq!(frames, output.len() / usize::from(self.spec().channels));
        Ok(EncodeChunkPayload::DecodedSamples(output))
    }

    #[cfg(feature = "progress")]
    fn input_bytes_processed(&self) -> u64 {
        0
    }

    fn update_streaminfo_md5(
        &mut self,
        md5: &mut crate::md5::StreaminfoMd5,
        samples: &[i32],
    ) -> Result<()> {
        md5.update_samples(samples)
    }

    #[doc(hidden)]
    #[allow(private_interfaces)]
    fn update_streaminfo_md5_for_payload(
        &mut self,
        md5: &mut crate::md5::StreaminfoMd5,
        payload: &EncodeChunkPayload,
    ) -> Result<()> {
        match payload {
            EncodeChunkPayload::DecodedSamples(samples) => self.update_streaminfo_md5(md5, samples),
            EncodeChunkPayload::PackedPcm { bytes, .. } => {
                md5.update_bytes(bytes);
                Ok(())
            }
        }
    }

    fn finish_streaminfo_md5(&mut self, md5: crate::md5::StreaminfoMd5) -> Result<[u8; 16]> {
        md5.finalize()
    }

    fn preferred_encode_chunk_max_frames(&self) -> Option<usize> {
        None
    }

    fn preferred_encode_chunk_target_pcm_frames(&self) -> Option<usize> {
        None
    }
}

#[cfg(feature = "progress")]
pub(crate) struct CountedEncodePcmStream<S> {
    stream: S,
    input_bytes_processed: u64,
}

#[cfg(feature = "progress")]
impl<S> CountedEncodePcmStream<S> {
    pub(crate) fn new(stream: S) -> Self {
        Self {
            stream,
            input_bytes_processed: 0,
        }
    }
}

#[cfg(not(feature = "progress"))]
pub(crate) type CountedEncodePcmStream<S> = S;

#[cfg(feature = "progress")]
pub(crate) fn counted_encode_pcm_stream<S>(stream: S) -> CountedEncodePcmStream<S> {
    CountedEncodePcmStream::new(stream)
}

#[cfg(not(feature = "progress"))]
pub(crate) fn counted_encode_pcm_stream<S>(stream: S) -> CountedEncodePcmStream<S> {
    stream
}

#[cfg(feature = "progress")]
impl<S: EncodePcmStream> EncodePcmStream for CountedEncodePcmStream<S> {
    fn spec(&self) -> PcmSpec {
        self.stream.spec()
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize> {
        let frames = self.stream.read_chunk(max_frames, output)?;
        self.input_bytes_processed = self
            .input_bytes_processed
            .saturating_add(pcm_bytes_for_frames(self.stream.spec(), frames));
        Ok(frames)
    }

    #[allow(private_interfaces)]
    fn read_chunk_payload(
        &mut self,
        max_frames: usize,
        reuse: Option<EncodeChunkPayload>,
    ) -> Result<EncodeChunkPayload> {
        let payload = self.stream.read_chunk_payload(max_frames, reuse)?;
        self.input_bytes_processed = self.input_bytes_processed.saturating_add(pcm_bytes_for_frames(
            self.stream.spec(),
            payload.pcm_frames(usize::from(self.stream.spec().channels)),
        ));
        Ok(payload)
    }

    #[cfg(feature = "progress")]
    fn input_bytes_processed(&self) -> u64 {
        self.input_bytes_processed
    }

    fn update_streaminfo_md5(
        &mut self,
        md5: &mut crate::md5::StreaminfoMd5,
        samples: &[i32],
    ) -> Result<()> {
        self.stream.update_streaminfo_md5(md5, samples)
    }

    #[allow(private_interfaces)]
    fn update_streaminfo_md5_for_payload(
        &mut self,
        md5: &mut crate::md5::StreaminfoMd5,
        payload: &EncodeChunkPayload,
    ) -> Result<()> {
        self.stream.update_streaminfo_md5_for_payload(md5, payload)
    }

    fn finish_streaminfo_md5(&mut self, md5: crate::md5::StreaminfoMd5) -> Result<[u8; 16]> {
        self.stream.finish_streaminfo_md5(md5)
    }

    fn preferred_encode_chunk_max_frames(&self) -> Option<usize> {
        self.stream.preferred_encode_chunk_max_frames()
    }

    fn preferred_encode_chunk_target_pcm_frames(&self) -> Option<usize> {
        self.stream.preferred_encode_chunk_target_pcm_frames()
    }
}

/// Owned encode-side handoff that keeps metadata and the PCM stream together.
///
/// Most explicit encode workflows construct one of these from a reader's
/// [`into_source`](PcmReader::into_source) method and then pass it to
/// [`crate::Encoder::encode_source`].
pub struct EncodeSource<S> {
    metadata: Metadata,
    stream: S,
}

impl<S> EncodeSource<S> {
    /// Create a new encode source from staged metadata and a PCM stream.
    #[must_use]
    pub fn new(metadata: Metadata, stream: S) -> Self {
        Self { metadata, stream }
    }

    /// Return the staged encode metadata.
    #[must_use]
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Return mutable access to the staged encode metadata.
    pub fn metadata_mut(&mut self) -> &mut Metadata {
        &mut self.metadata
    }

    /// Replace the staged metadata and return the updated source.
    #[must_use]
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Consume the source and return the metadata and stream.
    pub fn into_parts(self) -> (Metadata, S) {
        (self.metadata, self.stream)
    }
}

impl<S: EncodePcmStream> EncodeSource<S> {
    /// Return the PCM spec that will drive encode planning.
    #[must_use]
    pub fn spec(&self) -> PcmSpec {
        self.stream.spec()
    }
}

/// Family-dispatched PCM reader for the explicit encode workflow.
///
/// Use `PcmReader` when the input family is only known at runtime. If you know
/// the container ahead of time, prefer the more specific reader types such as
/// [`crate::WavReader`] or [`crate::AiffReader`].
#[derive(Debug)]
pub enum PcmReader<R: Read + Seek> {
    Wav(crate::wav_input::WavReader<R>),
    #[cfg(feature = "aiff")]
    Aiff(crate::aiff::AiffReader<R>),
    #[cfg(feature = "caf")]
    Caf(crate::caf::CafReader<R>),
}

impl<R: Read + Seek> PcmReader<R> {
    /// Parse a supported PCM container using explicit constructor-driven dispatch.
    pub fn new(reader: R) -> Result<Self> {
        Self::with_options(reader, PcmReaderOptions::default())
    }

    /// Parse a supported PCM container while applying RIFF/WAVE-family reader options.
    ///
    /// The options only affect WAV/RF64/Wave64 inputs. AIFF/AIFC and CAF
    /// inputs ignore them because their reader families do not share that
    /// policy surface.
    pub fn with_reader_options(reader: R, options: crate::WavReaderOptions) -> Result<Self> {
        Self::with_options(reader, options)
    }

    /// Return the parsed PCM specification without consuming the sample stream.
    #[must_use]
    pub fn spec(&self) -> PcmSpec {
        match self {
            Self::Wav(reader) => reader.spec(),
            #[cfg(feature = "aiff")]
            Self::Aiff(reader) => reader.spec(),
            #[cfg(feature = "caf")]
            Self::Caf(reader) => reader.spec(),
        }
    }

    /// Return the parsed encode-side metadata captured from the input.
    #[must_use]
    pub fn metadata(&self) -> &Metadata {
        match self {
            Self::Wav(reader) => reader.metadata(),
            #[cfg(feature = "aiff")]
            Self::Aiff(reader) => reader.metadata(),
            #[cfg(feature = "caf")]
            Self::Caf(reader) => reader.metadata(),
        }
    }

    /// Convert the parsed reader into an owned encode source.
    pub fn into_source(self) -> EncodeSource<impl EncodePcmStream> {
        let (metadata, stream) = self.into_session_parts();
        EncodeSource::new(metadata, stream)
    }

    #[allow(dead_code)]
    pub(crate) fn into_pcm_stream(self) -> AnyPcmStream<R> {
        self.into_session_parts().1
    }

    pub(crate) fn with_options(reader: R, options: PcmReaderOptions) -> Result<Self> {
        read_pcm_reader_with_options(reader, options)
    }

    pub(crate) fn into_session_parts(self) -> (Metadata, AnyPcmStream<R>) {
        match self {
            Self::Wav(reader) => {
                let (metadata, stream) = reader.into_session_parts();
                (
                    metadata,
                    AnyPcmStream::Wav(counted_encode_pcm_stream(stream)),
                )
            }
            #[cfg(feature = "aiff")]
            Self::Aiff(reader) => {
                let (metadata, stream) = reader.into_session_parts();
                (
                    metadata,
                    AnyPcmStream::Aiff(counted_encode_pcm_stream(stream)),
                )
            }
            #[cfg(feature = "caf")]
            Self::Caf(reader) => {
                let (metadata, stream) = reader.into_session_parts();
                (
                    metadata,
                    AnyPcmStream::Caf(counted_encode_pcm_stream(stream)),
                )
            }
        }
    }
}

/// Family-dispatched single-pass PCM stream.
pub(crate) enum AnyPcmStream<R: Read + Seek> {
    Wav(CountedEncodePcmStream<crate::wav_input::WavPcmStream<R>>),
    #[cfg(feature = "aiff")]
    Aiff(CountedEncodePcmStream<crate::aiff::AiffPcmStream<R>>),
    #[cfg(feature = "caf")]
    Caf(CountedEncodePcmStream<crate::caf::CafPcmStream<R>>),
}

impl<R: Read + Seek> EncodePcmStream for AnyPcmStream<R> {
    fn spec(&self) -> PcmSpec {
        match self {
            Self::Wav(stream) => stream.spec(),
            #[cfg(feature = "aiff")]
            Self::Aiff(stream) => stream.spec(),
            #[cfg(feature = "caf")]
            Self::Caf(stream) => stream.spec(),
        }
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize> {
        match self {
            Self::Wav(stream) => stream.read_chunk(max_frames, output),
            #[cfg(feature = "aiff")]
            Self::Aiff(stream) => stream.read_chunk(max_frames, output),
            #[cfg(feature = "caf")]
            Self::Caf(stream) => stream.read_chunk(max_frames, output),
        }
    }

    #[allow(private_interfaces)]
    fn read_chunk_payload(
        &mut self,
        max_frames: usize,
        reuse: Option<EncodeChunkPayload>,
    ) -> Result<EncodeChunkPayload> {
        match self {
            Self::Wav(stream) => stream.read_chunk_payload(max_frames, reuse),
            #[cfg(feature = "aiff")]
            Self::Aiff(stream) => stream.read_chunk_payload(max_frames, reuse),
            #[cfg(feature = "caf")]
            Self::Caf(stream) => stream.read_chunk_payload(max_frames, reuse),
        }
    }

    #[cfg(feature = "progress")]
    fn input_bytes_processed(&self) -> u64 {
        match self {
            Self::Wav(stream) => stream.input_bytes_processed(),
            #[cfg(feature = "aiff")]
            Self::Aiff(stream) => stream.input_bytes_processed(),
            #[cfg(feature = "caf")]
            Self::Caf(stream) => stream.input_bytes_processed(),
        }
    }

    fn update_streaminfo_md5(
        &mut self,
        md5: &mut crate::md5::StreaminfoMd5,
        samples: &[i32],
    ) -> Result<()> {
        match self {
            Self::Wav(stream) => stream.update_streaminfo_md5(md5, samples),
            #[cfg(feature = "aiff")]
            Self::Aiff(stream) => stream.update_streaminfo_md5(md5, samples),
            #[cfg(feature = "caf")]
            Self::Caf(stream) => stream.update_streaminfo_md5(md5, samples),
        }
    }

    #[allow(private_interfaces)]
    fn update_streaminfo_md5_for_payload(
        &mut self,
        md5: &mut crate::md5::StreaminfoMd5,
        payload: &EncodeChunkPayload,
    ) -> Result<()> {
        match self {
            Self::Wav(stream) => stream.update_streaminfo_md5_for_payload(md5, payload),
            #[cfg(feature = "aiff")]
            Self::Aiff(stream) => stream.update_streaminfo_md5_for_payload(md5, payload),
            #[cfg(feature = "caf")]
            Self::Caf(stream) => stream.update_streaminfo_md5_for_payload(md5, payload),
        }
    }
}

#[cfg(feature = "progress")]
fn pcm_bytes_for_frames(spec: PcmSpec, frames: usize) -> u64 {
    (frames as u64)
        .saturating_mul(u64::from(spec.channels))
        .saturating_mul(u64::from(spec.bytes_per_sample))
}

/// Parse a supported PCM container and return a family-specific reader for the
/// explicit encode workflow.
#[allow(dead_code)]
pub(crate) fn read_pcm_reader<R: Read + Seek>(reader: R) -> Result<PcmReader<R>> {
    read_pcm_reader_with_options(reader, PcmReaderOptions::default())
}

#[allow(dead_code)]
pub fn read_wav<R: Read + Seek>(reader: R) -> Result<PcmStream> {
    #[cfg(not(feature = "wav"))]
    {
        return Err(wav_feature_disabled_error());
    }
    crate::wav_input::read_wav(reader)
}

/// Inspect a supported PCM-container stream and return its total sample count.
///
/// This helper reads only the container metadata needed for sample counts.
/// It is useful for preflight checks and progress planning.
///
/// # Example
///
/// ```no_run
/// use flacx::inspect_pcm_total_samples;
/// use std::fs::File;
///
/// let total_samples = inspect_pcm_total_samples(File::open("input.wav").unwrap()).unwrap();
/// assert!(total_samples > 0);
/// ```
pub fn inspect_wav_total_samples<R: Read + Seek>(mut reader: R) -> Result<u64> {
    inspect_pcm_total_samples(&mut reader)
}

pub(crate) fn inspect_pcm_total_samples<R: Read + Seek>(reader: &mut R) -> Result<u64> {
    match sniff_pcm_input_kind(reader)? {
        PcmInputKind::AiffLike => inspect_aiff_total_samples_with_features(reader),
        PcmInputKind::Caf => inspect_caf_total_samples_with_features(reader),
        PcmInputKind::RiffLike => {
            ensure_wav_family_enabled()?;
            crate::wav_input::inspect_total_samples(reader)
        }
    }
}

pub(crate) fn read_pcm_reader_with_options<R: Read + Seek>(
    mut reader: R,
    options: PcmReaderOptions,
) -> Result<PcmReader<R>> {
    match sniff_pcm_input_kind(&mut reader)? {
        PcmInputKind::AiffLike => read_aiff_reader_with_features(reader),
        PcmInputKind::Caf => read_caf_reader_with_features(reader),
        PcmInputKind::RiffLike => {
            ensure_wav_family_enabled()?;
            Ok(PcmReader::Wav(
                crate::wav_input::WavReader::with_reader_options(reader, options)?,
            ))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PcmInputKind {
    RiffLike,
    AiffLike,
    Caf,
}

fn sniff_pcm_input_kind<R: Read + Seek>(reader: &mut R) -> Result<PcmInputKind> {
    let start = reader.stream_position()?;
    let mut magic = [0u8; 4];
    let kind = match reader.read_exact(&mut magic) {
        Ok(()) if magic == *b"FORM" => PcmInputKind::AiffLike,
        Ok(()) if magic == *b"caff" => PcmInputKind::Caf,
        Ok(()) => PcmInputKind::RiffLike,
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => PcmInputKind::RiffLike,
        Err(error) => return Err(error.into()),
    };
    reader.seek(SeekFrom::Start(start))?;
    Ok(kind)
}

#[cfg(feature = "aiff")]
fn inspect_aiff_total_samples_with_features<R: Read + Seek>(reader: &mut R) -> Result<u64> {
    crate::aiff::inspect_aiff_total_samples(reader)
}

#[cfg(not(feature = "aiff"))]
fn inspect_aiff_total_samples_with_features<R: Read + Seek>(_reader: &mut R) -> Result<u64> {
    Err(aiff_feature_disabled_error())
}

#[cfg(feature = "caf")]
fn inspect_caf_total_samples_with_features<R: Read + Seek>(reader: &mut R) -> Result<u64> {
    crate::caf::inspect_caf_total_samples(reader)
}

#[cfg(not(feature = "caf"))]
fn inspect_caf_total_samples_with_features<R: Read + Seek>(_reader: &mut R) -> Result<u64> {
    Err(caf_feature_disabled_error())
}

#[cfg(feature = "aiff")]
fn read_aiff_reader_with_features<R: Read + Seek>(reader: R) -> Result<PcmReader<R>> {
    Ok(PcmReader::Aiff(crate::aiff::AiffReader::new(reader)?))
}

#[cfg(not(feature = "aiff"))]
fn read_aiff_reader_with_features<R: Read + Seek>(_reader: R) -> Result<PcmReader<R>> {
    Err(aiff_feature_disabled_error())
}

#[cfg(feature = "caf")]
fn read_caf_reader_with_features<R: Read + Seek>(reader: R) -> Result<PcmReader<R>> {
    Ok(PcmReader::Caf(crate::caf::CafReader::new(reader)?))
}

#[cfg(not(feature = "caf"))]
fn read_caf_reader_with_features<R: Read + Seek>(_reader: R) -> Result<PcmReader<R>> {
    Err(caf_feature_disabled_error())
}

#[allow(dead_code)]
fn wav_feature_disabled_error() -> Error {
    Error::UnsupportedPcmContainer(
        "RIFF/WAVE family support requires the `wav` cargo feature".into(),
    )
}

#[allow(dead_code)]
fn aiff_feature_disabled_error() -> Error {
    Error::UnsupportedPcmContainer("AIFF/AIFC support requires the `aiff` cargo feature".into())
}

#[allow(dead_code)]
fn caf_feature_disabled_error() -> Error {
    Error::UnsupportedPcmContainer("CAF support requires the `caf` cargo feature".into())
}

fn ensure_wav_family_enabled() -> Result<()> {
    #[cfg(feature = "wav")]
    {
        Ok(())
    }
    #[cfg(not(feature = "wav"))]
    {
        Err(wav_feature_disabled_error())
    }
}
