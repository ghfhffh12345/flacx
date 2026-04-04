use std::io::{self, Seek, SeekFrom, Write};

use bitstream_io::{BigEndian, BitWrite, BitWriter};

use crate::{
    crc::{crc8, crc16},
    error::{Error, Result},
    model::{
        AnalyzedSubframe, ChannelAssignment, FrameAnalysis, ResidualEncoding, ResidualPartition,
    },
    stream_info::StreamInfo,
};

const STREAMINFO_LENGTH: [u8; 3] = [0x00, 0x00, 34];
const FLAC_SYNC_CODE: u16 = 0b11_1111_1111_1110;

pub(crate) struct EncodedFrame {
    pub(crate) bytes: Vec<u8>,
    pub(crate) sample_count: u16,
}

pub(crate) struct FlacWriter<W: Seek + Write> {
    writer: W,
    stream_info: StreamInfo,
    streaminfo_offset: u64,
}

impl<W: Seek + Write> FlacWriter<W> {
    pub(crate) fn new(mut writer: W, stream_info: StreamInfo) -> io::Result<Self> {
        writer.write_all(b"fLaC")?;
        writer.write_all(&[
            0x80,
            STREAMINFO_LENGTH[0],
            STREAMINFO_LENGTH[1],
            STREAMINFO_LENGTH[2],
        ])?;
        let streaminfo_offset = writer.stream_position()?;
        writer.write_all(&stream_info.to_bytes())?;

        Ok(Self {
            writer,
            stream_info,
            streaminfo_offset,
        })
    }

    pub(crate) fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.stream_info.update_frame_size(frame.len() as u32);
        self.writer.write_all(frame)
    }

    pub(crate) fn finalize(mut self) -> io::Result<(W, StreamInfo)> {
        let end_position = self.writer.stream_position()?;
        self.writer.seek(SeekFrom::Start(self.streaminfo_offset))?;
        self.writer.write_all(&self.stream_info.to_bytes())?;
        self.writer.seek(SeekFrom::Start(end_position))?;
        self.writer.flush()?;
        Ok((self.writer, self.stream_info))
    }
}

pub(crate) fn sample_rate_is_representable(sample_rate: u32) -> bool {
    sample_rate_code(sample_rate).is_some()
}

