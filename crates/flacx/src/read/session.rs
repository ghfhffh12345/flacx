use std::{collections::VecDeque, sync::mpsc};

use super::{
    DECODE_SESSION_QUEUE_DEPTH_MULTIPLIER, Error, Result, frame, profile,
    slab::{DecodeSlabPlan, DecodedSlab, OrderedDrainProgress, OrderedSlabDrain},
};

#[derive(Debug, PartialEq, Eq)]
pub(super) struct DecodeSessionResult {
    pub(super) slab: DecodedSlab,
    pub(super) active_window_slabs: usize,
}

impl DecodeSessionResult {
    fn from_slab(slab: DecodedSlab, active_window_slabs: usize) -> Self {
        Self {
            slab,
            active_window_slabs,
        }
    }
}

pub(super) struct StreamingDecodeSession {
    worker_pool: frame::FrameDecodeWorkerPool,
    ordered_drain: OrderedSlabDrain,
    outstanding_window_slabs: usize,
    result_channel_closed: bool,
    window_depth_limit: usize,
    worker_queue_saturated: bool,
    submitted_input_bytes: VecDeque<usize>,
    staged_input_bytes: usize,
    #[cfg(test)]
    worker_count: usize,
    #[cfg(test)]
    test_result_receiver: Option<mpsc::Receiver<Result<DecodeSessionResult>>>,
    #[cfg(test)]
    has_background_runtime: bool,
    #[cfg(test)]
    force_submit_failure: bool,
}

impl StreamingDecodeSession {
    #[cfg(test)]
    const LOCAL_RESULT_WINDOW_DEPTH: usize = 4;

    #[cfg(test)]
    pub(super) fn from_result_receiver(
        result_receiver: mpsc::Receiver<Result<DecodeSessionResult>>,
    ) -> Self {
        Self::from_result_receiver_with_window_depth(
            result_receiver,
            Self::LOCAL_RESULT_WINDOW_DEPTH,
        )
    }

    #[cfg(test)]
    fn from_result_receiver_with_window_depth(
        result_receiver: mpsc::Receiver<Result<DecodeSessionResult>>,
        window_depth_limit: usize,
    ) -> Self {
        let window_depth_limit = window_depth_limit.max(1);
        Self {
            worker_pool: frame::FrameDecodeWorkerPool::new(1, 1),
            ordered_drain: OrderedSlabDrain::with_window_capacity(window_depth_limit),
            outstanding_window_slabs: 0,
            result_channel_closed: false,
            window_depth_limit,
            worker_queue_saturated: false,
            submitted_input_bytes: VecDeque::new(),
            staged_input_bytes: 0,
            #[cfg(test)]
            worker_count: 0,
            #[cfg(test)]
            test_result_receiver: Some(result_receiver),
            #[cfg(test)]
            has_background_runtime: false,
            #[cfg(test)]
            force_submit_failure: false,
        }
    }

    pub(super) fn spawn(worker_count: usize, queue_depth: usize) -> Self {
        let window_depth_limit = queue_depth.max(1);
        let worker_count = worker_count.max(1);
        Self {
            worker_pool: frame::FrameDecodeWorkerPool::new(
                worker_count,
                DECODE_SESSION_QUEUE_DEPTH_MULTIPLIER.min(window_depth_limit),
            ),
            ordered_drain: OrderedSlabDrain::with_window_capacity(window_depth_limit),
            outstanding_window_slabs: 0,
            result_channel_closed: false,
            window_depth_limit,
            worker_queue_saturated: false,
            submitted_input_bytes: VecDeque::new(),
            staged_input_bytes: 0,
            #[cfg(test)]
            worker_count,
            #[cfg(test)]
            test_result_receiver: None,
            #[cfg(test)]
            has_background_runtime: true,
            #[cfg(test)]
            force_submit_failure: false,
        }
    }

