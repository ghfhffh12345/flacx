use bitstream_io::{BigEndian, BitWrite, BitWriter};

use crate::{
    crc::{crc8, crc16},
    error::{Error, Result},
    model::{
        AnalyzedSubframe, ChannelAssignment, FrameAnalysis, ResidualEncoding, ResidualPartition,
    },
    stream_info::MAX_STREAMINFO_SAMPLE_RATE,
};

const FLAC_SYNC_CODE: u16 = 0b11_1111_1111_1110;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameHeaderNumber {
    Frame(u64),
    Sample(u64),
}

pub(crate) struct EncodedFrame {
    pub(crate) bytes: Vec<u8>,
    pub(crate) sample_count: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeaderBytes {
    bytes: [u8; 7],
    len: usize,
}

impl HeaderBytes {
    fn empty() -> Self {
        Self {
            bytes: [0; 7],
            len: 0,
        }
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[cfg(test)]
pub(crate) fn sample_rate_is_representable(sample_rate: u32) -> bool {
    sample_rate_code(sample_rate).is_some()
}

pub(crate) fn serialize_frame(
    analysis: &FrameAnalysis,
    bits_per_sample: u8,
    sample_rate: u32,
    header_number: FrameHeaderNumber,
) -> Result<EncodedFrame> {
    if analysis.subframes.is_empty() {
        return Err(Error::Encode("frame analysis is missing subframes".into()));
    }
    if analysis.subframes.len() != analysis.channel_assignment.channel_count() {
        return Err(Error::Encode(
            "frame analysis subframe count does not match channel assignment".into(),
        ));
    }
    let mut frame = encode_frame_header(
        analysis.block_size,
        sample_rate,
        analysis.channel_assignment,
        bits_per_sample,
        header_number,
    )?;
    let mut subframes = BitWriter::endian(Vec::new(), BigEndian);
    match analysis.channel_assignment {
        ChannelAssignment::Independent(_) => {
            for subframe in &analysis.subframes {
                write_subframe(&mut subframes, subframe, bits_per_sample)?;
            }
        }
        ChannelAssignment::LeftSide => {
            write_subframe(&mut subframes, &analysis.subframes[0], bits_per_sample)?;
            write_subframe(&mut subframes, &analysis.subframes[1], bits_per_sample + 1)?;
        }
        ChannelAssignment::SideRight => {
            write_subframe(&mut subframes, &analysis.subframes[0], bits_per_sample + 1)?;
            write_subframe(&mut subframes, &analysis.subframes[1], bits_per_sample)?;
        }
        ChannelAssignment::MidSide => {
            write_subframe(&mut subframes, &analysis.subframes[0], bits_per_sample)?;
            write_subframe(&mut subframes, &analysis.subframes[1], bits_per_sample + 1)?;
        }
    }
    subframes.byte_align()?;
    frame.extend_from_slice(&subframes.into_writer());
    let footer_crc = crc16(&frame);
    frame.extend_from_slice(&footer_crc.to_be_bytes());
    Ok(EncodedFrame {
        bytes: frame,
        sample_count: analysis.block_size,
    })
}

pub(crate) fn encode_frame_header(
    block_size: u16,
    sample_rate: u32,
    assignment: ChannelAssignment,
    bits_per_sample: u8,
    header_number: FrameHeaderNumber,
) -> Result<Vec<u8>> {
    let mut writer = BitWriter::endian(Vec::new(), BigEndian);
    writer.write_unsigned_var(14, FLAC_SYNC_CODE)?;
    writer.write_bit(false)?;
    writer.write_bit(matches!(header_number, FrameHeaderNumber::Sample(_)))?;
    let (block_size_bits, block_size_extra) = block_size_code(block_size);
    let (sample_rate_bits, sample_rate_extra) = sample_rate_code_or_streaminfo(sample_rate)?;
    writer.write_unsigned_var(4, block_size_bits)?;
    writer.write_unsigned_var(4, sample_rate_bits)?;
    writer.write_unsigned_var(4, channel_assignment_bits(assignment))?;
    writer.write_unsigned_var(3, bit_depth_bits(bits_per_sample)?)?;
    writer.write_bit(false)?;
    let header_number_bytes = encode_utf8_number(header_number.value())?;
    writer.write_bytes(header_number_bytes.as_slice())?;
    writer.write_bytes(block_size_extra.as_slice())?;
    writer.write_bytes(sample_rate_extra.as_slice())?;
    writer.byte_align()?;
    let mut header = writer.into_writer();
    header.push(crc8(&header));
    Ok(header)
}

impl FrameHeaderNumber {
    fn value(self) -> u64 {
        match self {
            Self::Frame(value) | Self::Sample(value) => value,
        }
    }
}

pub(crate) fn channel_assignment_bits(assignment: ChannelAssignment) -> u8 {
    match assignment {
        ChannelAssignment::Independent(channels) => channels - 1,
        ChannelAssignment::LeftSide => 0b1000,
        ChannelAssignment::SideRight => 0b1001,
        ChannelAssignment::MidSide => 0b1010,
    }
}

pub(crate) fn bit_depth_bits(bits_per_sample: u8) -> Result<u8> {
    match bits_per_sample {
        8 => Ok(0b001),
        12 => Ok(0b010),
        16 => Ok(0b100),
        20 => Ok(0b101),
        24 => Ok(0b110),
        32 => Ok(0b111),
        4..=32 => Ok(0b000),
        _ => Err(Error::UnsupportedFlac(format!(
            "bit depth {bits_per_sample} is not supported by FLAC encoding"
        ))),
    }
}

fn block_size_code(block_size: u16) -> (u8, HeaderBytes) {
    match block_size {
        192 => (0b0001, HeaderBytes::empty()),
        576 => (0b0010, HeaderBytes::empty()),
        1152 => (0b0011, HeaderBytes::empty()),
        2304 => (0b0100, HeaderBytes::empty()),
        4608 => (0b0101, HeaderBytes::empty()),
        256 => (0b1000, HeaderBytes::empty()),
        512 => (0b1001, HeaderBytes::empty()),
        1024 => (0b1010, HeaderBytes::empty()),
        2048 => (0b1011, HeaderBytes::empty()),
        4096 => (0b1100, HeaderBytes::empty()),
        8192 => (0b1101, HeaderBytes::empty()),
        16384 => (0b1110, HeaderBytes::empty()),
        32768 => (0b1111, HeaderBytes::empty()),
        _ if block_size <= 256 => {
            let mut bytes = [0; 7];
            bytes[0] = (block_size - 1) as u8;
            (0b0110, HeaderBytes { bytes, len: 1 })
        }
        _ => {
            let mut bytes = [0; 7];
            bytes[..2].copy_from_slice(&(block_size - 1).to_be_bytes());
            (0b0111, HeaderBytes { bytes, len: 2 })
        }
    }
}

fn sample_rate_code_or_streaminfo(sample_rate: u32) -> Result<(u8, HeaderBytes)> {
    match sample_rate_code(sample_rate) {
        Some(encoding) => Ok(encoding),
        None if sample_rate != 0 && sample_rate <= MAX_STREAMINFO_SAMPLE_RATE => {
            Ok((0b0000, HeaderBytes::empty()))
        }
        None => Err(streaminfo_sample_rate_limit_error(sample_rate)),
    }
}

fn sample_rate_code(sample_rate: u32) -> Option<(u8, HeaderBytes)> {
    match sample_rate {
        0 => None,
        88_200 => Some((0b0001, HeaderBytes::empty())),
        176_400 => Some((0b0010, HeaderBytes::empty())),
        192_000 => Some((0b0011, HeaderBytes::empty())),
        8_000 => Some((0b0100, HeaderBytes::empty())),
        16_000 => Some((0b0101, HeaderBytes::empty())),
        22_050 => Some((0b0110, HeaderBytes::empty())),
        24_000 => Some((0b0111, HeaderBytes::empty())),
        32_000 => Some((0b1000, HeaderBytes::empty())),
        44_100 => Some((0b1001, HeaderBytes::empty())),
        48_000 => Some((0b1010, HeaderBytes::empty())),
        96_000 => Some((0b1011, HeaderBytes::empty())),
        _ if sample_rate.is_multiple_of(1000) && sample_rate / 1000 <= u32::from(u8::MAX) => {
            let mut bytes = [0; 7];
            bytes[0] = (sample_rate / 1000) as u8;
            Some((0b1100, HeaderBytes { bytes, len: 1 }))
        }
        _ if sample_rate <= u32::from(u16::MAX) => {
            let mut bytes = [0; 7];
            bytes[..2].copy_from_slice(&(sample_rate as u16).to_be_bytes());
            Some((0b1101, HeaderBytes { bytes, len: 2 }))
        }
        _ if sample_rate.is_multiple_of(10) && sample_rate / 10 <= u32::from(u16::MAX) => {
            let mut bytes = [0; 7];
            bytes[..2].copy_from_slice(&((sample_rate / 10) as u16).to_be_bytes());
            Some((0b1110, HeaderBytes { bytes, len: 2 }))
        }
        _ => None,
    }
}

fn streaminfo_sample_rate_limit_error(sample_rate: u32) -> Error {
    Error::UnsupportedFlac(format!(
        "sample rate {sample_rate} exceeds STREAMINFO's 20-bit limit of {MAX_STREAMINFO_SAMPLE_RATE}"
    ))
}

pub(crate) fn encode_utf8_number(value: u64) -> Result<HeaderBytes> {
    let bytes = match value {
        0x0000_0000_0000..=0x0000_0000_007f => HeaderBytes {
            bytes: [value as u8, 0, 0, 0, 0, 0, 0],
            len: 1,
        },
        0x0000_0000_0080..=0x0000_0000_07ff => HeaderBytes {
            bytes: [
                0b1100_0000 | ((value >> 6) as u8 & 0b0001_1111),
                0b1000_0000 | (value as u8 & 0b0011_1111),
                0,
                0,
                0,
                0,
                0,
            ],
            len: 2,
        },
        0x0000_0000_0800..=0x0000_0000_ffff => HeaderBytes {
            bytes: [
                0b1110_0000 | ((value >> 12) as u8 & 0b0000_1111),
                0b1000_0000 | ((value >> 6) as u8 & 0b0011_1111),
                0b1000_0000 | (value as u8 & 0b0011_1111),
                0,
                0,
                0,
                0,
            ],
            len: 3,
        },
        0x0000_0001_0000..=0x0000_001f_ffff => HeaderBytes {
            bytes: [
                0b1111_0000 | ((value >> 18) as u8 & 0b0000_0111),
                0b1000_0000 | ((value >> 12) as u8 & 0b0011_1111),
                0b1000_0000 | ((value >> 6) as u8 & 0b0011_1111),
                0b1000_0000 | (value as u8 & 0b0011_1111),
                0,
                0,
                0,
            ],
            len: 4,
        },
        0x0000_0020_0000..=0x0000_03ff_ffff => HeaderBytes {
            bytes: [
                0b1111_1000 | ((value >> 24) as u8 & 0b0000_0011),
                0b1000_0000 | ((value >> 18) as u8 & 0b0011_1111),
                0b1000_0000 | ((value >> 12) as u8 & 0b0011_1111),
                0b1000_0000 | ((value >> 6) as u8 & 0b0011_1111),
                0b1000_0000 | (value as u8 & 0b0011_1111),
                0,
                0,
            ],
            len: 5,
        },
        0x0000_0400_0000..=0x0000_7fff_ffff => HeaderBytes {
            bytes: [
                0b1111_1100 | ((value >> 30) as u8 & 0b0000_0001),
                0b1000_0000 | ((value >> 24) as u8 & 0b0011_1111),
                0b1000_0000 | ((value >> 18) as u8 & 0b0011_1111),
                0b1000_0000 | ((value >> 12) as u8 & 0b0011_1111),
                0b1000_0000 | ((value >> 6) as u8 & 0b0011_1111),
                0b1000_0000 | (value as u8 & 0b0011_1111),
                0,
            ],
            len: 6,
        },
        0x0000_8000_0000..=0x000f_ffff_ffff => HeaderBytes {
            bytes: [
                0b1111_1110,
                0b1000_0000 | ((value >> 30) as u8 & 0b0011_1111),
                0b1000_0000 | ((value >> 24) as u8 & 0b0011_1111),
                0b1000_0000 | ((value >> 18) as u8 & 0b0011_1111),
                0b1000_0000 | ((value >> 12) as u8 & 0b0011_1111),
                0b1000_0000 | ((value >> 6) as u8 & 0b0011_1111),
                0b1000_0000 | (value as u8 & 0b0011_1111),
            ],
            len: 7,
        },
        _ => {
            return Err(Error::UnsupportedFlac(format!(
                "coded header number {value} exceeds the FLAC limit"
            )));
        }
    };
    Ok(bytes)
}

fn write_subframe(
    writer: &mut BitWriter<Vec<u8>, BigEndian>,
    subframe: &AnalyzedSubframe,
    bits_per_sample: u8,
) -> Result<()> {
    match subframe {
        AnalyzedSubframe::Constant(sample) => {
            writer.write_bit(false)?;
            writer.write_unsigned_var(6, 0b000000u8)?;
            writer.write_bit(false)?;
            writer.write_signed_var(u32::from(bits_per_sample), *sample)?;
        }
        AnalyzedSubframe::Verbatim(samples) => {
            writer.write_bit(false)?;
            writer.write_unsigned_var(6, 0b000001u8)?;
            writer.write_bit(false)?;
            for &sample in samples {
                writer.write_signed_var(u32::from(bits_per_sample), sample)?;
            }
        }
        AnalyzedSubframe::Fixed {
            order,
            warmup,
            residual,
        } => {
            writer.write_bit(false)?;
            writer.write_unsigned_var(6, 0b001000u8 + *order)?;
            writer.write_bit(false)?;
            for &sample in warmup {
                writer.write_signed_var(u32::from(bits_per_sample), sample)?;
            }
            write_residual(writer, residual)?;
        }
        AnalyzedSubframe::Lpc {
            order,
            warmup,
            precision,
            shift,
            coefficients,
            residual,
        } => {
            writer.write_bit(false)?;
            writer.write_unsigned_var(6, 0b100000u8 + (*order - 1))?;
            writer.write_bit(false)?;
            for &sample in warmup {
                writer.write_signed_var(u32::from(bits_per_sample), sample)?;
            }
            writer.write_unsigned_var(4, *precision - 1)?;
            writer.write_signed_var(5, i32::from(*shift))?;
            for &coefficient in coefficients {
                writer.write_signed_var(u32::from(*precision), i32::from(coefficient))?;
            }
            write_residual(writer, residual)?;
        }
    }
    Ok(())
}

fn write_residual(
    writer: &mut BitWriter<Vec<u8>, BigEndian>,
    encoding: &ResidualEncoding,
) -> Result<()> {
    writer.write_unsigned_var(2, encoding.method.code())?;
    writer.write_unsigned_var(4, encoding.partition_order)?;
    for partition in &encoding.partitions {
        match partition {
            ResidualPartition::Rice {
                parameter,
                start,
                end,
            } => {
                writer.write_unsigned_var(encoding.method.parameter_bits() as u32, *parameter)?;
                let mask = if *parameter == 0 {
                    0
                } else {
                    (1u32 << u32::from(*parameter)) - 1
                };
                for &residual in &encoding.residuals[*start..*end] {
                    let folded = fold_residual(residual) as u32;
                    let quotient = folded >> u32::from(*parameter);
                    writer.write_unary::<1>(quotient)?;
                    if *parameter > 0 {
                        writer.write_unsigned_var(u32::from(*parameter), folded & mask)?;
                    }
                }
            }
            ResidualPartition::Escape { bits, start, end } => {
                writer.write_unsigned_var(
                    encoding.method.parameter_bits() as u32,
                    encoding.method.escape_code(),
                )?;
                writer.write_unsigned_var(5, *bits)?;
                if *bits > 0 {
                    for &residual in &encoding.residuals[*start..*end] {
                        writer.write_signed_var(u32::from(*bits), residual)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn fold_residual(residual: i32) -> usize {
    if residual >= 0 {
        (residual as usize) << 1
    } else {
        (((-i64::from(residual)) as usize) << 1) - 1
    }
}
