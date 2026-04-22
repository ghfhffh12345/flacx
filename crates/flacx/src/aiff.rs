use std::io::{Read, Seek, SeekFrom};

use crate::{
    error::{Error, Result},
    input::PcmSpec,
    metadata::Metadata,
    pcm::{PcmEnvelope, container_bits_from_valid_bits, ordinary_channel_mask},
    raw::RawPcmByteOrder,
};

const FORM_ID: [u8; 4] = *b"FORM";
const AIFF_FORM_TYPE: [u8; 4] = *b"AIFF";
const AIFC_FORM_TYPE: [u8; 4] = *b"AIFC";
const COMM_CHUNK_ID: [u8; 4] = *b"COMM";
const SSND_CHUNK_ID: [u8; 4] = *b"SSND";
const AIFC_NONE: [u8; 4] = *b"NONE";
const AIFC_SOWT: [u8; 4] = *b"sowt";
const AIFC_FL32: [u8; 4] = *b"fl32";
const AIFC_FL64: [u8; 4] = *b"fl64";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SampleEndianness {
    Big,
    Little,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedAiffLayout {
    envelope: PcmEnvelope,
    sample_rate: u32,
    total_samples: u64,
    data_offset: u64,
    data_size: u64,
    endianness: SampleEndianness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommonChunk {
    channels: u16,
    sample_frames: u64,
    valid_bits_per_sample: u16,
    sample_rate: u32,
    endianness: SampleEndianness,
}

/// Explicit descriptor for direct AIFF/AIFC PCM stream construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AiffPcmDescriptor {
    /// Samples per second.
    pub sample_rate: u32,
    /// Number of interleaved channels.
    pub channels: u8,
    /// Valid bits carried by each sample.
    pub valid_bits_per_sample: u8,
    /// Total samples per channel available from the positioned PCM payload.
    pub total_samples: u64,
    /// Byte order of the positioned PCM payload.
    pub byte_order: RawPcmByteOrder,
}

/// Reader façade for AIFF/AIFC encode inputs.
#[derive(Debug, Clone)]
pub struct AiffReader<R> {
    reader: R,
    spec: PcmSpec,
    metadata: Metadata,
    envelope: PcmEnvelope,
    endianness: SampleEndianness,
}

impl<R: Read + Seek> AiffReader<R> {
    /// Parse an AIFF or supported AIFC reader.
    pub fn new(mut reader: R) -> Result<Self> {
        let layout = parse_aiff_layout(&mut reader)?;
        reader.seek(SeekFrom::Start(layout.data_offset))?;
        Ok(Self {
            reader,
            spec: spec_from_envelope(layout.sample_rate, layout.total_samples, layout.envelope),
            metadata: Metadata::default(),
            envelope: layout.envelope,
            endianness: layout.endianness,
        })
    }

    /// Return the parsed PCM stream specification.
    #[must_use]
    pub fn spec(&self) -> PcmSpec {
        self.spec
    }

    /// Return metadata staged for encode-side preservation.
    #[must_use]
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Convert this reader into an owned encode source.
    pub fn into_source(self) -> crate::input::EncodeSource<impl crate::input::EncodePcmStream> {
        let (metadata, stream) = self.into_session_parts();
        crate::input::EncodeSource::new(metadata, stream)
    }

    #[allow(dead_code)]
    pub(crate) fn into_pcm_stream(self) -> AiffPcmStream<R> {
        self.into_session_parts().1
    }

    pub(crate) fn into_session_parts(self) -> (Metadata, AiffPcmStream<R>) {
        let Self {
            reader,
            spec,
            metadata,
            envelope,
            endianness,
        } = self;
        (
            metadata,
            AiffPcmStream::from_parts(reader, spec, envelope, endianness)
                .expect("validated AIFF reader state remains constructible"),
        )
    }
}

/// Single-pass PCM stream produced by [`AiffReader`].
#[derive(Debug, Clone)]
pub struct AiffPcmStream<R> {
    reader: R,
    spec: PcmSpec,
    envelope: PcmEnvelope,
    endianness: SampleEndianness,
    remaining_frames: u64,
    frame_bytes: usize,
    #[cfg(feature = "progress")]
    input_bytes_processed: u64,
}

impl<R: Read + Seek> AiffPcmStream<R> {
    /// Directly construct an AIFF/AIFC PCM stream from a positioned reader and
    /// explicit descriptor.
    pub fn new(reader: R, descriptor: AiffPcmDescriptor) -> Result<Self> {
        let (spec, envelope, endianness) = validate_direct_descriptor(descriptor)?;
        Self::from_parts(reader, spec, envelope, endianness)
    }

    fn from_parts(
        reader: R,
        spec: PcmSpec,
        envelope: PcmEnvelope,
        endianness: SampleEndianness,
    ) -> Result<Self> {
        let frame_bytes = usize::from(envelope.channels)
            .checked_mul(usize::from(envelope.container_bits_per_sample / 8))
            .ok_or_else(|| Error::UnsupportedPcmContainer("AIFF frame size overflows".into()))?;
        Ok(Self {
            reader,
            spec,
            envelope,
            endianness,
            remaining_frames: spec.total_samples,
            frame_bytes,
            #[cfg(feature = "progress")]
            input_bytes_processed: 0,
        })
    }
}

impl<R: Read + Seek> crate::input::EncodePcmStream for AiffPcmStream<R> {
    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize> {
        let frames = self.remaining_frames.min(max_frames as u64) as usize;
        if frames == 0 {
            return Ok(0);
        }

        let byte_len = frames
            .checked_mul(self.frame_bytes)
            .ok_or_else(|| Error::UnsupportedPcmContainer("AIFF chunk size overflows".into()))?;
        let mut data = vec![0u8; byte_len];
        self.reader.read_exact(&mut data)?;
        let samples = decode_aiff_samples(&data, self.envelope, self.endianness)?;
        output.extend(samples);
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
}

fn spec_from_envelope(sample_rate: u32, total_samples: u64, envelope: PcmEnvelope) -> PcmSpec {
    PcmSpec {
        sample_rate,
        channels: envelope.channels as u8,
        bits_per_sample: envelope.valid_bits_per_sample as u8,
        total_samples,
        bytes_per_sample: envelope.container_bits_per_sample / 8,
        channel_mask: envelope.channel_mask,
    }
}

fn validate_direct_descriptor(
    descriptor: AiffPcmDescriptor,
) -> Result<(PcmSpec, PcmEnvelope, SampleEndianness)> {
    let endianness = match descriptor.byte_order {
        RawPcmByteOrder::BigEndian => SampleEndianness::Big,
        RawPcmByteOrder::LittleEndian => SampleEndianness::Little,
    };
    let common = CommonChunk {
        channels: u16::from(descriptor.channels),
        sample_frames: descriptor.total_samples,
        valid_bits_per_sample: u16::from(descriptor.valid_bits_per_sample),
        sample_rate: descriptor.sample_rate,
        endianness,
    };
    let envelope = validate_common_chunk(common)?;
    if matches!(endianness, SampleEndianness::Little) && envelope.valid_bits_per_sample != 16 {
        return Err(Error::UnsupportedPcmContainer(
            "AIFC compression 'sowt' is only supported for 16-bit signed PCM".into(),
        ));
    }

    Ok((
        spec_from_envelope(descriptor.sample_rate, descriptor.total_samples, envelope),
        envelope,
        endianness,
    ))
}

pub(crate) fn inspect_aiff_total_samples<R: Read + Seek>(reader: &mut R) -> Result<u64> {
    Ok(parse_aiff_layout(reader)?.total_samples)
}

fn parse_aiff_layout<R: Read + Seek>(reader: &mut R) -> Result<ParsedAiffLayout> {
    let mut header = [0u8; 12];
    reader.read_exact(&mut header)?;
    if header[..4] != FORM_ID {
        return Err(Error::InvalidPcmContainer("expected FORM header"));
    }
    let form_type: [u8; 4] = header[8..12].try_into().expect("fixed form type");
    let is_aifc = match form_type {
        AIFF_FORM_TYPE => false,
        AIFC_FORM_TYPE => true,
        _ => {
            return Err(Error::InvalidPcmContainer(
                "expected AIFF or AIFC form type",
            ));
        }
    };

    let mut common = None;
    let mut data_offset = None;
    let mut data_size = None;

    loop {
        let mut chunk_header = [0u8; 8];
        match reader.read_exact(&mut chunk_header) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.into()),
        }

        let chunk_id: [u8; 4] = chunk_header[..4].try_into().expect("fixed chunk id");
        let chunk_size =
            u32::from_be_bytes(chunk_header[4..8].try_into().expect("fixed chunk size"));
        let chunk_start = reader.stream_position()?;

        match chunk_id {
            COMM_CHUNK_ID => {
                common = Some(read_common_chunk(reader, chunk_size, is_aifc)?);
                seek_forward(reader, chunk_start, u64::from(chunk_size))?;
            }
            SSND_CHUNK_ID => {
                if chunk_size < 8 {
                    return Err(Error::InvalidPcmContainer("AIFF SSND chunk is too short"));
                }
                let offset = read_u32_be(reader)?;
                let _block_size = read_u32_be(reader)?;
                let sample_data_size = u64::from(chunk_size)
                    .checked_sub(8)
                    .and_then(|size| size.checked_sub(u64::from(offset)))
                    .ok_or(Error::InvalidPcmContainer(
                        "AIFF SSND offset exceeds the chunk payload",
                    ))?;
                let data_start = reader
                    .stream_position()?
                    .checked_add(u64::from(offset))
                    .ok_or(Error::InvalidPcmContainer(
                        "AIFF SSND offset overflows the file cursor",
                    ))?;
                data_offset = Some(data_start);
                data_size = Some(sample_data_size);
                seek_forward(reader, chunk_start, u64::from(chunk_size))?;
            }
            _ => {
                seek_forward(reader, chunk_start, u64::from(chunk_size))?;
            }
        }

        if !chunk_size.is_multiple_of(2) {
            reader.seek(SeekFrom::Current(1))?;
        }
    }

    let common = common.ok_or(Error::InvalidPcmContainer("missing COMM chunk"))?;
    let data_offset = data_offset.ok_or(Error::InvalidPcmContainer("missing SSND chunk"))?;
    let data_size = data_size.ok_or(Error::InvalidPcmContainer("missing SSND data size"))?;
    let envelope = validate_common_chunk(common)?;
    let block_align =
        u64::from(envelope.channels) * u64::from(envelope.container_bits_per_sample / 8);
    if block_align == 0 {
        return Err(Error::InvalidPcmContainer(
            "AIFF block alignment must be non-zero",
        ));
    }
    if data_size % block_align != 0 {
        return Err(Error::InvalidPcmContainer(
            "SSND audio data is not aligned to the sample frame size",
        ));
    }
    let total_samples = data_size / block_align;
    if total_samples != common.sample_frames {
        return Err(Error::InvalidPcmContainer(
            "COMM sample frame count does not match SSND audio payload size",
        ));
    }

    Ok(ParsedAiffLayout {
        envelope,
        sample_rate: common.sample_rate,
        total_samples,
        data_offset,
        data_size,
        endianness: common.endianness,
    })
}

