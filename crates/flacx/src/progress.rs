//! Progress reporting primitives used by `flacx` when the `progress` feature is enabled.
//!
//! The encoder and decoder both report the same [`ProgressSnapshot`] shape.
//! [`EncodeProgress`] and [`DecodeProgress`] are aliases of that snapshot type.

use crate::error::Result;

/// A monotonic snapshot of encode or decode progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressSnapshot {
    /// Samples processed so far.
    pub processed_samples: u64,
    /// Total samples expected for the current input.
    pub total_samples: u64,
    /// Frames completed so far.
    pub completed_frames: usize,
    /// Total frames planned for the current input.
    pub total_frames: usize,
}

#[cfg(feature = "progress")]
/// Progress snapshot reported by encoder callbacks.
pub type EncodeProgress = ProgressSnapshot;

#[cfg(feature = "progress")]
/// Progress snapshot reported by decoder callbacks.
pub type DecodeProgress = ProgressSnapshot;

pub(crate) trait ProgressSink {
    fn on_frame(&mut self, progress: ProgressSnapshot) -> Result<()>;
}

pub(crate) struct NoProgress;

impl ProgressSink for NoProgress {
    fn on_frame(&mut self, _progress: ProgressSnapshot) -> Result<()> {
        Ok(())
    }
}

#[cfg(feature = "progress")]
pub(crate) struct CallbackProgress<F> {
    callback: F,
}

#[cfg(feature = "progress")]
impl<F> CallbackProgress<F> {
    pub(crate) fn new(callback: F) -> Self {
        Self { callback }
    }
}

#[cfg(feature = "progress")]
impl<F> ProgressSink for CallbackProgress<F>
where
    F: FnMut(ProgressSnapshot) -> Result<()>,
{
    fn on_frame(&mut self, progress: ProgressSnapshot) -> Result<()> {
        (self.callback)(progress)
    }
}
