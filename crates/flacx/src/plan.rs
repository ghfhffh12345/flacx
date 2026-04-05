use crate::{
    config::EncoderConfig,
    error::{Error, Result},
    input::WavSpec,
    level::LevelProfile,
    stream_info::StreamInfo,
    write::sample_rate_is_representable,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EncodePlan {
    pub(crate) spec: WavSpec,
    pub(crate) block_size: u16,
    pub(crate) profile: LevelProfile,
    pub(crate) total_frames: usize,
}

impl EncodePlan {
    pub(crate) fn new(spec: WavSpec, config: EncoderConfig) -> Result<Self> {
        validate_stream(&spec, config.block_size)?;
        let total_frames = if spec.total_samples == 0 {
            0
        } else {
            spec.total_samples.div_ceil(u64::from(config.block_size)) as usize
        };

        Ok(Self {
            spec,
            block_size: config.block_size,
            profile: config.level.profile(),
            total_frames,
        })
    }

    pub(crate) fn stream_info(self) -> StreamInfo {
        let mut stream_info = StreamInfo::new(
            self.spec.sample_rate,
            self.spec.channels,
            self.spec.bits_per_sample,
            self.spec.total_samples,
            [0; 16],
        );
        stream_info.update_block_size(self.block_size);
        stream_info
    }
}

pub(crate) fn summary_from_stream_info(
    stream_info: StreamInfo,
    frame_count: usize,
) -> crate::encoder::EncodeSummary {
    crate::encoder::EncodeSummary {
        frame_count,
        total_samples: stream_info.total_samples,
        block_size: stream_info.max_block_size,
        min_frame_size: stream_info.min_frame_size,
        max_frame_size: stream_info.max_frame_size,
        min_block_size: stream_info.min_block_size,
        max_block_size: stream_info.max_block_size,
        sample_rate: stream_info.sample_rate,
        channels: stream_info.channels,
        bits_per_sample: stream_info.bits_per_sample,
    }
}

fn validate_stream(spec: &WavSpec, block_size: u16) -> Result<()> {
    if spec.sample_rate == 0 {
        return Err(Error::UnsupportedFlac(
            "sample rate 0 is not allowed".into(),
        ));
    }
    if !(1..=8).contains(&spec.channels) {
        return Err(Error::UnsupportedFlac(format!(
            "only the ordinary 1..8 channel envelope is supported, found {} channels",
            spec.channels
        )));
    }
    if !(4..=32).contains(&spec.bits_per_sample) {
        return Err(Error::UnsupportedFlac(format!(
            "only FLAC-native 4..=32 valid bits/sample are supported, found {}",
            spec.bits_per_sample
        )));
    }

    if block_size < 16 {
        return Err(Error::UnsupportedFlac(
            "block size must be at least 16 to satisfy STREAMINFO bounds".into(),
        ));
    }

    if block_size > 16_384 {
        return Err(Error::UnsupportedFlac(
            "streamable subset requires block sizes <= 16384".into(),
        ));
    }

    if spec.sample_rate <= 48_000 && block_size > 4_608 {
        return Err(Error::UnsupportedFlac(
            "sample rates <= 48000 Hz require block sizes <= 4608 in the streamable subset".into(),
        ));
    }

    if !sample_rate_is_representable(spec.sample_rate) {
        return Err(Error::UnsupportedFlac(format!(
            "sample rate {} cannot be represented in a FLAC frame header without referring to STREAMINFO",
            spec.sample_rate
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::EncodePlan;
    use crate::{
        config::EncoderConfig,
        input::{WavSpec, ordinary_channel_mask},
        level::Level,
    };

    #[test]
    fn computes_total_frames_from_block_size() {
        let plan = EncodePlan::new(
            WavSpec {
                sample_rate: 44_100,
                channels: 2,
                bits_per_sample: 16,
                total_samples: 10_000,
                bytes_per_sample: 2,
                channel_mask: ordinary_channel_mask(2u16).unwrap(),
            },
            EncoderConfig::default()
                .with_level(Level::Level0)
                .with_block_size(576),
        )
        .unwrap();

        assert_eq!(plan.total_frames, 18);
        assert_eq!(plan.block_size, 576);
    }

    #[test]
    fn rejects_unrepresentable_sample_rates() {
        let error = EncodePlan::new(
            WavSpec {
                sample_rate: 700_001,
                channels: 2,
                bits_per_sample: 16,
                total_samples: 100,
                bytes_per_sample: 2,
                channel_mask: ordinary_channel_mask(2u16).unwrap(),
            },
            EncoderConfig::default(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("cannot be represented"));
    }
}
