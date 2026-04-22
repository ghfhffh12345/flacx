use std::collections::VecDeque;

use crate::{error::Result, stream_info::StreamInfo};

use super::{
    index::{PushFrameOutcome, RollingIndexConfig, RollingIndexWindow},
    profile,
    slab::DecodeSlabPlan,
    ParsedFrame,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProducerConfig {
    pub(super) target_pcm_frames_per_slab: usize,
    pub(super) max_frames_per_slab: usize,
    pub(super) max_bytes_per_slab: usize,
    pub(super) max_slabs_ahead: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SubmittedDecodeSlab {
    end_frame_index: usize,
    pcm_frames: usize,
    input_bytes: usize,
}

#[derive(Debug)]
pub(super) struct ProducerState {
    index_window: RollingIndexWindow,
    inflight_slabs: usize,
    submitted_slabs: VecDeque<SubmittedDecodeSlab>,
    staged_input_bytes: usize,
}

impl ProducerState {
    pub(super) fn new(stream_info: StreamInfo, config: ProducerConfig) -> Self {
        Self {
            index_window: RollingIndexWindow::new(
                stream_info,
                RollingIndexConfig {
                    target_pcm_frames_per_slab: config.target_pcm_frames_per_slab,
                    max_frames_per_slab: config.max_frames_per_slab,
                    max_bytes_per_slab: config.max_bytes_per_slab,
                    max_slabs_ahead: config.max_slabs_ahead,
                },
            ),
            inflight_slabs: 0,
            submitted_slabs: VecDeque::new(),
            staged_input_bytes: 0,
        }
    }

    pub(super) fn set_max_slabs_ahead(&mut self, max_slabs_ahead: usize) {
        self.index_window.set_max_slabs_ahead(max_slabs_ahead);
    }

    pub(super) fn has_capacity(&self) -> bool {
        self.index_window.has_capacity()
    }

    pub(super) fn push_frame(
        &mut self,
        frame: ParsedFrame,
        offset: usize,
    ) -> Result<PushFrameOutcome> {
        self.index_window.push_frame(frame, offset)
    }

    pub(super) fn finish(&mut self) -> Option<DecodeSlabPlan> {
        self.index_window.finish()
    }

    pub(super) fn retire_completed_input_frames(&mut self, completed_input_frames: usize) {
        self.index_window
            .retire_completed_input_frames(completed_input_frames);
    }

    pub(super) fn inflight_slabs(&self) -> usize {
        self.inflight_slabs
    }

    pub(super) fn staged_input_bytes(&self) -> usize {
        self.staged_input_bytes
    }

    pub(super) fn record_ready_submission(&mut self, plan: &DecodeSlabPlan) {
        let (end_frame_index, pcm_frames, input_bytes) = Self::submitted_slab_residency(plan);
        self.record_ready_submission_parts_with_input_bytes(
            end_frame_index,
            pcm_frames,
            input_bytes,
        );
    }

    pub(super) fn record_ready_submission_parts_with_input_bytes(
        &mut self,
        end_frame_index: usize,
        pcm_frames: usize,
        input_bytes: usize,
    ) {
        self.record_submitted_slab(end_frame_index, pcm_frames, input_bytes);
    }

    #[cfg(test)]
    pub(super) fn record_inflight_submission(&mut self, plan: &DecodeSlabPlan) -> usize {
        let (end_frame_index, pcm_frames, input_bytes) = Self::submitted_slab_residency(plan);
        self.record_inflight_submission_parts_with_input_bytes(
            end_frame_index,
            pcm_frames,
            input_bytes,
        )
    }

    pub(super) fn record_inflight_submission_parts(
        &mut self,
        end_frame_index: usize,
        pcm_frames: usize,
    ) -> usize {
        self.record_inflight_submission_parts_with_input_bytes(end_frame_index, pcm_frames, 0)
    }

    pub(super) fn record_inflight_submission_parts_with_input_bytes(
        &mut self,
        end_frame_index: usize,
        pcm_frames: usize,
        input_bytes: usize,
    ) -> usize {
        self.record_submitted_slab(end_frame_index, pcm_frames, input_bytes);
        self.inflight_slabs += 1;
        self.inflight_slabs
    }

    pub(super) fn finish_inflight_slabs(&mut self, completed_count: usize) {
        self.inflight_slabs = self.inflight_slabs.saturating_sub(completed_count);
    }

    pub(super) fn release_completed_slab_pcm_frames(
        &mut self,
        completed_input_frames: usize,
    ) -> usize {
        let mut released_pcm_frames = 0usize;
        while self
            .submitted_slabs
            .front()
            .is_some_and(|slab| slab.end_frame_index <= completed_input_frames)
        {
            let slab = self
                .submitted_slabs
                .pop_front()
                .expect("front slab exists while releasing completed slabs");
            released_pcm_frames = released_pcm_frames.saturating_add(slab.pcm_frames);
            self.staged_input_bytes = self.staged_input_bytes.saturating_sub(slab.input_bytes);
        }
        profile::observe_staged_input_bytes_for_current_thread(self.staged_input_bytes);
        released_pcm_frames
    }

    pub(super) fn submitted_slab(plan: &DecodeSlabPlan) -> (usize, usize) {
        let (end_frame_index, pcm_frames, _) = Self::submitted_slab_residency(plan);
        (end_frame_index, pcm_frames)
    }

    pub(super) fn submitted_slab_residency(plan: &DecodeSlabPlan) -> (usize, usize, usize) {
        (
            plan.start_frame_index + plan.frame_block_sizes.len(),
            plan.frame_block_sizes
                .iter()
                .map(|&block_size| usize::from(block_size))
                .sum(),
            plan.bytes.len(),
        )
    }

    fn record_submitted_slab(
        &mut self,
        end_frame_index: usize,
        pcm_frames: usize,
        input_bytes: usize,
    ) {
        self.submitted_slabs.push_back(SubmittedDecodeSlab {
            end_frame_index,
            pcm_frames,
            input_bytes,
        });
        self.staged_input_bytes = self.staged_input_bytes.saturating_add(input_bytes);
        profile::observe_staged_input_bytes_for_current_thread(self.staged_input_bytes);
    }

    #[cfg(test)]
    pub(super) fn push_mock_frame(
        &mut self,
        block_size: u16,
        bytes_consumed: usize,
    ) -> Result<Vec<DecodeSlabPlan>> {
        self.index_window
            .push_mock_frame(block_size, bytes_consumed)
    }
}

#[cfg(test)]
mod tests {
    use super::{ProducerConfig, ProducerState};
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

    fn config(max_slabs_ahead: usize) -> ProducerConfig {
        ProducerConfig {
            target_pcm_frames_per_slab: 32,
            max_frames_per_slab: 4,
            max_bytes_per_slab: 256,
            max_slabs_ahead,
        }
    }

    fn single_frame_config(max_slabs_ahead: usize) -> ProducerConfig {
        ProducerConfig {
            target_pcm_frames_per_slab: 16,
            max_frames_per_slab: 1,
            max_bytes_per_slab: 256,
            max_slabs_ahead,
        }
    }

    #[test]
    fn producer_state_seals_monotonic_slabs_within_budget() {
        let mut state = ProducerState::new(stream_info(), config(2));

        let first = state.push_mock_frame(16, 32).unwrap();
        assert!(first.is_empty());

        let second = state.push_mock_frame(16, 32).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].sequence, 0);
        assert_eq!(second[0].frame_block_sizes, vec![16, 16]);
        assert!(state.has_capacity());
    }

    #[test]
    fn producer_state_reopens_capacity_after_completed_input_progress() {
        let mut state = ProducerState::new(stream_info(), single_frame_config(2));

        let first = state.push_mock_frame(16, 32).unwrap();
        let second = state.push_mock_frame(16, 32).unwrap();
        assert_eq!(first[0].sequence, 0);
        assert_eq!(second[0].sequence, 1);
        assert!(!state.has_capacity());

        state.retire_completed_input_frames(1);
        assert!(state.has_capacity());
    }

    #[test]
    fn producer_state_tracks_submitted_slabs_and_releases_pcm_frames_in_order() {
        let mut state = ProducerState::new(stream_info(), single_frame_config(4));

        let first = state
            .push_mock_frame(16, 32)
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .seal_bytes(vec![0; 32]);
        let second = state
            .push_mock_frame(16, 32)
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .seal_bytes(vec![0; 32]);

        assert_eq!(state.record_inflight_submission(&first), 1);
        assert_eq!(state.record_inflight_submission(&second), 2);
        assert_eq!(state.inflight_slabs(), 2);
        assert_eq!(state.staged_input_bytes(), 64);

        state.finish_inflight_slabs(1);
        assert_eq!(state.inflight_slabs(), 1);
        assert_eq!(state.release_completed_slab_pcm_frames(1), 16);
        assert_eq!(state.staged_input_bytes(), 32);
        assert_eq!(state.release_completed_slab_pcm_frames(2), 16);
        assert_eq!(state.staged_input_bytes(), 0);
        assert_eq!(state.release_completed_slab_pcm_frames(2), 0);
    }
}
