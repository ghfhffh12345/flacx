use super::{StreamInfo, frame};

#[derive(Debug, PartialEq, Eq)]
pub(super) struct DecodedSlab {
    pub(super) sequence: usize,
    pub(super) start_frame_index: usize,
    pub(super) frame_block_sizes: Vec<u16>,
    pub(super) decoded_samples: Vec<i32>,
}

impl DecodedSlab {
    pub(super) fn frame_count(&self) -> usize {
        self.frame_block_sizes.len()
    }

    pub(super) fn pcm_frames(&self) -> usize {
        self.frame_block_sizes
            .iter()
            .map(|&block_size| usize::from(block_size))
            .sum()
    }
}

impl From<frame::DecodedWorkSlab> for DecodedSlab {
    fn from(slab: frame::DecodedWorkSlab) -> Self {
        Self {
            sequence: slab.sequence,
            start_frame_index: slab.start_frame_index,
            frame_block_sizes: slab.frame_block_sizes,
            decoded_samples: slab.decoded_samples,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct DecodeSlabPlan {
    pub(super) sequence: usize,
    pub(super) start_frame_index: usize,
    pub(super) start_sample_number: u64,
    pub(super) stream_info: StreamInfo,
    pub(super) frame_block_sizes: Vec<u16>,
    pub(super) bytes: std::sync::Arc<[u8]>,
    pub(super) frames: std::sync::Arc<[super::FrameIndex]>,
}

impl DecodeSlabPlan {
    pub(super) fn new(
        sequence: usize,
        start_frame_index: usize,
        start_sample_number: u64,
        stream_info: StreamInfo,
        frames: Vec<super::FrameIndex>,
    ) -> Self {
        let frame_block_sizes = frames.iter().map(|frame| frame.block_size).collect();
        Self {
            sequence,
            start_frame_index,
            start_sample_number,
            stream_info,
            frame_block_sizes,
            bytes: std::sync::Arc::from(Vec::<u8>::new()),
            frames: std::sync::Arc::from(frames),
        }
    }

    pub(super) fn seal_bytes(mut self, bytes: Vec<u8>) -> Self {
        self.bytes = std::sync::Arc::from(bytes);
        self
    }
}

impl From<DecodeSlabPlan> for frame::DecodeWorkSlab {
    fn from(plan: DecodeSlabPlan) -> Self {
        Self {
            sequence: plan.sequence,
            start_frame_index: plan.start_frame_index,
            start_sample_number: plan.start_sample_number,
            stream_info: plan.stream_info,
            bytes: plan.bytes,
        }
    }
}

#[derive(Debug)]
struct DrainingSlab {
    frame_block_sizes: Vec<u16>,
    decoded_samples: Vec<i32>,
    sample_cursor: usize,
    drained_input_frames: usize,
    drained_pcm_frames: usize,
}

impl DrainingSlab {
    fn new(slab: DecodedSlab) -> Self {
        Self {
            frame_block_sizes: slab.frame_block_sizes,
            decoded_samples: slab.decoded_samples,
            sample_cursor: 0,
            drained_input_frames: 0,
            drained_pcm_frames: 0,
        }
    }

    fn drain_into(
        &mut self,
        max_frames: usize,
        channels: usize,
        output: &mut Vec<i32>,
    ) -> (usize, usize) {
        let available_frames = self
            .decoded_samples
            .len()
            .saturating_sub(self.sample_cursor)
            / channels;
        let drained_frames = available_frames.min(max_frames);
        if drained_frames == 0 {
            return (0, 0);
        }

        if self.sample_cursor == 0 && drained_frames == available_frames && output.is_empty() {
            output.extend(std::mem::take(&mut self.decoded_samples));
            self.sample_cursor = drained_frames * channels;

            let completed_input_frames = self
                .frame_block_sizes
                .len()
                .saturating_sub(self.drained_input_frames);
            self.drained_input_frames = self.frame_block_sizes.len();
            self.drained_pcm_frames = total_pcm_frames(&self.frame_block_sizes);

            return (drained_frames, completed_input_frames);
        }

        let drained_samples = drained_frames * channels;
        let next_cursor = self.sample_cursor + drained_samples;
        output.extend_from_slice(&self.decoded_samples[self.sample_cursor..next_cursor]);
        self.sample_cursor = next_cursor;

        let mut completed_input_frames = 0usize;
        let total_drained_pcm_frames = self.sample_cursor / channels;
        while self.drained_input_frames < self.frame_block_sizes.len() {
            let next_frame_pcm_frames =
                usize::from(self.frame_block_sizes[self.drained_input_frames]);
            if self.drained_pcm_frames + next_frame_pcm_frames > total_drained_pcm_frames {
                break;
            }
            self.drained_pcm_frames += next_frame_pcm_frames;
            self.drained_input_frames += 1;
            completed_input_frames += 1;
        }

        (drained_frames, completed_input_frames)
    }

    fn is_fully_drained(&self) -> bool {
        self.sample_cursor >= self.decoded_samples.len()
    }
}

fn total_pcm_frames(frame_block_sizes: &[u16]) -> usize {
    frame_block_sizes
        .iter()
        .map(|&block_size| usize::from(block_size))
        .sum()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OrderedDrainProgress {
    pub(super) drained_frames: usize,
    pub(super) completed_input_frames: usize,
    pub(super) retired_slabs: usize,
}

#[derive(Debug)]
pub(super) struct OrderedSlabDrain {
    ready_slots: Vec<Option<DecodedSlab>>,
    next_ready_sequence: usize,
    next_ready_slab_start_frame: usize,
    draining_slab: Option<DrainingSlab>,
    completed_input_frames: usize,
}

impl OrderedSlabDrain {
    pub(super) fn new() -> Self {
        Self::with_window_capacity(1)
    }

    pub(super) fn with_window_capacity(window_capacity: usize) -> Self {
        let ready_slot_capacity = window_capacity.max(1);
        Self {
            ready_slots: (0..ready_slot_capacity).map(|_| None).collect(),
            next_ready_sequence: 0,
            next_ready_slab_start_frame: 0,
            draining_slab: None,
            completed_input_frames: 0,
        }
    }

    pub(super) fn push_ready<S>(&mut self, slab: S)
    where
        S: Into<DecodedSlab>,
    {
        let slab = slab.into();
        let sequence = slab.sequence;
        assert!(
            sequence >= self.next_ready_sequence,
            "ready slab sequence {sequence} is behind the next expected sequence {}",
            self.next_ready_sequence,
        );
        assert!(
            sequence < self.next_ready_sequence + self.ready_slots.len(),
            "ready slab sequence {sequence} exceeds bounded ready window ending before sequence {}",
            self.next_ready_sequence + self.ready_slots.len(),
        );

        let slot_index = self.ready_slot_index(sequence);
        let slot = &mut self.ready_slots[slot_index];
        assert!(
            slot.is_none(),
            "ready slot collision at sequence {sequence} (slot {slot_index})",
        );
        *slot = Some(slab);
    }

    pub(super) fn drain_into(
        &mut self,
        max_frames: usize,
        channels: usize,
        output: &mut Vec<i32>,
    ) -> OrderedDrainProgress {
        if max_frames == 0 {
            return OrderedDrainProgress {
                drained_frames: 0,
                completed_input_frames: 0,
                retired_slabs: 0,
            };
        }

        let can_span_slab_boundaries = self.draining_slab.is_none();
        let mut total_drained_frames = 0usize;
        let mut total_completed_input_frames = 0usize;
        let mut retired_slabs = 0usize;
        while total_drained_frames < max_frames && self.activate_next_ready_slab() {
            let (drained_frames, completed_input_frames, slab_finished) = {
                let slab = self
                    .draining_slab
                    .as_mut()
                    .expect("draining slab is present after activation");
                let (drained_frames, completed_input_frames) =
                    slab.drain_into(max_frames - total_drained_frames, channels, output);
                (
                    drained_frames,
                    completed_input_frames,
                    slab.is_fully_drained(),
                )
            };

            total_drained_frames += drained_frames;
            total_completed_input_frames += completed_input_frames;
            self.completed_input_frames += completed_input_frames;

            if slab_finished {
                self.draining_slab = None;
                retired_slabs += 1;
                if !can_span_slab_boundaries {
                    break;
                }
            }
            if drained_frames == 0 {
                break;
            }
        }

        OrderedDrainProgress {
            drained_frames: total_drained_frames,
            completed_input_frames: total_completed_input_frames,
            retired_slabs,
        }
    }

    pub(super) fn completed_input_frames(&self) -> usize {
        self.completed_input_frames
    }

    pub(super) fn ready_slab_count(&self) -> usize {
        self.ready_slots.iter().flatten().count()
    }

    pub(super) fn next_ready_slab_start_frame(&self) -> usize {
        self.next_ready_slab_start_frame
    }

    #[cfg(test)]
    pub(super) fn ready_slot_capacity(&self) -> usize {
        self.ready_slots.len()
    }

    pub(super) fn has_draining_slab(&self) -> bool {
        self.draining_slab.is_some()
    }

    pub(super) fn active_slab_count(&self) -> usize {
        self.ready_slab_count() + usize::from(self.draining_slab.is_some())
    }

    pub(super) fn is_idle(&self) -> bool {
        self.ready_slots.iter().all(Option::is_none) && self.draining_slab.is_none()
    }

    fn activate_next_ready_slab(&mut self) -> bool {
        if self.draining_slab.is_some() {
            return true;
        }

        let slot_index = self.ready_slot_index(self.next_ready_sequence);
        let Some(slab) = self.ready_slots[slot_index].take() else {
            return false;
        };

        debug_assert_eq!(slab.sequence, self.next_ready_sequence);
        self.next_ready_sequence += 1;
        self.next_ready_slab_start_frame += slab.frame_count();
        self.draining_slab = Some(DrainingSlab::new(slab));
        true
    }

    fn ready_slot_index(&self, sequence: usize) -> usize {
        sequence % self.ready_slots.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{DecodedSlab, OrderedSlabDrain};

    fn slab(
        sequence: usize,
        start_frame_index: usize,
        block_sizes: &[u16],
        samples: &[i32],
    ) -> DecodedSlab {
        DecodedSlab {
            sequence,
            start_frame_index,
            frame_block_sizes: block_sizes.to_vec(),
            decoded_samples: samples.to_vec(),
        }
    }

    #[test]
    fn drains_slabs_in_frame_order_even_when_completion_is_out_of_order() {
        let mut drain = OrderedSlabDrain::with_window_capacity(4);
        let mut output = Vec::new();

        drain.push_ready(slab(1, 2, &[2], &[30, 31]));
        drain.push_ready(slab(0, 0, &[2, 2], &[10, 11, 20, 21]));

        assert_eq!(drain.drain_into(2, 1, &mut output).drained_frames, 2);
        assert_eq!(output, vec![10, 11]);

        let progress = drain.drain_into(4, 1, &mut output);
        assert_eq!(progress.drained_frames, 2);
        assert_eq!(progress.completed_input_frames, 1);
        assert_eq!(progress.retired_slabs, 1);
        assert_eq!(output, vec![10, 11, 20, 21]);

        drain.push_ready(slab(2, 3, &[2], &[40, 41]));
        let progress = drain.drain_into(8, 1, &mut output);
        assert_eq!(progress.drained_frames, 4);
        assert_eq!(progress.completed_input_frames, 2);
        assert_eq!(progress.retired_slabs, 2);
        assert_eq!(output, vec![10, 11, 20, 21, 30, 31, 40, 41]);
    }

    #[test]
    fn preserves_partial_drain_frame_accounting() {
        let mut drain = OrderedSlabDrain::new();
        let mut output = Vec::new();

        drain.push_ready(slab(0, 0, &[3, 3], &[1, 2, 3, 4, 5, 6]));

        let progress = drain.drain_into(2, 1, &mut output);
        assert_eq!(progress.drained_frames, 2);
        assert_eq!(progress.completed_input_frames, 0);
        assert_eq!(progress.retired_slabs, 0);
        assert_eq!(drain.completed_input_frames(), 0);

        let progress = drain.drain_into(4, 1, &mut output);
        assert_eq!(progress.drained_frames, 4);
        assert_eq!(progress.completed_input_frames, 2);
        assert_eq!(progress.retired_slabs, 1);
        assert_eq!(drain.completed_input_frames(), 2);
        assert!(drain.is_idle());
    }
    #[test]
    fn ordered_drain_holds_out_of_order_results_until_sequence_gap_closes() {
        let mut drain = OrderedSlabDrain::with_window_capacity(3);
        let mut output = Vec::new();

        drain.push_ready(slab(1, 1, &[2], &[30, 31]));

        let progress = drain.drain_into(8, 1, &mut output);
        assert_eq!(progress.drained_frames, 0);
        assert_eq!(progress.completed_input_frames, 0);
        assert_eq!(progress.retired_slabs, 0);
        assert!(output.is_empty());

        drain.push_ready(slab(0, 0, &[2], &[10, 11]));

        let progress = drain.drain_into(8, 1, &mut output);
        assert_eq!(progress.drained_frames, 4);
        assert_eq!(progress.completed_input_frames, 2);
        assert_eq!(progress.retired_slabs, 2);
        assert_eq!(output, vec![10, 11, 30, 31]);
    }

    #[test]
    fn ordered_drain_keeps_ready_state_bounded_to_window_capacity() {
        let mut drain = OrderedSlabDrain::with_window_capacity(2);
        let mut output = Vec::new();

        drain.push_ready(slab(1, 1, &[1], &[20]));
        drain.push_ready(slab(0, 0, &[1], &[10]));

        assert_eq!(drain.ready_slot_capacity(), 2);
        assert_eq!(drain.ready_slab_count(), 2);

        let progress = drain.drain_into(1, 1, &mut output);
        assert_eq!(progress.drained_frames, 1);
        assert_eq!(progress.completed_input_frames, 1);
        assert_eq!(progress.retired_slabs, 1);
        assert_eq!(drain.ready_slot_capacity(), 2);
        assert_eq!(drain.ready_slab_count(), 1);

        drain.push_ready(slab(2, 2, &[1], &[30]));

        assert_eq!(drain.ready_slot_capacity(), 2);
        assert_eq!(drain.ready_slab_count(), 2);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drain.push_ready(slab(3, 3, &[1], &[40]));
        }));
        let message = panic.expect_err("sequence beyond the bounded ready window should panic");
        let message = if let Some(message) = message.downcast_ref::<String>() {
            message.as_str()
        } else if let Some(message) = message.downcast_ref::<&'static str>() {
            message
        } else {
            panic!("unexpected panic payload");
        };
        assert!(
            message.contains("exceeds bounded ready window"),
            "unexpected panic message: {message}"
        );
    }
}
