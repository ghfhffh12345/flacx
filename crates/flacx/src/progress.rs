use crate::error::Result;

#[cfg(feature = "progress")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeProgress {
    pub processed_samples: u64,
    pub total_samples: u64,
    pub completed_frames: usize,
    pub total_frames: usize,
}

pub(crate) trait ProgressSink {
    fn on_frame(
        &mut self,
        processed_samples: u64,
        total_samples: u64,
        completed_frames: usize,
        total_frames: usize,
    ) -> Result<()>;
}

pub(crate) struct NoProgress;

impl ProgressSink for NoProgress {
    fn on_frame(
        &mut self,
        _processed_samples: u64,
        _total_samples: u64,
        _completed_frames: usize,
        _total_frames: usize,
    ) -> Result<()> {
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
    F: FnMut(EncodeProgress) -> Result<()>,
{
    fn on_frame(
        &mut self,
        processed_samples: u64,
        total_samples: u64,
        completed_frames: usize,
        total_frames: usize,
    ) -> Result<()> {
        (self.callback)(EncodeProgress {
            processed_samples,
            total_samples,
            completed_frames,
            total_frames,
        })
    }
}