fn read_common_chunk<R: Read>(
    reader: &mut R,
    chunk_size: u32,
    is_aifc: bool,
) -> Result<CommonChunk> {
    let minimum_size = if is_aifc { 23 } else { 18 };
    if chunk_size < minimum_size {
        return Err(Error::InvalidPcmContainer(if is_aifc {
            "AIFC COMM chunk is too short"
        } else {
            "AIFF COMM chunk is too short"
        }));
    }

    let channels = read_u16_be(reader)?;
    let sample_frames = u64::from(read_u32_be(reader)?);
    let valid_bits_per_sample = read_u16_be(reader)?;
    let sample_rate = read_extended_sample_rate(reader)?;

    let endianness = if is_aifc {
        let mut compression_id = [0u8; 4];
        reader.read_exact(&mut compression_id)?;
        match compression_id {
            AIFC_NONE => SampleEndianness::Big,
            AIFC_SOWT => {
                if valid_bits_per_sample != 16 {
                    return Err(Error::UnsupportedPcmContainer(
                        "AIFC compression 'sowt' is only supported for 16-bit signed PCM".into(),
                    ));
                }
                SampleEndianness::Little
            }
            AIFC_FL32 | AIFC_FL64 => {
                return Err(Error::UnsupportedPcmContainer(format!(
                    "float AIFC compression '{}' is not supported",
                    fourcc_to_string(compression_id)
                )));
            }
            _ => {
                return Err(Error::UnsupportedPcmContainer(format!(
                    "AIFC compression '{}' is not supported",
                    fourcc_to_string(compression_id)
                )));
            }
        }
    } else {
        SampleEndianness::Big
    };

    Ok(CommonChunk {
        channels,
        sample_frames,
        valid_bits_per_sample,
        sample_rate,
        endianness,
    })
}

