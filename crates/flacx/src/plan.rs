use crate::{
    config::EncoderConfig,
    error::{Error, Result},
    input::PcmSpec,
    level::LevelProfile,
    stream_info::{MAX_STREAMINFO_SAMPLE_RATE, StreamInfo},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameCodedNumberKind {
    FrameNumber,
    SampleNumber,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlannedFrame {
    pub(crate) block_size: u16,
    pub(crate) sample_offset: u64,
    pub(crate) coded_number_kind: FrameCodedNumberKind,
    pub(crate) coded_number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FramingPlan {
    Fixed { block_size: u16 },
    Variable { frame_schedule: Vec<PlannedFrame> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EncodePlan {
    pub(crate) spec: PcmSpec,
    pub(crate) framing: FramingPlan,
    pub(crate) profile: LevelProfile,
    pub(crate) total_frames: usize,
}

impl EncodePlan {
    pub(crate) fn new(spec: PcmSpec, config: EncoderConfig) -> Result<Self> {
        let framing = framing_plan(&spec, &config)?;
        let total_frames = match &framing {
            FramingPlan::Fixed { block_size } => {
                if spec.total_samples == 0 {
                    0
                } else {
                    spec.total_samples.div_ceil(u64::from(*block_size)) as usize
                }
            }
            FramingPlan::Variable { frame_schedule } => frame_schedule.len(),
        };

        Ok(Self {
            spec,
            framing,
            profile: config.level.profile(),
            total_frames,
        })
    }

    pub(crate) fn stream_info(&self) -> StreamInfo {
        let mut stream_info = StreamInfo::new(
            self.spec.sample_rate,
            self.spec.channels,
            self.spec.bits_per_sample,
            self.spec.total_samples,
            [0; 16],
        );
        match &self.framing {
            FramingPlan::Fixed { block_size } => stream_info.update_block_size(*block_size),
            FramingPlan::Variable { frame_schedule } => {
                for frame in frame_schedule {
                    stream_info.update_block_size(frame.block_size);
                }
            }
        }
        stream_info
    }

    pub(crate) fn frame(&self, frame_index: usize) -> PlannedFrame {
        match &self.framing {
            FramingPlan::Fixed { block_size } => {
                let sample_offset = frame_index as u64 * u64::from(*block_size);
                let block_size =
                    (self.spec.total_samples - sample_offset).min(u64::from(*block_size)) as u16;
                PlannedFrame {
                    block_size,
                    sample_offset,
                    coded_number_kind: FrameCodedNumberKind::FrameNumber,
                    coded_number: frame_index as u64,
                }
            }
            FramingPlan::Variable { frame_schedule } => frame_schedule[frame_index],
        }
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

fn validate_stream(spec: &PcmSpec, block_size: u16) -> Result<()> {
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

    if spec.sample_rate > MAX_STREAMINFO_SAMPLE_RATE {
        return Err(Error::UnsupportedFlac(format!(
            "sample rate {} exceeds STREAMINFO's 20-bit limit of {}",
            spec.sample_rate, MAX_STREAMINFO_SAMPLE_RATE
        )));
    }

    Ok(())
}

fn framing_plan(spec: &PcmSpec, config: &EncoderConfig) -> Result<FramingPlan> {
    match &config.block_schedule {
        Some(block_schedule) => variable_framing_plan(spec, block_schedule),
        None => {
            validate_stream(spec, config.block_size)?;
            Ok(FramingPlan::Fixed {
                block_size: config.block_size,
            })
        }
    }
}

fn variable_framing_plan(spec: &PcmSpec, block_schedule: &[u16]) -> Result<FramingPlan> {
    if spec.total_samples == 0 {
        if block_schedule.is_empty() {
            return Ok(FramingPlan::Variable {
                frame_schedule: Vec::new(),
            });
        }
        return Err(Error::Encode(
            "variable block schedule must be empty when there are no samples".into(),
        ));
    }

    if block_schedule.is_empty() {
        return Err(Error::Encode(
            "variable block schedule must contain at least one frame".into(),
        ));
    }

    let mut sample_offset = 0u64;
    let mut frame_schedule = Vec::with_capacity(block_schedule.len());
    for &block_size in block_schedule {
        validate_stream(spec, block_size)?;
        frame_schedule.push(PlannedFrame {
            block_size,
            sample_offset,
            coded_number_kind: FrameCodedNumberKind::SampleNumber,
            coded_number: sample_offset,
        });
        sample_offset += u64::from(block_size);
    }

    if sample_offset != spec.total_samples {
        return Err(Error::Encode(format!(
            "variable block schedule must sum to total samples: expected {}, got {sample_offset}",
            spec.total_samples
        )));
    }

    Ok(FramingPlan::Variable { frame_schedule })
}

#[cfg(test)]
mod tests {
    use super::{EncodePlan, FrameCodedNumberKind, FramingPlan};
    use crate::{
        config::EncoderConfig,
        input::{PcmSpec, ordinary_channel_mask},
        level::Level,
        stream_info::MAX_STREAMINFO_SAMPLE_RATE,
    };

    #[test]
    fn computes_total_frames_from_block_size() {
        let plan = EncodePlan::new(
            PcmSpec {
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
        assert_eq!(plan.frame(0).block_size, 576);
        assert_eq!(
            plan.frame(0).coded_number_kind,
            FrameCodedNumberKind::FrameNumber
        );
    }

    #[test]
    fn accepts_legal_streaminfo_fallback_sample_rates() {
        let plan = EncodePlan::new(
            PcmSpec {
                sample_rate: 700_001,
                channels: 2,
                bits_per_sample: 16,
                total_samples: 100,
                bytes_per_sample: 2,
                channel_mask: ordinary_channel_mask(2u16).unwrap(),
            },
            EncoderConfig::default(),
        )
        .unwrap();

        assert_eq!(plan.total_frames, 1);
        assert_eq!(plan.frame(0).block_size, 100);
        assert_eq!(plan.stream_info().sample_rate, 700_001);
    }

    #[test]
    fn rejects_out_of_model_streaminfo_sample_rates() {
        let error = EncodePlan::new(
            PcmSpec {
                sample_rate: MAX_STREAMINFO_SAMPLE_RATE + 1,
                channels: 2,
                bits_per_sample: 16,
                total_samples: 100,
                bytes_per_sample: 2,
                channel_mask: ordinary_channel_mask(2u16).unwrap(),
            },
            EncoderConfig::default(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("exceeds STREAMINFO"));
    }

    #[test]
    fn accepts_large_legal_block_sizes_within_current_u16_model() {
        let plan = EncodePlan::new(
            PcmSpec {
                sample_rate: 48_000,
                channels: 1,
                bits_per_sample: 16,
                total_samples: 40_000,
                bytes_per_sample: 2,
                channel_mask: ordinary_channel_mask(1u16).unwrap(),
            },
            EncoderConfig::default().with_block_size(40_000),
        )
        .unwrap();

        assert_eq!(plan.total_frames, 1);
        assert_eq!(plan.frame(0).block_size, 40_000);
    }

    #[test]
    fn variable_schedule_uses_sample_numbers_and_exact_offsets() {
        let plan = EncodePlan::new(
            PcmSpec {
                sample_rate: 44_100,
                channels: 1,
                bits_per_sample: 16,
                total_samples: 4_352,
                bytes_per_sample: 2,
                channel_mask: ordinary_channel_mask(1u16).unwrap(),
            },
            EncoderConfig::default().with_block_schedule(vec![576, 1152, 576, 2048]),
        )
        .unwrap();

        assert_eq!(plan.total_frames, 4);
        assert!(matches!(plan.framing, FramingPlan::Variable { .. }));
        assert_eq!(
            plan.frame(0).coded_number_kind,
            FrameCodedNumberKind::SampleNumber
        );
        assert_eq!(plan.frame(0).coded_number, 0);
        assert_eq!(plan.frame(1).coded_number, 576);
        assert_eq!(plan.frame(2).coded_number, 1_728);
        assert_eq!(plan.frame(3).coded_number, 2_304);
        let stream_info = plan.stream_info();
        assert_eq!(stream_info.min_block_size, 576);
        assert_eq!(stream_info.max_block_size, 2_048);
    }

    #[test]
    fn rejects_variable_schedule_when_total_samples_do_not_match() {
        let error = EncodePlan::new(
            PcmSpec {
                sample_rate: 44_100,
                channels: 1,
                bits_per_sample: 16,
                total_samples: 4_352,
                bytes_per_sample: 2,
                channel_mask: ordinary_channel_mask(1u16).unwrap(),
            },
            EncoderConfig::default().with_block_schedule(vec![576, 1152, 576]),
        )
        .unwrap_err();

        assert!(error.to_string().contains("must sum to total samples"));
    }
}