    pub(super) fn submit(&mut self, plan: DecodeSlabPlan) -> Result<bool> {
        #[cfg(test)]
        if self.force_submit_failure {
            return Err(Error::Thread(
                "decode worker channel closed unexpectedly".into(),
            ));
        }

        let input_bytes = plan.bytes.len();
        match self.worker_pool.try_submit(plan.into()) {
            Ok(()) => {
                self.worker_queue_saturated = false;
            }
            Err(mpsc::TrySendError::Full(_)) => {
                self.worker_queue_saturated = true;
                return Ok(false);
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                return Err(Error::Thread(
                    "decode worker channel closed unexpectedly".into(),
                ));
            }
        }
        self.outstanding_window_slabs = self.outstanding_window_slabs.saturating_add(1);
        self.submitted_input_bytes.push_back(input_bytes);
        self.staged_input_bytes = self.staged_input_bytes.saturating_add(input_bytes);
        profile::observe_staged_input_bytes_for_current_thread(self.staged_input_bytes);
        Ok(true)
    }

    pub(super) fn collect_ready_slabs(&mut self) -> Result<usize> {
        let mut collected = 0usize;
        loop {
            match self.try_recv_result()? {
                Some(result) => {
                    self.accept_result(result);
                    collected += 1;
                }
                None => return Ok(collected),
            }
        }
    }

    pub(super) fn wait_for_ready_slab(&mut self) -> Result<bool> {
        if self.background_work_is_exhausted() {
            return Ok(false);
        }

        #[cfg(test)]
        if let Some(receiver) = self.test_result_receiver.as_ref() {
            return match receiver.recv() {
                Ok(result) => {
                    self.accept_result(result?);
                    Ok(true)
                }
                Err(_) => {
                    self.result_channel_closed = true;
                    Ok(false)
                }
            };
        }

        let result = self.worker_pool.recv()?;
        self.accept_result(self.ready_result_from_worker(result.into()));
        Ok(true)
    }

    pub(super) fn drain_into(
        &mut self,
        max_frames: usize,
        channels: usize,
        output: &mut Vec<i32>,
    ) -> (usize, usize) {
        let OrderedDrainProgress {
            drained_frames,
            completed_input_frames,
            retired_slabs,
        } = self.ordered_drain.drain_into(max_frames, channels, output);
        if retired_slabs > 0 {
            self.release_window_capacity(retired_slabs);
        }
        (drained_frames, completed_input_frames)
    }

    pub(super) fn completed_input_frames(&self) -> usize {
        self.ordered_drain.completed_input_frames()
    }

    pub(super) fn ready_slab_count(&self) -> usize {
        self.ordered_drain.ready_slab_count()
    }

    pub(super) fn next_ready_slab_start_frame(&self) -> usize {
        self.ordered_drain.next_ready_slab_start_frame()
    }

    pub(super) fn has_draining_slab(&self) -> bool {
        self.ordered_drain.has_draining_slab()
    }

    pub(super) fn active_slab_count(&self) -> usize {
        self.ordered_drain.active_slab_count()
    }

    pub(super) fn is_idle(&self) -> bool {
        if self.has_background_runtime() {
            self.background_work_is_exhausted()
        } else {
            self.result_channel_closed && self.ordered_drain.is_idle()
        }
    }

    pub(super) fn has_submit_capacity(&self) -> bool {
        if self.has_background_runtime() {
            !self.worker_queue_saturated
                && self.outstanding_window_slabs() < self.window_depth_limit()
        } else {
            self.active_slab_count() < self.window_depth_limit()
        }
    }

    #[cfg(test)]
    pub(super) fn accept_ready_slab(&mut self, slab: DecodedSlab, active_window_slabs: usize) {
        self.accept_result(DecodeSessionResult::from_slab(slab, active_window_slabs));
    }

    fn accept_result(&mut self, result: DecodeSessionResult) {
        self.worker_queue_saturated = false;
        let pcm_frames = result.slab.pcm_frames();
        self.ordered_drain.push_ready(result.slab);
        profile::accept_ready_pcm_frames_for_current_thread(
            pcm_frames,
            self.active_slab_count().max(result.active_window_slabs),
        );
    }

