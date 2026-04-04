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

pub(crate) fn interleave_channels(
    assignment: ChannelAssignment,
    channels: &[Vec<i32>],
) -> Result<Vec<i32>> {
    match assignment {
        ChannelAssignment::IndependentMono => Ok(channels
            .first()
            .cloned()
            .ok_or_else(|| Error::Decode("mono frame is missing samples".into()))?),
        ChannelAssignment::IndependentStereo => {
            let left = channels
                .first()
                .ok_or_else(|| Error::Decode("stereo frame is missing left samples".into()))?;
            let right = channels
                .get(1)
                .ok_or_else(|| Error::Decode("stereo frame is missing right samples".into()))?;
            interleave_stereo(left, right)
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
            let mut interleaved = Vec::with_capacity(left.len() * 2);
            for (&left_sample, &side_sample) in left.iter().zip(side) {
                let right_sample =
                    i32::try_from(i64::from(left_sample) - i64::from(side_sample))
                        .map_err(|_| Error::Decode("left+side reconstruction overflowed".into()))?;
                interleaved.push(left_sample);
                interleaved.push(right_sample);
            }
            Ok(interleaved)
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
            let mut interleaved = Vec::with_capacity(side.len() * 2);
            for (&side_sample, &right_sample) in side.iter().zip(right) {
                let left_sample = i32::try_from(i64::from(side_sample) + i64::from(right_sample))
                    .map_err(|_| {
                    Error::Decode("side+right reconstruction overflowed".into())
                })?;
                interleaved.push(left_sample);
                interleaved.push(right_sample);
            }
            Ok(interleaved)
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
            let mut interleaved = Vec::with_capacity(mid.len() * 2);
            for (&mid_sample, &side_sample) in mid.iter().zip(side) {
                let left_sample = i32::try_from(
                    i64::from(mid_sample)
                        + ((i64::from(side_sample) + i64::from(side_sample & 1)) >> 1),
                )
                .map_err(|_| Error::Decode("mid+side reconstruction overflowed".into()))?;
                let right_sample =
                    i32::try_from(i64::from(left_sample) - i64::from(side_sample))
                        .map_err(|_| Error::Decode("mid+side reconstruction overflowed".into()))?;
                interleaved.push(left_sample);
                interleaved.push(right_sample);
            }
            Ok(interleaved)
        }
    }
}

fn interleave_stereo(left: &[i32], right: &[i32]) -> Result<Vec<i32>> {
    if left.len() != right.len() {
        return Err(Error::Decode(
            "left and right channels differ in length".into(),
        ));
    }

    let mut interleaved = Vec::with_capacity(left.len() * 2);
    for (&left_sample, &right_sample) in left.iter().zip(right) {
        interleaved.push(left_sample);
        interleaved.push(right_sample);
    }
    Ok(interleaved)
}

#[cfg(test)]
mod tests {
    use crate::model::ChannelAssignment;

    use super::{interleave_channels, unfold_residual};

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
}
