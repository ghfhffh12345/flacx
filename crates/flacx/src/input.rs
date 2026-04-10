use std::io::{Read, Seek, SeekFrom};

use crate::config::EncoderConfig;
use crate::error::{Error, Result};
use crate::metadata::EncodeMetadata;

pub(crate) use crate::pcm::{
    PcmEnvelope, PcmSpec as WavSpec, PcmStream as WavData, append_encoded_sample,
    container_bits_from_valid_bits, ordinary_channel_mask,
};
pub use crate::pcm::{PcmSpec, PcmStream};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EncodeWavData {
    pub(crate) wav: WavData,
    pub(crate) metadata: EncodeMetadata,
    pub(crate) streaminfo_md5: [u8; 16],
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

pub(crate) fn read_pcm_for_encode_with_config<R: Read + Seek>(
    reader: &mut R,
    config: &EncoderConfig,
) -> Result<EncodeWavData> {
    match sniff_pcm_input_kind(reader)? {
        PcmInputKind::AiffLike => read_aiff_for_encode_with_features(reader),
        PcmInputKind::Caf => read_caf_for_encode_with_features(reader),
        PcmInputKind::RiffLike => {
            ensure_wav_family_enabled()?;
            crate::wav_input::read_wav_for_encode_with_config(reader, config)
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
fn read_aiff_for_encode_with_features<R: Read + Seek>(reader: &mut R) -> Result<EncodeWavData> {
    crate::aiff::read_aiff_for_encode(reader)
}

#[cfg(not(feature = "aiff"))]
fn read_aiff_for_encode_with_features<R: Read + Seek>(_reader: &mut R) -> Result<EncodeWavData> {
    Err(aiff_feature_disabled_error())
}

#[cfg(feature = "caf")]
fn read_caf_for_encode_with_features<R: Read + Seek>(reader: &mut R) -> Result<EncodeWavData> {
    crate::caf::read_caf_for_encode(reader)
}

#[cfg(not(feature = "caf"))]
fn read_caf_for_encode_with_features<R: Read + Seek>(_reader: &mut R) -> Result<EncodeWavData> {
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
