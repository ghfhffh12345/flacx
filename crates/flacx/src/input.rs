use std::io::{Read, Seek, SeekFrom};

use crate::error::{Error, Result};
use crate::metadata::EncodeMetadata;

pub(crate) use crate::pcm::{
    PcmEnvelope, PcmSpec as WavSpec, PcmStream as WavData, append_encoded_sample,
    container_bits_from_valid_bits, ordinary_channel_mask,
};
pub use crate::pcm::{PcmSpec, PcmStream};
pub type PcmReaderOptions = crate::wav_input::WavReaderOptions;

/// Single-pass PCM sample source consumed by the encode session.
pub trait EncodePcmStream {
    /// Return the stream specification that drives encode planning.
    fn spec(&self) -> PcmSpec;

    /// Read up to `max_frames` interleaved PCM frames into `output`.
    ///
    /// Returns the number of frames appended to `output`.
    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize>;

    fn update_streaminfo_md5(
        &mut self,
        md5: &mut crate::md5::StreaminfoMd5,
        samples: &[i32],
    ) -> Result<()> {
        md5.update_samples(samples)
    }
}

/// Family-dispatched PCM reader for the explicit encode workflow.
pub enum PcmReader<R: Read + Seek> {
    Wav(crate::wav_input::WavReader<R>),
    #[cfg(feature = "aiff")]
    Aiff(crate::aiff::AiffReader<R>),
    #[cfg(feature = "caf")]
    Caf(crate::caf::CafReader<R>),
}

impl<R: Read + Seek> PcmReader<R> {
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
    pub fn metadata(&self) -> &EncodeMetadata {
        match self {
            Self::Wav(reader) => reader.metadata(),
            #[cfg(feature = "aiff")]
            Self::Aiff(reader) => reader.metadata(),
            #[cfg(feature = "caf")]
            Self::Caf(reader) => reader.metadata(),
        }
    }

    /// Convert the parsed reader into its single-pass PCM stream.
    pub fn into_pcm_stream(self) -> AnyPcmStream<R> {
        match self {
            Self::Wav(reader) => AnyPcmStream::Wav(reader.into_pcm_stream()),
            #[cfg(feature = "aiff")]
            Self::Aiff(reader) => AnyPcmStream::Aiff(reader.into_pcm_stream()),
            #[cfg(feature = "caf")]
            Self::Caf(reader) => AnyPcmStream::Caf(reader.into_pcm_stream()),
        }
    }
}

/// Family-dispatched single-pass PCM stream.
pub enum AnyPcmStream<R: Read + Seek> {
    Wav(crate::wav_input::WavPcmStream<R>),
    #[cfg(feature = "aiff")]
    Aiff(crate::aiff::AiffPcmStream<R>),
    #[cfg(feature = "caf")]
    Caf(crate::caf::CafPcmStream<R>),
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
}

/// Parse a supported PCM container and return a family-specific reader for the
/// explicit encode workflow.
pub fn read_pcm_reader<R: Read + Seek>(reader: R) -> Result<PcmReader<R>> {
    read_pcm_reader_with_options(reader, PcmReaderOptions::default())
}

#[allow(dead_code)]
pub fn read_wav<R: Read + Seek>(reader: R) -> Result<WavData> {
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
/// use flacx::inspect_wav_total_samples;
/// use std::fs::File;
///
/// let total_samples = inspect_wav_total_samples(File::open("input.wav").unwrap()).unwrap();
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

pub fn read_pcm_reader_with_options<R: Read + Seek>(
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
    Error::UnsupportedWav("RIFF/WAVE family support requires the `wav` cargo feature".into())
}

#[allow(dead_code)]
fn aiff_feature_disabled_error() -> Error {
    Error::UnsupportedWav("AIFF/AIFC support requires the `aiff` cargo feature".into())
}

#[allow(dead_code)]
fn caf_feature_disabled_error() -> Error {
    Error::UnsupportedWav("CAF support requires the `caf` cargo feature".into())
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
