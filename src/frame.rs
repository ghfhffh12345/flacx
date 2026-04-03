use bitstream_io::{BigEndian, BitWrite, BitWriter};

use crate::{
    crc::{crc16, crc8},
    error::{Error, Result},
};

const FLAC_SYNC_CODE: u16 = 0b11_1111_1111_1110;

pub(crate) struct EncodedFrame {
    pub(crate) bytes: Vec<u8>,
}

pub(crate) fn sample_rate_is_representable(sample_rate: u32) -> bool {
    sample_rate_code(sample_rate).is_some()
}

pub(crate) fn encode_frame(
    interleaved_samples: &[i32],
    channels: u8,
    bits_per_sample: u8,
    sample_rate: u32,
    frame_index: u64,
    max_fixed_order: u8,
) -> Result<EncodedFrame> {
    let channel_count = usize::from(channels);
    if interleaved_samples.len() % channel_count != 0 {
        return Err(Error::Encode("frame samples are not aligned to channel count".into()));
    }

    let block_size_usize = interleaved_samples.len() / channel_count;
    let block_size = u16::try_from(block_size_usize)
        .map_err(|_| Error::Encode("frame block size exceeds u16".into()))?;

    let mut frame = encode_frame_header(
        block_size,
        sample_rate,
        channels,
        bits_per_sample,
        frame_index,
    )?;

    let mut subframes = BitWriter::endian(Vec::new(), BigEndian);
    for channel in 0..channel_count {
        let channel_samples = extract_channel(interleaved_samples, channel_count, channel);
        let subframe = choose_subframe(&channel_samples, bits_per_sample, max_fixed_order)?;
        write_subframe(&mut subframes, &subframe, bits_per_sample)?;
    }
    subframes.byte_align()?;
    frame.extend_from_slice(&subframes.into_writer());

    let footer_crc = crc16(&frame);
    frame.extend_from_slice(&footer_crc.to_be_bytes());

    Ok(EncodedFrame { bytes: frame })
}

fn encode_frame_header(
    block_size: u16,
    sample_rate: u32,
    channels: u8,
    bits_per_sample: u8,
    frame_index: u64,
) -> Result<Vec<u8>> {
    let mut writer = BitWriter::endian(Vec::new(), BigEndian);

    writer.write_unsigned_var(14, FLAC_SYNC_CODE)?;
    writer.write_bit(false)?;
    writer.write_bit(false)?;

    let (block_size_bits, block_size_extra) = block_size_code(block_size);
    let (sample_rate_bits, sample_rate_extra) = sample_rate_code(sample_rate)
        .ok_or_else(|| Error::UnsupportedFlac(format!("sample rate {sample_rate} cannot be written in a FLAC frame header")))?;

    writer.write_unsigned_var(4, block_size_bits)?;
    writer.write_unsigned_var(4, sample_rate_bits)?;
    writer.write_unsigned_var(4, channel_assignment_bits(channels)?)?;
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

fn channel_assignment_bits(channels: u8) -> Result<u8> {
    match channels {
        1 => Ok(0b0000),
        2 => Ok(0b0001),
        _ => Err(Error::UnsupportedFlac(format!(
            "only mono/stereo frame assignment is supported, found {channels} channels"
        ))),
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
            )))
        }
    };

    Ok(bytes)
}

fn extract_channel(interleaved: &[i32], channels: usize, channel: usize) -> Vec<i32> {
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame[channel])
        .collect()
}

#[derive(Debug, Clone)]
enum Subframe {
    Constant(i32),
    Verbatim(Vec<i32>),
    Fixed {
        order: u8,
        warmup: Vec<i32>,
        residual: ResidualEncoding,
    },
}

#[derive(Debug, Clone)]
enum ResidualEncoding {
    Rice { parameter: u8, residuals: Vec<i32> },
    Escape { bits: u8, residuals: Vec<i32> },
}

impl ResidualEncoding {
    fn bit_len(&self) -> usize {
        match self {
            Self::Rice {
                parameter,
                residuals,
            } => {
                let parameter = usize::from(*parameter);
                2 + 4 + 4
                    + residuals
                        .iter()
                        .map(|&residual| {
                            let folded = fold_residual(residual) as usize;
                            (folded >> parameter) + 1 + parameter
                        })
                        .sum::<usize>()
            }
            Self::Escape { bits, residuals } => 2 + 4 + 4 + 5 + residuals.len() * usize::from(*bits),
        }
    }
}

fn choose_subframe(samples: &[i32], bits_per_sample: u8, max_fixed_order: u8) -> Result<Subframe> {
    let mut best = if samples.iter().all(|&sample| sample == samples[0]) {
        Some((usize::from(bits_per_sample) + 8, Subframe::Constant(samples[0])))
    } else {
        None
    };

    let verbatim_bits = 8 + samples.len() * usize::from(bits_per_sample);
    best = pick_better(best, verbatim_bits, Subframe::Verbatim(samples.to_vec()));

    for order in 0..=max_fixed_order.min(4) {
        if samples.len() < usize::from(order) {
            continue;
        }

        if let Some(residuals) = fixed_residuals(samples, order) {
            let residual = choose_residual_encoding(&residuals);
            let estimated_bits =
                8 + usize::from(order) * usize::from(bits_per_sample) + residual.bit_len();
            best = pick_better(
                best,
                estimated_bits,
                Subframe::Fixed {
                    order,
                    warmup: samples[..usize::from(order)].to_vec(),
                    residual,
                },
            );
        }
    }

    best.map(|(_, subframe)| subframe)
        .ok_or_else(|| Error::Encode("unable to select a subframe".into()))
}

