use std::{
    cell::RefCell,
    env,
    fs::OpenOptions,
    io::Write as _,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

std::thread_local! {
    static TEST_PROFILE_PATH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static CURRENT_PROFILE_SESSION: RefCell<Option<DecodeProfileSessionHandle>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DecodeProfileSession {
    summary: DecodeProfileSummary,
    resident_pcm_frames: usize,
    handed_out_pcm_frames: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct DecodeProfileSessionHandle(Arc<Mutex<DecodeProfileSession>>);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DecodeProfileSummary {
    pub(crate) worker_count: usize,
    pub(crate) queue_limit: usize,
    pub(crate) target_pcm_frames: usize,
    pub(crate) max_input_bytes_per_chunk: usize,
    pub(crate) peak_active_window_slabs: usize,
    pub(crate) peak_resident_pcm_frames: usize,
    pub(crate) peak_staged_input_bytes: usize,
}

impl DecodeProfileSummary {
    pub(crate) fn new(
        worker_count: usize,
        queue_limit: usize,
        target_pcm_frames: usize,
        max_input_bytes_per_chunk: usize,
    ) -> Self {
        Self {
            worker_count,
            queue_limit,
            target_pcm_frames,
            max_input_bytes_per_chunk,
            ..Self::default()
        }
    }

    pub(crate) fn observe_active_window_slabs(&mut self, active_window_slabs: usize) {
        self.peak_active_window_slabs = self.peak_active_window_slabs.max(active_window_slabs);
    }

    pub(crate) fn observe_resident_pcm_frames(&mut self, resident_pcm_frames: usize) {
        self.peak_resident_pcm_frames = self.peak_resident_pcm_frames.max(resident_pcm_frames);
    }
}

pub(crate) fn set_decode_profile_path_for_current_thread(path: Option<PathBuf>) {
    TEST_PROFILE_PATH.with(|profile_path| {
        *profile_path.borrow_mut() = path;
    });
}

pub(crate) fn active_decode_profile_path() -> Option<PathBuf> {
    TEST_PROFILE_PATH
        .with(|profile_path| profile_path.borrow().clone())
        .or_else(|| env::var_os("FLACX_DECODE_PROFILE").map(PathBuf::from))
}

pub(crate) fn begin_decode_profile_session_for_current_thread(
    worker_count: usize,
    queue_limit: usize,
    target_pcm_frames: usize,
    max_input_bytes_per_chunk: usize,
) {
    if active_decode_profile_path().is_none() {
        clear_decode_profile_session_for_current_thread();
        return;
    }

    CURRENT_PROFILE_SESSION.with(|session| {
        let mut session = session.borrow_mut();
        if session.is_none() {
            *session = Some(DecodeProfileSessionHandle(Arc::new(Mutex::new(
                DecodeProfileSession {
                    summary: DecodeProfileSummary::new(
                        worker_count,
                        queue_limit,
                        target_pcm_frames,
                        max_input_bytes_per_chunk,
                    ),
                    resident_pcm_frames: 0,
                    handed_out_pcm_frames: 0,
                },
            ))));
        }
    });
}

pub(crate) fn clear_decode_profile_session_for_current_thread() {
    CURRENT_PROFILE_SESSION.with(|session| {
        *session.borrow_mut() = None;
    });
}

pub(crate) fn accept_ready_pcm_frames_for_current_thread(
    pcm_frames: usize,
    active_window_slabs: usize,
) {
    CURRENT_PROFILE_SESSION.with(|session| {
        if let Some(session) = session.borrow().as_ref() {
            let mut session = session.0.lock().unwrap();
            session
                .summary
                .observe_active_window_slabs(active_window_slabs);
            session.resident_pcm_frames = session.resident_pcm_frames.saturating_add(pcm_frames);
            let resident_pcm_frames = session.resident_pcm_frames;
            session
                .summary
                .observe_resident_pcm_frames(resident_pcm_frames);
        }
    });
}

pub(crate) fn observe_staged_input_bytes_for_current_thread(staged_input_bytes: usize) {
    CURRENT_PROFILE_SESSION.with(|session| {
        if let Some(session) = session.borrow().as_ref() {
            let mut session = session.0.lock().unwrap();
            session.summary.peak_staged_input_bytes = session
                .summary
                .peak_staged_input_bytes
                .max(staged_input_bytes);
        }
    });
}

pub(crate) fn hand_out_pcm_frames_for_current_thread(pcm_frames: usize) {
    CURRENT_PROFILE_SESSION.with(|session| {
        if let Some(session) = session.borrow().as_ref() {
            let mut session = session.0.lock().unwrap();
            session.handed_out_pcm_frames =
                session.handed_out_pcm_frames.saturating_add(pcm_frames);
        }
    });
}

pub(crate) fn release_decode_output_buffer_for_current_thread() {
    CURRENT_PROFILE_SESSION.with(|session| {
        if let Some(session) = session.borrow().as_ref() {
            let mut session = session.0.lock().unwrap();
            session.resident_pcm_frames = session
                .resident_pcm_frames
                .saturating_sub(session.handed_out_pcm_frames);
            session.handed_out_pcm_frames = 0;
        }
    });
}

pub(crate) fn finish_successful_decode_profile_for_current_thread() {
    let profile_path = active_decode_profile_path();
    let session = CURRENT_PROFILE_SESSION.with(|session| session.borrow_mut().take());
    let Some(session) = session else {
        return;
    };
    let session = *session.0.lock().unwrap();
    if session.resident_pcm_frames != 0 || session.handed_out_pcm_frames != 0 {
        return;
    }
    append_decode_session_summary(profile_path.as_deref(), &session.summary);
}

pub(crate) fn append_decode_session_summary(
    profile_path: Option<&Path>,
    profile: &DecodeProfileSummary,
) {
    let Some(mut file) = open_profile_file(profile_path) else {
        return;
    };
    let _ = writeln!(
        file,
        "event=decode_session_summary\tworker_count={}\tqueue_limit={}\tpeak_active_window_slabs={}\tpeak_dispatch_window_slabs={}\tpeak_resident_pcm_frames={}\tpeak_dispatch_pcm_frames={}\tpeak_staged_input_bytes={}\ttarget_pcm_frames={}\tmax_input_bytes_per_chunk={}",
        profile.worker_count,
        profile.queue_limit,
        profile.peak_active_window_slabs,
        profile.peak_active_window_slabs,
        profile.peak_resident_pcm_frames,
        profile.peak_resident_pcm_frames,
        profile.peak_staged_input_bytes,
        profile.target_pcm_frames,
        profile.max_input_bytes_per_chunk,
    );
}

fn open_profile_file(profile_path: Option<&Path>) -> Option<std::fs::File> {
    let profile_path = profile_path?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(profile_path)
        .ok()
}