fn validate_common_chunk(common: CommonChunk) -> Result<PcmEnvelope> {
    if common.sample_rate == 0 {
        return Err(Error::UnsupportedPcmContainer(
            "sample rate 0 is not allowed".into(),
        ));
    }
    if !(1..=8).contains(&common.channels) {
        return Err(Error::UnsupportedPcmContainer(format!(
            "AIFF/AIFC input only supports ordinary 1..8 channel layouts, found {} channels",
            common.channels
        )));
    }
    if !(4..=32).contains(&common.valid_bits_per_sample) {
        return Err(Error::UnsupportedPcmContainer(format!(
            "valid bits must be in the FLAC-native 4..32 range, found {}",
            common.valid_bits_per_sample
        )));
    }

    let container_bits_per_sample = container_bits_from_valid_bits(common.valid_bits_per_sample);
    if !matches!(container_bits_per_sample, 8 | 16 | 24 | 32) {
        return Err(Error::UnsupportedPcmContainer(format!(
            "only byte-aligned PCM containers are supported, found {container_bits_per_sample} bits/sample"
        )));
    }

    Ok(PcmEnvelope {
        channels: common.channels,
        valid_bits_per_sample: common.valid_bits_per_sample,
        container_bits_per_sample,
        channel_mask: ordinary_channel_mask(common.channels).ok_or_else(|| {
            Error::UnsupportedPcmContainer(format!(
                "no ordinary channel mask exists for {} channels",
                common.channels
            ))
        })?,
    })
}

