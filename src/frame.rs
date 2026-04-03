use bitstream_io::{BigEndian, BitWrite, BitWriter};

use crate::{
    crc::{crc8, crc16},
    error::{Error, Result},
    level::LevelProfile,
};

const FLAC_SYNC_CODE: u16 = 0b11_1111_1111_1110;
const MAX_STREAMABLE_LPC_ORDER_AT_48KHZ: u8 = 12;
const MAX_FLAC_LPC_ORDER: u8 = 32;
const MAX_RICE_PARTITION_ORDER: u8 = 8;
const MAX_ESCAPE_BITS: u8 = 31;

pub(crate) struct EncodedFrame {
    pub(crate) bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelAssignment {
    IndependentMono,
    IndependentStereo,
    LeftSide,
    SideRight,
    MidSide,
}

#[derive(Debug, Clone)]
struct FrameAnalysis {
    block_size: u16,
    channel_assignment: ChannelAssignment,
    subframes: Vec<AnalyzedSubframe>,
}

#[derive(Debug, Clone)]
enum AnalyzedSubframe {
    Constant(i32),
    Verbatim(Vec<i32>),
    Fixed {
        order: u8,
        warmup: Vec<i32>,
        residual: ResidualEncoding,
    },
    Lpc {
        order: u8,
        warmup: Vec<i32>,
        precision: u8,
        shift: u8,
        coefficients: Vec<i16>,
        residual: ResidualEncoding,
    },
}

#[derive(Debug, Clone)]
struct ResidualEncoding {
    method: RiceMethod,
    partition_order: u8,
    partitions: Vec<ResidualPartition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RiceMethod {
    FourBit,
    FiveBit,
}

#[derive(Debug, Clone)]
enum ResidualPartition {
    Rice { parameter: u8, residuals: Vec<i32> },
    Escape { bits: u8, residuals: Vec<i32> },
}

#[derive(Debug, Clone)]
struct Candidate {
    bits: usize,
    assignment: ChannelAssignment,
    subframes: Vec<AnalyzedSubframe>,
}

#[derive(Debug, Clone)]
struct QuantizedLpc {
    precision: u8,
    shift: u8,
    coefficients: Vec<i16>,
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
    profile: LevelProfile,
) -> Result<EncodedFrame> {
    let analysis = analyze_frame(
        interleaved_samples,
        channels,
        bits_per_sample,
        sample_rate,
        profile,
    )?;
    serialize_frame(&analysis, bits_per_sample, sample_rate, frame_index)
}

fn analyze_frame(
    interleaved_samples: &[i32],
    channels: u8,
    bits_per_sample: u8,
    sample_rate: u32,
    profile: LevelProfile,
) -> Result<FrameAnalysis> {
    let channel_count = usize::from(channels);
    if interleaved_samples.len() % channel_count != 0 {
        return Err(Error::Encode(
            "frame samples are not aligned to channel count".into(),
        ));
    }

    let mut candidates = Vec::new();
    match channels {
        1 => {
            let samples = extract_channel(interleaved_samples, channel_count, 0);
            let subframe = choose_subframe(&samples, bits_per_sample, sample_rate, profile)?;
            candidates.push(Candidate {
                bits: subframe.bit_len(bits_per_sample),
                assignment: ChannelAssignment::IndependentMono,
                subframes: vec![subframe],
            });
        }
        2 => {
            let left = extract_channel(interleaved_samples, channel_count, 0);
            let right = extract_channel(interleaved_samples, channel_count, 1);
            candidates.push(analyze_stereo_candidate(
                ChannelAssignment::IndependentStereo,
                [left.clone(), right.clone()],
                [bits_per_sample, bits_per_sample],
                sample_rate,
                profile,
            )?);

            if profile.use_mid_side_stereo {
                let side: Vec<i32> = left.iter().zip(&right).map(|(&l, &r)| l - r).collect();
                let mid: Vec<i32> = left
                    .iter()
                    .zip(&right)
                    .map(|(&l, &r)| ((i64::from(l) + i64::from(r)) >> 1) as i32)
                    .collect();
                candidates.push(analyze_stereo_candidate(
                    ChannelAssignment::LeftSide,
                    [left.clone(), side.clone()],
                    [bits_per_sample, bits_per_sample + 1],
                    sample_rate,
                    profile,
                )?);
                candidates.push(analyze_stereo_candidate(
                    ChannelAssignment::SideRight,
                    [side.clone(), right.clone()],
                    [bits_per_sample + 1, bits_per_sample],
                    sample_rate,
                    profile,
                )?);
                candidates.push(analyze_stereo_candidate(
                    ChannelAssignment::MidSide,
                    [mid, side],
                    [bits_per_sample, bits_per_sample + 1],
                    sample_rate,
                    profile,
                )?);
            }
        }
        _ => {
            return Err(Error::UnsupportedFlac(format!(
                "only mono/stereo frame assignment is supported, found {channels} channels"
            )));
        }
    }

    let best = candidates
        .into_iter()
        .min_by_key(|candidate| candidate.bits)
        .ok_or_else(|| Error::Encode("no frame candidate available".into()))?;
    let block_size = u16::try_from(interleaved_samples.len() / channel_count)
        .map_err(|_| Error::Encode("frame block size exceeds u16".into()))?;
    Ok(FrameAnalysis {
        block_size,
        channel_assignment: best.assignment,
        subframes: best.subframes,
    })
}

fn analyze_stereo_candidate(
    assignment: ChannelAssignment,
    transformed_channels: [Vec<i32>; 2],
    bits_per_subframe: [u8; 2],
    sample_rate: u32,
    profile: LevelProfile,
) -> Result<Candidate> {
    let mut bits = 0usize;
    let mut subframes = Vec::with_capacity(2);
    for (samples, bits_per_sample) in transformed_channels.into_iter().zip(bits_per_subframe) {
        let subframe = choose_subframe(&samples, bits_per_sample, sample_rate, profile)?;
        bits += subframe.bit_len(bits_per_sample);
        subframes.push(subframe);
    }
    Ok(Candidate {
        bits,
        assignment,
        subframes,
    })
}

fn serialize_frame(
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
    Ok(EncodedFrame { bytes: frame })
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

fn extract_channel(interleaved: &[i32], channels: usize, channel: usize) -> Vec<i32> {
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame[channel])
        .collect()
}

impl AnalyzedSubframe {
    fn bit_len(&self, bits_per_sample: u8) -> usize {
        match self {
            Self::Constant(_) => 1 + 6 + 1 + usize::from(bits_per_sample),
            Self::Verbatim(samples) => 1 + 6 + 1 + samples.len() * usize::from(bits_per_sample),
            Self::Fixed {
                warmup, residual, ..
            } => 1 + 6 + 1 + warmup.len() * usize::from(bits_per_sample) + residual.bit_len(),
            Self::Lpc {
                order,
                warmup,
                precision,
                residual,
                ..
            } => {
                1 + 6
                    + 1
                    + warmup.len() * usize::from(bits_per_sample)
                    + 4
                    + 5
                    + usize::from(*precision) * usize::from(*order)
                    + residual.bit_len()
            }
        }
    }
}

impl ResidualEncoding {
    fn bit_len(&self) -> usize {
        2 + 4
            + self
                .partitions
                .iter()
                .map(|partition| partition.bit_len(self.method))
                .sum::<usize>()
    }
}

impl ResidualPartition {
    fn bit_len(&self, method: RiceMethod) -> usize {
        let parameter_bits = method.parameter_bits();
        match self {
            Self::Rice {
                parameter,
                residuals,
            } => {
                parameter_bits
                    + residuals
                        .iter()
                        .map(|&residual| {
                            let folded = fold_residual(residual);
                            (folded >> usize::from(*parameter)) + 1 + usize::from(*parameter)
                        })
                        .sum::<usize>()
            }
            Self::Escape { bits, residuals } => {
                parameter_bits + 5 + residuals.len() * usize::from(*bits)
            }
        }
    }
}

impl RiceMethod {
    const fn parameter_bits(self) -> usize {
        match self {
            Self::FourBit => 4,
            Self::FiveBit => 5,
        }
    }

