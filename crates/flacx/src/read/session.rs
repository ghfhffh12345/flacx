use std::{
    collections::VecDeque,
    sync::{
        mpsc::{self, Receiver, SyncSender},
        Arc, Condvar, Mutex,
    },
    thread,
};

use super::{
    frame,
    producer::ProducerState,
    profile,
    slab::{DecodeSlabPlan, DecodedSlab, OrderedSlabDrain},
    Error, Result, DECODE_SESSION_QUEUE_DEPTH_MULTIPLIER,
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

#[derive(Debug, Default)]
struct ProducerWakeState {
    active_window_slabs: usize,
    submitted_input_bytes: VecDeque<usize>,
    staged_input_bytes: usize,
    cancelled: bool,
}

pub(super) struct SessionProducer {
    job_sender: SyncSender<frame::DecodeWorkSlab>,
    producer_wakeup: Arc<(Mutex<ProducerWakeState>, Condvar)>,
    window_depth_limit: usize,
}

impl SessionProducer {
    pub(super) fn submit(&self, plan: DecodeSlabPlan) -> Result<bool> {
        self.submit_with_on_success(plan, || {})
    }

    fn submit_with_on_success<F>(&self, plan: DecodeSlabPlan, on_success: F) -> Result<bool>
    where
        F: FnOnce(),
    {
        let input_bytes = plan.bytes.len();
        let (lock, condvar) = &*self.producer_wakeup;
        let mut state = lock.lock().unwrap();
        while state.active_window_slabs >= self.window_depth_limit && !state.cancelled {
            state = condvar.wait(state).unwrap();
        }
        if state.cancelled {
            return Ok(false);
        }
        state.active_window_slabs += 1;
        drop(state);

        match self.job_sender.send(plan.into()) {
            Ok(()) => {
                let mut state = lock.lock().unwrap();
                state.submitted_input_bytes.push_back(input_bytes);
                state.staged_input_bytes = state.staged_input_bytes.saturating_add(input_bytes);
                profile::observe_staged_input_bytes_for_current_thread(state.staged_input_bytes);
                drop(state);
                on_success();
                Ok(true)
            }
            Err(_) => {
                let mut state = lock.lock().unwrap();
                state.active_window_slabs = state.active_window_slabs.saturating_sub(1);
                condvar.notify_one();
                Err(Error::Thread(
                    "decode session job channel closed unexpectedly".into(),
                ))
            }
        }
    }

    pub(super) fn submit_tracked(
        &self,
        producer_state: &mut ProducerState,
        plan: DecodeSlabPlan,
    ) -> Result<bool> {
        let (end_frame_index, pcm_frames, input_bytes) =
            ProducerState::submitted_slab_residency(&plan);
        self.submit_with_on_success(plan, || {
            producer_state.record_ready_submission_parts_with_input_bytes(
                end_frame_index,
                pcm_frames,
                input_bytes,
            );
        })
    }
}

#[derive(Debug)]
pub(super) struct StreamingDecodeSession {
    submit_sender: Option<SyncSender<DecodeSlabPlan>>,
    job_sender: Option<SyncSender<frame::DecodeWorkSlab>>,
    result_receiver: Receiver<Result<DecodeSessionResult>>,
    ordered_drain: OrderedSlabDrain,
    coordinator_handle: Option<thread::JoinHandle<()>>,
    producer_handle: Option<thread::JoinHandle<()>>,
    producer_wakeup: Arc<(Mutex<ProducerWakeState>, Condvar)>,
    producer_finished: bool,
    coordinator_finished: bool,
    window_depth_limit: usize,
}

impl StreamingDecodeSession {
    pub(super) fn new_local() -> Self {
        let (_sender, result_receiver) = mpsc::sync_channel(1);
        Self::from_result_receiver_with_window_depth(result_receiver, 1)
    }

    pub(super) fn from_result_receiver(
        result_receiver: Receiver<Result<DecodeSessionResult>>,
    ) -> Self {
        Self::from_result_receiver_with_window_depth(result_receiver, 1)
    }

    fn from_result_receiver_with_window_depth(
        result_receiver: Receiver<Result<DecodeSessionResult>>,
        window_depth_limit: usize,
    ) -> Self {
        Self {
            submit_sender: None,
            job_sender: None,
            result_receiver,
            ordered_drain: OrderedSlabDrain::new(),
            coordinator_handle: None,
            producer_handle: None,
            producer_wakeup: Arc::new((Mutex::new(ProducerWakeState::default()), Condvar::new())),
            producer_finished: true,
            coordinator_finished: false,
            window_depth_limit: window_depth_limit.max(1),
        }
    }

    fn spawn_runtime(
        worker_count: usize,
        queue_depth: usize,
    ) -> (Self, SyncSender<Result<DecodeSessionResult>>) {
        let window_depth_limit = queue_depth.max(1);
        let (job_sender, job_receiver) = mpsc::sync_channel(window_depth_limit);
        let (result_sender, result_receiver) = mpsc::sync_channel(window_depth_limit);
        let coordinator_sender = result_sender.clone();
        let coordinator_result_sender = result_sender.clone();
        let coordinator_handle = thread::spawn(move || {
            if let Err(error) =
                run_decode_coordinator(job_receiver, coordinator_sender, worker_count, queue_depth)
            {
                let _ = coordinator_result_sender.send(Err(error));
            }
        });

        (
            Self {
                submit_sender: None,
                job_sender: Some(job_sender),
                result_receiver,
                ordered_drain: OrderedSlabDrain::new(),
                coordinator_handle: Some(coordinator_handle),
                producer_handle: None,
                producer_wakeup: Arc::new((
                    Mutex::new(ProducerWakeState::default()),
                    Condvar::new(),
                )),
                producer_finished: true,
                coordinator_finished: false,
                window_depth_limit,
            },
            result_sender,
        )
    }

    pub(super) fn spawn(worker_count: usize, queue_depth: usize) -> Self {
        let (submit_sender, submit_receiver) = mpsc::sync_channel(0);
        let mut session = Self::spawn_with_producer(worker_count, queue_depth, move |producer| {
            while let Ok(plan) = submit_receiver.recv() {
                if !producer.submit(plan)? {
                    break;
                }
            }
            Ok(())
        });
        session.submit_sender = Some(submit_sender);
        session
    }

    pub(super) fn spawn_with_producer<P>(
        worker_count: usize,
        queue_depth: usize,
        run_producer: P,
    ) -> Self
    where
        P: FnOnce(SessionProducer) -> Result<()> + Send + 'static,
    {
        let window_depth_limit = queue_depth.max(1);
        let (mut session, result_sender) = Self::spawn_runtime(worker_count, queue_depth);
        let job_sender = session
            .job_sender
            .as_ref()
            .expect("spawned session owns a job sender")
            .clone();
        let producer_wakeup = Arc::clone(&session.producer_wakeup);
        let profile_session = profile::clone_decode_profile_session_for_current_thread();
        session.producer_finished = false;
        session.producer_handle = Some(thread::spawn(move || {
            profile::attach_decode_profile_session_to_current_thread(profile_session);
            let result = run_producer(SessionProducer {
                job_sender,
                producer_wakeup: Arc::clone(&producer_wakeup),
                window_depth_limit,
            });
            let (lock, _) = &*producer_wakeup;
            let cancelled = lock.lock().unwrap().cancelled;
            if let Err(error) = result {
                if !cancelled {
                    let _ = result_sender.send(Err(error));
                }
            }
            profile::attach_decode_profile_session_to_current_thread(None);
        }));
        session
    }

    pub(super) fn submit(&self, plan: DecodeSlabPlan) -> Result<()> {
        if let Some(submit_sender) = self.submit_sender.as_ref() {
            return submit_sender.send(plan).map_err(|_| {
                Error::Thread("decode session job channel closed unexpectedly".into())
            });
        }

        self.job_sender
            .as_ref()
            .expect("streaming decode session always owns a job sender while active")
            .send(plan.into())
            .map_err(|_| Error::Thread("decode session job channel closed unexpectedly".into()))
    }

    pub(super) fn collect_ready_slabs(&mut self) -> Result<usize> {
        self.refresh_producer_finished();
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
        self.refresh_producer_finished();
        if self.background_work_is_exhausted() {
            return Ok(false);
        }
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
        self.refresh_producer_finished();
        let active_before = self.active_slab_count();
        let drained = self.ordered_drain.drain_into(max_frames, channels, output);
        let active_after = self.active_slab_count();
        if active_after < active_before {
            self.release_producer_capacity(active_before - active_after);
        }
        drained
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
        if self.has_background_runtime() {
            return self.background_work_is_exhausted();
        }

        (self.producer_finished
            || self
                .producer_handle
                .as_ref()
                .is_none_or(thread::JoinHandle::is_finished))
            && self.coordinator_finished
            && self.ordered_drain.is_idle()
    }

    pub(super) fn producer_has_capacity(&self) -> bool {
        if self.has_background_runtime() {
            return self.outstanding_window_slabs() < self.window_depth_limit();
        }
        self.active_slab_count() < self.window_depth_limit()
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
        self.accept_result(DecodeSessionResult::from_packet(
            packet,
            producer_window_slabs,
        ));
    }

    fn accept_result(&mut self, result: DecodeSessionResult) {
        let pcm_frames = result.slab.pcm_frames();
        self.ordered_drain.push_ready(result.slab);
        profile::accept_ready_pcm_frames_for_current_thread(
            pcm_frames,
            self.active_slab_count().max(result.producer_window_slabs),
        );
    }

    fn window_depth_limit(&self) -> usize {
        self.window_depth_limit
    }

    fn release_producer_capacity(&self, completed_slabs: usize) {
        let (lock, condvar) = &*self.producer_wakeup;
        let mut state = lock.lock().unwrap();
        state.active_window_slabs = state.active_window_slabs.saturating_sub(completed_slabs);
        for _ in 0..completed_slabs {
            let Some(input_bytes) = state.submitted_input_bytes.pop_front() else {
                break;
            };
            state.staged_input_bytes = state.staged_input_bytes.saturating_sub(input_bytes);
        }
        profile::observe_staged_input_bytes_for_current_thread(state.staged_input_bytes);
        condvar.notify_all();
    }

    fn has_background_runtime(&self) -> bool {
        self.job_sender.is_some() || self.producer_handle.is_some()
    }

    fn outstanding_window_slabs(&self) -> usize {
        self.producer_wakeup.0.lock().unwrap().active_window_slabs
    }

    fn background_work_is_exhausted(&self) -> bool {
        self.has_background_runtime()
            && self.outstanding_window_slabs() == 0
            && self.ordered_drain.is_idle()
    }

    fn cancel_producer(&self) {
        let (lock, condvar) = &*self.producer_wakeup;
        let mut state = lock.lock().unwrap();
        state.cancelled = true;
        condvar.notify_all();
    }

    fn refresh_producer_finished(&mut self) {
        if !self.producer_finished
            && self
                .producer_handle
                .as_ref()
                .is_some_and(thread::JoinHandle::is_finished)
        {
            self.producer_finished = true;
        }
    }
}

impl Drop for StreamingDecodeSession {
    fn drop(&mut self) {
        self.submit_sender.take();
        self.cancel_producer();
        self.job_sender.take();
        self.producer_finished = true;
        self.coordinator_finished = true;
        if let Some(handle) = self.producer_handle.take() {
            let _ = handle.join();
        }
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
                if !send_ready_slab(&result_sender, pool.recv()?.into(), producer_window_slabs)? {
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
    use std::{
        sync::{mpsc, Arc, Condvar, Mutex},
        time::Duration,
    };

    use super::{DecodeSessionResult, ProducerWakeState, SessionProducer, StreamingDecodeSession};
    use crate::read::{
        producer::{ProducerConfig, ProducerState},
        profile,
        slab::{DecodeSlabPlan, DecodedSlab},
        FrameIndex, StreamInfo,
    };

    fn slab(start_frame_index: usize, block_sizes: &[u16], samples: &[i32]) -> DecodedSlab {
        DecodedSlab {
            start_frame_index,
            frame_block_sizes: block_sizes.to_vec(),
            decoded_samples: samples.to_vec(),
        }
    }

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

    fn producer_config(max_slabs_ahead: usize) -> ProducerConfig {
        ProducerConfig {
            target_pcm_frames_per_slab: 16,
            max_frames_per_slab: 1,
            max_bytes_per_slab: 256,
            max_slabs_ahead,
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
    fn spawned_producer_reopens_when_ordered_completion_retires_a_slab() {
        let (plans, channels) = fixture_plans(3);
        let mut session = StreamingDecodeSession::spawn(2, 1);
        let submit_sender = session
            .submit_sender
            .as_ref()
            .expect("spawned session owns a producer submission channel")
            .clone();
        let submitted = mpsc::channel();
        let submitter = std::thread::spawn(move || {
            for (index, plan) in plans.into_iter().enumerate() {
                submit_sender.send(plan).unwrap();
                submitted.0.send(index).unwrap();
            }
        });

        assert_eq!(
            submitted
                .1
                .recv_timeout(Duration::from_secs(1))
                .expect("first submission should pass through immediately"),
            0
        );
        assert_eq!(
            submitted
                .1
                .recv_timeout(Duration::from_secs(1))
                .expect("second submission should be received before the producer blocks"),
            1
        );
        assert!(
            submitted
                .1
                .recv_timeout(Duration::from_millis(200))
                .is_err(),
            "third submission should stay blocked until ordered drain retires capacity"
        );

        assert!(session.wait_for_ready_slab().unwrap());
        assert_eq!(session.active_slab_count(), 1);
        assert!(!session.producer_has_capacity());

        let mut output = Vec::new();
        let (drained_frames, completed_input_frames) =
            session.drain_into(usize::MAX, channels, &mut output);
        assert!(drained_frames > 0);
        assert_eq!(completed_input_frames, 1);
        assert_eq!(session.completed_input_frames(), 1);
        assert!(session.producer_has_capacity());
        assert_eq!(
            submitted
                .1
                .recv_timeout(Duration::from_secs(1))
                .expect("producer should wake after ordered drain retires capacity"),
            2
        );

        submitter.join().unwrap();
    }

    #[test]
    fn spawned_session_reports_idle_after_real_work_is_drained() {
        let (plans, channels) = fixture_plans(1);
        let mut session = StreamingDecodeSession::spawn(2, 1);

        session.submit(plans.into_iter().next().unwrap()).unwrap();
        assert!(session.wait_for_ready_slab().unwrap());

        let mut output = Vec::new();
        assert!(session.drain_into(usize::MAX, channels, &mut output).0 > 0);
        assert!(session.is_idle());
    }

    #[test]
    fn wait_for_ready_slab_returns_false_without_waiting_for_producer_thread_exit() {
        let (plans, channels) = fixture_plans(1);
        let plan = plans.into_iter().next().unwrap();
        let release = mpsc::channel();
        let submitted = mpsc::channel();
        let mut session = StreamingDecodeSession::spawn_with_producer(1, 1, move |producer| {
            assert!(producer.submit(plan).unwrap());
            submitted.0.send(()).unwrap();
            release.1.recv().unwrap();
            Ok(())
        });

        submitted
            .1
            .recv_timeout(Duration::from_secs(1))
            .expect("producer should submit the first slab before exhaustion wait");
        assert!(session.wait_for_ready_slab().unwrap());
        let mut output = Vec::new();
        assert!(session.drain_into(usize::MAX, channels, &mut output).0 > 0);

        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            let _ = release.0.send(());
        });
        let start = std::time::Instant::now();
        let exhausted = session.wait_for_ready_slab().unwrap();
        let elapsed = start.elapsed();
        releaser.join().unwrap();

        assert!(!exhausted);
        assert!(
            elapsed < Duration::from_millis(100),
            "expected exhausted wait to return before producer thread exit, took {elapsed:?}"
        );
    }

    #[test]
    fn dropping_session_cancels_waiting_background_producer() {
        let finished_sender = mpsc::channel();
        let session = StreamingDecodeSession::spawn_with_producer(1, 1, move |producer| {
            assert!(producer.submit(test_plan(0)).unwrap());
            finished_sender.0.send(()).unwrap();
            assert!(!producer.submit(test_plan(1)).unwrap());
            Ok(())
        });

        finished_sender
            .1
            .recv_timeout(Duration::from_secs(1))
            .expect("producer should submit the first slab before cancellation");

        drop(session);
    }

    #[test]
    fn spawned_producer_tracks_staged_input_residency_in_decode_profile() {
        let (plans, channels) = fixture_plans(1);
        let plan = plans.into_iter().next().unwrap();
        let expected_staged_input_bytes = plan.bytes.len();
        let profile_path = std::env::temp_dir().join(format!(
            "flacx-session-profile-{}-{}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        profile::set_decode_profile_path_for_current_thread(Some(profile_path.clone()));
        profile::begin_decode_profile_session_for_current_thread(1, 1, 16);

        let mut session = StreamingDecodeSession::spawn(1, 1);
        session.submit(plan).unwrap();

        assert!(session.wait_for_ready_slab().unwrap());
        let mut output = Vec::new();
        let (drained_frames, _) = session.drain_into(usize::MAX, channels, &mut output);
        profile::hand_out_pcm_frames_for_current_thread(drained_frames);
        profile::release_decode_output_buffer_for_current_thread();
        profile::finish_successful_decode_profile_for_current_thread();

        let summary = std::fs::read_to_string(&profile_path).unwrap();
        assert!(
            summary.contains(&format!(
                "peak_staged_input_bytes={expected_staged_input_bytes}"
            )),
            "expected staged-input residency in summary: {summary}"
        );

        let _ = std::fs::remove_file(profile_path);
        profile::set_decode_profile_path_for_current_thread(None);
    }

    #[test]
    fn tracked_submit_does_not_record_state_when_cancelled_before_send() {
        let (job_sender, _job_receiver) = mpsc::sync_channel(1);
        let producer_wakeup = Arc::new((
            Mutex::new(ProducerWakeState {
                active_window_slabs: 0,
                submitted_input_bytes: Default::default(),
                staged_input_bytes: 0,
                cancelled: true,
            }),
            Condvar::new(),
        ));
        let producer = SessionProducer {
            job_sender,
            producer_wakeup,
            window_depth_limit: 1,
        };
        let mut state = ProducerState::new(stream_info(), producer_config(1));

        assert!(!producer
            .submit_tracked(&mut state, fixture_plans(1).0.into_iter().next().unwrap())
            .unwrap());
        assert_eq!(state.staged_input_bytes(), 0);
    }

    #[test]
    fn tracked_submit_does_not_record_state_when_send_fails() {
        let (job_sender, job_receiver) = mpsc::sync_channel(1);
        drop(job_receiver);
        let producer = SessionProducer {
            job_sender,
            producer_wakeup: Arc::new((Mutex::new(ProducerWakeState::default()), Condvar::new())),
            window_depth_limit: 1,
        };
        let mut state = ProducerState::new(stream_info(), producer_config(1));

        assert!(producer
            .submit_tracked(&mut state, fixture_plans(1).0.into_iter().next().unwrap())
            .is_err());
        assert_eq!(state.staged_input_bytes(), 0);
    }

    #[test]
    fn spawn_owns_a_background_producer_thread() {
        let session = StreamingDecodeSession::spawn(2, 1);

        assert!(session.producer_handle.is_some());
        assert!(!session.producer_finished);
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

    fn fixture_plans(count: usize) -> (Vec<DecodeSlabPlan>, usize) {
        let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-flacs/case1/test01.flac");
        let bytes = std::fs::read(fixture_path).unwrap();
        let (stream_info, _, frame_offset) =
            crate::read::metadata::parse_metadata(&bytes, false).unwrap();
        let plans = crate::read::frame::index_frames(&bytes, frame_offset, stream_info)
            .unwrap()
            .into_iter()
            .take(count)
            .enumerate()
            .map(|(sequence, frame)| {
                let frame_bytes = bytes[frame.offset..frame.offset + frame.bytes_consumed].to_vec();
                let frame = FrameIndex { offset: 0, ..frame };
                DecodeSlabPlan::new(sequence, sequence, vec![frame]).seal_bytes(frame_bytes)
            })
            .collect();
        (plans, usize::from(stream_info.channels))
    }
}