fn decode_aiff_samples(
    data: &[u8],
    envelope: PcmEnvelope,
    endianness: SampleEndianness,
) -> Result<Vec<i32>> {
    let shift = envelope
        .container_bits_per_sample
        .checked_sub(envelope.valid_bits_per_sample)
        .ok_or(Error::InvalidPcmContainer(
            "valid bits cannot exceed container bits for decoding",
        ))? as u32;

    match envelope.container_bits_per_sample {
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
                let value = match endianness {
                    SampleEndianness::Big => i16::from_be_bytes([chunk[0], chunk[1]]) as i32,
                    SampleEndianness::Little => i16::from_le_bytes([chunk[0], chunk[1]]) as i32,
                };
                if shift == 0 { value } else { value >> shift }
            })
            .collect()),
        24 => Ok(data
            .chunks_exact(3)
            .map(|chunk| {
                let mut value = match endianness {
                    SampleEndianness::Big => {
                        (i32::from(chunk[0]) << 16)
                            | (i32::from(chunk[1]) << 8)
                            | i32::from(chunk[2])
                    }
                    SampleEndianness::Little => {
                        i32::from(chunk[0])
                            | (i32::from(chunk[1]) << 8)
                            | (i32::from(chunk[2]) << 16)
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
                let value = match endianness {
                    SampleEndianness::Big => {
                        i32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
                    }
                    SampleEndianness::Little => {
                        i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
                    }
                };
                if shift == 0 { value } else { value >> shift }
            })
            .collect()),
        _ => Err(Error::UnsupportedPcmContainer(format!(
            "unsupported container bits/sample for decoder: {}",
            envelope.container_bits_per_sample
        ))),
    }
}