fn pick_better(current: Option<(usize, Subframe)>, bits: usize, candidate: Subframe) -> Option<(usize, Subframe)> {
    match current {
        Some((best_bits, _)) if best_bits <= bits => current,
        _ => Some((bits, candidate)),
    }
}

fn fixed_residuals(samples: &[i32], order: u8) -> Option<Vec<i32>> {
    let order_usize = usize::from(order);
    let mut residuals = Vec::with_capacity(samples.len().saturating_sub(order_usize));

    for index in order_usize..samples.len() {
        let prediction = match order {
            0 => 0i64,
            1 => i64::from(samples[index - 1]),
            2 => 2 * i64::from(samples[index - 1]) - i64::from(samples[index - 2]),
            3 => {
                3 * i64::from(samples[index - 1])
                    - 3 * i64::from(samples[index - 2])
                    + i64::from(samples[index - 3])
            }
            4 => {
                4 * i64::from(samples[index - 1])
                    - 6 * i64::from(samples[index - 2])
                    + 4 * i64::from(samples[index - 3])
                    - i64::from(samples[index - 4])
            }
            _ => return None,
        };

        let residual = i64::from(samples[index]) - prediction;
        if residual <= i64::from(i32::MIN) || residual > i64::from(i32::MAX) {
            return None;
        }

        residuals.push(residual as i32);
    }

    Some(residuals)
}

fn choose_residual_encoding(residuals: &[i32]) -> ResidualEncoding {
    let escape_bits = residual_escape_width(residuals);
    let mut best_encoding = ResidualEncoding::Escape {
        bits: escape_bits,
        residuals: residuals.to_vec(),
    };
    let mut best_bits = best_encoding.bit_len();

    for parameter in 0..=14u8 {
        let parameter_usize = usize::from(parameter);
        let bit_len = 2 + 4 + 4
            + residuals
                .iter()
                .map(|&residual| {
                    let folded = fold_residual(residual) as usize;
                    (folded >> parameter_usize) + 1 + parameter_usize
                })
                .sum::<usize>();

        if bit_len < best_bits {
            best_bits = bit_len;
            best_encoding = ResidualEncoding::Rice {
                parameter,
                residuals: residuals.to_vec(),
            };
        }
    }

    best_encoding
}

fn residual_escape_width(residuals: &[i32]) -> u8 {
    if residuals.iter().all(|&value| value == 0) {
        return 0;
    }

    (1..=31)
        .find(|&bits| {
            let minimum = -(1i64 << (bits - 1));
            let maximum = (1i64 << (bits - 1)) - 1;
            residuals.iter().all(|&value| {
                let value = i64::from(value);
                value >= minimum && value <= maximum
            })
        })
        .expect("non-i32::MIN residuals always fit into 31 bits") as u8
}

fn fold_residual(residual: i32) -> u32 {
    if residual >= 0 {
        (residual as u32) << 1
    } else {
        ((-(residual as i64) as u32) << 1) - 1
    }
}

fn write_subframe(
    writer: &mut BitWriter<Vec<u8>, BigEndian>,
    subframe: &Subframe,
    bits_per_sample: u8,
) -> Result<()> {
    match subframe {
        Subframe::Constant(sample) => {
            writer.write_bit(false)?;
            writer.write_unsigned_var(6, 0b000000u8)?;
            writer.write_bit(false)?;
            writer.write_signed_var(u32::from(bits_per_sample), *sample)?;
        }
        Subframe::Verbatim(samples) => {
            writer.write_bit(false)?;
            writer.write_unsigned_var(6, 0b000001u8)?;
            writer.write_bit(false)?;
            for &sample in samples {
                writer.write_signed_var(u32::from(bits_per_sample), sample)?;
            }
        }
        Subframe::Fixed {
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
    }

    Ok(())
}

fn write_residual(writer: &mut BitWriter<Vec<u8>, BigEndian>, encoding: &ResidualEncoding) -> Result<()> {
    writer.write_unsigned_var(2, 0b00u8)?;
    writer.write_unsigned_var(4, 0u8)?;

    match encoding {
        ResidualEncoding::Rice {
            parameter,
            residuals,
        } => {
            writer.write_unsigned_var(4, *parameter)?;
            let mask = if *parameter == 0 {
                0
            } else {
                (1u32 << u32::from(*parameter)) - 1
            };

            for &residual in residuals {
                let folded = fold_residual(residual);
                let quotient = folded >> u32::from(*parameter);
                writer.write_unary::<1>(quotient)?;
                if *parameter > 0 {
                    writer.write_unsigned_var(u32::from(*parameter), folded & mask)?;
                }
            }
        }
        ResidualEncoding::Escape { bits, residuals } => {
            writer.write_unsigned_var(4, 0b1111u8)?;
            writer.write_unsigned_var(5, *bits)?;
            if *bits > 0 {
                for &residual in residuals {
                    writer.write_signed_var(u32::from(*bits), residual)?;
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{encode_utf8_number, sample_rate_is_representable};

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
}
