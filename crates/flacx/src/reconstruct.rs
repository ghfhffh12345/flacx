use crate::{
    error::{Error, Result},
    model::ChannelAssignment,
};

pub(crate) fn unfold_residual(folded: u32) -> i32 {
    if folded & 1 == 0 {
        (folded >> 1) as i32
    } else {
        -(((folded >> 1) as i32) + 1)
    }
}

pub(crate) fn restore_fixed(order: u8, warmup: Vec<i32>, residuals: Vec<i32>) -> Result<Vec<i32>> {
    if warmup.len() != usize::from(order) {
        return Err(Error::Decode(
            "warmup length does not match predictor order".into(),
        ));
    }

    let mut samples = warmup;
    samples.reserve(residuals.len());
    for residual in residuals {
        let index = samples.len();
        let predicted = match order {
            0 => 0,
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
            _ => {
                return Err(Error::UnsupportedFlac(format!(
                    "fixed predictor order {order} is out of scope"
                )));
            }
        };
        let sample = i32::try_from(predicted + i64::from(residual))
            .map_err(|_| Error::Decode("fixed predictor overflowed".into()))?;
        samples.push(sample);
    }
    Ok(samples)
}

pub(crate) fn restore_lpc(
    order: u8,
    warmup: Vec<i32>,
    shift: i8,
    coefficients: &[i16],
    residuals: Vec<i32>,
) -> Result<Vec<i32>> {
    if warmup.len() != usize::from(order) || coefficients.len() != usize::from(order) {
        return Err(Error::Decode(
            "LPC coefficient/warmup length does not match predictor order".into(),
        ));
    }

    let mut samples = warmup;
    samples.reserve(residuals.len());
    for residual in residuals {
        let index = samples.len();
        let mut predicted = 0i64;
        for (offset, &coefficient) in coefficients.iter().enumerate() {
            predicted += i64::from(coefficient) * i64::from(samples[index - offset - 1]);
        }
        if shift >= 0 {
            predicted >>= i32::from(shift);
        } else {
            predicted <<= i32::from(-shift);
        }
        let sample = i32::try_from(predicted + i64::from(residual))
            .map_err(|_| Error::Decode("LPC predictor overflowed".into()))?;
        samples.push(sample);
    }
    Ok(samples)
}

#[cfg(test)]
pub(crate) fn interleave_channels(
    assignment: ChannelAssignment,
    channels: &[Vec<i32>],
) -> Result<Vec<i32>> {
    let mut interleaved = Vec::with_capacity(interleaved_sample_count(assignment, channels)?);
    interleave_channels_into(assignment, channels, &mut interleaved)?;
    Ok(interleaved)
}

pub(crate) fn interleave_channels_into(
    assignment: ChannelAssignment,
    channels: &[Vec<i32>],
    output: &mut Vec<i32>,
) -> Result<()> {
    output.reserve(interleaved_sample_count(assignment, channels)?);
    match assignment {
        ChannelAssignment::Independent(channels_count) => {
            interleave_independent(usize::from(channels_count), channels, output)
        }
        ChannelAssignment::LeftSide => {
            let left = channels
                .first()
                .ok_or_else(|| Error::Decode("left+side frame is missing left samples".into()))?;
            let side = channels
                .get(1)
                .ok_or_else(|| Error::Decode("left+side frame is missing side samples".into()))?;
            if left.len() != side.len() {
                return Err(Error::Decode(
                    "left and side channels differ in length".into(),
                ));
            }
            for (&left_sample, &side_sample) in left.iter().zip(side) {
                let right_sample =
                    i32::try_from(i64::from(left_sample) - i64::from(side_sample))
                        .map_err(|_| Error::Decode("left+side reconstruction overflowed".into()))?;
                output.push(left_sample);
                output.push(right_sample);
            }
            Ok(())
        }
        ChannelAssignment::SideRight => {
            let side = channels
                .first()
                .ok_or_else(|| Error::Decode("side+right frame is missing side samples".into()))?;
            let right = channels
                .get(1)
                .ok_or_else(|| Error::Decode("side+right frame is missing right samples".into()))?;
            if side.len() != right.len() {
                return Err(Error::Decode(
                    "side and right channels differ in length".into(),
                ));
            }
            for (&side_sample, &right_sample) in side.iter().zip(right) {
                let left_sample = i32::try_from(i64::from(side_sample) + i64::from(right_sample))
                    .map_err(|_| {
                    Error::Decode("side+right reconstruction overflowed".into())
                })?;
                output.push(left_sample);
                output.push(right_sample);
            }
            Ok(())
        }
        ChannelAssignment::MidSide => {
            let mid = channels
                .first()
                .ok_or_else(|| Error::Decode("mid+side frame is missing mid samples".into()))?;
            let side = channels
                .get(1)
                .ok_or_else(|| Error::Decode("mid+side frame is missing side samples".into()))?;
            if mid.len() != side.len() {
                return Err(Error::Decode(
                    "mid and side channels differ in length".into(),
                ));
            }
            for (&mid_sample, &side_sample) in mid.iter().zip(side) {
                let left_sample = i32::try_from(
                    i64::from(mid_sample)
                        + ((i64::from(side_sample) + i64::from(side_sample & 1)) >> 1),
                )
                .map_err(|_| Error::Decode("mid+side reconstruction overflowed".into()))?;
                let right_sample =
                    i32::try_from(i64::from(left_sample) - i64::from(side_sample))
                        .map_err(|_| Error::Decode("mid+side reconstruction overflowed".into()))?;
                output.push(left_sample);
                output.push(right_sample);
            }
            Ok(())
        }
    }
}

