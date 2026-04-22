use crate::error::{Error, Result};

/// Immutable description of a PCM stream.
///
/// This value is shared across reader, source, and summary surfaces so callers
/// can reason about sample rate, channel layout, sample depth, and total sample
/// counts without reading the full stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmSpec {
    /// Samples per second.
    pub sample_rate: u32,
    /// Number of interleaved channels.
    pub channels: u8,
    /// Valid bits per sample.
    pub bits_per_sample: u8,
    /// Total samples per channel.
    pub total_samples: u64,
    /// Stored container bits per sample expressed in bytes.
    pub bytes_per_sample: u16,
    /// RFC 9639-style channel mask when one is known.
    pub channel_mask: u32,
}

/// Fully materialized interleaved PCM samples plus their [`PcmSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcmStream {
    /// Stream specification shared by the sample buffer.
    pub spec: PcmSpec,
    /// Interleaved samples stored as signed 32-bit values.
    pub samples: Vec<i32>,
}

const MAX_RFC9639_CHANNEL_MASK: u32 = 0x0003_FFFF;

/// Output container family for decode and PCM writing operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PcmContainer {
    /// Choose the crate default for the active operation.
    #[default]
    Auto,
    /// RIFF/WAVE output.
    Wave,
    /// RF64 output.
    Rf64,
    /// Sony Wave64 output.
    Wave64,
    /// AIFF output.
    Aiff,
    /// AIFC output.
    Aifc,
    /// CAF output.
    Caf,
}

impl PcmContainer {
    pub(crate) const fn family_label(self) -> &'static str {
        match self {
            Self::Auto | Self::Wave | Self::Rf64 | Self::Wave64 => "WAV/RF64/Wave64",
            Self::Aiff | Self::Aifc => "AIFF/AIFC",
            Self::Caf => "CAF",
        }
    }

    pub(crate) const fn feature_name(self) -> &'static str {
        match self {
            Self::Auto | Self::Wave | Self::Rf64 | Self::Wave64 => "wav",
            Self::Aiff | Self::Aifc => "aiff",
            Self::Caf => "caf",
        }
    }

    pub(crate) fn is_enabled(self) -> bool {
        match self {
            Self::Auto | Self::Wave | Self::Rf64 | Self::Wave64 => cfg!(feature = "wav"),
            Self::Aiff | Self::Aifc => cfg!(feature = "aiff"),
            Self::Caf => cfg!(feature = "caf"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PcmEnvelope {
    pub(crate) channels: u16,
    pub(crate) valid_bits_per_sample: u16,
    pub(crate) container_bits_per_sample: u16,
    pub(crate) channel_mask: u32,
}

pub(crate) fn ordinary_channel_mask(channels: u16) -> Option<u32> {
    match channels {
        1 => Some(0x0004),
        2 => Some(0x0003),
        3 => Some(0x0007),
        4 => Some(0x0033),
        5 => Some(0x0037),
        6 => Some(0x003F),
        7 => Some(0x070F),
        8 => Some(0x063F),
        _ => None,
    }
}

pub(crate) fn is_supported_channel_mask(channels: u16, mask: u32) -> bool {
    if mask & !MAX_RFC9639_CHANNEL_MASK != 0 {
        return false;
    }
    mask.count_ones() <= u32::from(channels)
}

pub(crate) fn container_bits_from_valid_bits(valid_bits: u16) -> u16 {
    match valid_bits {
        0..=8 => 8,
        9..=16 => 16,
        17..=24 => 24,
        25..=32 => 32,
        _ => valid_bits.div_ceil(8) * 8,
    }
}

pub(crate) fn append_encoded_sample(
    buffer: &mut Vec<u8>,
    sample: i32,
    envelope: PcmEnvelope,
) -> Result<()> {
    let shift = envelope
        .container_bits_per_sample
        .checked_sub(envelope.valid_bits_per_sample)
        .ok_or(Error::InvalidPcmContainer(
            "valid bits cannot exceed container bits for encoding",
        ))? as u32;

    match envelope.container_bits_per_sample {
        8 => {
            let bias = 1i32 << (envelope.valid_bits_per_sample - 1);
            let stored = (i64::from(sample) + i64::from(bias))
                .checked_shl(shift)
                .ok_or_else(|| {
                    Error::UnsupportedPcmContainer("8-bit sample is out of range".into())
                })?;
            let stored = u8::try_from(stored).map_err(|_| {
                Error::UnsupportedPcmContainer("8-bit sample is out of range".into())
            })?;
            buffer.push(stored);
            Ok(())
        }
        16 => {
            let stored = i16::try_from(i64::from(sample).checked_shl(shift).ok_or_else(|| {
                Error::UnsupportedPcmContainer("16-bit sample is out of range".into())
            })?)
            .map_err(|_| Error::UnsupportedPcmContainer("16-bit sample is out of range".into()))?;
            buffer.extend_from_slice(&stored.to_le_bytes());
            Ok(())
        }
        24 => {
            let stored = i32::try_from(i64::from(sample).checked_shl(shift).ok_or_else(|| {
                Error::UnsupportedPcmContainer("24-bit sample is out of range".into())
            })?)
            .map_err(|_| Error::UnsupportedPcmContainer("24-bit sample is out of range".into()))?;
            let value = stored as u32;
            buffer.extend_from_slice(&[
                (value & 0xff) as u8,
                ((value >> 8) & 0xff) as u8,
                ((value >> 16) & 0xff) as u8,
            ]);
            Ok(())
        }
        32 => {
            let stored = i32::try_from(i64::from(sample).checked_shl(shift).ok_or_else(|| {
                Error::UnsupportedPcmContainer("32-bit sample is out of range".into())
            })?)
            .map_err(|_| Error::UnsupportedPcmContainer("32-bit sample is out of range".into()))?;
            buffer.extend_from_slice(&stored.to_le_bytes());
            Ok(())
        }
        _ => Err(Error::UnsupportedPcmContainer(format!(
            "unsupported container bits/sample for encoder: {}",
            envelope.container_bits_per_sample
        ))),
    }
}
