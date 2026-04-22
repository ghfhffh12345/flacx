use std::io::{Read, Seek, SeekFrom};

use crate::{
    error::{Error, Result},
    input::PcmSpec,
    pcm::{PcmEnvelope, is_supported_channel_mask, ordinary_channel_mask},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Byte order for explicit raw signed-integer PCM descriptors.
pub enum RawPcmByteOrder {
    /// Least-significant byte first.
    LittleEndian,
    /// Most-significant byte first.
    BigEndian,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Explicit descriptor for raw signed-integer PCM inputs.
pub struct RawPcmDescriptor {
    /// Samples per second.
    pub sample_rate: u32,
    /// Number of interleaved channels.
    pub channels: u8,
    /// Valid bits carried by each sample.
    pub valid_bits_per_sample: u8,
    /// Stored container width for each sample.
    pub container_bits_per_sample: u8,
    /// Sample byte order in the raw stream.
    pub byte_order: RawPcmByteOrder,
    /// Optional RFC 9639 channel mask.
    pub channel_mask: Option<u32>,
}

/// Reader façade for explicit raw signed-integer PCM input.
#[derive(Debug)]
pub struct RawPcmReader<R: Read + Seek> {
    reader: R,
    spec: PcmSpec,
    descriptor: RawPcmDescriptor,
}

impl<R: Read + Seek> RawPcmReader<R> {
    /// Create a raw PCM reader from a seekable byte stream and descriptor.
    pub fn new(mut reader: R, descriptor: RawPcmDescriptor) -> Result<Self> {
        let total_samples = inspect_raw_pcm_total_samples_impl(&mut reader, descriptor)?;
        let validated = validate_raw_descriptor(descriptor)?;
        Ok(Self {
            reader,
            spec: spec_from_validated_descriptor(validated, total_samples),
            descriptor,
        })
    }

    /// Return the parsed PCM spec derived from the supplied descriptor.
    #[must_use]
    pub fn spec(&self) -> PcmSpec {
        self.spec
    }

    /// Consume the reader and return the corresponding single-pass PCM stream.
    pub fn into_pcm_stream(self) -> Result<RawPcmStream<R>> {
        RawPcmStream::new(self.reader, self.descriptor, self.spec.total_samples)
    }
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

/// Single-pass raw PCM stream produced by [`RawPcmReader`].
#[derive(Debug)]
pub struct RawPcmStream<R> {
    reader: R,
    spec: PcmSpec,
    validated: ValidatedRawDescriptor,
    remaining_frames: u64,
    #[cfg(feature = "progress")]
    input_bytes_processed: u64,
    last_chunk_bytes: Vec<u8>,
}

impl<R: Read + Seek> RawPcmStream<R> {
    /// Directly construct a raw PCM stream from a positioned reader, explicit
    /// descriptor, and known per-channel sample count.
    pub fn new(reader: R, descriptor: RawPcmDescriptor, total_samples: u64) -> Result<Self> {
        let validated = validate_raw_descriptor(descriptor)?;
        Ok(Self {
            reader,
            spec: spec_from_validated_descriptor(validated, total_samples),
            validated,
            remaining_frames: total_samples,
            #[cfg(feature = "progress")]
            input_bytes_processed: 0,
            last_chunk_bytes: Vec::new(),
        })
    }

    #[must_use]
    pub fn spec(&self) -> PcmSpec {
        self.spec
    }
}

impl<R: Read + Seek> crate::input::EncodePcmStream for RawPcmStream<R> {
    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize> {
        let frames = self.remaining_frames.min(max_frames as u64) as usize;
        if frames == 0 {
            return Ok(0);
        }

        let byte_len = usize::try_from(
            (frames as u64)
                .checked_mul(self.validated.frame_bytes)
                .ok_or_else(|| {
                    Error::UnsupportedPcmContainer("raw PCM chunk size overflows".into())
                })?,
        )
        .map_err(|_| {
            Error::UnsupportedPcmContainer("raw PCM chunk size exceeds addressable memory".into())
        })?;
        self.last_chunk_bytes.clear();
        self.last_chunk_bytes.resize(byte_len, 0);
        self.reader.read_exact(&mut self.last_chunk_bytes)?;
        decode_raw_samples_into(&self.last_chunk_bytes, self.validated, output)?;
        self.remaining_frames -= frames as u64;
        #[cfg(feature = "progress")]
        {
            self.input_bytes_processed = self.input_bytes_processed.saturating_add(byte_len as u64);
        }
        Ok(frames)
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
        match self.validated.descriptor.byte_order {
            RawPcmByteOrder::LittleEndian => {
                md5.update_bytes(&self.last_chunk_bytes);
                Ok(())
            }
            RawPcmByteOrder::BigEndian => md5.update_samples(samples),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ValidatedRawDescriptor {
    descriptor: RawPcmDescriptor,
    envelope: PcmEnvelope,
    channel_mask: u32,
    frame_bytes: u64,
}

fn spec_from_validated_descriptor(
    validated: ValidatedRawDescriptor,
    total_samples: u64,
) -> PcmSpec {
    PcmSpec {
        sample_rate: validated.descriptor.sample_rate,
        channels: validated.descriptor.channels,
        bits_per_sample: validated.descriptor.valid_bits_per_sample,
        total_samples,
        bytes_per_sample: u16::from(validated.descriptor.container_bits_per_sample) / 8,
        channel_mask: validated.channel_mask,
    }
}

fn validate_raw_descriptor(descriptor: RawPcmDescriptor) -> Result<ValidatedRawDescriptor> {
    if descriptor.sample_rate == 0 {
        return Err(Error::UnsupportedPcmContainer(
            "sample rate 0 is not allowed".into(),
        ));
    }
    if !(1..=8).contains(&descriptor.channels) {
        return Err(Error::UnsupportedPcmContainer(format!(
            "raw PCM input only supports 1..8 channel layouts, found {} channels",
            descriptor.channels
        )));
    }
    if !(4..=32).contains(&descriptor.valid_bits_per_sample) {
        return Err(Error::UnsupportedPcmContainer(format!(
            "valid bits must be in the FLAC-native 4..32 range, found {}",
            descriptor.valid_bits_per_sample
        )));
    }
    if !matches!(descriptor.container_bits_per_sample, 8 | 16 | 24 | 32) {
        return Err(Error::UnsupportedPcmContainer(format!(
            "only byte-aligned PCM containers are supported, found {} bits/sample",
            descriptor.container_bits_per_sample
        )));
    }
    if descriptor.valid_bits_per_sample > descriptor.container_bits_per_sample {
        return Err(Error::UnsupportedPcmContainer(format!(
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
            return Err(Error::UnsupportedPcmContainer(
                "raw PCM 3..8 channel inputs require an explicit non-zero channel mask".into(),
            ));
        }
        _ => unreachable!("channels already validated"),
    };

    if !is_supported_channel_mask(u16::from(descriptor.channels), channel_mask) {
        return Err(Error::UnsupportedPcmContainer(format!(
            "channel mask {channel_mask:#010x} is not supported for {} channels",
            descriptor.channels
        )));
    }

    let frame_bytes = u64::from(descriptor.channels)
        .checked_mul(u64::from(descriptor.container_bits_per_sample / 8))
        .ok_or_else(|| Error::UnsupportedPcmContainer("raw PCM frame size overflows".into()))?;

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
        return Err(Error::InvalidPcmContainer("frame size must be non-zero"));
    }
    if !byte_len.is_multiple_of(frame_bytes) {
        return Err(Error::InvalidPcmContainer(
            "PCM payload is not aligned to the sample frame size",
        ));
    }
    Ok(byte_len / frame_bytes)
}

fn decode_raw_samples_into(
    data: &[u8],
    descriptor: ValidatedRawDescriptor,
    output: &mut Vec<i32>,
) -> Result<()> {
    let shift = descriptor
        .envelope
        .container_bits_per_sample
        .checked_sub(descriptor.envelope.valid_bits_per_sample)
        .ok_or(Error::InvalidPcmContainer(
            "valid bits cannot exceed container bits for decoding",
        ))? as u32;

    match descriptor.envelope.container_bits_per_sample {
        8 => {
            output.reserve(data.len());
            for &byte in data {
                let value = i32::from(i8::from_ne_bytes([byte]));
                output.push(if shift == 0 { value } else { value >> shift });
            }
            Ok(())
        }
        16 => {
            let sample_count = data.len() / 2;
            output.reserve(sample_count);
            for chunk in data.chunks_exact(2) {
                let value = match descriptor.descriptor.byte_order {
                    RawPcmByteOrder::LittleEndian => {
                        i16::from_le_bytes([chunk[0], chunk[1]]) as i32
                    }
                    RawPcmByteOrder::BigEndian => i16::from_be_bytes([chunk[0], chunk[1]]) as i32,
                };
                output.push(if shift == 0 { value } else { value >> shift });
            }
            Ok(())
        }
        24 => {
            let sample_count = data.len() / 3;
            output.reserve(sample_count);
            for chunk in data.chunks_exact(3) {
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
                output.push(if shift == 0 { value } else { value >> shift });
            }
            Ok(())
        }
        32 => {
            let sample_count = data.len() / 4;
            output.reserve(sample_count);
            for chunk in data.chunks_exact(4) {
                let value = match descriptor.descriptor.byte_order {
                    RawPcmByteOrder::LittleEndian => {
                        i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
                    }
                    RawPcmByteOrder::BigEndian => {
                        i32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
                    }
                };
                output.push(if shift == 0 { value } else { value >> shift });
            }
            Ok(())
        }
        _ => Err(Error::UnsupportedPcmContainer(format!(
            "unsupported container bits/sample for decoder: {}",
            descriptor.envelope.container_bits_per_sample
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{RawPcmByteOrder, RawPcmDescriptor, RawPcmReader, inspect_raw_pcm_total_samples};
    use crate::input::EncodePcmStream;

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
        let reader = RawPcmReader::new(Cursor::new(data), descriptor).unwrap();
        let mut stream = reader.into_pcm_stream().unwrap();
        let mut samples = Vec::new();
        let frames = stream.read_chunk(2, &mut samples).unwrap();

        assert_eq!(frames, 2);
        assert_eq!(samples, vec![1, -2, 3, -4]);
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

    #[test]
    fn directly_constructs_raw_pcm_stream() {
        let descriptor = RawPcmDescriptor {
            sample_rate: 48_000,
            channels: 1,
            valid_bits_per_sample: 24,
            container_bits_per_sample: 24,
            byte_order: RawPcmByteOrder::BigEndian,
            channel_mask: None,
        };
        let data = vec![0x00, 0x00, 0x01, 0xff, 0xff, 0xfe];

        let mut stream = super::RawPcmStream::new(Cursor::new(data), descriptor, 2).unwrap();
        let mut samples = Vec::new();
        let frames = stream.read_chunk(2, &mut samples).unwrap();

        assert_eq!(frames, 2);
        assert_eq!(stream.spec().channel_mask, 0x0004);
        assert_eq!(samples, vec![1, -2]);
    }
}
