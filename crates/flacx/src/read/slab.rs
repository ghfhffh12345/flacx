use std::collections::BTreeMap;

use super::frame;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct DecodedSlab {
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

impl From<frame::DecodedWorkPacket> for DecodedSlab {
    fn from(packet: frame::DecodedWorkPacket) -> Self {
        Self {
            start_frame_index: packet.start_frame_index,
            frame_block_sizes: packet.frame_block_sizes,
            decoded_samples: packet.decoded_samples,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct DecodeSlabPlan {
    pub(super) sequence: usize,
    pub(super) start_frame_index: usize,
    pub(super) frame_block_sizes: Vec<u16>,
    pub(super) bytes: std::sync::Arc<[u8]>,
    pub(super) frames: std::sync::Arc<[super::FrameIndex]>,
}

impl DecodeSlabPlan {
    pub(super) fn new(
        sequence: usize,
        start_frame_index: usize,
        frames: Vec<super::FrameIndex>,
    ) -> Self {
        let frame_block_sizes = frames.iter().map(|frame| frame.block_size).collect();
        Self {
            sequence,
            start_frame_index,
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

impl From<DecodeSlabPlan> for frame::DecodeWorkPacket {
    fn from(plan: DecodeSlabPlan) -> Self {
        Self {
            start_frame_index: plan.start_frame_index,
            frame_block_sizes: plan.frame_block_sizes,
            bytes: plan.bytes,
            frames: plan.frames,
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

#[derive(Debug, Default)]
pub(super) struct OrderedSlabDrain {
    ready_slabs: BTreeMap<usize, DecodedSlab>,
    next_ready_slab_start_frame: usize,
    draining_slab: Option<DrainingSlab>,
    completed_input_frames: usize,
}

impl OrderedSlabDrain {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn push_ready<S>(&mut self, slab: S)
    where
        S: Into<DecodedSlab>,
    {
        let slab = slab.into();
        self.ready_slabs.insert(slab.start_frame_index, slab);
    }

    pub(super) fn drain_into(
        &mut self,
        max_frames: usize,
        channels: usize,
        output: &mut Vec<i32>,
    ) -> (usize, usize) {
        if max_frames == 0 {
            return (0, 0);
        }

        let can_span_slab_boundaries = self.draining_slab.is_none();
        let mut total_drained_frames = 0usize;
        let mut total_completed_input_frames = 0usize;
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
                if !can_span_slab_boundaries {
                    break;
                }
            }
            if drained_frames == 0 {
                break;
            }
        }

        (total_drained_frames, total_completed_input_frames)
    }

    pub(super) fn completed_input_frames(&self) -> usize {
        self.completed_input_frames
    }

    pub(super) fn ready_slab_count(&self) -> usize {
        self.ready_slabs.len()
    }

    pub(super) fn next_ready_slab_start_frame(&self) -> usize {
        self.next_ready_slab_start_frame
    }

    pub(super) fn has_draining_slab(&self) -> bool {
        self.draining_slab.is_some()
    }

    pub(super) fn active_slab_count(&self) -> usize {
        self.ready_slabs.len() + usize::from(self.draining_slab.is_some())
    }

    pub(super) fn is_idle(&self) -> bool {
        self.ready_slabs.is_empty() && self.draining_slab.is_none()
    }

    fn activate_next_ready_slab(&mut self) -> bool {
        if self.draining_slab.is_some() {
            return true;
        }

        let Some(slab) = self.ready_slabs.remove(&self.next_ready_slab_start_frame) else {
            return false;
        };

        self.next_ready_slab_start_frame += slab.frame_count();
        self.draining_slab = Some(DrainingSlab::new(slab));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{DecodedSlab, OrderedSlabDrain};

    fn slab(start_frame_index: usize, block_sizes: &[u16], samples: &[i32]) -> DecodedSlab {
        DecodedSlab {
            start_frame_index,
            frame_block_sizes: block_sizes.to_vec(),
            decoded_samples: samples.to_vec(),
        }
    }

    #[test]
    fn drains_slabs_in_frame_order_even_when_completion_is_out_of_order() {
        let mut drain = OrderedSlabDrain::new();
        let mut output = Vec::new();

        drain.push_ready(slab(2, &[2], &[30, 31]));
        drain.push_ready(slab(0, &[2, 2], &[10, 11, 20, 21]));

        assert_eq!(drain.drain_into(2, 1, &mut output), (2, 1));
        assert_eq!(output, vec![10, 11]);

        assert_eq!(drain.drain_into(4, 1, &mut output), (2, 1));
        assert_eq!(output, vec![10, 11, 20, 21]);

        drain.push_ready(slab(3, &[2], &[40, 41]));
        assert_eq!(drain.drain_into(8, 1, &mut output), (4, 2));
        assert_eq!(output, vec![10, 11, 20, 21, 30, 31, 40, 41]);
    }

    #[test]
    fn preserves_partial_drain_frame_accounting() {
        let mut drain = OrderedSlabDrain::new();
        let mut output = Vec::new();

        drain.push_ready(slab(0, &[3, 3], &[1, 2, 3, 4, 5, 6]));

        assert_eq!(drain.drain_into(2, 1, &mut output), (2, 0));
        assert_eq!(drain.completed_input_frames(), 0);

        assert_eq!(drain.drain_into(4, 1, &mut output), (4, 2));
        assert_eq!(drain.completed_input_frames(), 2);
        assert!(drain.is_idle());
    }
}
