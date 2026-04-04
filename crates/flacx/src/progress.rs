use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressSnapshot {
    pub processed_samples: u64,
    pub total_samples: u64,
    pub completed_frames: usize,
    pub total_frames: usize,
}

#[cfg(feature = "progress")]
pub type EncodeProgress = ProgressSnapshot;

#[cfg(feature = "progress")]
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
