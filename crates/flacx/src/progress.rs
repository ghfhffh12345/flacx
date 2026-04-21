//! Progress reporting primitives used by `flacx` when the `progress` feature is enabled.
//!
//! The encoder and decoder both report the same [`ProgressSnapshot`] shape.
//! [`EncodeProgress`] and [`DecodeProgress`] are aliases of that snapshot type.

use crate::error::Result;

macro_rules! emit_progress {
    ($progress:expr, $snapshot:expr) => {{
        #[cfg(feature = "progress")]
        {
            $progress.on_frame($snapshot)
        }
        #[cfg(not(feature = "progress"))]
        {
            let _ = $progress;
            Ok::<(), crate::error::Error>(())
        }
    }};
}

pub(crate) use emit_progress;

/// A monotonic snapshot of encode or decode progress.
#[cfg(feature = "progress")]
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
    /// Input bytes read so far.
    pub input_bytes_read: u64,
    /// Output bytes written so far.
    pub output_bytes_written: u64,
}

#[cfg(not(feature = "progress"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProgressSnapshot {
    /// Samples processed so far.
    pub processed_samples: u64,
    /// Total samples expected for the current input.
    pub total_samples: u64,
    /// Frames completed so far.
    pub completed_frames: usize,
    /// Total frames planned for the current input.
    pub total_frames: usize,
    /// Input bytes read so far.
    pub input_bytes_read: u64,
    /// Output bytes written so far.
    pub output_bytes_written: u64,
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

#[cfg(test)]
mod tests {
    use super::ProgressSnapshot;

    #[test]
    fn progress_snapshot_carries_input_bytes_read_and_output_bytes_written() {
        let snapshot = ProgressSnapshot {
            processed_samples: 128,
            total_samples: 256,
            completed_frames: 2,
            total_frames: 4,
            input_bytes_read: 4_096,
            output_bytes_written: 1_024,
        };

        assert_eq!(snapshot.input_bytes_read, 4_096);
        assert_eq!(snapshot.output_bytes_written, 1_024);
    }

    #[cfg(not(feature = "progress"))]
    #[test]
    fn progress_types_are_not_reexported_without_progress_feature() {
        let _ = crate::Error::Encode("no progress".into());
    }
}