    const fn code(self) -> u8 {
        match self {
            Self::FourBit => 0b00,
            Self::FiveBit => 0b01,
        }
    }

    const fn escape_code(self) -> u8 {
        match self {
            Self::FourBit => 0b1111,
            Self::FiveBit => 0b1_1111,
        }
    }

    const fn max_parameter(self) -> u8 {
        match self {
            Self::FourBit => 14,
            Self::FiveBit => 30,
        }
    }
}

fn choose_subframe(
    samples: &[i32],
    bits_per_sample: u8,
    sample_rate: u32,
    profile: LevelProfile,
) -> Result<AnalyzedSubframe> {
    let mut best = None;
    if samples.iter().all(|&sample| sample == samples[0]) {
        let constant = AnalyzedSubframe::Constant(samples[0]);
        best = pick_better(best, constant.bit_len(bits_per_sample), constant);
    }

    let verbatim = AnalyzedSubframe::Verbatim(samples.to_vec());
    best = pick_better(best, verbatim.bit_len(bits_per_sample), verbatim);

    let max_partition_order = profile
        .max_residual_partition_order
        .min(MAX_RICE_PARTITION_ORDER);
    for order in 0..=profile.max_fixed_order.min(4) {
        if samples.len() < usize::from(order) {
            continue;
        }
        if let Some(residuals) = fixed_residuals(samples, order)
            && let Some(residual) = choose_residual_encoding(&residuals, order, max_partition_order)
        {
            let candidate = AnalyzedSubframe::Fixed {
                order,
                warmup: samples[..usize::from(order)].to_vec(),
                residual,
            };
            best = pick_better(best, candidate.bit_len(bits_per_sample), candidate);
        }
    }

    let max_lpc_order =
        max_lpc_order_for_stream(profile, sample_rate).min((samples.len().saturating_sub(1)) as u8);
    if max_lpc_order > 0
        && let Some(candidate) =
            choose_lpc_subframe(samples, bits_per_sample, max_lpc_order, max_partition_order)
    {
        best = pick_better(best, candidate.bit_len(bits_per_sample), candidate);
    }

    best.map(|(_, subframe)| subframe)
        .ok_or_else(|| Error::Encode("unable to select a subframe".into()))
}

fn max_lpc_order_for_stream(profile: LevelProfile, sample_rate: u32) -> u8 {
    let mut max_order = profile.max_lpc_order.min(MAX_FLAC_LPC_ORDER);
    if sample_rate <= 48_000 {
        max_order = max_order.min(MAX_STREAMABLE_LPC_ORDER_AT_48KHZ);
    }
    max_order
}

fn choose_lpc_subframe(
    samples: &[i32],
    bits_per_sample: u8,
    max_lpc_order: u8,
    max_partition_order: u8,
) -> Option<AnalyzedSubframe> {
    let lpc_coefficients = estimate_lpc_coefficients(samples, max_lpc_order)?;
    let mut best = None;
    for (order_index, coefficients) in lpc_coefficients.iter().enumerate() {
        let order = (order_index + 1) as u8;
        for precision in 7..=15u8 {
            let quantized = quantize_lpc_coefficients(coefficients, precision)?;
            let residuals = lpc_residuals(samples, &quantized.coefficients, quantized.shift)?;
            let residual = choose_residual_encoding(&residuals, order, max_partition_order)?;
            let candidate = AnalyzedSubframe::Lpc {
                order,
                warmup: samples[..usize::from(order)].to_vec(),
                precision: quantized.precision,
                shift: quantized.shift,
                coefficients: quantized.coefficients.clone(),
                residual,
            };
            best = pick_better(best, candidate.bit_len(bits_per_sample), candidate);
        }
    }
    best.map(|(_, subframe)| subframe)
}

fn pick_better(
    current: Option<(usize, AnalyzedSubframe)>,
    bits: usize,
    candidate: AnalyzedSubframe,
) -> Option<(usize, AnalyzedSubframe)> {
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
                3 * i64::from(samples[index - 1]) - 3 * i64::from(samples[index - 2])
                    + i64::from(samples[index - 3])
            }
            4 => {
                4 * i64::from(samples[index - 1]) - 6 * i64::from(samples[index - 2])
                    + 4 * i64::from(samples[index - 3])
                    - i64::from(samples[index - 4])
            }
            _ => return None,
        };
        let residual = i64::from(samples[index]) - prediction;
        if !residual_is_encodable(residual) {
            return None;
        }
        residuals.push(residual as i32);
    }
    Some(residuals)
}