fn interleaved_sample_count(assignment: ChannelAssignment, channels: &[Vec<i32>]) -> Result<usize> {
    let channel_count = match assignment {
        ChannelAssignment::Independent(expected_channels) => usize::from(expected_channels),
        _ => assignment.channel_count(),
    };
    let Some(first) = channels.first() else {
        return Err(Error::Decode("independent frame is missing samples".into()));
    };
    Ok(first.len() * channel_count)
}

fn interleave_independent(
    expected_channels: usize,
    channels: &[Vec<i32>],
    output: &mut Vec<i32>,
) -> Result<()> {
    if channels.len() != expected_channels {
        return Err(Error::Decode(format!(
            "independent frame expected {expected_channels} channels, found {}",
            channels.len()
        )));
    }

    let Some(first) = channels.first() else {
        return Err(Error::Decode("independent frame is missing samples".into()));
    };
    if channels
        .iter()
        .skip(1)
        .any(|channel| channel.len() != first.len())
    {
        return Err(Error::Decode(
            "independent channels differ in length".into(),
        ));
    }

    if expected_channels == 2 {
        let left = &channels[0];
        let right = &channels[1];
        for (&left_sample, &right_sample) in left.iter().zip(right) {
            output.push(left_sample);
            output.push(right_sample);
        }
        return Ok(());
    }

    for frame_index in 0..first.len() {
        for channel in channels {
            output.push(channel[frame_index]);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::model::ChannelAssignment;

    use super::{interleave_channels, interleave_channels_into, unfold_residual};

    #[test]
    fn unfold_residual_matches_rice_mapping() {
        assert_eq!(unfold_residual(0), 0);
        assert_eq!(unfold_residual(1), -1);
        assert_eq!(unfold_residual(2), 1);
        assert_eq!(unfold_residual(3), -2);
    }

    #[test]
    fn reconstructs_mid_side_channels() {
        let interleaved =
            interleave_channels(ChannelAssignment::MidSide, &[vec![10, 11], vec![2, -1]]).unwrap();

        assert_eq!(interleaved, vec![11, 9, 11, 12]);
    }

    #[test]
    fn reconstructs_independent_multichannel_frames() {
        let interleaved = interleave_channels(
            ChannelAssignment::Independent(3),
            &[vec![1, 2], vec![3, 4], vec![5, 6]],
        )
        .unwrap();

        assert_eq!(interleaved, vec![1, 3, 5, 2, 4, 6]);
    }

    #[test]
    fn interleave_channels_into_appends_samples_to_existing_output() {
        let mut interleaved = vec![-1, -2];
        interleave_channels_into(
            ChannelAssignment::Independent(2),
            &[vec![1, 2], vec![3, 4]],
            &mut interleaved,
        )
        .unwrap();

        assert_eq!(interleaved, vec![-1, -2, 1, 3, 2, 4]);
    }
}
