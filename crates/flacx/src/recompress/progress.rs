use crate::{
    error::Result,
    progress::{ProgressSink, ProgressSnapshot},
};

/// Phase marker for recompress progress reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecompressPhase {
    Decode,
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
        })
    }
}

pub(crate) const fn overall_total_samples(total_samples: u64) -> u64 {
    total_samples.saturating_mul(2)
}