fn choose_residual_encoding(
    residuals: &[i32],
    predictor_order: u8,
    max_partition_order: u8,
) -> Option<ResidualEncoding> {
    let block_size = residuals.len() + usize::from(predictor_order);
    let mut best: Option<(usize, ResidualEncoding)> = None;
    let capped_partition_order = max_partition_order.min(MAX_RICE_PARTITION_ORDER);

    for partition_order in 0..=capped_partition_order {
        let partition_count = 1usize << partition_order;
        if block_size % partition_count != 0 {
            continue;
        }
        let partition_len = block_size >> partition_order;
        if partition_len <= usize::from(predictor_order) {
            continue;
        }
        let first_partition_len = partition_len - usize::from(predictor_order);
        if first_partition_len == 0 {
            continue;
        }

        let mut counts = Vec::with_capacity(partition_count);
        counts.push(first_partition_len);
        counts.extend(std::iter::repeat_n(partition_len, partition_count - 1));
        if counts.iter().sum::<usize>() != residuals.len() {
            continue;
        }

        for method in [RiceMethod::FourBit, RiceMethod::FiveBit] {
            let mut offset = 0usize;
            let mut partitions = Vec::with_capacity(partition_count);
            let mut valid = true;
            for count in &counts {
                let end = offset + count;
                if let Some(partition) = choose_partition_encoding(&residuals[offset..end], method)
                {
                    partitions.push(partition);
                } else {
                    valid = false;
                    break;
                }
                offset = end;
            }
            if valid {
                let encoding = ResidualEncoding {
                    method,
                    partition_order,
                    partitions,
                };
                let bit_len = encoding.bit_len();
                best = match best {
                    Some((best_bits, _)) if best_bits <= bit_len => best,
                    _ => Some((bit_len, encoding)),
                };
            }
        }
    }

    best.map(|(_, encoding)| encoding)
}