fn read_extended_sample_rate<R: Read>(reader: &mut R) -> Result<u32> {
    let mut bytes = [0u8; 10];
    reader.read_exact(&mut bytes)?;
    let exponent_word = u16::from_be_bytes(bytes[..2].try_into().expect("fixed exponent"));
    let sign = exponent_word & 0x8000;
    let exponent = exponent_word & 0x7fff;
    let mantissa = u64::from_be_bytes(bytes[2..].try_into().expect("fixed mantissa"));

    if sign != 0 {
        return Err(Error::UnsupportedPcmContainer(
            "negative AIFF sample rates are not supported".into(),
        ));
    }
    if exponent == 0x7fff {
        return Err(Error::UnsupportedPcmContainer(
            "non-finite AIFF sample rates are not supported".into(),
        ));
    }
    if exponent == 0 || mantissa == 0 {
        return Ok(0);
    }

    let exponent = i32::from(exponent) - 16_383;
    let integer_shift = 63 - exponent;
    let value = if integer_shift >= 0 {
        let shift = u32::try_from(integer_shift).expect("non-negative shift");
        if shift >= 64 {
            return Err(Error::UnsupportedPcmContainer(
                "fractional AIFF sample rates are not supported".into(),
            ));
        }
        let remainder_mask = if shift == 0 { 0 } else { (1u64 << shift) - 1 };
        if mantissa & remainder_mask != 0 {
            return Err(Error::UnsupportedPcmContainer(
                "fractional AIFF sample rates are not supported".into(),
            ));
        }
        mantissa >> shift
    } else {
        mantissa
            .checked_shl(integer_shift.unsigned_abs())
            .ok_or_else(|| {
                Error::UnsupportedPcmContainer("AIFF sample rate exceeds supported range".into())
            })?
    };

    u32::try_from(value).map_err(|_| {
        Error::UnsupportedPcmContainer("AIFF sample rate exceeds supported range".into())
    })
}

