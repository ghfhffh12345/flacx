use std::io::{Read, Seek, SeekFrom};

use crate::{
    error::{Error, Result},
    input::{EncodeWavData, WavData, WavSpec},
    md5::streaminfo_md5,
    pcm::{PcmEnvelope, is_supported_channel_mask, ordinary_channel_mask},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Byte order for explicit raw signed-integer PCM descriptors.
pub enum RawPcmByteOrder {
    LittleEndian,
    BigEndian,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Explicit descriptor for raw signed-integer PCM inputs.
pub struct RawPcmDescriptor {
    pub sample_rate: u32,
    pub channels: u8,
    pub valid_bits_per_sample: u8,
    pub container_bits_per_sample: u8,
    pub byte_order: RawPcmByteOrder,
    pub channel_mask: Option<u32>,
}

/// Inspect raw signed-integer PCM and return its total sample count when an
/// explicit descriptor is supplied.
pub fn inspect_raw_pcm_total_samples<R: Read + Seek>(
    mut reader: R,
    descriptor: RawPcmDescriptor,
) -> Result<u64> {
    inspect_raw_pcm_total_samples_impl(&mut reader, descriptor)
}

pub(crate) fn inspect_raw_pcm_total_samples_impl<R: Read + Seek>(
    reader: &mut R,
    descriptor: RawPcmDescriptor,
) -> Result<u64> {
    let validated = validate_raw_descriptor(descriptor)?;
    let start = reader.stream_position()?;
    let end = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(start))?;
    total_samples_from_byte_len(end.saturating_sub(start), validated.frame_bytes)
}

#[allow(dead_code)]
pub(crate) fn total_samples_from_byte_len_with_descriptor(
    byte_len: u64,
    descriptor: RawPcmDescriptor,
) -> Result<u64> {
    let validated = validate_raw_descriptor(descriptor)?;
    total_samples_from_byte_len(byte_len, validated.frame_bytes)
}

pub(crate) fn read_raw_for_encode<R: Read + Seek>(
    reader: &mut R,
    descriptor: RawPcmDescriptor,
) -> Result<EncodeWavData> {
    let start = reader.stream_position()?;
    validate_raw_descriptor(descriptor)?;
    let end = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(start))?;
    let data_len = usize::try_from(end.saturating_sub(start))
        .map_err(|_| Error::UnsupportedWav("PCM payload exceeds memory-addressable size".into()))?;
    let mut data = vec![0u8; data_len];
    reader.read_exact(&mut data)?;
    read_raw_bytes_for_encode(data, descriptor)
}