fn choose_partition_encoding(residuals: &[i32], method: RiceMethod) -> Option<ResidualPartition> {
    let mut best: Option<(usize, ResidualPartition)> = None;
    if let Some(bits) = residual_escape_width(residuals) {
        let partition = ResidualPartition::Escape {
            bits,
            residuals: residuals.to_vec(),
        };
        best = Some((partition.bit_len(method), partition));
    }

    for parameter in 0..=method.max_parameter() {
        let mut bit_len = method.parameter_bits();
        let parameter_shift = usize::from(parameter);
        let mut valid = true;
        for &residual in residuals {
            let folded = fold_residual(residual);
            let quotient = folded >> parameter_shift;
            if quotient > u32::MAX as usize {
                valid = false;
                break;
            }
            bit_len += quotient + 1 + parameter_shift;
        }
        if valid {
            let partition = ResidualPartition::Rice {
                parameter,
                residuals: residuals.to_vec(),
            };
            best = match best {
                Some((best_bits, _)) if best_bits <= bit_len => best,
                _ => Some((bit_len, partition)),
            };
        }
    }

    best.map(|(_, partition)| partition)
}

fn residual_escape_width(residuals: &[i32]) -> Option<u8> {
    if residuals.iter().all(|&value| value == 0) {
        return Some(0);
    }
    (1..=MAX_ESCAPE_BITS).find(|&bits| {
        let minimum = -(1i64 << (bits - 1));
        let maximum = (1i64 << (bits - 1)) - 1;
        residuals.iter().all(|&value| {
            let value = i64::from(value);
            value >= minimum && value <= maximum
        })
    })
}

fn residual_is_encodable(residual: i64) -> bool {
    residual >= i64::from(i32::MIN)
        && residual <= i64::from(i32::MAX)
        && residual_escape_width(&[residual as i32]).is_some()
}

fn fold_residual(residual: i32) -> usize {
    if residual >= 0 {
        (residual as usize) << 1
    } else {
        (((-i64::from(residual)) as usize) << 1) - 1
    }
}

fn estimate_lpc_coefficients(samples: &[i32], max_order: u8) -> Option<Vec<Vec<f64>>> {
    let max_order = usize::from(max_order);
    if samples.len() <= max_order {
        return None;
    }
    let mut autocorrelation = vec![0.0; max_order + 1];
    for lag in 0..=max_order {
        autocorrelation[lag] = samples[lag..]
            .iter()
            .zip(&samples[..samples.len() - lag])
            .map(|(&a, &b)| f64::from(a) * f64::from(b))
            .sum();
    }
    if autocorrelation[0] == 0.0 {
        return None;
    }

    let mut prediction_error = autocorrelation[0];
    let mut coefficients = vec![0.0f64; max_order + 1];
    let mut orders = Vec::with_capacity(max_order);
    for order in 1..=max_order {
        let mut reflection = autocorrelation[order];
        for index in 1..order {
            reflection -= coefficients[index] * autocorrelation[order - index];
        }
        if prediction_error.abs() < f64::EPSILON {
            break;
        }
        reflection /= prediction_error;

        let mut next_coefficients = coefficients.clone();
        next_coefficients[order] = reflection;
        for index in 1..order {
            next_coefficients[index] =
                coefficients[index] - reflection * coefficients[order - index];
        }
        coefficients = next_coefficients;
        prediction_error *= 1.0 - reflection * reflection;
        if !prediction_error.is_finite() || prediction_error <= f64::EPSILON {
            break;
        }
        orders.push(coefficients[1..=order].to_vec());
    }

    if orders.is_empty() {
        None
    } else {
        Some(orders)
    }
}

