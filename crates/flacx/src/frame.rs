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
    pub(crate) sample_count: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
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
            let (left, right) = extract_stereo_channels(interleaved_samples);
            if profile.use_mid_side_stereo {
                let mut side = Vec::with_capacity(left.len());
                let mut mid = Vec::with_capacity(left.len());
                for (&left_sample, &right_sample) in left.iter().zip(&right) {
                    side.push(left_sample - right_sample);
                    mid.push(((i64::from(left_sample) + i64::from(right_sample)) >> 1) as i32);
                }
                let independent_cost = sample_magnitude_cost(&left) + sample_magnitude_cost(&right);
                let mid_side_cost = sample_magnitude_cost(&mid) + sample_magnitude_cost(&side);

                if mid_side_cost < independent_cost {
                    candidates.push(analyze_stereo_candidate(
                        ChannelAssignment::MidSide,
                        [&mid, &side],
                        [bits_per_sample, bits_per_sample + 1],
                        sample_rate,
                        profile,
                    )?);
                } else {
                    candidates.push(analyze_stereo_candidate(
                        ChannelAssignment::IndependentStereo,
                        [&left, &right],
                        [bits_per_sample, bits_per_sample],
                        sample_rate,
                        profile,
                    )?);
                }
            } else {
                candidates.push(analyze_stereo_candidate(
                    ChannelAssignment::IndependentStereo,
                    [&left, &right],
                    [bits_per_sample, bits_per_sample],
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
    transformed_channels: [&[i32]; 2],
    bits_per_subframe: [u8; 2],
    sample_rate: u32,
    profile: LevelProfile,
) -> Result<Candidate> {
    let mut bits = 0usize;
    let mut subframes = Vec::with_capacity(2);
    for (samples, bits_per_sample) in transformed_channels.into_iter().zip(bits_per_subframe) {
        let subframe = choose_subframe(samples, bits_per_sample, sample_rate, profile)?;
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

fn extract_channel(interleaved: &[i32], channels: usize, channel: usize) -> Vec<i32> {
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame[channel])
        .collect()
}

fn extract_stereo_channels(interleaved: &[i32]) -> (Vec<i32>, Vec<i32>) {
    let frame_count = interleaved.len() / 2;
    let mut left = Vec::with_capacity(frame_count);
    let mut right = Vec::with_capacity(frame_count);
    for frame in interleaved.chunks_exact(2) {
        left.push(frame[0]);
        right.push(frame[1]);
    }
    (left, right)
}

fn sample_magnitude_cost(samples: &[i32]) -> u64 {
    samples
        .iter()
        .map(|&sample| i64::from(sample).unsigned_abs())
        .sum()
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
    let mut best_bits = usize::MAX;
    let mut best_is_verbatim = false;
    if samples.iter().all(|&sample| sample == samples[0]) {
        let constant = AnalyzedSubframe::Constant(samples[0]);
        best_bits = constant.bit_len(bits_per_sample);
        best = Some(constant);
    }

    let verbatim_bits = SUBFRAME_HEADER_BITS + samples.len() * usize::from(bits_per_sample);
    if verbatim_bits < best_bits {
        best_bits = verbatim_bits;
        best = None;
        best_is_verbatim = true;
    }

    let max_partition_order = profile
        .max_residual_partition_order
        .min(MAX_RICE_PARTITION_ORDER);
    for order in 0..=profile.max_fixed_order.min(4) {
        if samples.len() < usize::from(order) {
            continue;
        }
        if fixed_subframe_lower_bound(order, bits_per_sample, samples.len()) >= best_bits {
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
            let candidate_bits = candidate.bit_len(bits_per_sample);
            if candidate_bits < best_bits {
                best_bits = candidate_bits;
                best = Some(candidate);
                best_is_verbatim = false;
            }
        }
    }

    let max_lpc_order =
        max_lpc_order_for_stream(profile, sample_rate).min((samples.len().saturating_sub(1)) as u8);
    if max_lpc_order > 0
        && let Some(candidate) = choose_lpc_subframe(
            samples,
            bits_per_sample,
            max_lpc_order,
            max_partition_order,
            best_bits,
        )
    {
        let candidate_bits = candidate.bit_len(bits_per_sample);
        if candidate_bits < best_bits {
            best = Some(candidate);
            best_is_verbatim = false;
        }
    }

    if best_is_verbatim {
        Ok(AnalyzedSubframe::Verbatim(samples.to_vec()))
    } else {
        best.ok_or_else(|| Error::Encode("unable to select a subframe".into()))
    }
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
    current_best_bits: usize,
) -> Option<AnalyzedSubframe> {
    let lpc_coefficients = estimate_lpc_coefficients(samples, max_lpc_order)?;
    let mut best = None;
    let mut best_bits = current_best_bits;
    for order in lpc_order_candidates(max_lpc_order) {
        let coefficients = &lpc_coefficients[usize::from(order - 1)];
        if lpc_subframe_lower_bound(order, 7, bits_per_sample, samples.len()) >= best_bits {
            continue;
        }
        for precision in lpc_precision_candidates().iter().copied() {
            if lpc_subframe_lower_bound(order, precision, bits_per_sample, samples.len())
                >= best_bits
            {
                continue;
            }
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
            let candidate_bits = candidate.bit_len(bits_per_sample);
            if candidate_bits < best_bits {
                best_bits = candidate_bits;
                best = Some((candidate_bits, candidate));
            }
        }
    }
    best.map(|(_, subframe)| subframe)
}

fn lpc_order_candidates(max_lpc_order: u8) -> Vec<u8> {
    if max_lpc_order <= 4 {
        return (1..=max_lpc_order).collect();
    }

    let mut candidates = Vec::with_capacity(6);
    for order in [8, 12, max_lpc_order] {
        if order <= max_lpc_order && !candidates.contains(&order) {
            candidates.push(order);
        }
    }
    candidates
}

fn lpc_precision_candidates() -> &'static [u8] {
    &[15]
}

const SUBFRAME_HEADER_BITS: usize = 1 + 6 + 1;
const MIN_RESIDUAL_ENCODING_BITS: usize = 2 + 4 + 4;

fn fixed_subframe_lower_bound(order: u8, bits_per_sample: u8, sample_count: usize) -> usize {
    let residual_count = sample_count.saturating_sub(usize::from(order));
    SUBFRAME_HEADER_BITS
        + usize::from(order) * usize::from(bits_per_sample)
        + MIN_RESIDUAL_ENCODING_BITS
        + residual_count
}

fn lpc_subframe_lower_bound(
    order: u8,
    precision: u8,
    bits_per_sample: u8,
    sample_count: usize,
) -> usize {
    let residual_count = sample_count.saturating_sub(usize::from(order));
    SUBFRAME_HEADER_BITS
        + usize::from(order) * usize::from(bits_per_sample)
        + 4
        + 5
        + usize::from(order) * usize::from(precision)
        + MIN_RESIDUAL_ENCODING_BITS
        + residual_count
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
    for partition_order in
        partition_order_candidates(block_size, predictor_order, max_partition_order)
    {
        let partition_count = 1usize << partition_order;
        let partition_len = block_size >> partition_order;
        let first_partition_len = partition_len - usize::from(predictor_order);

        for method in candidate_rice_methods(residuals) {
            let mut offset = 0usize;
            let mut partitions = Vec::with_capacity(partition_count);
            let mut valid = true;
            for partition_index in 0..partition_count {
                let count = if partition_index == 0 {
                    first_partition_len
                } else {
                    partition_len
                };
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

fn partition_order_candidates(
    block_size: usize,
    predictor_order: u8,
    max_partition_order: u8,
) -> Vec<u8> {
    let predictor_order = usize::from(predictor_order);
    let capped_partition_order = max_partition_order.min(MAX_RICE_PARTITION_ORDER);
    let highest_valid = (0..=capped_partition_order).rev().find(|&partition_order| {
        let partition_count = 1usize << partition_order;
        if block_size % partition_count != 0 {
            return false;
        }
        let partition_len = block_size >> partition_order;
        partition_len > predictor_order
    });

    let mut candidates = Vec::with_capacity(3);
    if let Some(highest_valid) = highest_valid {
        for partition_order in [highest_valid, highest_valid.saturating_sub(1), 0] {
            if !candidates.contains(&partition_order) {
                candidates.push(partition_order);
            }
        }
    }
    candidates
}

fn candidate_rice_methods(residuals: &[i32]) -> Vec<RiceMethod> {
    let estimated = estimate_rice_parameter_from_residuals(residuals);
    if estimated <= RiceMethod::FourBit.max_parameter() {
        vec![RiceMethod::FourBit]
    } else {
        vec![RiceMethod::FourBit, RiceMethod::FiveBit]
    }
}

fn choose_partition_encoding(residuals: &[i32], method: RiceMethod) -> Option<ResidualPartition> {
    enum BestPartition {
        Rice(u8),
        Escape(u8),
    }

    let folded_residuals: Vec<usize> = residuals
        .iter()
        .map(|&residual| fold_residual(residual))
        .collect();
    let mut best: Option<(usize, BestPartition)> = None;
    if let Some(bits) = residual_escape_width(residuals) {
        let bit_len = method.parameter_bits() + 5 + residuals.len() * usize::from(bits);
        best = Some((bit_len, BestPartition::Escape(bits)));
    }

    for parameter in candidate_rice_parameters(&folded_residuals, method) {
        let mut bit_len = method.parameter_bits();
        let parameter_shift = usize::from(parameter);
        let mut valid = true;
        for &folded in &folded_residuals {
            let quotient = folded >> parameter_shift;
            if quotient > u32::MAX as usize {
                valid = false;
                break;
            }
            bit_len += quotient + 1 + parameter_shift;
        }
        if valid {
            best = match best {
                Some((best_bits, _)) if best_bits <= bit_len => best,
                _ => Some((bit_len, BestPartition::Rice(parameter))),
            };
        }
    }

    best.map(|(_, partition)| match partition {
        BestPartition::Rice(parameter) => ResidualPartition::Rice {
            parameter,
            residuals: residuals.to_vec(),
        },
        BestPartition::Escape(bits) => ResidualPartition::Escape {
            bits,
            residuals: residuals.to_vec(),
        },
    })
}

fn candidate_rice_parameters(folded_residuals: &[usize], method: RiceMethod) -> Vec<u8> {
    let max_parameter = method.max_parameter();
    if folded_residuals.is_empty() {
        return vec![0];
    }

    let estimated = estimate_rice_parameter(folded_residuals).min(max_parameter);

    let mut parameters = Vec::with_capacity(2);
    let mut candidates = Vec::with_capacity(4);
    if estimated <= 2 {
        candidates.push(0);
    }
    candidates.push(estimated);
    for candidate in candidates {
        if !parameters.contains(&candidate) {
            parameters.push(candidate);
        }
    }
    parameters
}

fn estimate_rice_parameter(folded_residuals: &[usize]) -> u8 {
    let mean_folded =
        folded_residuals.iter().copied().sum::<usize>() / folded_residuals.len().max(1);
    if mean_folded == 0 {
        0
    } else {
        (usize::BITS - mean_folded.leading_zeros() - 1) as u8
    }
}

fn estimate_rice_parameter_from_residuals(residuals: &[i32]) -> u8 {
    let mean_folded =
        residuals.iter().copied().map(fold_residual).sum::<usize>() / residuals.len().max(1);
    if mean_folded == 0 {
        0
    } else {
        (usize::BITS - mean_folded.leading_zeros() - 1) as u8
    }
}

fn residual_escape_width(residuals: &[i32]) -> Option<u8> {
    if residuals.iter().all(|&value| value == 0) {
        return Some(0);
    }
    let (min_value, max_value) =
        residuals
            .iter()
            .fold((i64::MAX, i64::MIN), |(min_value, max_value), &value| {
                let value = i64::from(value);
                (min_value.min(value), max_value.max(value))
            });
    (1..=MAX_ESCAPE_BITS).find(|&bits| {
        let minimum = -(1i64 << (bits - 1));
        let maximum = (1i64 << (bits - 1)) - 1;
        min_value >= minimum && max_value <= maximum
    })
}

fn residual_is_encodable(residual: i64) -> bool {
    let minimum = -(1i64 << (MAX_ESCAPE_BITS - 1));
    let maximum = (1i64 << (MAX_ESCAPE_BITS - 1)) - 1;
    residual >= minimum && residual <= maximum
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
    let mut next_coefficients = vec![0.0f64; max_order + 1];
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

        next_coefficients[order] = reflection;
        for index in 1..order {
            next_coefficients[index] =
                coefficients[index] - reflection * coefficients[order - index];
        }
        coefficients[1..=order].copy_from_slice(&next_coefficients[1..=order]);
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
