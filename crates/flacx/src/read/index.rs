use std::collections::VecDeque;

use crate::{error::Result, stream_info::StreamInfo};

use super::{FrameIndex, ParsedFrame, slab::DecodeSlabPlan};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RollingIndexConfig {
    pub(super) target_pcm_frames_per_slab: usize,
    pub(super) max_frames_per_slab: usize,
    pub(super) max_bytes_per_slab: usize,
    pub(super) max_slabs_ahead: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FrameDescriptor {
    pub(super) frame: FrameIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SealedSlab {
    sequence: usize,
    end_frame_index: usize,
}

#[derive(Debug)]
pub(super) struct RollingIndexWindow {
    #[cfg(test)]
    stream_info: StreamInfo,
    config: RollingIndexConfig,
    next_sequence: usize,
    next_frame_index: usize,
    retired_sequence: usize,
    pending_frames: Vec<FrameDescriptor>,
    pending_pcm_frames: usize,
    pending_bytes: usize,
    sealed_slabs: VecDeque<SealedSlab>,
    in_window_slabs: usize,
}

impl RollingIndexWindow {
    pub(super) fn new(stream_info: StreamInfo, config: RollingIndexConfig) -> Self {
        #[cfg(not(test))]
        let _ = stream_info;
        Self {
            #[cfg(test)]
            stream_info,
            config,
            next_sequence: 0,
            next_frame_index: 0,
            retired_sequence: 0,
            pending_frames: Vec::new(),
            pending_pcm_frames: 0,
            pending_bytes: 0,
            sealed_slabs: VecDeque::new(),
            in_window_slabs: 0,
        }
    }

    pub(super) fn set_max_slabs_ahead(&mut self, max_slabs_ahead: usize) {
        self.config.max_slabs_ahead = max_slabs_ahead;
    }

    pub(super) fn has_capacity(&self) -> bool {
        self.in_window_slabs < self.config.max_slabs_ahead
    }

    pub(super) fn retire_sequence(&mut self, sequence: usize) {
        if sequence + 1 > self.retired_sequence {
            let retired = sequence + 1 - self.retired_sequence;
            self.retired_sequence = sequence + 1;
            self.in_window_slabs = self.in_window_slabs.saturating_sub(retired);
        }
    }

    pub(super) fn retire_completed_input_frames(&mut self, completed_input_frames: usize) {
        while self
            .sealed_slabs
            .front()
            .is_some_and(|slab| slab.end_frame_index <= completed_input_frames)
        {
            let slab = self
                .sealed_slabs
                .pop_front()
                .expect("front slab exists while retiring completed input frames");
            self.retire_sequence(slab.sequence);
        }
    }

    pub(super) fn push_frame(
        &mut self,
        frame: ParsedFrame,
        offset: usize,
    ) -> Result<Option<DecodeSlabPlan>> {
        if self.should_seal_before_adding(&frame) {
            let sealed = self.seal_pending_slab();
            self.append_frame(frame, offset);
            return Ok(Some(sealed));
        }

        self.append_frame(frame, offset);
        Ok(self.should_seal_slab().then(|| self.seal_pending_slab()))
    }

    pub(super) fn finish(&mut self) -> Option<DecodeSlabPlan> {
        (!self.pending_frames.is_empty()).then(|| self.seal_pending_slab())
    }

    fn append_frame(&mut self, frame: ParsedFrame, offset: usize) {
        let frame_index = FrameIndex {
            header_number: frame.header_number,
            offset,
            header_bytes_consumed: frame.header_bytes_consumed,
            bytes_consumed: frame.bytes_consumed,
            block_size: frame.block_size,
            bits_per_sample: frame.bits_per_sample,
            assignment: frame.assignment,
        };
        self.pending_pcm_frames += usize::from(frame.block_size);
        self.pending_bytes += frame.bytes_consumed;
        self.pending_frames
            .push(FrameDescriptor { frame: frame_index });
        self.next_frame_index += 1;
    }

    fn should_seal_slab(&self) -> bool {
        !self.pending_frames.is_empty()
            && (self.pending_pcm_frames >= self.config.target_pcm_frames_per_slab
                || self.pending_frames.len() >= self.config.max_frames_per_slab
                || self.pending_bytes >= self.config.max_bytes_per_slab)
    }

    fn should_seal_before_adding(&self, frame: &ParsedFrame) -> bool {
        !self.pending_frames.is_empty()
            && (self
                .pending_pcm_frames
                .saturating_add(usize::from(frame.block_size))
                > self.config.target_pcm_frames_per_slab
                || self.pending_bytes.saturating_add(frame.bytes_consumed)
                    > self.config.max_bytes_per_slab)
    }

    fn seal_pending_slab(&mut self) -> DecodeSlabPlan {
        let start_frame_index = self
            .next_frame_index
            .checked_sub(self.pending_frames.len())
            .expect("pending slab always contains at least one frame");
        let sequence = self.next_sequence;
        self.next_sequence += 1;

        let end_frame_index = start_frame_index + self.pending_frames.len();
        self.sealed_slabs.push_back(SealedSlab {
            sequence,
            end_frame_index,
        });
        self.in_window_slabs += 1;

        let frames = self
            .pending_frames
            .drain(..)
            .map(|descriptor| descriptor.frame)
            .collect();
        self.pending_pcm_frames = 0;
        self.pending_bytes = 0;

        DecodeSlabPlan::new(sequence, start_frame_index, frames)
    }

    #[cfg(test)]
    pub(super) fn push_mock_frame(
        &mut self,
        block_size: u16,
        bytes_consumed: usize,
    ) -> Result<Vec<DecodeSlabPlan>> {
        let parsed = ParsedFrame {
            header_number: super::FrameHeaderNumber {
                kind: super::FrameHeaderNumberKind::FrameNumber,
                value: self.next_frame_index as u64,
            },
            block_size,
            bits_per_sample: self.stream_info.bits_per_sample,
            assignment: crate::model::ChannelAssignment::Independent(0),
            header_bytes_consumed: 4,
            bytes_consumed,
        };
        Ok(self.push_frame(parsed, 0)?.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{RollingIndexConfig, RollingIndexWindow};
    use crate::stream_info::StreamInfo;

    fn stream_info() -> StreamInfo {
        StreamInfo {
            min_block_size: 16,
            max_block_size: 16,
            min_frame_size: 32,
            max_frame_size: 32,
            sample_rate: 44_100,
            channels: 1,
            bits_per_sample: 16,
            total_samples: 16 * 12,
            md5: [0; 16],
        }
    }

    #[test]
    fn seals_monotonic_slabs_without_indexing_the_whole_stream() {
        let config = RollingIndexConfig {
            target_pcm_frames_per_slab: 32,
            max_frames_per_slab: 4,
            max_bytes_per_slab: 256,
            max_slabs_ahead: 2,
        };
        let mut window = RollingIndexWindow::new(stream_info(), config);

        let first = window.push_mock_frame(16, 32).unwrap();
        assert_eq!(first.len(), 0);

        let second = window.push_mock_frame(16, 32).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].sequence, 0);
        assert_eq!(second[0].frame_block_sizes, vec![16, 16]);
        assert!(window.has_capacity());
    }

    #[test]
    fn blocks_further_indexing_when_the_sliding_window_is_full() {
        let config = RollingIndexConfig {
            target_pcm_frames_per_slab: 16,
            max_frames_per_slab: 1,
            max_bytes_per_slab: 128,
            max_slabs_ahead: 1,
        };
        let mut window = RollingIndexWindow::new(stream_info(), config);

        let sealed = window.push_mock_frame(16, 32).unwrap();
        assert_eq!(sealed.len(), 1);
        assert!(!window.has_capacity());

        window.retire_sequence(0);
        assert!(window.has_capacity());
    }

    #[test]
    fn seals_before_admitting_a_frame_that_would_exceed_the_pcm_budget() {
        let config = RollingIndexConfig {
            target_pcm_frames_per_slab: 24,
            max_frames_per_slab: 4,
            max_bytes_per_slab: 256,
            max_slabs_ahead: 4,
        };
        let mut window = RollingIndexWindow::new(stream_info(), config);

        assert!(window.push_mock_frame(16, 32).unwrap().is_empty());
        let sealed = window.push_mock_frame(16, 32).unwrap();

        assert_eq!(sealed.len(), 1);
        assert_eq!(sealed[0].sequence, 0);
        assert_eq!(sealed[0].frame_block_sizes, vec![16]);

        let flushed = window.finish().expect("partial slab flushes at EOF");
        assert_eq!(flushed.sequence, 1);
        assert_eq!(flushed.frame_block_sizes, vec![16]);
    }

    #[test]
    fn seals_before_admitting_a_frame_that_would_exceed_the_byte_budget() {
        let config = RollingIndexConfig {
            target_pcm_frames_per_slab: 64,
            max_frames_per_slab: 4,
            max_bytes_per_slab: 48,
            max_slabs_ahead: 4,
        };
        let mut window = RollingIndexWindow::new(stream_info(), config);

        assert!(window.push_mock_frame(16, 24).unwrap().is_empty());
        let sealed = window.push_mock_frame(16, 25).unwrap();

        assert_eq!(sealed.len(), 1);
        assert_eq!(sealed[0].sequence, 0);
        assert_eq!(sealed[0].frame_block_sizes, vec![16]);

        let flushed = window.finish().expect("partial slab flushes at EOF");
        assert_eq!(flushed.sequence, 1);
        assert_eq!(flushed.frame_block_sizes, vec![16]);
    }

    #[test]
    fn finish_flushes_a_partial_slab() {
        let config = RollingIndexConfig {
            target_pcm_frames_per_slab: 64,
            max_frames_per_slab: 4,
            max_bytes_per_slab: 256,
            max_slabs_ahead: 2,
        };
        let mut window = RollingIndexWindow::new(stream_info(), config);

        assert!(window.push_mock_frame(16, 32).unwrap().is_empty());

        let flushed = window.finish().expect("partial slab flushes at EOF");
        assert_eq!(flushed.sequence, 0);
        assert_eq!(flushed.frame_block_sizes, vec![16]);
        assert!(window.finish().is_none());
    }

    #[test]
    fn completed_input_frame_progress_reopens_capacity() {
        let config = RollingIndexConfig {
            target_pcm_frames_per_slab: 16,
            max_frames_per_slab: 1,
            max_bytes_per_slab: 128,
            max_slabs_ahead: 2,
        };
        let mut window = RollingIndexWindow::new(stream_info(), config);

        let first = window.push_mock_frame(16, 32).unwrap();
        let second = window.push_mock_frame(16, 32).unwrap();
        assert_eq!(first[0].sequence, 0);
        assert_eq!(second[0].sequence, 1);
        assert!(!window.has_capacity());

        window.retire_completed_input_frames(1);
        assert!(window.has_capacity());
    }
}
