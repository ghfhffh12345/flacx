use crate::{
    error::Result,
    progress::{ProgressSink, ProgressSnapshot},
};

/// Phase marker for recompress progress reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecompressPhase {
    /// The source FLAC is being decoded and verified.
    Decode,
    /// The verified PCM is being encoded back to FLAC.
    Encode,
}

#[cfg_attr(not(feature = "progress"), allow(dead_code))]
impl RecompressPhase {
    /// Return the user-facing phase label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Decode => "Decode",
            Self::Encode => "Encode",
        }
    }
}

/// A phase-aware recompress progress snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecompressProgress {
    /// The active recompress phase.
    pub phase: RecompressPhase,
    /// Samples processed so far within the active phase.
    pub phase_processed_samples: u64,
    /// Total samples expected within the active phase.
    pub phase_total_samples: u64,
    /// Samples processed so far across the full decode+encode operation.
    pub overall_processed_samples: u64,
    /// Total samples expected across the full decode+encode operation.
    pub overall_total_samples: u64,
    /// Frames completed so far within the active phase.
    pub completed_frames: usize,
    /// Total frames expected within the active phase when known.
    pub total_frames: usize,
    /// Input bytes read so far within the active phase.
    pub phase_input_bytes_read: u64,
    /// Output bytes written so far within the active phase.
    pub phase_output_bytes_written: u64,
    /// Input bytes read so far across the full decode+encode operation.
    pub overall_input_bytes_read: u64,
    /// Output bytes written so far across the full decode+encode operation.
    pub overall_output_bytes_written: u64,
}

pub(crate) fn encode_phase_transition_progress(
    total_samples: u64,
    total_frames: usize,
    decode_input_bytes_read: u64,
) -> RecompressProgress {
    RecompressProgress {
        phase: RecompressPhase::Encode,
        phase_processed_samples: 0,
        phase_total_samples: total_samples,
        overall_processed_samples: total_samples,
        overall_total_samples: overall_total_samples(total_samples),
        completed_frames: 0,
        total_frames,
        phase_input_bytes_read: 0,
        phase_output_bytes_written: 0,
        overall_input_bytes_read: decode_input_bytes_read,
        overall_output_bytes_written: 0,
    }
}

pub(crate) trait RecompressProgressSink {
    fn on_progress(&mut self, progress: RecompressProgress) -> Result<()>;
}

impl RecompressProgressSink for crate::progress::NoProgress {
    fn on_progress(&mut self, _progress: RecompressProgress) -> Result<()> {
        Ok(())
    }
}

#[cfg(feature = "progress")]
impl<F> RecompressProgressSink for F
where
    F: FnMut(RecompressProgress) -> Result<()>,
{
    fn on_progress(&mut self, progress: RecompressProgress) -> Result<()> {
        self(progress)
    }
}

pub(crate) struct EncodePhaseProgress<'a, P> {
    pub(crate) sink: &'a mut P,
    pub(crate) total_samples: u64,
    pub(crate) decode_input_bytes_read: u64,
}

impl<P> ProgressSink for EncodePhaseProgress<'_, P>
where
    P: RecompressProgressSink,
{
    fn on_frame(&mut self, progress: ProgressSnapshot) -> Result<()> {
        self.sink.on_progress(RecompressProgress {
            phase: RecompressPhase::Encode,
            phase_processed_samples: progress.processed_samples,
            phase_total_samples: progress.total_samples,
            overall_processed_samples: self
                .total_samples
                .saturating_add(progress.processed_samples),
            overall_total_samples: overall_total_samples(self.total_samples),
            completed_frames: progress.completed_frames,
            total_frames: progress.total_frames,
            phase_input_bytes_read: progress.input_bytes_read,
            phase_output_bytes_written: progress.output_bytes_written,
            overall_input_bytes_read: self
                .decode_input_bytes_read
                .saturating_add(progress.input_bytes_read),
            overall_output_bytes_written: progress.output_bytes_written,
        })
    }
}

pub(crate) const fn overall_total_samples(total_samples: u64) -> u64 {
    total_samples.saturating_mul(2)
}

#[cfg(test)]
mod tests {
    use super::{
        EncodePhaseProgress, RecompressPhase, RecompressProgress, encode_phase_transition_progress,
    };
    use crate::error::Result;
    use crate::progress::{ProgressSink, ProgressSnapshot};

    struct CaptureSink(Option<RecompressProgress>);

    impl super::RecompressProgressSink for CaptureSink {
        fn on_progress(&mut self, progress: RecompressProgress) -> Result<()> {
            self.0 = Some(progress);
            Ok(())
        }
    }

    #[test]
    fn recompress_progress_carries_phase_and_overall_read_write_counters() {
        let progress = RecompressProgress {
            phase: RecompressPhase::Decode,
            phase_processed_samples: 512,
            phase_total_samples: 1_024,
            overall_processed_samples: 512,
            overall_total_samples: 2_048,
            completed_frames: 3,
            total_frames: 6,
            phase_input_bytes_read: 8_192,
            phase_output_bytes_written: 16_384,
            overall_input_bytes_read: 8_192,
            overall_output_bytes_written: 16_384,
        };

        assert_eq!(progress.phase_input_bytes_read, 8_192);
        assert_eq!(progress.overall_output_bytes_written, 16_384);
    }

    #[test]
    fn encode_phase_transition_starts_with_zero_output_bytes_written() {
        let progress = encode_phase_transition_progress(1_024, 8, 4_096);

        assert_eq!(progress.phase, RecompressPhase::Encode);
        assert_eq!(progress.overall_input_bytes_read, 4_096);
        assert_eq!(progress.overall_output_bytes_written, 0);
    }

    #[test]
    fn encode_phase_progress_uses_actual_output_bytes_written() {
        let mut sink = CaptureSink(None);
        let mut progress = EncodePhaseProgress {
            sink: &mut sink,
            total_samples: 2_048,
            decode_input_bytes_read: 8_192,
        };

        progress
            .on_frame(ProgressSnapshot {
                processed_samples: 256,
                total_samples: 512,
                completed_frames: 1,
                total_frames: 4,
                input_bytes_read: 128,
                output_bytes_written: 512,
            })
            .unwrap();

        let captured = sink.0.unwrap();
        assert_eq!(captured.overall_input_bytes_read, 8_320);
        assert_eq!(captured.overall_output_bytes_written, 512);
    }
}
