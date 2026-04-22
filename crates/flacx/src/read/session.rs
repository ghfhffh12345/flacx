use std::{
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
};

use super::{
    DECODE_SESSION_QUEUE_DEPTH_MULTIPLIER, Error, Result, frame, profile,
    slab::{DecodeSlabPlan, DecodedSlab, OrderedSlabDrain},
};

#[derive(Debug, PartialEq, Eq)]
pub(super) struct DecodeSessionResult {
    pub(super) slab: DecodedSlab,
    pub(super) producer_window_slabs: usize,
}

impl DecodeSessionResult {
    fn from_slab(slab: DecodedSlab, producer_window_slabs: usize) -> Self {
        Self {
            slab,
            producer_window_slabs,
        }
    }

    fn from_packet(packet: frame::DecodedWorkPacket, producer_window_slabs: usize) -> Self {
        Self::from_slab(packet.into(), producer_window_slabs)
    }
}

#[derive(Debug)]
pub(super) struct StreamingDecodeSession {
    job_sender: Option<SyncSender<frame::DecodeWorkSlab>>,
    result_receiver: Receiver<Result<DecodeSessionResult>>,
    ordered_drain: OrderedSlabDrain,
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
            ordered_drain: OrderedSlabDrain::new(),
            coordinator_handle: None,
            coordinator_finished: false,
        }
    }

    pub(super) fn spawn(worker_count: usize, queue_depth: usize) -> Self {
        let (job_sender, job_receiver) = mpsc::sync_channel(queue_depth.max(1));
        let (result_sender, result_receiver) = mpsc::sync_channel(queue_depth.max(1));
        let coordinator_handle = thread::spawn(move || {
            let _ = run_decode_coordinator(job_receiver, result_sender, worker_count, queue_depth);
        });

        Self {
            job_sender: Some(job_sender),
            result_receiver,
            ordered_drain: OrderedSlabDrain::new(),
            coordinator_handle: Some(coordinator_handle),
            coordinator_finished: false,
        }
    }

    pub(super) fn submit(&self, plan: DecodeSlabPlan) -> Result<()> {
        self.job_sender
            .as_ref()
            .expect("streaming decode session always owns a job sender while active")
            .send(plan.into())
            .map_err(|_| Error::Thread("decode session job channel closed unexpectedly".into()))
    }

    pub(super) fn collect_ready_slabs(&mut self) -> Result<usize> {
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

    pub(super) fn collect_ready_packets(&mut self) -> Result<usize> {
        self.collect_ready_slabs()
    }

    pub(super) fn wait_for_ready_slab(&mut self) -> Result<bool> {
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

    pub(super) fn wait_for_ready_packet(&mut self) -> Result<bool> {
        self.wait_for_ready_slab()
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

    pub(super) fn ready_slab_count(&self) -> usize {
        self.ordered_drain.ready_slab_count()
    }

    pub(super) fn ready_packet_count(&self) -> usize {
        self.ready_slab_count()
    }

    pub(super) fn next_ready_slab_start_frame(&self) -> usize {
        self.ordered_drain.next_ready_slab_start_frame()
    }

    pub(super) fn next_ready_packet_start_frame(&self) -> usize {
        self.next_ready_slab_start_frame()
    }

    pub(super) fn has_draining_slab(&self) -> bool {
        self.ordered_drain.has_draining_slab()
    }

    pub(super) fn has_draining_packet(&self) -> bool {
        self.has_draining_slab()
    }

    pub(super) fn active_slab_count(&self) -> usize {
        self.ordered_drain.active_slab_count()
    }

    pub(super) fn active_packet_count(&self) -> usize {
        self.active_slab_count()
    }

    pub(super) fn is_idle(&self) -> bool {
        self.coordinator_finished && self.ordered_drain.is_idle()
    }

    #[allow(dead_code)]
    pub(super) fn accept_ready_slab(&mut self, slab: DecodedSlab, producer_window_slabs: usize) {
        self.accept_result(DecodeSessionResult::from_slab(slab, producer_window_slabs));
    }

    pub(super) fn accept_ready_packet(
        &mut self,
        packet: frame::DecodedWorkPacket,
        producer_window_slabs: usize,
    ) {
        self.accept_result(DecodeSessionResult::from_packet(packet, producer_window_slabs));
    }

    fn accept_result(&mut self, result: DecodeSessionResult) {
        let pcm_frames = result.slab.pcm_frames();
        self.ordered_drain.push_ready(result.slab);
        profile::accept_ready_pcm_frames_for_current_thread(
            pcm_frames,
            self.active_slab_count().max(result.producer_window_slabs),
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
    job_receiver: Receiver<frame::DecodeWorkSlab>,
    result_sender: SyncSender<Result<DecodeSessionResult>>,
    worker_count: usize,
    queue_depth: usize,
) -> Result<()> {
    let window_limit = queue_depth.max(1);
    let mut decoder_pool = (worker_count > 1).then(|| {
        frame::FrameDecodeWorkerPool::new(
            worker_count,
            DECODE_SESSION_QUEUE_DEPTH_MULTIPLIER.min(window_limit),
        )
    });
    let mut producer_window_slabs = 0usize;

    loop {
        if let Some(pool) = decoder_pool.as_mut() {
            match pool.try_recv() {
                frame::DecodeWorkerRecv::Slab(slab) => {
                    producer_window_slabs = producer_window_slabs.saturating_sub(1);
                    if !send_ready_slab(&result_sender, slab?.into(), producer_window_slabs)? {
                        return Ok(());
                    }
                    continue;
                }
                frame::DecodeWorkerRecv::Empty => {}
            }

            if producer_window_slabs < window_limit {
                match job_receiver.try_recv() {
                    Ok(job) => match pool.try_submit(job) {
                        Ok(()) => {
                            producer_window_slabs += 1;
                            continue;
                        }
                        Err(mpsc::TrySendError::Full(job)) => {
                            if producer_window_slabs > 0 {
                                producer_window_slabs = producer_window_slabs.saturating_sub(1);
                                if !send_ready_slab(
                                    &result_sender,
                                    pool.recv()?.into(),
                                    producer_window_slabs,
                                )? {
                                    return Ok(());
                                }
                                pool.submit(job)?;
                                producer_window_slabs += 1;
                                continue;
                            }
                            pool.submit(job)?;
                            producer_window_slabs += 1;
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
                        if producer_window_slabs == 0 {
                            return Ok(());
                        }
                    }
                }
            }

            if producer_window_slabs > 0 {
                producer_window_slabs = producer_window_slabs.saturating_sub(1);
                if !send_ready_slab(
                    &result_sender,
                    pool.recv()?.into(),
                    producer_window_slabs,
                )? {
                    return Ok(());
                }
                continue;
            }
        } else {
            match job_receiver.recv() {
                Ok(job) => {
                    if !send_ready_slab(&result_sender, frame::decode_work_slab(job)?.into(), 0)? {
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
                producer_window_slabs += 1;
            }
            Err(_) => return Ok(()),
        }
    }
}

fn send_ready_slab(
    result_sender: &SyncSender<Result<DecodeSessionResult>>,
    slab: DecodedSlab,
    producer_window_slabs: usize,
) -> Result<bool> {
    let result = DecodeSessionResult::from_slab(slab, producer_window_slabs.saturating_add(1));
    match result_sender.send(Ok(result)) {
        Ok(()) => Ok(true),
        Err(_) => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::{Arc, mpsc}, time::Duration};

    use super::{DecodeSessionResult, StreamingDecodeSession};
    use crate::read::{FrameIndex, slab::{DecodeSlabPlan, DecodedSlab}};

    fn slab(start_frame_index: usize, block_sizes: &[u16], samples: &[i32]) -> DecodedSlab {
        DecodedSlab {
            start_frame_index,
            frame_block_sizes: block_sizes.to_vec(),
            decoded_samples: samples.to_vec(),
        }
    }

    #[test]
    fn streaming_session_drains_background_packets_in_frame_order() {
        let (sender, receiver) = mpsc::sync_channel(4);
        let mut session = StreamingDecodeSession::from_result_receiver(receiver);
        let mut output = Vec::new();

        sender
            .send(Ok(DecodeSessionResult {
                slab: slab(2, &[2], &[30, 31]),
                producer_window_slabs: 1,
            }))
            .unwrap();
        sender
            .send(Ok(DecodeSessionResult {
                slab: slab(0, &[2, 2], &[10, 11, 20, 21]),
                producer_window_slabs: 2,
            }))
            .unwrap();
        drop(sender);

        session.collect_ready_slabs().unwrap();
        let (drained_frames, _) = session.drain_into(8, 1, &mut output);
        assert_eq!(drained_frames, 6);
        assert_eq!(output, vec![10, 11, 20, 21, 30, 31]);
        assert_eq!(session.completed_input_frames(), 3);
        assert!(session.is_idle());
    }

    #[test]
    fn streaming_session_holds_out_of_order_slabs_until_the_gap_closes() {
        let (sender, receiver) = mpsc::sync_channel(4);
        let mut session = StreamingDecodeSession::from_result_receiver(receiver);
        let mut output = Vec::new();

        sender
            .send(Ok(DecodeSessionResult {
                slab: slab(2, &[2], &[30, 31]),
                producer_window_slabs: 2,
            }))
            .unwrap();

        assert_eq!(session.collect_ready_slabs().unwrap(), 1);
        assert_eq!(session.ready_slab_count(), 1);
        assert_eq!(session.next_ready_slab_start_frame(), 0);
        assert_eq!(session.active_slab_count(), 1);
        assert_eq!(session.drain_into(8, 1, &mut output), (0, 0));
        assert!(output.is_empty());

        sender
            .send(Ok(DecodeSessionResult {
                slab: slab(0, &[2, 2], &[10, 11, 20, 21]),
                producer_window_slabs: 1,
            }))
            .unwrap();
        drop(sender);

        assert_eq!(session.collect_ready_slabs().unwrap(), 1);
        assert_eq!(session.ready_slab_count(), 2);
        assert_eq!(session.active_slab_count(), 2);
        assert_eq!(session.drain_into(8, 1, &mut output), (6, 3));
        assert_eq!(output, vec![10, 11, 20, 21, 30, 31]);
    }

    #[test]
    fn coordinator_reports_a_bounded_active_window_for_ready_slabs() {
        let session = StreamingDecodeSession::spawn(2, 2);
        let submitter = std::thread::spawn({
            let job_sender = session
                .job_sender
                .as_ref()
                .expect("spawned session owns a job sender")
                .clone();
            move || {
                for sequence in 0..6 {
                    job_sender.send(test_plan(sequence).into()).unwrap();
                }
            }
        });

        std::thread::sleep(Duration::from_millis(50));

        let mut active_counts = Vec::new();
        for _ in 0..6 {
            match session
                .result_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap()
            {
                DecodeSessionResult {
                    producer_window_slabs,
                    ..
                } => active_counts.push(producer_window_slabs),
            }
        }
        submitter.join().unwrap();

        assert!(
            active_counts.iter().all(|&count| count <= 2),
            "expected active window to stay within queue depth, got {active_counts:?}"
        );
    }

    fn test_plan(sequence: usize) -> DecodeSlabPlan {
        DecodeSlabPlan {
            sequence,
            start_frame_index: sequence,
            frame_block_sizes: vec![1],
            bytes: Arc::from(Vec::<u8>::new()),
            frames: Arc::<[FrameIndex]>::from(Vec::new()),
        }
    }
}
