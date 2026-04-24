use std::{
    cell::RefCell,
    collections::BTreeMap,
    env,
    fs::OpenOptions,
    io::{Seek, Write},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    config::EncoderConfig,
    encoder::EncodeSummary,
    error::{Error, Result},
    input::{EncodeChunkPayload, EncodePcmStream},
    md5::StreaminfoMd5,
    metadata::Metadata,
    model::encode_frame,
    plan::{EncodePlan, FrameCodedNumberKind, summary_from_stream_info},
    progress::ProgressSink,
    wav_input::decode_samples_into,
    write::{EncodedFrame, FlacWriter, FrameHeaderNumber},
};

#[cfg(feature = "progress")]
use crate::progress::emit_progress;

const FRAME_CHUNK_SIZE: usize = 32;
const ENCODE_CHUNK_MAX_FRAMES: usize = 256;
const ENCODE_CHUNK_TARGET_PCM_FRAMES: usize = 1 << 20;
const ENCODE_SESSION_QUEUE_DEPTH_MULTIPLIER: usize = 2;
const ENCODE_SESSION_RESULT_BACKLOG_PER_WORKER: usize = 1;
const ENCODE_SESSION_WINDOW_DEPTH: usize =
    ENCODE_SESSION_QUEUE_DEPTH_MULTIPLIER + ENCODE_SESSION_RESULT_BACKLOG_PER_WORKER + 1;

thread_local! {
    static TEST_PROFILE_PATH: RefCell<Option<std::path::PathBuf>> = const { RefCell::new(None) };
    #[cfg(feature = "progress")]
    static CURRENT_INPUT_BYTES_READ: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

#[derive(Clone, Copy)]
struct EncodeChunkPolicy {
    max_frames: usize,
    target_pcm_frames: usize,
}

pub(crate) struct EncodedChunk {
    pub(crate) start_frame: usize,
    pub(crate) frames: Vec<EncodedFrame>,
}

struct EncodeJob {
    start_frame: usize,
    end_frame: usize,
    pcm_frames: usize,
    chunk_base_sample: u64,
    payload: EncodeChunkPayload,
}

struct EncodedWorkChunk {
    start_frame: usize,
    pcm_frames: usize,
    frame_count: usize,
    encode_elapsed: Duration,
    frames: Vec<EncodedFrame>,
    payload: EncodeChunkPayload,
}

#[derive(Default)]
struct EncodeProfileSummary {
    read_decode_md5: Duration,
    wait_for_results: Duration,
    write_progress: Duration,
    worker_encode_cpu: Duration,
    peak_requested_pcm_frames: usize,
    peak_inflight_pcm_frames: usize,
    total_chunks: usize,
    out_of_order_results: usize,
}

struct EncodeStreamingSession<'a, W: Write + Seek, S, P> {
    plan: &'a EncodePlan,
    stream: &'a mut S,
    writer: &'a mut FlacWriter<W>,
    progress: &'a mut P,
    md5: &'a mut StreaminfoMd5,
}