pub(crate) fn serialize_frame(
    analysis: &FrameAnalysis,
    bits_per_sample: u8,
    sample_rate: u32,
    frame_index: u64,
) -> Result<EncodedFrame> {
    if analysis.subframes.is_empty() {
        return Err(Error::Encode("frame analysis is missing subframes".into()));
    }
    let mut frame = encode_frame_header(
        analysis.block_size,
        sample_rate,
        analysis.channel_assignment,
        bits_per_sample,
        frame_index,
    )?;
    let effective_bps = channel_bits_per_sample(analysis.channel_assignment, bits_per_sample);
    let mut subframes = BitWriter::endian(Vec::new(), BigEndian);
    for (subframe, subframe_bps) in analysis.subframes.iter().zip(effective_bps) {
        write_subframe(&mut subframes, subframe, subframe_bps)?;
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

fn encode_frame_header(
    block_size: u16,
    sample_rate: u32,
    assignment: ChannelAssignment,
    bits_per_sample: u8,
    frame_index: u64,
) -> Result<Vec<u8>> {
    let mut writer = BitWriter::endian(Vec::new(), BigEndian);
    writer.write_unsigned_var(14, FLAC_SYNC_CODE)?;
    writer.write_bit(false)?;
    writer.write_bit(false)?;
    let (block_size_bits, block_size_extra) = block_size_code(block_size);
    let (sample_rate_bits, sample_rate_extra) = sample_rate_code(sample_rate).ok_or_else(|| {
        Error::UnsupportedFlac(format!(
            "sample rate {sample_rate} cannot be written in a FLAC frame header"
        ))
    })?;
    writer.write_unsigned_var(4, block_size_bits)?;
    writer.write_unsigned_var(4, sample_rate_bits)?;
    writer.write_unsigned_var(4, channel_assignment_bits(assignment))?;
    writer.write_unsigned_var(3, bit_depth_bits(bits_per_sample)?)?;
    writer.write_bit(false)?;
    writer.write_bytes(&encode_utf8_number(frame_index)?)?;
    writer.write_bytes(&block_size_extra)?;
    writer.write_bytes(&sample_rate_extra)?;
    writer.byte_align()?;
    let mut header = writer.into_writer();
    header.push(crc8(&header));
    Ok(header)
}

fn channel_assignment_bits(assignment: ChannelAssignment) -> u8 {
    match assignment {
        ChannelAssignment::IndependentMono => 0b0000,
        ChannelAssignment::IndependentStereo => 0b0001,
        ChannelAssignment::LeftSide => 0b1000,
        ChannelAssignment::SideRight => 0b1001,
        ChannelAssignment::MidSide => 0b1010,
    }
}

fn channel_bits_per_sample(assignment: ChannelAssignment, bits_per_sample: u8) -> [u8; 2] {
    match assignment {
        ChannelAssignment::IndependentMono => [bits_per_sample, bits_per_sample],
        ChannelAssignment::IndependentStereo => [bits_per_sample, bits_per_sample],
        ChannelAssignment::LeftSide => [bits_per_sample, bits_per_sample + 1],
        ChannelAssignment::SideRight => [bits_per_sample + 1, bits_per_sample],
        ChannelAssignment::MidSide => [bits_per_sample, bits_per_sample + 1],
    }
}

fn bit_depth_bits(bits_per_sample: u8) -> Result<u8> {
    match bits_per_sample {
        8 => Ok(0b001),
        12 => Ok(0b010),
        16 => Ok(0b100),
        20 => Ok(0b101),
        24 => Ok(0b110),
        32 => Ok(0b111),
        _ => Err(Error::UnsupportedFlac(format!(
            "bit depth {bits_per_sample} is not encodable in a frame header"
        ))),
    }
}

fn block_size_code(block_size: u16) -> (u8, Vec<u8>) {
    match block_size {
        192 => (0b0001, Vec::new()),
        576 => (0b0010, Vec::new()),
        1152 => (0b0011, Vec::new()),
        2304 => (0b0100, Vec::new()),
        4608 => (0b0101, Vec::new()),
        256 => (0b1000, Vec::new()),
        512 => (0b1001, Vec::new()),
        1024 => (0b1010, Vec::new()),
        2048 => (0b1011, Vec::new()),
        4096 => (0b1100, Vec::new()),
        8192 => (0b1101, Vec::new()),
        16384 => (0b1110, Vec::new()),
        32768 => (0b1111, Vec::new()),
        _ if block_size <= 256 => (0b0110, vec![(block_size - 1) as u8]),
        _ => (0b0111, (block_size - 1).to_be_bytes().to_vec()),
    }
}

fn sample_rate_code(sample_rate: u32) -> Option<(u8, Vec<u8>)> {
    match sample_rate {
        0 => None,
        88_200 => Some((0b0001, Vec::new())),
        176_400 => Some((0b0010, Vec::new())),
        192_000 => Some((0b0011, Vec::new())),
        8_000 => Some((0b0100, Vec::new())),
        16_000 => Some((0b0101, Vec::new())),
        22_050 => Some((0b0110, Vec::new())),
        24_000 => Some((0b0111, Vec::new())),
        32_000 => Some((0b1000, Vec::new())),
        44_100 => Some((0b1001, Vec::new())),
        48_000 => Some((0b1010, Vec::new())),
        96_000 => Some((0b1011, Vec::new())),
        _ if sample_rate % 1000 == 0 && sample_rate / 1000 <= u32::from(u8::MAX) => {
            Some((0b1100, vec![(sample_rate / 1000) as u8]))
        }
        _ if sample_rate <= u32::from(u16::MAX) => {
            Some((0b1101, (sample_rate as u16).to_be_bytes().to_vec()))
        }
        _ if sample_rate % 10 == 0 && sample_rate / 10 <= u32::from(u16::MAX) => {
            Some((0b1110, ((sample_rate / 10) as u16).to_be_bytes().to_vec()))
        }
        _ => None,
    }
}

fn encode_utf8_number(value: u64) -> Result<Vec<u8>> {
    let bytes = match value {
        0x0000_0000_0000..=0x0000_0000_007f => vec![value as u8],
        0x0000_0000_0080..=0x0000_0000_07ff => vec![
            0b1100_0000 | ((value >> 6) as u8 & 0b0001_1111),
            0b1000_0000 | (value as u8 & 0b0011_1111),
        ],
        0x0000_0000_0800..=0x0000_0000_ffff => vec![
            0b1110_0000 | ((value >> 12) as u8 & 0b0000_1111),
            0b1000_0000 | ((value >> 6) as u8 & 0b0011_1111),
            0b1000_0000 | (value as u8 & 0b0011_1111),
        ],
        0x0000_0001_0000..=0x0000_001f_ffff => vec![
            0b1111_0000 | ((value >> 18) as u8 & 0b0000_0111),
            0b1000_0000 | ((value >> 12) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 6) as u8 & 0b0011_1111),
            0b1000_0000 | (value as u8 & 0b0011_1111),
        ],
        0x0000_0020_0000..=0x0000_03ff_ffff => vec![
            0b1111_1000 | ((value >> 24) as u8 & 0b0000_0011),
            0b1000_0000 | ((value >> 18) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 12) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 6) as u8 & 0b0011_1111),
            0b1000_0000 | (value as u8 & 0b0011_1111),
        ],
        0x0000_0400_0000..=0x0000_7fff_ffff => vec![
            0b1111_1100 | ((value >> 30) as u8 & 0b0000_0001),
            0b1000_0000 | ((value >> 24) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 18) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 12) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 6) as u8 & 0b0011_1111),
            0b1000_0000 | (value as u8 & 0b0011_1111),
        ],
        0x0000_8000_0000..=0x000f_ffff_ffff => vec![
            0b1111_1110,
            0b1000_0000 | ((value >> 30) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 24) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 18) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 12) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 6) as u8 & 0b0011_1111),
            0b1000_0000 | (value as u8 & 0b0011_1111),
        ],
        _ => {
            return Err(Error::UnsupportedFlac(format!(
                "coded frame number {value} exceeds the FLAC limit"
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
                residuals,
            } => {
                writer.write_unsigned_var(encoding.method.parameter_bits() as u32, *parameter)?;
                let mask = if *parameter == 0 {
                    0
                } else {
                    (1u32 << u32::from(*parameter)) - 1
                };
                for &residual in residuals {
                    let folded = fold_residual(residual) as u32;
                    let quotient = folded >> u32::from(*parameter);
                    writer.write_unary::<1>(quotient)?;
                    if *parameter > 0 {
                        writer.write_unsigned_var(u32::from(*parameter), folded & mask)?;
                    }
                }
            }
            ResidualPartition::Escape { bits, residuals } => {
                writer.write_unsigned_var(
                    encoding.method.parameter_bits() as u32,
                    encoding.method.escape_code(),
                )?;
                writer.write_unsigned_var(5, *bits)?;
                if *bits > 0 {
                    for &residual in residuals {
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

#[cfg(test)]
mod tests {
    use super::{channel_assignment_bits, encode_utf8_number, sample_rate_is_representable};
    use crate::{
        level::LevelProfile,
        model::{ChannelAssignment, encode_frame},
    };

    #[test]
    fn sample_rate_representation_matches_streamable_header_limits() {
        assert!(!sample_rate_is_representable(0));
        assert!(sample_rate_is_representable(44_100));
        assert!(sample_rate_is_representable(50_000));
        assert!(sample_rate_is_representable(65_000));
        assert!(sample_rate_is_representable(65_350));
        assert!(!sample_rate_is_representable(700_001));
    }

    #[test]
    fn utf8_like_frame_numbers_match_rfc_ranges() {
        assert_eq!(encode_utf8_number(0).unwrap(), vec![0x00]);
        assert_eq!(encode_utf8_number(0x7f).unwrap(), vec![0x7f]);
        assert_eq!(encode_utf8_number(0x80).unwrap(), vec![0xc2, 0x80]);
        assert_eq!(encode_utf8_number(0x800).unwrap(), vec![0xe0, 0xa0, 0x80]);
        assert_eq!(
            encode_utf8_number(0x1f_ffff).unwrap(),
            vec![0xf7, 0xbf, 0xbf, 0xbf]
        );
    }

    #[test]
    fn stereo_assignment_bits_match_rfc_table() {
        assert_eq!(
            channel_assignment_bits(ChannelAssignment::IndependentStereo),
            0b0001
        );
        assert_eq!(channel_assignment_bits(ChannelAssignment::LeftSide), 0b1000);
        assert_eq!(
            channel_assignment_bits(ChannelAssignment::SideRight),
            0b1001
        );
        assert_eq!(channel_assignment_bits(ChannelAssignment::MidSide), 0b1010);
    }

    #[test]
    fn frame_header_uses_supported_stereo_assignment_bits() {
        let mut interleaved = Vec::new();
        for sample in 0..32i32 {
            interleaved.push(sample * 16);
            interleaved.push(sample * 16 + (sample & 1));
        }
        let profile = LevelProfile::new(256, 4, 12, 4, true, true);
        let encoded = encode_frame(&interleaved, 2, 16, 44_100, 0, profile).unwrap();
        let assignment = (encoded.bytes[3] >> 4) & 0x0f;
        assert!(matches!(assignment, 0b0001 | 0b1000 | 0b1001 | 0b1010));
    }
}
