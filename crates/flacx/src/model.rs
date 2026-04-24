use crate::{
    error::{Error, Result},
    level::LevelProfile,
    write::{EncodedFrame, FrameHeaderNumber, serialize_frame},
};

const MAX_STREAMABLE_LPC_ORDER_AT_48KHZ: u8 = 12;
const MAX_FLAC_LPC_ORDER: u8 = 32;
const MAX_RICE_PARTITION_ORDER: u8 = 8;
const MAX_ESCAPE_BITS: u8 = 31;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum ChannelAssignment {
    Independent(u8),
    LeftSide,
    SideRight,
    MidSide,
}

impl ChannelAssignment {
    pub(crate) fn channel_count(self) -> usize {
        match self {
            Self::Independent(channels) => usize::from(channels),
            Self::LeftSide | Self::SideRight | Self::MidSide => 2,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FrameAnalysis {
    pub(crate) block_size: u16,
    pub(crate) channel_assignment: ChannelAssignment,
    pub(crate) subframes: Vec<AnalyzedSubframe>,
}

#[derive(Debug, Clone)]
pub(crate) enum AnalyzedSubframe {
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
pub(crate) struct ResidualEncoding {
    pub(crate) method: RiceMethod,
    pub(crate) partition_order: u8,
    pub(crate) bit_len: usize,
    pub(crate) residuals: Box<[i32]>,
    pub(crate) partitions: Box<[ResidualPartition]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RiceMethod {
    FourBit,
    FiveBit,
}

#[derive(Debug, Clone)]
pub(crate) enum ResidualPartition {
    Rice {
        parameter: u8,
        start: usize,
        end: usize,
    },
    Escape {
        bits: u8,
        start: usize,
        end: usize,
    },
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

#[derive(Debug, Clone, Copy)]
enum PartitionEncodingKind {
    Rice { parameter: u8 },
    Escape { bits: u8 },
}

#[derive(Debug, Clone, Copy)]
struct PartitionEncodingSpec {
    kind: PartitionEncodingKind,
    start: usize,
    end: usize,
}

pub(crate) fn encode_frame(
    interleaved_samples: &[i32],
    channels: u8,
    bits_per_sample: u8,
    sample_rate: u32,
    header_number: FrameHeaderNumber,
    profile: LevelProfile,
) -> Result<EncodedFrame> {
    let analysis = analyze_frame(
        interleaved_samples,
        channels,
        bits_per_sample,
        sample_rate,
        profile,
    )?;
    serialize_frame(&analysis, bits_per_sample, sample_rate, header_number)
}

fn analyze_frame(
    interleaved_samples: &[i32],
    channels: u8,
    bits_per_sample: u8,
    sample_rate: u32,
    profile: LevelProfile,
) -> Result<FrameAnalysis> {
    if !(1..=8).contains(&channels) {
        return Err(Error::UnsupportedFlac(format!(
            "only independent 1..8 channel frame assignments are supported, found {channels} channels"
        )));
    }

    let channel_count = usize::from(channels);
    if !interleaved_samples.len().is_multiple_of(channel_count) {
        return Err(Error::Encode(
            "frame samples are not aligned to channel count".into(),
        ));
    }

    let candidates = match channels {
        1 => {
            let subframe =
                choose_subframe(interleaved_samples, bits_per_sample, sample_rate, profile)?;
            vec![Candidate {
                bits: subframe.bit_len(bits_per_sample),
                assignment: ChannelAssignment::Independent(1),
                subframes: vec![subframe],
            }]
        }
        2 => {
            if profile.use_mid_side_stereo {
                let (independent_cost, mid_side_cost) =
                    stereo_assignment_costs(interleaved_samples);

                if mid_side_cost < independent_cost {
                    let split = split_mid_side_channels(interleaved_samples);
                    let frame_count = split.len() / 2;
                    let (mid, side) = split.split_at(frame_count);
                    vec![analyze_stereo_candidate(
                        ChannelAssignment::MidSide,
                        [mid, side],
                        [bits_per_sample, bits_per_sample + 1],
                        sample_rate,
                        profile,
                    )?]
                } else {
                    let split = split_stereo_channels(interleaved_samples);
                    let frame_count = split.len() / 2;
                    let (left, right) = split.split_at(frame_count);
                    vec![analyze_stereo_candidate(
                        ChannelAssignment::Independent(2),
                        [left, right],
                        [bits_per_sample, bits_per_sample],
                        sample_rate,
                        profile,
                    )?]
                }
            } else {
                let split = split_stereo_channels(interleaved_samples);
                let frame_count = split.len() / 2;
                let (left, right) = split.split_at(frame_count);
                vec![analyze_stereo_candidate(
                    ChannelAssignment::Independent(2),
                    [left, right],
                    [bits_per_sample, bits_per_sample],
                    sample_rate,
                    profile,
                )?]
            }
        }
        _ => {
            let split_channels = split_channels(interleaved_samples, channel_count);
            vec![analyze_independent_candidate(
                ChannelAssignment::Independent(channels),
                &split_channels,
                channel_count,
                bits_per_sample,
                sample_rate,
                profile,
            )?]
        }
    };

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

fn analyze_independent_candidate(
    assignment: ChannelAssignment,
    channels: &[i32],
    channel_count: usize,
    bits_per_sample: u8,
    sample_rate: u32,
    profile: LevelProfile,
) -> Result<Candidate> {
    let mut bits = 0usize;
    let frame_count = channels.len() / channel_count;
    let mut subframes = Vec::with_capacity(channel_count);
    for samples in channels.chunks_exact(frame_count) {
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

fn split_stereo_channels(interleaved: &[i32]) -> Vec<i32> {
    let frame_count = interleaved.len() / 2;
    let mut split = vec![0; interleaved.len()];
    for (frame_index, frame) in interleaved.chunks_exact(2).enumerate() {
        split[frame_index] = frame[0];
        split[frame_count + frame_index] = frame[1];
    }
    split
}

fn split_mid_side_channels(interleaved: &[i32]) -> Vec<i32> {
    let frame_count = interleaved.len() / 2;
    let mut split = vec![0; interleaved.len()];
    for (frame_index, frame) in interleaved.chunks_exact(2).enumerate() {
        let left_sample = frame[0];
        let right_sample = frame[1];
        split[frame_index] = ((i64::from(left_sample) + i64::from(right_sample)) >> 1) as i32;
        split[frame_count + frame_index] = left_sample - right_sample;
    }
    split
}

fn stereo_assignment_costs(interleaved: &[i32]) -> (u64, u64) {
    let mut independent_cost = 0u64;
    let mut mid_side_cost = 0u64;
    for frame in interleaved.chunks_exact(2) {
        let left_sample = frame[0];
        let right_sample = frame[1];
        independent_cost += i64::from(left_sample).unsigned_abs();
        independent_cost += i64::from(right_sample).unsigned_abs();
        mid_side_cost += ((i64::from(left_sample) + i64::from(right_sample)) >> 1).unsigned_abs();
        mid_side_cost += i64::from(left_sample - right_sample).unsigned_abs();
    }
    (independent_cost, mid_side_cost)
}

fn split_channels(interleaved: &[i32], channels: usize) -> Vec<i32> {
    let frame_count = interleaved.len() / channels;
    let mut split = vec![0; interleaved.len()];
    for (frame_index, frame) in interleaved.chunks_exact(channels).enumerate() {
        for (channel_index, &sample) in frame.iter().enumerate() {
            split[channel_index * frame_count + frame_index] = sample;
        }
    }
    split
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
        self.bit_len
    }
}

impl RiceMethod {
    pub(crate) const fn parameter_bits(self) -> usize {
        match self {
            Self::FourBit => 4,
            Self::FiveBit => 5,
        }
    }

    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::FourBit => 0b00,
            Self::FiveBit => 0b01,
        }
    }

    pub(crate) const fn escape_code(self) -> u8 {
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
        let fixed_overhead =
            SUBFRAME_HEADER_BITS + usize::from(order) * usize::from(bits_per_sample);
        let Some(residual_bit_limit) = best_bits.checked_sub(fixed_overhead) else {
            continue;
        };
        if let Some(residuals) = fixed_residuals(samples, order)
            && let Some(residual) =
                choose_residual_encoding(residuals, order, max_partition_order, residual_bit_limit)
        {
            let candidate_bits = fixed_overhead + residual.bit_len();
            let candidate = AnalyzedSubframe::Fixed {
                order,
                warmup: samples[..usize::from(order)].to_vec(),
                residual,
            };
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
    let (order_candidates, order_count) = lpc_order_candidates(max_lpc_order);
    for &order in order_candidates[..order_count].iter() {
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
            let lpc_overhead = SUBFRAME_HEADER_BITS
                + usize::from(order) * usize::from(bits_per_sample)
                + 4
                + 5
                + usize::from(order) * usize::from(precision);
            let Some(residual_bit_limit) = best_bits.checked_sub(lpc_overhead) else {
                continue;
            };
            let QuantizedLpc {
                precision,
                shift,
                coefficients,
            } = quantize_lpc_coefficients(coefficients, precision)?;
            let residuals = lpc_residuals(samples, &coefficients, shift)?;
            let residual = choose_residual_encoding(
                residuals,
                order,
                max_partition_order,
                residual_bit_limit,
            )?;
            let candidate_bits = lpc_overhead + residual.bit_len();
            let candidate = AnalyzedSubframe::Lpc {
                order,
                warmup: samples[..usize::from(order)].to_vec(),
                precision,
                shift,
                coefficients,
                residual,
            };
            if candidate_bits < best_bits {
                best_bits = candidate_bits;
                best = Some((candidate_bits, candidate));
            }
        }
    }
    best.map(|(_, subframe)| subframe)
}

fn lpc_order_candidates(max_lpc_order: u8) -> ([u8; 4], usize) {
    if max_lpc_order <= 4 {
        let mut candidates = [0; 4];
        for order in 1..=max_lpc_order {
            candidates[usize::from(order - 1)] = order;
        }
        return (candidates, usize::from(max_lpc_order));
    }

    let mut candidates = [0; 4];
    let mut count = 0usize;
    for order in [8, 12, max_lpc_order] {
        if order <= max_lpc_order && !candidates[..count].contains(&order) {
            candidates[count] = order;
            count += 1;
        }
    }
    (candidates, count)
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
    residuals: Vec<i32>,
    predictor_order: u8,
    max_partition_order: u8,
    residual_bit_limit: usize,
) -> Option<ResidualEncoding> {
    let block_size = residuals.len() + usize::from(predictor_order);
    let mut best: Option<(usize, RiceMethod, u8, Vec<PartitionEncodingSpec>)> = None;
    let (partition_orders, partition_order_count) =
        partition_order_candidates(block_size, predictor_order, max_partition_order);
    let (methods, method_count) = candidate_rice_methods(&residuals);
    for &partition_order in partition_orders[..partition_order_count].iter() {
        let partition_count = 1usize << partition_order;
        let partition_len = block_size >> partition_order;
        let first_partition_len = partition_len - usize::from(predictor_order);

        for &method in methods[..method_count].iter() {
            let mut offset = 0usize;
            let mut bit_len = 2 + 4;
            let current_limit = best
                .as_ref()
                .map(|(best_bits, ..)| *best_bits)
                .unwrap_or(residual_bit_limit)
                .min(residual_bit_limit);
            let mut partitions = Vec::with_capacity(partition_count);
            let mut valid = true;
            for partition_index in 0..partition_count {
                let count = if partition_index == 0 {
                    first_partition_len
                } else {
                    partition_len
                };
                let end = offset + count;
                if bit_len >= current_limit {
                    valid = false;
                    break;
                }
                let remaining_limit = current_limit - bit_len;
                if let Some((partition_bits, kind)) =
                    choose_partition_encoding(&residuals[offset..end], method, remaining_limit)
                {
                    bit_len += partition_bits;
                    partitions.push(PartitionEncodingSpec {
                        kind,
                        start: offset,
                        end,
                    });
                    if bit_len >= current_limit {
                        valid = false;
                        break;
                    }
                } else {
                    valid = false;
                    break;
                }
                offset = end;
            }
            if valid {
                best = match best {
                    Some((best_bits, ..)) if best_bits <= bit_len => best,
                    _ if bit_len < residual_bit_limit => {
                        Some((bit_len, method, partition_order, partitions))
                    }
                    _ => best,
                };
            }
        }
    }

    let residuals = residuals.into_boxed_slice();
    best.map(|(bit_len, method, partition_order, partitions)| ResidualEncoding {
        method,
        partition_order,
        bit_len,
        residuals,
        partitions: partitions
            .into_iter()
            .map(materialize_partition_encoding)
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    })
}

fn partition_order_candidates(
    block_size: usize,
    predictor_order: u8,
    max_partition_order: u8,
) -> ([u8; 3], usize) {
    let predictor_order = usize::from(predictor_order);
    let capped_partition_order = max_partition_order.min(MAX_RICE_PARTITION_ORDER);
    let highest_valid = (0..=capped_partition_order).rev().find(|&partition_order| {
        let partition_count = 1usize << partition_order;
        if !block_size.is_multiple_of(partition_count) {
            return false;
        }
        let partition_len = block_size >> partition_order;
        partition_len > predictor_order
    });

    let mut candidates = [0; 3];
    let mut count = 0usize;
    if let Some(highest_valid) = highest_valid {
        for partition_order in [highest_valid, highest_valid.saturating_sub(1), 0] {
            if !candidates[..count].contains(&partition_order) {
                candidates[count] = partition_order;
                count += 1;
            }
        }
    }
    (candidates, count)
}

fn candidate_rice_methods(residuals: &[i32]) -> ([RiceMethod; 2], usize) {
    let estimated = estimate_rice_parameter_from_residuals(residuals);
    if estimated <= RiceMethod::FourBit.max_parameter() {
        ([RiceMethod::FourBit, RiceMethod::FourBit], 1)
    } else {
        ([RiceMethod::FourBit, RiceMethod::FiveBit], 2)
    }
}

fn choose_partition_encoding(
    residuals: &[i32],
    method: RiceMethod,
    bit_limit: usize,
) -> Option<(usize, PartitionEncodingKind)> {
    let mut min_value = i64::MAX;
    let mut max_value = i64::MIN;
    let mut sum_folded = 0usize;
    let mut all_zero = true;
    for &residual in residuals {
        let value = i64::from(residual);
        min_value = min_value.min(value);
        max_value = max_value.max(value);
        all_zero &= residual == 0;
        sum_folded += fold_residual(residual);
    }

    let mut best: Option<(usize, PartitionEncodingKind)> = None;
    if let Some(bits) = residual_escape_width_from_range(min_value, max_value, all_zero) {
        let bit_len = method.parameter_bits() + 5 + residuals.len() * usize::from(bits);
        if bit_len < bit_limit {
            best = Some((bit_len, PartitionEncodingKind::Escape { bits }));
        }
    }

    let estimated = estimate_rice_parameter_from_mean_folded(sum_folded / residuals.len().max(1))
        .min(method.max_parameter());
    let (parameters, parameter_count) = candidate_rice_parameters_from_estimated(estimated);
    if parameter_count == 0 {
        return best;
    }

    let current_limit = best
        .as_ref()
        .map(|(best_bits, _)| *best_bits)
        .unwrap_or(bit_limit)
        .min(bit_limit);
    let mut rice_bit_lens = [method.parameter_bits(); 2];
    let mut rice_valid = [true; 2];
    for &residual in residuals {
        let folded = fold_residual(residual);
        for (index, &parameter) in parameters[..parameter_count].iter().enumerate() {
            if !rice_valid[index] {
                continue;
            }
            let parameter_shift = usize::from(parameter);
            let quotient = folded >> parameter_shift;
            if quotient > u32::MAX as usize {
                rice_valid[index] = false;
                continue;
            }
            rice_bit_lens[index] += quotient + 1 + parameter_shift;
            if rice_bit_lens[index] >= current_limit {
                rice_valid[index] = false;
            }
        }
        if rice_valid[..parameter_count].iter().all(|valid| !valid) {
            break;
        }
    }

    for (index, &parameter) in parameters[..parameter_count].iter().enumerate() {
        let bit_len = rice_bit_lens[index];
        if rice_valid[index] {
            best = match best {
                Some((best_bits, _)) if best_bits <= bit_len => best,
                _ if bit_len < bit_limit => {
                    Some((bit_len, PartitionEncodingKind::Rice { parameter }))
                }
                _ => best,
            };
        }
    }

    best
}

fn materialize_partition_encoding(partition: PartitionEncodingSpec) -> ResidualPartition {
    match partition.kind {
        PartitionEncodingKind::Rice { parameter } => ResidualPartition::Rice {
            parameter,
            start: partition.start,
            end: partition.end,
        },
        PartitionEncodingKind::Escape { bits } => ResidualPartition::Escape {
            bits,
            start: partition.start,
            end: partition.end,
        },
    }
}

fn candidate_rice_parameters_from_estimated(estimated: u8) -> ([u8; 2], usize) {
    let mut parameters = [0; 2];
    let mut count = 0usize;
    if estimated <= 2 {
        parameters[count] = 0;
        count += 1;
    }
    if !parameters[..count].contains(&estimated) {
        parameters[count] = estimated;
        count += 1;
    }
    (parameters, count)
}

fn estimate_rice_parameter_from_residuals(residuals: &[i32]) -> u8 {
    let mean_folded =
        residuals.iter().copied().map(fold_residual).sum::<usize>() / residuals.len().max(1);
    estimate_rice_parameter_from_mean_folded(mean_folded)
}

fn estimate_rice_parameter_from_mean_folded(mean_folded: usize) -> u8 {
    if mean_folded == 0 {
        0
    } else {
        (usize::BITS - mean_folded.leading_zeros() - 1) as u8
    }
}

fn residual_escape_width_from_range(min_value: i64, max_value: i64, all_zero: bool) -> Option<u8> {
    if all_zero {
        return Some(0);
    }
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
    let mut best: Option<(f64, u8)> = None;
    for shift in (0..=15u8).rev() {
        let scale = f64::from(1u32 << shift);
        let mut error = 0.0;
        let mut valid = true;
        for &coefficient in coefficients {
            let rounded = (coefficient * scale).round() as i32;
            if rounded.abs() > max_value {
                valid = false;
                break;
            }
            let restored = f64::from(rounded) / scale;
            let delta = coefficient - restored;
            error += delta * delta;
        }
        if valid {
            best = match best {
                Some((best_error, _)) if best_error <= error => best,
                _ => Some((error, shift)),
            };
        }
    }
    let (_, shift) = best?;
    let scale = f64::from(1u32 << shift);
    let mut quantized = Vec::with_capacity(coefficients.len());
    for &coefficient in coefficients {
        quantized.push((coefficient * scale).round() as i16);
    }
    Some(QuantizedLpc {
        precision,
        shift,
        coefficients: quantized,
    })
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
#[cfg(test)]
mod tests {
    use super::{
        AnalyzedSubframe, ChannelAssignment, MAX_STREAMABLE_LPC_ORDER_AT_48KHZ, analyze_frame,
        choose_residual_encoding, max_lpc_order_for_stream,
    };
    use crate::level::LevelProfile;

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
            ChannelAssignment::Independent(2) | ChannelAssignment::MidSide
        ));
    }

    #[test]
    fn multichannel_analysis_uses_independent_assignment() {
        let mut interleaved = Vec::new();
        for sample in 0..32i32 {
            interleaved.push(sample);
            interleaved.push(sample + 1);
            interleaved.push(sample + 2);
        }
        let profile = LevelProfile::new(64, 4, 12, 4, true, true);
        let analysis = analyze_frame(&interleaved, 3, 16, 44_100, profile).unwrap();
        assert!(matches!(
            analysis.channel_assignment,
            ChannelAssignment::Independent(3)
        ));
        assert_eq!(analysis.subframes.len(), 3);
    }

    #[test]
    fn chosen_residual_encoding_keeps_the_exact_selected_bit_len() {
        let residuals = vec![0, 1, -1, 4, -2, 0, 3, -3, 2, -1];

        let encoding = choose_residual_encoding(residuals.clone(), 0, 4, usize::MAX).unwrap();

        assert_eq!(encoding.bit_len, encoding.bit_len());
        assert_eq!(&*encoding.residuals, residuals.as_slice());
    }

    #[test]
    fn chosen_lpc_candidate_keeps_cached_residual_cost() {
        let residual = super::ResidualEncoding {
            method: super::RiceMethod::FourBit,
            partition_order: 0,
            bit_len: 17,
            residuals: Box::new([0, 1, -1]),
            partitions: Box::new([super::ResidualPartition::Rice {
                parameter: 0,
                start: 0,
                end: 3,
            }]),
        };
        let candidate = AnalyzedSubframe::Lpc {
            order: 2,
            warmup: vec![11, 12],
            precision: 7,
            shift: 0,
            coefficients: vec![2, -1],
            residual,
        };

        assert_eq!(candidate.bit_len(16), 1 + 6 + 1 + 2 * 16 + 4 + 5 + 2 * 7 + 17);
    }
}