impl<W, S, P> EncodeStreamingSession<'_, W, S, P>
where
    W: Write + Seek,
    S: EncodePcmStream,
    P: ProgressSink,
{
    fn run(self, chunk_policy: EncodeChunkPolicy, worker_count: usize) -> Result<()> {
        let Self {
            plan,
            stream,
            writer,
            progress,
            md5,
        } = self;

        let queue_limit = worker_count
            .checked_mul(ENCODE_SESSION_WINDOW_DEPTH)
            .unwrap_or(worker_count)
            .max(worker_count);
        let profile_path = active_encode_profile_path();
        let mut profile = EncodeProfileSummary::default();
        let mut chunk_start = 0usize;
        let channels = usize::from(plan.spec.channels);
        let mut processed_samples = 0u64;
        let mut next_expected = 0usize;
        let mut inflight_pcm_frames = 0usize;
        let mut pending: BTreeMap<usize, EncodedWorkChunk> = BTreeMap::new();
        let mut reusable_payloads = Vec::with_capacity(queue_limit);

        thread::scope(|scope| -> Result<()> {
            let (result_sender, result_receiver) = mpsc::sync_channel::<Result<EncodedWorkChunk>>(
                worker_count * ENCODE_SESSION_RESULT_BACKLOG_PER_WORKER,
            );
            let mut worker_senders = Vec::with_capacity(worker_count);

            for _ in 0..worker_count {
                let (sender, receiver) =
                    mpsc::sync_channel::<EncodeJob>(ENCODE_SESSION_QUEUE_DEPTH_MULTIPLIER);
                worker_senders.push(sender);
                let result_sender = result_sender.clone();
                let plan = plan.clone();

                scope.spawn(move || {
                    let mut decoded_samples = Vec::new();
                    while let Ok(job) = receiver.recv() {
                        let encode_start = Instant::now();
                        let encoded = (|| -> Result<EncodedChunk> {
                            match &job.payload {
                                EncodeChunkPayload::DecodedSamples(samples) => encode_frame_batch(
                                    samples,
                                    &plan,
                                    job.start_frame,
                                    0,
                                    job.end_frame - job.start_frame,
                                    job.chunk_base_sample,
                                ),
                                EncodeChunkPayload::PackedPcm {
                                    bytes, envelope, ..
                                } => {
                                    decoded_samples.clear();
                                    decode_samples_into(bytes, *envelope, &mut decoded_samples)?;
                                    encode_frame_batch(
                                        &decoded_samples,
                                        &plan,
                                        job.start_frame,
                                        0,
                                        job.end_frame - job.start_frame,
                                        job.chunk_base_sample,
                                    )
                                }
                            }
                        })();
                        let elapsed = encode_start.elapsed();
                        let encoded = encoded.map(|chunk| EncodedWorkChunk {
                            start_frame: job.start_frame,
                            pcm_frames: job.pcm_frames,
                            frame_count: job.end_frame - job.start_frame,
                            encode_elapsed: elapsed,
                            frames: chunk.frames,
                            payload: job.payload,
                        });
                        if result_sender.send(encoded).is_err() {
                            return;
                        }
                    }
                });
            }
            drop(result_sender);

            let session_result = (|| -> Result<()> {
                let mut next_worker = 0usize;
                while chunk_start < plan.total_frames {
                    let chunk_end = chunk_end_for_plan(plan, chunk_start, chunk_policy);
                    let pcm_frames = expected_frames_for_chunk(plan, chunk_start, chunk_end);
                    let reuse = reusable_payloads.pop();
                    let read_start = Instant::now();
                    let payload = read_planned_chunk(
                        stream,
                        plan,
                        chunk_start,
                        chunk_end,
                        channels,
                        md5,
                        reuse,
                    )?;
                    profile.read_decode_md5 += read_start.elapsed();
                    profile.total_chunks += 1;
                    profile.peak_requested_pcm_frames =
                        profile.peak_requested_pcm_frames.max(pcm_frames);

                    let mut job = EncodeJob {
                        start_frame: chunk_start,
                        end_frame: chunk_end,
                        pcm_frames,
                        chunk_base_sample: plan.frame(chunk_start).sample_offset,
                        payload,
                    };

                    loop {
                        match try_dispatch_job(job, &worker_senders, &mut next_worker) {
                            Ok(()) => {
                                inflight_pcm_frames =
                                    inflight_pcm_frames.saturating_add(pcm_frames);
                                profile.peak_inflight_pcm_frames =
                                    profile.peak_inflight_pcm_frames.max(inflight_pcm_frames);
                                break;
                            }
                            Err(DispatchError::Full(returned)) => {
                                job = returned;
                                #[cfg(feature = "progress")]
                                receive_and_drain_ready(
                                    &result_receiver,
                                    &mut pending,
                                    writer,
                                    progress,
                                    &mut reusable_payloads,
                                    &mut processed_samples,
                                    plan.spec.total_samples,
                                    &mut next_expected,
                                    plan.total_frames,
                                    stream.input_bytes_processed(),
                                    &mut inflight_pcm_frames,
                                    &mut profile,
                                )?;
                                #[cfg(not(feature = "progress"))]
                                receive_and_drain_ready(
                                    &result_receiver,
                                    &mut pending,
                                    writer,
                                    progress,
                                    &mut reusable_payloads,
                                    &mut processed_samples,
                                    plan.spec.total_samples,
                                    &mut next_expected,
                                    plan.total_frames,
                                    &mut inflight_pcm_frames,
                                    &mut profile,
                                )?;
                            }
                            Err(DispatchError::Disconnected) => {
                                return Err(Error::Thread(
                                    "encode worker queue disconnected before the session completed"
                                        .into(),
                                ));
                            }
                        }
                    }

                    chunk_start = chunk_end;
                }

                while next_expected < plan.total_frames {
                    #[cfg(feature = "progress")]
                    receive_and_drain_ready(
                        &result_receiver,
                        &mut pending,
                        writer,
                        progress,
                        &mut reusable_payloads,
                        &mut processed_samples,
                        plan.spec.total_samples,
                        &mut next_expected,
                        plan.total_frames,
                        stream.input_bytes_processed(),
                        &mut inflight_pcm_frames,
                        &mut profile,
                    )?;
                    #[cfg(not(feature = "progress"))]
                    receive_and_drain_ready(
                        &result_receiver,
                        &mut pending,
                        writer,
                        progress,
                        &mut reusable_payloads,
                        &mut processed_samples,
                        plan.spec.total_samples,
                        &mut next_expected,
                        plan.total_frames,
                        &mut inflight_pcm_frames,
                        &mut profile,
                    )?;
                }

                Ok(())
            })();

            drop(worker_senders);
            session_result
        })?;

        maybe_append_encode_profile(
            profile_path.as_deref(),
            &profile,
            worker_count,
            queue_limit,
            chunk_policy,
        );
        Ok(())
    }
}