    fn try_recv_result(&mut self) -> Result<Option<DecodeSessionResult>> {
        #[cfg(test)]
        if let Some(receiver) = self.test_result_receiver.as_ref() {
            return match receiver.try_recv() {
                Ok(result) => Ok(Some(result?)),
                Err(mpsc::TryRecvError::Empty) => Ok(None),
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.result_channel_closed = true;
                    Ok(None)
                }
            };
        }

        match self.worker_pool.try_recv() {
            frame::DecodeWorkerRecv::Empty => Ok(None),
            frame::DecodeWorkerRecv::Slab(result) => {
                Ok(Some(self.ready_result_from_worker(result?.into())))
            }
        }
    }

    fn ready_result_from_worker(&self, slab: DecodedSlab) -> DecodeSessionResult {
        DecodeSessionResult::from_slab(slab, self.outstanding_window_slabs())
    }

    fn window_depth_limit(&self) -> usize {
        self.window_depth_limit
    }

    pub(super) fn set_window_depth_limit(&mut self, window_depth_limit: usize) {
        self.window_depth_limit = window_depth_limit.max(1);
        self.ordered_drain
            .set_window_capacity(self.window_depth_limit);
    }

    fn release_window_capacity(&mut self, completed_slabs: usize) {
        self.outstanding_window_slabs = self
            .outstanding_window_slabs
            .saturating_sub(completed_slabs);
        for _ in 0..completed_slabs {
            let Some(input_bytes) = self.submitted_input_bytes.pop_front() else {
                break;
            };
            self.staged_input_bytes = self.staged_input_bytes.saturating_sub(input_bytes);
        }
        profile::observe_staged_input_bytes_for_current_thread(self.staged_input_bytes);
    }

    fn has_background_runtime(&self) -> bool {
        #[cfg(test)]
        {
            self.has_background_runtime
        }

        #[cfg(not(test))]
        {
            true
        }
    }

    fn outstanding_window_slabs(&self) -> usize {
        self.outstanding_window_slabs
    }

    fn background_work_is_exhausted(&self) -> bool {
        if self.has_background_runtime() {
            self.outstanding_window_slabs() == 0 && self.ordered_drain.is_idle()
        } else {
            self.result_channel_closed && self.ordered_drain.is_idle()
        }
    }

    #[cfg(test)]
    pub(super) fn broken_for_submit_failure() -> Self {
        let mut session = Self::spawn(1, 1);
        session.force_submit_failure = true;
        session
    }

    #[cfg(test)]
    pub(super) fn spawn_with_blocked_worker_receives(
        worker_count: usize,
        window_depth_limit: usize,
        worker_queue_depth: usize,
    ) -> (Self, frame::WorkerReceiveHold) {
        let window_depth_limit = window_depth_limit.max(1);
        let worker_count = worker_count.max(1);
        let (worker_pool, receive_gate) = frame::FrameDecodeWorkerPool::new_with_blocked_receives(
            worker_count,
            worker_queue_depth.max(1),
        );
        (
            Self {
                worker_pool,
                ordered_drain: OrderedSlabDrain::with_window_capacity(window_depth_limit),
                outstanding_window_slabs: 0,
                result_channel_closed: false,
                window_depth_limit,
                worker_queue_saturated: false,
                submitted_input_bytes: VecDeque::new(),
                staged_input_bytes: 0,
                #[cfg(test)]
                worker_count,
                test_result_receiver: None,
                has_background_runtime: true,
                force_submit_failure: false,
            },
            receive_gate,
        )
    }

    #[cfg(test)]
    pub(super) fn window_depth_limit_for_tests(&self) -> usize {
        self.window_depth_limit
    }

    #[cfg(test)]
    pub(super) fn outstanding_window_slabs_for_tests(&self) -> usize {
        self.outstanding_window_slabs()
    }

    #[cfg(test)]
    pub(super) fn worker_count_for_tests(&self) -> usize {
        self.worker_count
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

    use super::{DecodeSessionResult, StreamingDecodeSession};
    use crate::read::{
        FrameIndex, profile,
        slab::{DecodeSlabPlan, DecodedSlab},
    };

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
    fn streaming_session_drains_background_packets_in_frame_order() {
        let (sender, receiver) = mpsc::sync_channel(4);
        let mut session = StreamingDecodeSession::from_result_receiver(receiver);
        let mut output = Vec::new();

        sender
            .send(Ok(DecodeSessionResult {
                slab: slab(1, 2, &[2], &[30, 31]),
                active_window_slabs: 1,
            }))
            .unwrap();
        sender
            .send(Ok(DecodeSessionResult {
                slab: slab(0, 0, &[2, 2], &[10, 11, 20, 21]),
                active_window_slabs: 2,
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
                slab: slab(1, 2, &[2], &[30, 31]),
                active_window_slabs: 2,
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
                slab: slab(0, 0, &[2, 2], &[10, 11, 20, 21]),
                active_window_slabs: 1,
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
    fn ordered_completion_reopens_submit_capacity_after_retiring_a_slab() {
        let (plans, channels) = fixture_plans(2);
        let mut plans = plans.into_iter();
        let mut session = StreamingDecodeSession::spawn(1, 2);
        assert!(session.submit(plans.next().unwrap()).unwrap());
        assert!(session.submit(plans.next().unwrap()).unwrap());

        assert!(!session.has_submit_capacity());
        assert!(session.wait_for_ready_slab().unwrap());
        assert!(!session.has_submit_capacity());

        let mut output = Vec::new();
        let (drained_frames, completed_input_frames) =
            session.drain_into(usize::MAX, channels, &mut output);
        assert!(drained_frames > 0);
        assert_eq!(completed_input_frames, 1);
        assert_eq!(session.completed_input_frames(), 1);
        assert!(session.has_submit_capacity());
    }

    #[test]
    fn spawned_session_reports_idle_after_real_work_is_drained() {
        let (plans, channels) = fixture_plans(1);
        let mut session = StreamingDecodeSession::spawn(2, 1);

        assert!(session.submit(plans.into_iter().next().unwrap()).unwrap());
        assert!(session.wait_for_ready_slab().unwrap());

        let mut output = Vec::new();
        assert!(session.drain_into(usize::MAX, channels, &mut output).0 > 0);
        assert!(session.is_idle());
    }

    #[test]
    fn wait_for_ready_slab_returns_false_after_all_work_is_drained() {
        let (plans, channels) = fixture_plans(1);
        let mut session = StreamingDecodeSession::spawn(1, 1);
        assert!(session.submit(plans.into_iter().next().unwrap()).unwrap());

        assert!(session.wait_for_ready_slab().unwrap());
        let mut output = Vec::new();
        assert!(session.drain_into(usize::MAX, channels, &mut output).0 > 0);

        let start = std::time::Instant::now();
        let exhausted = session.wait_for_ready_slab().unwrap();
        let elapsed = start.elapsed();

        assert!(!exhausted);
        assert!(
            elapsed < Duration::from_millis(100),
            "expected exhausted wait to return immediately after the final ordered drain, took {elapsed:?}"
        );
    }

    #[test]
    fn spawned_session_tracks_staged_input_residency_in_decode_profile() {
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
        profile::begin_decode_profile_session_for_current_thread(1, 1, 16, plan.bytes.len());

        let mut session = StreamingDecodeSession::spawn(1, 1);
        assert!(session.submit(plan).unwrap());

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
    fn spawn_owns_a_direct_dispatch_runtime() {
        let session = StreamingDecodeSession::spawn(2, 1);

        assert!(session.has_background_runtime());
        assert_eq!(session.worker_count_for_tests(), 2);
        assert!(session.has_submit_capacity());
    }

    #[test]
    fn direct_dispatch_submit_stops_at_worker_queue_backpressure_without_blocking() {
        let (plans, _) = fixture_plans(2);
        let mut plans = plans.into_iter();
        let (mut session, receive_hold) =
            StreamingDecodeSession::spawn_with_blocked_worker_receives(1, 3, 1);

        assert!(session.has_submit_capacity());
        assert!(session.submit(plans.next().unwrap()).unwrap());
        assert!(session.has_submit_capacity());
        assert!(
            !session.submit(plans.next().unwrap()).unwrap(),
            "submission should surface worker-queue backpressure instead of blocking"
        );
        assert!(!session.has_submit_capacity());

        receive_hold.release();
        assert!(session.wait_for_ready_slab().unwrap());
        assert!(session.has_submit_capacity());
    }

    #[test]
    fn ordered_drain_reopens_one_window_slot_at_a_time() {
        let mut session = StreamingDecodeSession::from_result_receiver_with_window_depth(
            mpsc::sync_channel::<crate::read::Result<DecodeSessionResult>>(1).1,
            2,
        );
        let mut output = Vec::new();

        session.accept_ready_slab(slab(1, 1, &[1], &[20]), 2);
        session.accept_ready_slab(slab(0, 0, &[1], &[10]), 2);

        assert!(!session.has_submit_capacity());
        assert_eq!(session.drain_into(1, 1, &mut output), (1, 1));
        assert_eq!(output, vec![10]);
        assert!(session.has_submit_capacity());
        assert_eq!(session.active_slab_count(), 1);
        assert_eq!(session.drain_into(1, 1, &mut output), (1, 1));
        assert_eq!(output, vec![10, 20]);
    }

    #[test]
    fn increasing_window_depth_limit_expands_ready_capacity_for_out_of_order_results() {
        let mut session = StreamingDecodeSession::from_result_receiver_with_window_depth(
            mpsc::sync_channel::<crate::read::Result<DecodeSessionResult>>(1).1,
            1,
        );
        let mut output = Vec::new();

        session.set_window_depth_limit(3);
        session.accept_ready_slab(slab(2, 2, &[1], &[30]), 3);
        session.accept_ready_slab(slab(0, 0, &[1], &[10]), 3);
        session.accept_ready_slab(slab(1, 1, &[1], &[20]), 3);

        assert_eq!(session.drain_into(usize::MAX, 1, &mut output), (3, 3));
        assert_eq!(output, vec![10, 20, 30]);
    }

    fn fixture_plans(count: usize) -> (Vec<DecodeSlabPlan>, usize) {
        let fixture_path = workspace_fixture_dir("test-flacs").join("case1/test01.flac");
        let bytes = std::fs::read(fixture_path).unwrap();
        let (stream_info, _, frame_offset) =
            crate::read::metadata::parse_metadata(&bytes, false).unwrap();
        let plans = crate::read::frame::index_frames(&bytes, frame_offset, stream_info)
            .unwrap()
            .into_iter()
            .take(count)
            .scan(0u64, |start_sample_number, frame| {
                let current = *start_sample_number;
                *start_sample_number =
                    start_sample_number.saturating_add(u64::from(frame.block_size));
                Some((current, frame))
            })
            .enumerate()
            .map(|(sequence, (start_sample_number, frame))| {
                let frame_bytes = bytes[frame.offset..frame.offset + frame.bytes_consumed].to_vec();
                let frame = FrameIndex { offset: 0, ..frame };
                DecodeSlabPlan::new(
                    sequence,
                    sequence,
                    start_sample_number,
                    stream_info,
                    vec![frame],
                )
                .seal_bytes(frame_bytes)
            })
            .collect();
        (plans, usize::from(stream_info.channels))
    }

    fn workspace_fixture_dir(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .map(|path| path.join(name))
            .find(|path| path.is_dir())
            .unwrap_or_else(|| {
                panic!("fixture directory '{name}' should exist from the workspace root")
            })
    }
}