fn read_u16_be<R: Read>(reader: &mut R) -> Result<u16> {
    let mut bytes = [0u8; 2];
    reader.read_exact(&mut bytes)?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_u32_be<R: Read>(reader: &mut R) -> Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

fn seek_forward<R: Seek>(reader: &mut R, chunk_start: u64, chunk_size: u64) -> Result<()> {
    let target = chunk_start
        .checked_add(chunk_size)
        .ok_or(Error::InvalidPcmContainer(
            "chunk length overflows the file cursor",
        ))?;
    reader.seek(SeekFrom::Start(target))?;
    Ok(())
}

fn fourcc_to_string(fourcc: [u8; 4]) -> String {
    String::from_utf8_lossy(&fourcc).into_owned()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{
        AIFC_FORM_TYPE, AIFC_NONE, AIFC_SOWT, AIFF_FORM_TYPE, AiffPcmDescriptor, AiffPcmStream,
        AiffReader, inspect_aiff_total_samples,
    };
    use crate::{RawPcmByteOrder, input::EncodePcmStream};

    fn encode_extended_u32(value: u32) -> [u8; 10] {
        assert!(value > 0);
        let exponent = 31 - value.leading_zeros();
        let biased_exponent = (16_383 + exponent as u16).to_be_bytes();
        let mantissa = (u64::from(value)) << (63 - exponent);
        let mut bytes = [0u8; 10];
        bytes[..2].copy_from_slice(&biased_exponent);
        bytes[2..].copy_from_slice(&mantissa.to_be_bytes());
        bytes
    }

    fn container_bits(valid_bits: u16) -> u16 {
        match valid_bits {
            0..=8 => 8,
            9..=16 => 16,
            17..=24 => 24,
            _ => 32,
        }
    }

    fn write_samples(bytes: &mut Vec<u8>, valid_bits: u16, samples: &[i32], little_endian: bool) {
        let container_bits = container_bits(valid_bits);
        let shift = u32::from(container_bits - valid_bits);
        for &sample in samples {
            let shifted = if shift == 0 { sample } else { sample << shift };
            match container_bits {
                8 => bytes.push(i8::try_from(shifted).unwrap() as u8),
                16 => {
                    let value = i16::try_from(shifted).unwrap();
                    if little_endian {
                        bytes.extend_from_slice(&value.to_le_bytes());
                    } else {
                        bytes.extend_from_slice(&value.to_be_bytes());
                    }
                }
                24 => {
                    let value = shifted as u32;
                    let chunk = if little_endian {
                        [
                            (value & 0xff) as u8,
                            ((value >> 8) & 0xff) as u8,
                            ((value >> 16) & 0xff) as u8,
                        ]
                    } else {
                        [
                            ((value >> 16) & 0xff) as u8,
                            ((value >> 8) & 0xff) as u8,
                            (value & 0xff) as u8,
                        ]
                    };
                    bytes.extend_from_slice(&chunk);
                }
                32 => {
                    if little_endian {
                        bytes.extend_from_slice(&shifted.to_le_bytes());
                    } else {
                        bytes.extend_from_slice(&shifted.to_be_bytes());
                    }
                }
                _ => unreachable!(),
            }
        }
    }

    fn append_chunk(bytes: &mut Vec<u8>, chunk_id: [u8; 4], payload: &[u8]) {
        bytes.extend_from_slice(&chunk_id);
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(payload);
        if !payload.len().is_multiple_of(2) {
            bytes.push(0);
        }
    }

    fn aiff_like_bytes(
        form_type: [u8; 4],
        compression_id: Option<[u8; 4]>,
        valid_bits: u16,
        channels: u16,
        sample_rate: u32,
        samples: &[i32],
    ) -> Vec<u8> {
        let sample_frames = (samples.len() / usize::from(channels)) as u32;
        let little_endian = compression_id == Some(AIFC_SOWT);
        let mut comm = Vec::new();
        comm.extend_from_slice(&channels.to_be_bytes());
        comm.extend_from_slice(&sample_frames.to_be_bytes());
        comm.extend_from_slice(&valid_bits.to_be_bytes());
        comm.extend_from_slice(&encode_extended_u32(sample_rate));
        if let Some(compression_id) = compression_id {
            comm.extend_from_slice(&compression_id);
            comm.push(0);
        }

        let mut ssnd = Vec::new();
        ssnd.extend_from_slice(&0u32.to_be_bytes());
        ssnd.extend_from_slice(&0u32.to_be_bytes());
        write_samples(&mut ssnd, valid_bits, samples, little_endian);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FORM");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&form_type);
        append_chunk(&mut bytes, *b"COMM", &comm);
        append_chunk(&mut bytes, *b"SSND", &ssnd);
        let form_size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&form_size.to_be_bytes());
        bytes
    }

    #[test]
    fn inspects_and_reads_aiff_integer_pcm() {
        let bytes = aiff_like_bytes(AIFF_FORM_TYPE, None, 24, 3, 48_000, &[1, -2, 3, -4, 5, -6]);
        assert_eq!(
            inspect_aiff_total_samples(&mut Cursor::new(&bytes)).unwrap(),
            2
        );
        let reader = AiffReader::new(Cursor::new(bytes)).unwrap();
        let spec = reader.spec();
        let mut stream = reader.into_pcm_stream();
        let mut samples = Vec::new();
        stream.read_chunk(2, &mut samples).unwrap();
        assert_eq!(spec.sample_rate, 48_000);
        assert_eq!(spec.channels, 3);
        assert_eq!(spec.bits_per_sample, 24);
        assert_eq!(samples, vec![1, -2, 3, -4, 5, -6]);
    }

    #[test]
    fn accepts_aifc_none_and_sowt() {
        let none = aiff_like_bytes(
            AIFC_FORM_TYPE,
            Some(AIFC_NONE),
            20,
            2,
            44_100,
            &[1, -2, 3, -4],
        );
        let parsed_none = AiffReader::new(Cursor::new(none)).unwrap();
        assert_eq!(parsed_none.spec().bits_per_sample, 20);

        let sowt = aiff_like_bytes(
            AIFC_FORM_TYPE,
            Some(AIFC_SOWT),
            16,
            2,
            44_100,
            &[10, -11, 12, -13],
        );
        let parsed_sowt = AiffReader::new(Cursor::new(sowt)).unwrap();
        let mut stream = parsed_sowt.into_pcm_stream();
        let mut samples = Vec::new();
        stream.read_chunk(2, &mut samples).unwrap();
        assert_eq!(samples, vec![10, -11, 12, -13]);
    }

    #[test]
    fn rejects_unsupported_aifc_compression_variants() {
        for compression in [*b"ACE2", *b"ACE8", *b"MAC3", *b"MAC6", *b"fl32", *b"????"] {
            let bytes = aiff_like_bytes(AIFC_FORM_TYPE, Some(compression), 16, 1, 44_100, &[1, -1]);
            let error = AiffReader::new(Cursor::new(bytes)).unwrap_err();
            assert!(error.to_string().contains("AIFC"));
        }
    }

    #[test]
    fn rejects_sowt_outside_16bit_pcm() {
        let bytes = aiff_like_bytes(AIFC_FORM_TYPE, Some(AIFC_SOWT), 24, 1, 44_100, &[1, -1]);
        let error = AiffReader::new(Cursor::new(bytes)).unwrap_err();
        assert!(error.to_string().contains("16-bit"));
    }

    #[test]
    fn rejects_malformed_ssnd_size_mismatch() {
        let mut bytes = aiff_like_bytes(AIFF_FORM_TYPE, None, 16, 2, 44_100, &[1, -2, 3, -4]);
        let ssnd_header = bytes
            .windows(4)
            .position(|window| window == b"SSND")
            .expect("SSND chunk present");
        let payload_size_offset = ssnd_header + 4;
        bytes[payload_size_offset..payload_size_offset + 4].copy_from_slice(&8u32.to_be_bytes());
        let error = AiffReader::new(Cursor::new(bytes)).unwrap_err();
        assert!(error.to_string().contains("sample frame count"));
    }

    #[test]
    fn rejects_denormal_sample_rates_without_panicking() {
        let mut bytes = aiff_like_bytes(AIFF_FORM_TYPE, None, 16, 1, 44_100, &[1, -1, 2, -2]);
        bytes[28..38].copy_from_slice(&[0x00, 0x01, 0x80, 0, 0, 0, 0, 0, 0, 0]);
        let error = AiffReader::new(Cursor::new(bytes)).unwrap_err();
        assert!(
            error.to_string().contains("sample rate") || error.to_string().contains("fractional")
        );
    }

    #[test]
    fn directly_constructs_big_endian_aiff_stream() {
        let descriptor = AiffPcmDescriptor {
            sample_rate: 48_000,
            channels: 3,
            valid_bits_per_sample: 24,
            total_samples: 2,
            byte_order: RawPcmByteOrder::BigEndian,
        };
        let mut data = Vec::new();
        write_samples(&mut data, 24, &[1, -2, 3, -4, 5, -6], false);

        let mut stream = AiffPcmStream::new(Cursor::new(data), descriptor).unwrap();
        let mut samples = Vec::new();
        stream.read_chunk(2, &mut samples).unwrap();

        assert_eq!(stream.spec().channel_mask, 0x0007);
        assert_eq!(samples, vec![1, -2, 3, -4, 5, -6]);
    }

    #[test]
    fn directly_constructs_little_endian_sowt_stream() {
        let descriptor = AiffPcmDescriptor {
            sample_rate: 44_100,
            channels: 2,
            valid_bits_per_sample: 16,
            total_samples: 2,
            byte_order: RawPcmByteOrder::LittleEndian,
        };
        let mut data = Vec::new();
        write_samples(&mut data, 16, &[10, -11, 12, -13], true);

        let mut stream = AiffPcmStream::new(Cursor::new(data), descriptor).unwrap();
        let mut samples = Vec::new();
        stream.read_chunk(2, &mut samples).unwrap();

        assert_eq!(samples, vec![10, -11, 12, -13]);
    }

    #[test]
    fn rejects_direct_little_endian_non_16bit_aiff() {
        let descriptor = AiffPcmDescriptor {
            sample_rate: 44_100,
            channels: 1,
            valid_bits_per_sample: 24,
            total_samples: 2,
            byte_order: RawPcmByteOrder::LittleEndian,
        };
        let error = AiffPcmStream::new(Cursor::new(vec![0; 6]), descriptor).unwrap_err();
        assert!(error.to_string().contains("16-bit"));
    }
}