pub(crate) fn set_encode_profile_path_for_current_thread(path: Option<std::path::PathBuf>) {
    TEST_PROFILE_PATH.with(|profile_path| {
        *profile_path.borrow_mut() = path;
    });
}

fn active_encode_profile_path() -> Option<std::path::PathBuf> {
    TEST_PROFILE_PATH
        .with(|profile_path| profile_path.borrow().clone())
        .or_else(|| env::var_os("FLACX_ENCODE_PROFILE").map(std::path::PathBuf::from))
}

#[cfg(feature = "progress")]
fn set_current_input_bytes_read(bytes: u64) {
    CURRENT_INPUT_BYTES_READ.with(|current| current.set(Some(bytes)));
}

#[cfg(feature = "progress")]
fn clear_current_input_bytes_read() {
    CURRENT_INPUT_BYTES_READ.with(|current| current.set(None));
}

#[cfg(feature = "progress")]
fn current_input_bytes_read() -> u64 {
    CURRENT_INPUT_BYTES_READ
        .with(std::cell::Cell::get)
        .unwrap_or(0)
}

pub(crate) fn encode_stream<W, S, P>(
    config: &EncoderConfig,
    metadata: Metadata,
    mut stream: S,
    output: &mut W,
    progress: &mut P,
) -> Result<EncodeSummary>
where
    W: Write + Seek,
    S: EncodePcmStream,
    P: ProgressSink,
{
    #[cfg(feature = "progress")]
    clear_current_input_bytes_read();
    let spec = stream.spec();
    let chunk_policy = EncodeChunkPolicy {
        max_frames: stream
            .preferred_encode_chunk_max_frames()
            .unwrap_or(ENCODE_CHUNK_MAX_FRAMES),
        target_pcm_frames: stream
            .preferred_encode_chunk_target_pcm_frames()
            .unwrap_or(ENCODE_CHUNK_TARGET_PCM_FRAMES),
    };
    let plan = EncodePlan::new(spec, config.clone())?;
    let worker_count = config.threads().max(1).min(plan.total_frames.max(1));
    let stream_info = plan.stream_info();
    let has_preserved_bundle = metadata.has_preserved_bundle();
    let metadata_blocks = metadata.flac_blocks(spec.total_samples);
    let mut writer = FlacWriter::new(
        output,
        stream_info,
        &metadata_blocks,
        plan.total_frames,
        !has_preserved_bundle,
    )?;
    let mut md5 = StreaminfoMd5::new(spec);

    if plan.total_frames == 0 {
        writer.set_streaminfo_md5(stream.finish_streaminfo_md5(md5)?);
        let (_, stream_info) = writer.finalize()?;
        #[cfg(feature = "progress")]
        clear_current_input_bytes_read();
        return Ok(summary_from_stream_info(stream_info, 0));
    }

    if worker_count == 1 {
        encode_streaming_session_single_thread(
            config,
            &plan,
            &mut stream,
            &mut writer,
            progress,
            &mut md5,
            chunk_policy,
        )?;
    } else {
        EncodeStreamingSession {
            plan: &plan,
            stream: &mut stream,
            writer: &mut writer,
            progress,
            md5: &mut md5,
        }
        .run(chunk_policy, worker_count)?;
    }

    let extra_frames = stream.read_chunk(1, &mut Vec::new())?;
    if extra_frames != 0 {
        return Err(Error::Encode(
            "PCM stream produced more frames than declared in the spec".into(),
        ));
    }

    writer.set_streaminfo_md5(stream.finish_streaminfo_md5(md5)?);
    let (_, stream_info) = writer.finalize()?;
    #[cfg(feature = "progress")]
    clear_current_input_bytes_read();
    Ok(summary_from_stream_info(stream_info, plan.total_frames))
}

