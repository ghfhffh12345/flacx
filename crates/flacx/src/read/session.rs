use std::collections::BTreeMap;

use super::frame;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct DecodedPacket {
    pub(super) start_frame_index: usize,
    pub(super) frame_block_sizes: Vec<u16>,
    pub(super) decoded_samples: Vec<i32>,
}

impl DecodedPacket {
    fn frame_count(&self) -> usize {
        self.frame_block_sizes.len()
    }
}

impl From<frame::DecodedWorkPacket> for DecodedPacket {
    fn from(packet: frame::DecodedWorkPacket) -> Self {
        Self {
            start_frame_index: packet.start_frame_index,
            frame_block_sizes: packet.frame_block_sizes,
            decoded_samples: packet.decoded_samples,
        }
    }
}

#[derive(Debug)]
struct DrainingPacket {
    frame_block_sizes: Vec<u16>,
    decoded_samples: Vec<i32>,
    sample_cursor: usize,
    drained_input_frames: usize,
    drained_pcm_frames: usize,
}

impl DrainingPacket {
    fn new(packet: DecodedPacket) -> Self {
        Self {
            frame_block_sizes: packet.frame_block_sizes,
            decoded_samples: packet.decoded_samples,
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
        let available_frames = (self.decoded_samples.len() - self.sample_cursor) / channels;
        let drained_frames = available_frames.min(max_frames);
        if drained_frames == 0 {
            return (0, 0);
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
        self.sample_cursor == self.decoded_samples.len()
    }
}

#[derive(Debug, Default)]
pub(super) struct OrderedDrainState {
    ready_packets: BTreeMap<usize, DecodedPacket>,
    next_ready_packet_start_frame: usize,
    draining_packet: Option<DrainingPacket>,
    completed_input_frames: usize,
}

impl OrderedDrainState {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn push_ready<P>(&mut self, packet: P)
    where
        P: Into<DecodedPacket>,
    {
        let packet = packet.into();
        self.ready_packets.insert(packet.start_frame_index, packet);
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

        let can_span_packet_boundaries = self.draining_packet.is_none();
        let mut total_drained_frames = 0usize;
        let mut total_completed_input_frames = 0usize;
        while total_drained_frames < max_frames && self.activate_next_ready_packet() {
            let (drained_frames, completed_input_frames, packet_finished) = {
                let packet = self
                    .draining_packet
                    .as_mut()
                    .expect("draining packet is present after activation");
                let (drained_frames, completed_input_frames) =
                    packet.drain_into(max_frames - total_drained_frames, channels, output);
                (
                    drained_frames,
                    completed_input_frames,
                    packet.is_fully_drained(),
                )
            };

            total_drained_frames += drained_frames;
            total_completed_input_frames += completed_input_frames;
            self.completed_input_frames += completed_input_frames;

            if packet_finished {
                self.draining_packet = None;
                if !can_span_packet_boundaries {
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

    pub(super) fn ready_packet_count(&self) -> usize {
        self.ready_packets.len()
    }

    pub(super) fn next_ready_packet_start_frame(&self) -> usize {
        self.next_ready_packet_start_frame
    }

    pub(super) fn has_draining_packet(&self) -> bool {
        self.draining_packet.is_some()
    }

    pub(super) fn active_packet_count(&self) -> usize {
        self.ready_packets.len() + usize::from(self.draining_packet.is_some())
    }

    pub(super) fn is_idle(&self) -> bool {
        self.ready_packets.is_empty() && self.draining_packet.is_none()
    }

    pub(super) fn mark_fully_drained(&mut self, completed_input_frames: usize) {
        self.ready_packets.clear();
        self.draining_packet = None;
        self.next_ready_packet_start_frame = completed_input_frames;
        self.completed_input_frames = completed_input_frames;
    }

    fn activate_next_ready_packet(&mut self) -> bool {
        if self.draining_packet.is_some() {
            return true;
        }

        let Some(packet) = self
            .ready_packets
            .remove(&self.next_ready_packet_start_frame)
        else {
            return false;
        };

        self.next_ready_packet_start_frame += packet.frame_count();
        self.draining_packet = Some(DrainingPacket::new(packet));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{DecodedPacket, OrderedDrainState};

    fn packet(start_frame_index: usize, block_sizes: &[u16], samples: &[i32]) -> DecodedPacket {
        DecodedPacket {
            start_frame_index,
            frame_block_sizes: block_sizes.to_vec(),
            decoded_samples: samples.to_vec(),
        }
    }

    #[test]
    fn drains_packets_in_frame_order_even_when_completion_is_out_of_order() {
        let mut drain = OrderedDrainState::new();
        let mut output = Vec::new();

        drain.push_ready(packet(2, &[2], &[30, 31]));
        drain.push_ready(packet(0, &[2, 2], &[10, 11, 20, 21]));

        assert_eq!(drain.drain_into(2, 1, &mut output), (2, 1));
        assert_eq!(output, vec![10, 11]);

        assert_eq!(drain.drain_into(4, 1, &mut output), (2, 1));
        assert_eq!(output, vec![10, 11, 20, 21]);

        drain.push_ready(packet(3, &[2], &[40, 41]));
        assert_eq!(drain.drain_into(8, 1, &mut output), (4, 2));
        assert_eq!(output, vec![10, 11, 20, 21, 30, 31, 40, 41]);
    }

    #[test]
    fn supports_partial_packet_drain_without_losing_frame_accounting() {
        let mut drain = OrderedDrainState::new();
        let mut output = Vec::new();

        drain.push_ready(packet(0, &[3, 3], &[1, 2, 3, 4, 5, 6]));

        assert_eq!(drain.drain_into(2, 1, &mut output), (2, 0));
        assert_eq!(drain.completed_input_frames(), 0);

        assert_eq!(drain.drain_into(4, 1, &mut output), (4, 2));
        assert_eq!(drain.completed_input_frames(), 2);
        assert!(drain.is_idle());
    }
}
