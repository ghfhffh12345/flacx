use std::io::Write;

use crate::{
    error::{Error, Result},
    input::WavSpec,
};

pub(crate) fn write_wav<W: Write>(writer: &mut W, spec: WavSpec, samples: &[i32]) -> Result<()> {
    if !(1..=2).contains(&spec.channels) {
        return Err(Error::UnsupportedWav(format!(
            "only mono/stereo output is supported, found {} channels",
            spec.channels
        )));
    }
    if !matches!(spec.bits_per_sample, 16 | 24) {
        return Err(Error::UnsupportedWav(format!(
            "only 16-bit and 24-bit PCM are supported, found {} bits/sample",
            spec.bits_per_sample
        )));
    }
    if samples.len() % usize::from(spec.channels) != 0 {
        return Err(Error::Decode(
            "decoded samples are not aligned to the channel count".into(),
        ));
    }

    let bytes_per_sample = usize::from(spec.bits_per_sample / 8);
    let block_align = usize::from(spec.channels) * bytes_per_sample;
    let data_bytes = samples.len() * bytes_per_sample;
    let riff_size = 4 + (8 + 16) + (8 + data_bytes);

    writer.write_all(b"RIFF")?;
    writer.write_all(&(riff_size as u32).to_le_bytes())?;
    writer.write_all(b"WAVE")?;

    writer.write_all(b"fmt ")?;
    writer.write_all(&16u32.to_le_bytes())?;
    writer.write_all(&1u16.to_le_bytes())?;
    writer.write_all(&(u16::from(spec.channels)).to_le_bytes())?;
    writer.write_all(&spec.sample_rate.to_le_bytes())?;
    writer.write_all(&(spec.sample_rate * block_align as u32).to_le_bytes())?;
    writer.write_all(&(block_align as u16).to_le_bytes())?;
    writer.write_all(&(u16::from(spec.bits_per_sample)).to_le_bytes())?;

    writer.write_all(b"data")?;
    writer.write_all(&(data_bytes as u32).to_le_bytes())?;

    match spec.bits_per_sample {
        16 => {
            for &sample in samples {
                writer.write_all(&(sample as i16).to_le_bytes())?;
            }
        }
        24 => {
            for &sample in samples {
                let value = sample as u32;
                writer.write_all(&[
                    (value & 0xff) as u8,
                    ((value >> 8) & 0xff) as u8,
                    ((value >> 16) & 0xff) as u8,
                ])?;
            }
        }
        _ => unreachable!(),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::input::WavSpec;

    use super::write_wav;

    #[test]
    fn writes_canonical_16bit_wav() {
        let spec = WavSpec {
            sample_rate: 44_100,
            channels: 2,
            bits_per_sample: 16,
            total_samples: 2,
            bytes_per_sample: 2,
        };
        let samples = [1, -2, 3, -4];
        let mut wav = Vec::new();

        write_wav(&mut wav, spec, &samples).unwrap();

        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
    }
}