fn expected_frames_for_chunk(plan: &EncodePlan, chunk_start: usize, chunk_end: usize) -> usize {
    (chunk_start..chunk_end)
        .map(|frame_index| usize::from(plan.frame(frame_index).block_size))
        .sum()
}

fn read_planned_chunk<S>(
    stream: &mut S,
    plan: &EncodePlan,
    chunk_start: usize,
    chunk_end: usize,
    channels: usize,
    md5: &mut StreaminfoMd5,
    reuse: Option<EncodeChunkPayload>,
) -> Result<EncodeChunkPayload>
where
    S: EncodePcmStream,
{
    let expected_frames = expected_frames_for_chunk(plan, chunk_start, chunk_end);
    let payload = stream.read_chunk_payload(expected_frames, reuse)?;
    let read_frames = payload.pcm_frames(channels);
    if read_frames != expected_frames {
        return Err(Error::Encode(format!(
            "PCM stream ended early: expected {expected_frames} frames, read {read_frames}"
        )));
    }
    if let Some(sample_len) = payload.decoded_samples_len() {
        let expected_samples = expected_frames
            .checked_mul(channels)
            .ok_or_else(|| Error::Encode("PCM chunk sample count overflows".into()))?;
        if sample_len != expected_samples {
            return Err(Error::Encode(format!(
                "PCM stream yielded {sample_len} samples for {expected_frames} frames across {channels} channels"
            )));
        }
    }
    stream.update_streaminfo_md5_for_payload(md5, &payload)?;
    Ok(payload)
}

fn chunk_end_for_plan(
    plan: &EncodePlan,
    chunk_start: usize,
    chunk_policy: EncodeChunkPolicy,
) -> usize {
    let mut chunk_end = chunk_start;
    let mut chunk_frames = 0usize;
    let mut pcm_frames = 0usize;

    while chunk_end < plan.total_frames && chunk_frames < chunk_policy.max_frames {
        let next_block_size = usize::from(plan.frame(chunk_end).block_size);
        if chunk_frames > 0
            && pcm_frames.saturating_add(next_block_size) > chunk_policy.target_pcm_frames
        {
            break;
        }
        pcm_frames = pcm_frames.saturating_add(next_block_size);
        chunk_frames += 1;
        chunk_end += 1;
    }
    chunk_end
}

