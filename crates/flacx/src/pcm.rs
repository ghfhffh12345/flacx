use crate::error::{Error, Result};

const MAX_RFC9639_CHANNEL_MASK: u32 = 0x0003_FFFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PcmContainer {
    #[default]
    Auto,
    Wave,
    Rf64,
    Wave64,
    Aiff,
    Aifc,
    Caf,
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
        .ok_or(Error::InvalidWav(
            "valid bits cannot exceed container bits for encoding",
        ))? as u32;
    let sample = sample
        .checked_shl(shift)
        .ok_or(Error::UnsupportedWav(format!(
            "unsupported valid bits/container bits combination: {}/{}",
            envelope.valid_bits_per_sample, envelope.container_bits_per_sample
        )))?;

    match envelope.container_bits_per_sample {
        8 => {
            let bias = 1i32 << (envelope.valid_bits_per_sample - 1);
            let value = sample
                .checked_add(bias)
                .ok_or_else(|| Error::UnsupportedWav("8-bit sample is out of range".into()))?;
            let value = u8::try_from(value)
                .map_err(|_| Error::UnsupportedWav("8-bit sample is out of range".into()))?;
            buffer.push(value);
            Ok(())
        }
        16 => {
            let value = i16::try_from(sample)
                .map_err(|_| Error::UnsupportedWav("16-bit sample is out of range".into()))?;
            buffer.extend_from_slice(&value.to_le_bytes());
            Ok(())
        }
        24 => {
            if !(-8_388_608..=8_388_607).contains(&sample) {
                return Err(Error::UnsupportedWav(
                    "24-bit sample is out of range".into(),
                ));
            }
            buffer.extend_from_slice(&(sample as u32).to_le_bytes()[..3]);
            Ok(())
        }
        32 => {
            buffer.extend_from_slice(&sample.to_le_bytes());
            Ok(())
        }
        _ => Err(Error::UnsupportedWav(format!(
            "unsupported container bits/sample for encoder: {}",
            envelope.container_bits_per_sample
        ))),
    }
}
