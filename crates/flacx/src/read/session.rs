use std::{
    collections::BTreeMap,
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
};

use super::{DECODE_SESSION_QUEUE_DEPTH_MULTIPLIER, Error, Result, frame, profile};

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

    fn pcm_frames(&self) -> usize {
        self.frame_block_sizes
            .iter()
            .map(|&block_size| usize::from(block_size))
            .sum()
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
pub(super) struct DecodeSessionResult {
    pub(super) packet: DecodedPacket,
    pub(super) producer_active_packets: usize,
}

impl DecodeSessionResult {
    fn from_packet(packet: frame::DecodedWorkPacket, producer_active_packets: usize) -> Self {
        Self {
            packet: packet.into(),
            producer_active_packets,
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

#[derive(Debug)]
pub(super) struct StreamingDecodeSession {
    job_sender: Option<SyncSender<frame::DecodeWorkPacket>>,
    result_receiver: Receiver<Result<DecodeSessionResult>>,
    ordered_drain: OrderedDrainState,
    coordinator_handle: Option<thread::JoinHandle<()>>,
    coordinator_finished: bool,
}

impl StreamingDecodeSession {
    pub(super) fn new_local() -> Self {
        let (_sender, result_receiver) = mpsc::sync_channel(1);
        Self::from_result_receiver(result_receiver)
    }

    pub(super) fn from_result_receiver(
        result_receiver: Receiver<Result<DecodeSessionResult>>,
    ) -> Self {
        Self {
            job_sender: None,
            result_receiver,
            ordered_drain: OrderedDrainState::new(),
            coordinator_handle: None,
            coordinator_finished: false,
        }
    }

    pub(super) fn spawn(worker_count: usize, queue_depth: usize) -> Self {
        let (job_sender, job_receiver) = mpsc::sync_channel(queue_depth.max(1));
        let (result_sender, result_receiver) = mpsc::sync_channel(queue_depth.max(1));
        let coordinator_handle = thread::spawn(move || {
            let _ = run_decode_coordinator(job_receiver, result_sender, worker_count);
        });

        Self {
            job_sender: Some(job_sender),
            result_receiver,
            ordered_drain: OrderedDrainState::new(),
            coordinator_handle: Some(coordinator_handle),
            coordinator_finished: false,
        }
    }

    pub(super) fn submit(&self, packet: frame::DecodeWorkPacket) -> Result<()> {
        self.job_sender
            .as_ref()
            .expect("streaming decode session always owns a job sender while active")
            .send(packet)
            .map_err(|_| Error::Thread("decode session job channel closed unexpectedly".into()))
    }

    pub(super) fn collect_ready_packets(&mut self) -> Result<usize> {
        let mut collected = 0usize;
        loop {
            match self.result_receiver.try_recv() {
                Ok(result) => {
                    self.accept_result(result?);
                    collected += 1;
                }
                Err(mpsc::TryRecvError::Empty) => return Ok(collected),
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.coordinator_finished = true;
                    return Ok(collected);
                }
            }
        }
    }

    pub(super) fn wait_for_ready_packet(&mut self) -> Result<bool> {
        match self.result_receiver.recv() {
            Ok(result) => {
                self.accept_result(result?);
                Ok(true)
            }
            Err(_) => {
                self.coordinator_finished = true;
                Ok(false)
            }
        }
    }

    pub(super) fn drain_into(
        &mut self,
        max_frames: usize,
        channels: usize,
        output: &mut Vec<i32>,
    ) -> (usize, usize) {
        self.ordered_drain.drain_into(max_frames, channels, output)
    }

    pub(super) fn completed_input_frames(&self) -> usize {
        self.ordered_drain.completed_input_frames()
    }

    pub(super) fn ready_packet_count(&self) -> usize {
        self.ordered_drain.ready_packet_count()
    }

    pub(super) fn next_ready_packet_start_frame(&self) -> usize {
        self.ordered_drain.next_ready_packet_start_frame()
    }

    pub(super) fn has_draining_packet(&self) -> bool {
        self.ordered_drain.has_draining_packet()
    }

    pub(super) fn active_packet_count(&self) -> usize {
        self.ordered_drain.active_packet_count()
    }

    pub(super) fn is_idle(&self) -> bool {
        self.coordinator_finished && self.ordered_drain.is_idle()
    }

    pub(super) fn accept_ready_packet(
        &mut self,
        packet: frame::DecodedWorkPacket,
        producer_active_packets: usize,
    ) {
        self.accept_result(DecodeSessionResult::from_packet(
            packet,
            producer_active_packets,
        ));
    }

    fn accept_result(&mut self, result: DecodeSessionResult) {
        let pcm_frames = result.packet.pcm_frames();
        self.ordered_drain.push_ready(result.packet);
        profile::accept_ready_pcm_frames_for_current_thread(
            pcm_frames,
            self.active_packet_count()
                .max(result.producer_active_packets),
        );
    }
}

impl Drop for StreamingDecodeSession {
    fn drop(&mut self) {
        self.job_sender.take();
        self.coordinator_finished = true;
        if let Some(handle) = self.coordinator_handle.take() {
            let _ = handle.join();
        }
    }
}

fn run_decode_coordinator(
    job_receiver: Receiver<frame::DecodeWorkPacket>,
    result_sender: SyncSender<Result<DecodeSessionResult>>,
    worker_count: usize,
) -> Result<()> {
    let mut decoder_pool = (worker_count > 1).then(|| {
        frame::FrameDecodeWorkerPool::new(worker_count, DECODE_SESSION_QUEUE_DEPTH_MULTIPLIER)
    });
    let mut inflight_packets = 0usize;

    loop {
        if let Some(pool) = decoder_pool.as_mut() {
            match pool.try_recv() {
                frame::DecodeWorkerRecv::Packet(packet) => {
                    inflight_packets = inflight_packets.saturating_sub(1);
                    if !send_ready_packet(&result_sender, packet?, inflight_packets)? {
                        return Ok(());
                    }
                    continue;
                }
                frame::DecodeWorkerRecv::Empty => {}
            }

            match job_receiver.try_recv() {
                Ok(job) => match pool.try_submit(job) {
                    Ok(()) => {
                        inflight_packets += 1;
                        continue;
                    }
                    Err(mpsc::TrySendError::Full(job)) => {
                        if inflight_packets > 0 {
                            inflight_packets = inflight_packets.saturating_sub(1);
                            if !send_ready_packet(&result_sender, pool.recv()?, inflight_packets)? {
                                return Ok(());
                            }
                            pool.submit(job)?;
                            inflight_packets += 1;
                            continue;
                        }
                        pool.submit(job)?;
                        inflight_packets += 1;
                        continue;
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        return Err(Error::Thread(
                            "decode worker channel closed unexpectedly".into(),
                        ));
                    }
                },
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    if inflight_packets == 0 {
                        return Ok(());
                    }
                }
            }

            if inflight_packets > 0 {
                inflight_packets = inflight_packets.saturating_sub(1);
                if !send_ready_packet(&result_sender, pool.recv()?, inflight_packets)? {
                    return Ok(());
                }
                continue;
            }
        } else {
            match job_receiver.recv() {
                Ok(job) => {
                    if !send_ready_packet(&result_sender, frame::decode_work_packet(job)?, 0)? {
                        return Ok(());
                    }
                    continue;
                }
                Err(_) => return Ok(()),
            }
        }

        match job_receiver.recv() {
            Ok(job) => {
                let pool = decoder_pool
                    .as_mut()
                    .expect("worker pool is present for multithreaded coordination");
                pool.submit(job)?;
                inflight_packets += 1;
            }
            Err(_) => return Ok(()),
        }
    }
}

fn send_ready_packet(
    result_sender: &SyncSender<Result<DecodeSessionResult>>,
    packet: frame::DecodedWorkPacket,
    inflight_packets: usize,
) -> Result<bool> {
    let result = DecodeSessionResult::from_packet(packet, inflight_packets.saturating_add(1));
    match result_sender.send(Ok(result)) {
        Ok(()) => Ok(true),
        Err(_) => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::{DecodeSessionResult, DecodedPacket, OrderedDrainState, StreamingDecodeSession};

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

    #[test]
    fn streaming_session_drains_background_packets_in_frame_order() {
        let (sender, receiver) = mpsc::sync_channel(4);
        let mut session = StreamingDecodeSession::from_result_receiver(receiver);
        let mut output = Vec::new();

        sender
            .send(Ok(DecodeSessionResult {
                packet: packet(2, &[2], &[30, 31]),
                producer_active_packets: 1,
            }))
            .unwrap();
        sender
            .send(Ok(DecodeSessionResult {
                packet: packet(0, &[2, 2], &[10, 11, 20, 21]),
                producer_active_packets: 2,
            }))
            .unwrap();
        drop(sender);

        session.collect_ready_packets().unwrap();
        let (drained_frames, _) = session.drain_into(8, 1, &mut output);
        assert_eq!(drained_frames, 6);
        assert_eq!(output, vec![10, 11, 20, 21, 30, 31]);
        assert_eq!(session.completed_input_frames(), 3);
        assert!(session.is_idle());
    }
}