fn encode_streaming_session_single_thread<W, S, P>(
    config: &EncoderConfig,
    plan: &EncodePlan,
    stream: &mut S,
    writer: &mut FlacWriter<W>,
    progress: &mut P,
    md5: &mut StreaminfoMd5,
    chunk_policy: EncodeChunkPolicy,
) -> Result<()>
where
    W: Write + Seek,
    S: EncodePcmStream,
    P: ProgressSink,
{
    let channels = usize::from(plan.spec.channels);
    let mut processed_samples = 0u64;
    let mut reusable_payload = None;
    let mut decoded_samples = Vec::new();
    let mut chunk_start = 0usize;
    let profile_path = active_encode_profile_path();
    let mut profile = EncodeProfileSummary::default();

    while chunk_start < plan.total_frames {
        let chunk_end = chunk_end_for_plan(plan, chunk_start, chunk_policy);
        let pcm_frames = expected_frames_for_chunk(plan, chunk_start, chunk_end);
        let read_start = Instant::now();
        let payload = read_planned_chunk(
            stream,
            plan,
            chunk_start,
            chunk_end,
            channels,
            md5,
            reusable_payload.take(),
        )?;
        profile.read_decode_md5 += read_start.elapsed();
        profile.total_chunks += 1;
        profile.peak_requested_pcm_frames = profile.peak_requested_pcm_frames.max(pcm_frames);
        profile.peak_inflight_pcm_frames = profile.peak_inflight_pcm_frames.max(pcm_frames);

        let encode_start = Instant::now();
        let encoded = match &payload {
            EncodeChunkPayload::DecodedSamples(chunk_samples) => {
                let mut chunk_samples = chunk_samples.clone();
                encode_chunk(config, plan, chunk_start, chunk_end, &mut chunk_samples)?
            }
            EncodeChunkPayload::PackedPcm { bytes, envelope, .. } => {
                decoded_samples.clear();
                decode_samples_into(bytes, *envelope, &mut decoded_samples)?;
                encode_chunk(config, plan, chunk_start, chunk_end, &mut decoded_samples)?
            }
        };
        profile.worker_encode_cpu += encode_start.elapsed();
        reusable_payload = Some(payload.clear_for_reuse());

        let write_start = Instant::now();
        #[cfg(feature = "progress")]
        set_current_input_bytes_read(stream.input_bytes_processed());
        processed_samples = write_encoded_chunk(
            writer,
            encoded,
            processed_samples,
            plan.spec.total_samples,
            chunk_start,
            plan.total_frames,
            progress,
        )?;
        profile.write_progress += write_start.elapsed();
        chunk_start = chunk_end;
    }

    maybe_append_encode_profile(profile_path.as_deref(), &profile, 1, 1, chunk_policy);
    Ok(())
}

enum DispatchError {
    Full(EncodeJob),
    Disconnected,
}

fn try_dispatch_job(
    mut job: EncodeJob,
    worker_senders: &[mpsc::SyncSender<EncodeJob>],
    next_worker: &mut usize,
) -> std::result::Result<(), DispatchError> {
    for offset in 0..worker_senders.len() {
        let worker_index = (*next_worker + offset) % worker_senders.len();
        match worker_senders[worker_index].try_send(job) {
            Ok(()) => {
                *next_worker = (worker_index + 1) % worker_senders.len();
                return Ok(());
            }
            Err(TrySendError::Full(returned)) => {
                job = returned;
            }
            Err(TrySendError::Disconnected(_)) => return Err(DispatchError::Disconnected),
        }
    }

    Err(DispatchError::Full(job))
}