fn quantize_lpc_coefficients(coefficients: &[f64], precision: u8) -> Option<QuantizedLpc> {
    let max_value = (1i32 << (precision - 1)) - 1;
    let mut best: Option<(f64, QuantizedLpc)> = None;
    for shift in (0..=15u8).rev() {
        let scale = f64::from(1u32 << shift);
        let mut quantized = Vec::with_capacity(coefficients.len());
        let mut error = 0.0;
        let mut valid = true;
        for &coefficient in coefficients {
            let rounded = (coefficient * scale).round() as i32;
            if rounded.abs() > max_value {
                valid = false;
                break;
            }
            quantized.push(rounded as i16);
            let restored = f64::from(rounded) / scale;
            let delta = coefficient - restored;
            error += delta * delta;
        }
        if valid {
            let candidate = QuantizedLpc {
                precision,
                shift,
                coefficients: quantized,
            };
            best = match best {
                Some((best_error, _)) if best_error <= error => best,
                _ => Some((error, candidate)),
            };
        }
    }
    best.map(|(_, candidate)| candidate)
}

fn lpc_residuals(samples: &[i32], coefficients: &[i16], shift: u8) -> Option<Vec<i32>> {
    let order = coefficients.len();
    let mut residuals = Vec::with_capacity(samples.len().saturating_sub(order));
    for index in order..samples.len() {
        let mut prediction = 0i64;
        for (coefficient_index, &coefficient) in coefficients.iter().enumerate() {
            prediction +=
                i64::from(coefficient) * i64::from(samples[index - coefficient_index - 1]);
        }
        let prediction = prediction >> shift;
        let residual = i64::from(samples[index]) - prediction;
        if !residual_is_encodable(residual) {
            return None;
        }
        residuals.push(residual as i32);
    }
    Some(residuals)
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

#[cfg(test)]
mod tests {
    use super::{
        ChannelAssignment, MAX_STREAMABLE_LPC_ORDER_AT_48KHZ, analyze_frame,
        channel_assignment_bits, encode_frame, encode_utf8_number, max_lpc_order_for_stream,
        sample_rate_is_representable,
    };
    use crate::level::LevelProfile;

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
    fn streamable_subset_clamps_lpc_order_at_48khz() {
        let profile = LevelProfile::new(4096, 4, 32, 6, true, true);
        assert_eq!(
            max_lpc_order_for_stream(profile, 44_100),
            MAX_STREAMABLE_LPC_ORDER_AT_48KHZ
        );
        assert_eq!(max_lpc_order_for_stream(profile, 96_000), 32);
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
    fn out_of_range_residuals_fall_back_to_verbatim() {
        let profile = LevelProfile::new(32, 4, 8, 4, false, false);
        let analysis = analyze_frame(&[0, i32::MAX], 1, 32, 96_000, profile).unwrap();
        let debug = format!("{analysis:?}");
        assert!(debug.contains("Verbatim"));
    }

    #[test]
    fn stereo_analysis_returns_legal_assignment() {
        let mut interleaved = Vec::new();
        for sample in 0..128i32 {
            interleaved.push(sample * 8);
            interleaved.push(sample * 8 + (sample & 1));
        }
        let profile = LevelProfile::new(256, 4, 12, 4, true, true);
        let analysis = analyze_frame(&interleaved, 2, 16, 44_100, profile).unwrap();
        assert!(matches!(
            analysis.channel_assignment,
            ChannelAssignment::IndependentStereo
                | ChannelAssignment::LeftSide
                | ChannelAssignment::SideRight
                | ChannelAssignment::MidSide
        ));
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