pub(crate) fn read_raw_bytes_for_encode(
    data: Vec<u8>,
    descriptor: RawPcmDescriptor,
) -> Result<EncodeWavData> {
    let validated = validate_raw_descriptor(descriptor)?;
    let total_samples = total_samples_from_byte_len(
        u64::try_from(data.len()).expect("vector length fits u64"),
        validated.frame_bytes,
    )?;
    let samples = decode_raw_samples(&data, validated)?;
    let wav = WavData {
        spec: WavSpec {
            sample_rate: descriptor.sample_rate,
            channels: descriptor.channels,
            bits_per_sample: descriptor.valid_bits_per_sample,
            total_samples,
            bytes_per_sample: u16::from(descriptor.container_bits_per_sample) / 8,
            channel_mask: validated.channel_mask,
        },
        samples,
    };
    let streaminfo_md5 = streaminfo_md5(wav.spec, &wav.samples)?;

    Ok(EncodeWavData {
        wav,
        metadata: Default::default(),
        streaminfo_md5,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ValidatedRawDescriptor {
    descriptor: RawPcmDescriptor,
    envelope: PcmEnvelope,
    channel_mask: u32,
    frame_bytes: u64,
}

fn validate_raw_descriptor(descriptor: RawPcmDescriptor) -> Result<ValidatedRawDescriptor> {
    if descriptor.sample_rate == 0 {
        return Err(Error::UnsupportedWav("sample rate 0 is not allowed".into()));
    }
    if !(1..=8).contains(&descriptor.channels) {
        return Err(Error::UnsupportedWav(format!(
            "raw PCM input only supports 1..8 channel layouts, found {} channels",
            descriptor.channels
        )));
    }
    if !(4..=32).contains(&descriptor.valid_bits_per_sample) {
        return Err(Error::UnsupportedWav(format!(
            "valid bits must be in the FLAC-native 4..32 range, found {}",
            descriptor.valid_bits_per_sample
        )));
    }
    if !matches!(descriptor.container_bits_per_sample, 8 | 16 | 24 | 32) {
        return Err(Error::UnsupportedWav(format!(
            "only byte-aligned PCM containers are supported, found {} bits/sample",
            descriptor.container_bits_per_sample
        )));
    }
    if descriptor.valid_bits_per_sample > descriptor.container_bits_per_sample {
        return Err(Error::UnsupportedWav(format!(
            "valid bits cannot exceed container bits ({} > {})",
            descriptor.valid_bits_per_sample, descriptor.container_bits_per_sample
        )));
    }

    let channel_mask = match (descriptor.channels, descriptor.channel_mask) {
        (1 | 2, Some(mask)) => mask,
        (1 | 2, None) => {
            ordinary_channel_mask(u16::from(descriptor.channels)).expect("mono/stereo mask exists")
        }
        (3..=8, Some(mask)) if mask != 0 => mask,
        (3..=8, Some(_)) | (3..=8, None) => {
            return Err(Error::UnsupportedWav(
                "raw PCM 3..8 channel inputs require an explicit non-zero channel mask".into(),
            ));
        }
        _ => unreachable!("channels already validated"),
    };

    if !is_supported_channel_mask(u16::from(descriptor.channels), channel_mask) {
        return Err(Error::UnsupportedWav(format!(
            "channel mask {channel_mask:#010x} is not supported for {} channels",
            descriptor.channels
        )));
    }

    let frame_bytes = u64::from(descriptor.channels)
        .checked_mul(u64::from(descriptor.container_bits_per_sample / 8))
        .ok_or_else(|| Error::UnsupportedWav("raw PCM frame size overflows".into()))?;

    Ok(ValidatedRawDescriptor {
        descriptor,
        envelope: PcmEnvelope {
            channels: u16::from(descriptor.channels),
            valid_bits_per_sample: u16::from(descriptor.valid_bits_per_sample),
            container_bits_per_sample: u16::from(descriptor.container_bits_per_sample),
            channel_mask,
        },
        channel_mask,
        frame_bytes,
    })
}

fn total_samples_from_byte_len(byte_len: u64, frame_bytes: u64) -> Result<u64> {
    if frame_bytes == 0 {
        return Err(Error::InvalidWav("frame size must be non-zero"));
    }
    if !byte_len.is_multiple_of(frame_bytes) {
        return Err(Error::InvalidWav(
            "PCM payload is not aligned to the sample frame size",
        ));
    }
    Ok(byte_len / frame_bytes)
}

fn decode_raw_samples(data: &[u8], descriptor: ValidatedRawDescriptor) -> Result<Vec<i32>> {
    let shift = descriptor
        .envelope
        .container_bits_per_sample
        .checked_sub(descriptor.envelope.valid_bits_per_sample)
        .ok_or(Error::InvalidWav(
            "valid bits cannot exceed container bits for decoding",
        ))? as u32;

    match descriptor.envelope.container_bits_per_sample {
        8 => Ok(data
            .iter()
            .map(|&byte| {
                let value = i32::from(i8::from_ne_bytes([byte]));
                if shift == 0 { value } else { value >> shift }
            })
            .collect()),
        16 => Ok(data
            .chunks_exact(2)
            .map(|chunk| {
                let value = match descriptor.descriptor.byte_order {
                    RawPcmByteOrder::LittleEndian => {
                        i16::from_le_bytes([chunk[0], chunk[1]]) as i32
                    }
                    RawPcmByteOrder::BigEndian => i16::from_be_bytes([chunk[0], chunk[1]]) as i32,
                };
                if shift == 0 { value } else { value >> shift }
            })
            .collect()),
        24 => Ok(data
            .chunks_exact(3)
            .map(|chunk| {
                let mut value = match descriptor.descriptor.byte_order {
                    RawPcmByteOrder::LittleEndian => {
                        i32::from(chunk[0])
                            | (i32::from(chunk[1]) << 8)
                            | (i32::from(chunk[2]) << 16)
                    }
                    RawPcmByteOrder::BigEndian => {
                        (i32::from(chunk[0]) << 16)
                            | (i32::from(chunk[1]) << 8)
                            | i32::from(chunk[2])
                    }
                };
                if value & 0x0080_0000 != 0 {
                    value |= !0x00ff_ffff;
                }
                if shift == 0 { value } else { value >> shift }
            })
            .collect()),
        32 => Ok(data
            .chunks_exact(4)
            .map(|chunk| {
                let value = match descriptor.descriptor.byte_order {
                    RawPcmByteOrder::LittleEndian => {
                        i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
                    }
                    RawPcmByteOrder::BigEndian => {
                        i32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
                    }
                };
                if shift == 0 { value } else { value >> shift }
            })
            .collect()),
        _ => Err(Error::UnsupportedWav(format!(
            "unsupported container bits/sample for decoder: {}",
            descriptor.envelope.container_bits_per_sample
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{
        RawPcmByteOrder, RawPcmDescriptor, inspect_raw_pcm_total_samples, read_raw_bytes_for_encode,
    };

    #[test]
    fn inspects_and_reads_little_endian_raw_pcm() {
        let descriptor = RawPcmDescriptor {
            sample_rate: 44_100,
            channels: 2,
            valid_bits_per_sample: 16,
            container_bits_per_sample: 16,
            byte_order: RawPcmByteOrder::LittleEndian,
            channel_mask: None,
        };
        let data = [1i16, -2, 3, -4]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(
            inspect_raw_pcm_total_samples(Cursor::new(&data), descriptor).unwrap(),
            2
        );
        let parsed = read_raw_bytes_for_encode(data, descriptor).unwrap();
        assert_eq!(parsed.wav.samples, vec![1, -2, 3, -4]);
    }

    #[test]
    fn rejects_multichannel_raw_without_mask() {
        let descriptor = RawPcmDescriptor {
            sample_rate: 48_000,
            channels: 4,
            valid_bits_per_sample: 16,
            container_bits_per_sample: 16,
            byte_order: RawPcmByteOrder::BigEndian,
            channel_mask: None,
        };
        let error =
            inspect_raw_pcm_total_samples(Cursor::new(vec![0u8; 16]), descriptor).unwrap_err();
        assert!(error.to_string().contains("channel mask"));
    }
}
