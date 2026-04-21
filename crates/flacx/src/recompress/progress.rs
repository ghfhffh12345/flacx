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
    pub(crate) decode_output_bytes_written: u64,
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
            overall_output_bytes_written: self
                .decode_output_bytes_written
                .saturating_add(progress.output_bytes_written),
        })
    }
}

pub(crate) const fn overall_total_samples(total_samples: u64) -> u64 {
    total_samples.saturating_mul(2)
}

#[cfg(test)]
mod tests {
    use super::{RecompressPhase, RecompressProgress};

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
}