#[allow(clippy::too_many_arguments)]
#[cfg(feature = "progress")]
fn receive_and_drain_ready<W, P>(
    result_receiver: &mpsc::Receiver<Result<EncodedWorkChunk>>,
    pending: &mut BTreeMap<usize, EncodedWorkChunk>,
    writer: &mut FlacWriter<W>,
    progress: &mut P,
    reusable_payloads: &mut Vec<EncodeChunkPayload>,
    processed_samples: &mut u64,
    total_samples: u64,
    next_expected: &mut usize,
    total_frames: usize,
    input_bytes_read: u64,
    inflight_pcm_frames: &mut usize,
    profile: &mut EncodeProfileSummary,
) -> Result<()>
where
    W: Write + Seek,
    P: ProgressSink,
{
    let wait_start = Instant::now();
    let chunk = result_receiver.recv().map_err(|_| {
        Error::Thread("encode result channel closed before the session completed".into())
    })??;
    profile.wait_for_results += wait_start.elapsed();
    profile.worker_encode_cpu += chunk.encode_elapsed;
    if chunk.start_frame != *next_expected {
        profile.out_of_order_results += 1;
    }
    pending.insert(chunk.start_frame, chunk);

    let write_start = Instant::now();
    set_current_input_bytes_read(input_bytes_read);
    while let Some(mut chunk) = pending.remove(next_expected) {
        let frame_count = chunk.frame_count;
        *processed_samples = write_encoded_chunk(
            writer,
            EncodedChunk {
                start_frame: chunk.start_frame,
                frames: std::mem::take(&mut chunk.frames),
            },
            *processed_samples,
            total_samples,
            chunk.start_frame,
            total_frames,
            progress,
        )?;
        *next_expected += frame_count;
        *inflight_pcm_frames = inflight_pcm_frames.saturating_sub(chunk.pcm_frames);
        reusable_payloads.push(chunk.payload.clear_for_reuse());
    }
    profile.write_progress += write_start.elapsed();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[cfg(not(feature = "progress"))]
fn receive_and_drain_ready<W, P>(
    result_receiver: &mpsc::Receiver<Result<EncodedWorkChunk>>,
    pending: &mut BTreeMap<usize, EncodedWorkChunk>,
    writer: &mut FlacWriter<W>,
    progress: &mut P,
    reusable_payloads: &mut Vec<EncodeChunkPayload>,
    processed_samples: &mut u64,
    total_samples: u64,
    next_expected: &mut usize,
    total_frames: usize,
    inflight_pcm_frames: &mut usize,
    profile: &mut EncodeProfileSummary,
) -> Result<()>
where
    W: Write + Seek,
    P: ProgressSink,
{
    let wait_start = Instant::now();
    let chunk = result_receiver.recv().map_err(|_| {
        Error::Thread("encode result channel closed before the session completed".into())
    })??;
    profile.wait_for_results += wait_start.elapsed();
    profile.worker_encode_cpu += chunk.encode_elapsed;
    if chunk.start_frame != *next_expected {
        profile.out_of_order_results += 1;
    }
    pending.insert(chunk.start_frame, chunk);

    let write_start = Instant::now();
    while let Some(mut chunk) = pending.remove(next_expected) {
        let frame_count = chunk.frame_count;
        *processed_samples = write_encoded_chunk(
            writer,
            EncodedChunk {
                start_frame: chunk.start_frame,
                frames: std::mem::take(&mut chunk.frames),
            },
            *processed_samples,
            total_samples,
            chunk.start_frame,
            total_frames,
            progress,
        )?;
        *next_expected += frame_count;
        *inflight_pcm_frames = inflight_pcm_frames.saturating_sub(chunk.pcm_frames);
        reusable_payloads.push(chunk.payload.clear_for_reuse());
    }
    profile.write_progress += write_start.elapsed();
    Ok(())
}

fn maybe_append_encode_profile(
    profile_path: Option<&std::path::Path>,
    profile: &EncodeProfileSummary,
    worker_count: usize,
    queue_limit: usize,
    chunk_policy: EncodeChunkPolicy,
) {
    let Some(profile_path) = profile_path else {
        return;
    };
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(profile_path)
    else {
        return;
    };

    let _ = writeln!(
        file,
        "event=encode_phase\tphase=read_decode_md5\telapsed_seconds={:.9}",
        profile.read_decode_md5.as_secs_f64()
    );
    let _ = writeln!(
        file,
        "event=encode_phase\tphase=wait_for_results\telapsed_seconds={:.9}",
        profile.wait_for_results.as_secs_f64()
    );
    let _ = writeln!(
        file,
        "event=encode_phase\tphase=write_progress\telapsed_seconds={:.9}",
        profile.write_progress.as_secs_f64()
    );
    let _ = writeln!(
        file,
        "event=encode_phase\tphase=worker_encode_cpu\telapsed_seconds={:.9}",
        profile.worker_encode_cpu.as_secs_f64()
    );
    let _ = writeln!(
        file,
        "event=encode_session_summary\tworker_count={worker_count}\tqueue_limit={queue_limit}\tchunk_policy_max_frames={}\tchunk_policy_target_pcm_frames={}\tpeak_requested_pcm_frames={}\tpeak_inflight_pcm_frames={}\ttotal_chunks={}\tout_of_order_results={}",
        chunk_policy.max_frames,
        chunk_policy.target_pcm_frames,
        profile.peak_requested_pcm_frames,
        profile.peak_inflight_pcm_frames,
        profile.total_chunks,
        profile.out_of_order_results,
    );
}

pub(crate) fn encode_chunk(
    config: &EncoderConfig,
    plan: &EncodePlan,
    chunk_start: usize,
    chunk_end: usize,
    chunk_samples: &mut Vec<i32>,
) -> Result<EncodedChunk> {
    let frame_count = chunk_end - chunk_start;
    let worker_count = config.threads().max(1).min(frame_count.max(1));
    let chunk_base_sample = plan.frame(chunk_start).sample_offset;

    if worker_count == 1 || frame_count <= FRAME_CHUNK_SIZE {
        return encode_frame_batch(
            chunk_samples,
            plan,
            chunk_start,
            0,
            frame_count,
            chunk_base_sample,
        );
    }

    let next_frame = Arc::new(AtomicUsize::new(0));
    let samples = Arc::new(std::mem::take(chunk_samples));
    let encoded = thread::scope(|scope| -> Result<EncodedChunk> {
        let (sender, receiver) = mpsc::channel();
        for _ in 0..worker_count {
            let sender = sender.clone();
            let next_frame = Arc::clone(&next_frame);
            let samples = Arc::clone(&samples);
            let plan = plan.clone();

            scope.spawn(move || {
                loop {
                    let local_index = next_frame.fetch_add(FRAME_CHUNK_SIZE, Ordering::Relaxed);
                    if local_index >= frame_count {
                        break;
                    }
                    let local_end = (local_index + FRAME_CHUNK_SIZE).min(frame_count);
                    if sender
                        .send(encode_frame_batch(
                            samples.as_slice(),
                            &plan,
                            chunk_start,
                            local_index,
                            local_end,
                            chunk_base_sample,
                        ))
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }

        drop(sender);
        let mut next_expected = chunk_start;
        let mut pending: BTreeMap<usize, EncodedChunk> = BTreeMap::new();
        let mut ordered_frames = Vec::with_capacity(frame_count);
        while next_expected < chunk_end {
            let encoded_chunk = receiver.recv().map_err(|_| {
                Error::Thread("frame worker channel closed before the chunk completed".into())
            })??;
            if encoded_chunk.start_frame == next_expected {
                next_expected = drain_chunk(encoded_chunk, &mut ordered_frames, next_expected);
                while let Some(chunk) = pending.remove(&next_expected) {
                    next_expected = drain_chunk(chunk, &mut ordered_frames, next_expected);
                }
            } else {
                pending.insert(encoded_chunk.start_frame, encoded_chunk);
            }
        }

        Ok(EncodedChunk {
            start_frame: chunk_start,
            frames: ordered_frames,
        })
    })?;
    *chunk_samples = Arc::try_unwrap(samples).map_err(|_| {
        Error::Thread("frame worker sample buffer remained shared after chunk encoding".into())
    })?;
    Ok(encoded)
}

pub(crate) fn encode_frame_batch(
    samples: &[i32],
    plan: &EncodePlan,
    chunk_start: usize,
    local_start: usize,
    local_end: usize,
    chunk_base_sample: u64,
) -> Result<EncodedChunk> {
    let channels = usize::from(plan.spec.channels);
    let mut encoded_frames = Vec::with_capacity(local_end.saturating_sub(local_start));

    for local_frame in local_start..local_end {
        let frame_index = chunk_start + local_frame;
        let frame_plan = plan.frame(frame_index);
        let frame_samples = usize::from(frame_plan.block_size);
        let local_sample_start = usize::try_from(frame_plan.sample_offset - chunk_base_sample)
            .expect("sample offset fits usize")
            * channels;
        let local_sample_end = local_sample_start + frame_samples * channels;
        encoded_frames.push(encode_frame(
            &samples[local_sample_start..local_sample_end],
            plan.spec.channels,
            plan.spec.bits_per_sample,
            plan.spec.sample_rate,
            frame_header_number(frame_plan.coded_number_kind, frame_plan.coded_number),
            plan.profile,
        )?);
    }

    Ok(EncodedChunk {
        start_frame: chunk_start + local_start,
        frames: encoded_frames,
    })
}

fn frame_header_number(kind: FrameCodedNumberKind, coded_number: u64) -> FrameHeaderNumber {
    match kind {
        FrameCodedNumberKind::FrameNumber => FrameHeaderNumber::Frame(coded_number),
        FrameCodedNumberKind::SampleNumber => FrameHeaderNumber::Sample(coded_number),
    }
}

fn drain_chunk(
    chunk: EncodedChunk,
    ordered_frames: &mut Vec<EncodedFrame>,
    mut next_expected: usize,
) -> usize {
    for frame in chunk.frames {
        ordered_frames.push(frame);
        next_expected += 1;
    }
    next_expected
}

#[cfg(feature = "progress")]
pub(crate) fn write_encoded_chunk<W, P>(
    writer: &mut FlacWriter<W>,
    chunk: EncodedChunk,
    mut processed_samples: u64,
    total_samples: u64,
    chunk_start: usize,
    total_frames: usize,
    progress: &mut P,
) -> Result<u64>
where
    W: Write + Seek,
    P: ProgressSink,
{
    for (offset, frame) in chunk.frames.iter().enumerate() {
        let frame_index = chunk_start + offset;
        writer.write_frame(
            frame_index,
            processed_samples,
            frame.sample_count,
            &frame.bytes,
        )?;
        processed_samples += u64::from(frame.sample_count);
        emit_progress!(
            progress,
            crate::progress::ProgressSnapshot {
                processed_samples,
                total_samples,
                completed_frames: frame_index + 1,
                total_frames,
                input_bytes_read: current_input_bytes_read(),
                output_bytes_written: writer.bytes_written(),
            }
        )?;
    }
    Ok(processed_samples)
}

#[cfg(not(feature = "progress"))]
pub(crate) fn write_encoded_chunk<W, P>(
    writer: &mut FlacWriter<W>,
    chunk: EncodedChunk,
    mut processed_samples: u64,
    _total_samples: u64,
    chunk_start: usize,
    _total_frames: usize,
    _progress: &mut P,
) -> Result<u64>
where
    W: Write + Seek,
    P: ProgressSink,
{
    for (offset, frame) in chunk.frames.iter().enumerate() {
        let frame_index = chunk_start + offset;
        writer.write_frame(
            frame_index,
            processed_samples,
            frame.sample_count,
            &frame.bytes,
        )?;
        processed_samples += u64::from(frame.sample_count);
    }
    Ok(processed_samples)
}
