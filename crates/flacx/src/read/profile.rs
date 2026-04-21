use std::{
    cell::RefCell,
    env,
    fs::OpenOptions,
    io::Write as _,
    path::{Path, PathBuf},
    time::Duration,
};

std::thread_local! {
    static TEST_PROFILE_PATH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct DecodeProfileSummary {
    pub(crate) worker_count: usize,
    pub(crate) queue_limit: usize,
    pub(crate) target_pcm_frames: usize,
    pub(crate) peak_inflight_packets: usize,
    pub(crate) peak_inflight_pcm_frames: usize,
}

impl DecodeProfileSummary {
    pub(crate) fn new(worker_count: usize, queue_limit: usize, target_pcm_frames: usize) -> Self {
        Self {
            worker_count,
            queue_limit,
            target_pcm_frames,
            ..Self::default()
        }
    }

    pub(crate) fn observe_inflight_packets(&mut self, inflight_packets: usize) {
        self.peak_inflight_packets = self.peak_inflight_packets.max(inflight_packets);
    }

    pub(crate) fn observe_inflight_pcm_frames(&mut self, inflight_pcm_frames: usize) {
        self.peak_inflight_pcm_frames = self.peak_inflight_pcm_frames.max(inflight_pcm_frames);
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

pub(crate) fn append_decode_phase(profile_path: Option<&Path>, phase: &str, elapsed: Duration) {
    let Some(mut file) = open_profile_file(profile_path) else {
        return;
    };
    let _ = writeln!(
        file,
        "event=decode_phase\tphase={phase}\telapsed_seconds={:.9}",
        elapsed.as_secs_f64()
    );
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
        "event=decode_session_summary\tworker_count={}\tqueue_limit={}\tpeak_inflight_packets={}\tpeak_inflight_pcm_frames={}\ttarget_pcm_frames={}",
        profile.worker_count,
        profile.queue_limit,
        profile.peak_inflight_packets,
        profile.peak_inflight_pcm_frames,
        profile.target_pcm_frames,
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
