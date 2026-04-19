use std::{
    collections::BTreeMap,
    io::{Seek, Write},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

use crate::{
    config::EncoderConfig,
    encoder::EncodeSummary,
    error::{Error, Result},
    input::EncodePcmStream,
    md5::StreaminfoMd5,
    metadata::Metadata,
    model::encode_frame,
    plan::{EncodePlan, FrameCodedNumberKind, summary_from_stream_info},
    progress::{ProgressSink, ProgressSnapshot},
    write::{EncodedFrame, FlacWriter, FrameHeaderNumber},
};

const FRAME_CHUNK_SIZE: usize = 32;
const ENCODE_CHUNK_MAX_FRAMES: usize = 256;
const ENCODE_CHUNK_TARGET_PCM_FRAMES: usize = 1 << 20;

#[derive(Clone, Copy)]
struct EncodeChunkPolicy {
    max_frames: usize,
    target_pcm_frames: usize,
}

pub(crate) struct EncodedChunk {
    pub(crate) start_frame: usize,
    pub(crate) frames: Vec<EncodedFrame>,
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
        return Ok(summary_from_stream_info(stream_info, 0));
    }

    let channels = usize::from(spec.channels);
    let mut processed_samples = 0u64;
    let mut chunk_samples = Vec::new();
    let mut chunk_start = 0usize;
    while chunk_start < plan.total_frames {
        let chunk_end = chunk_end_for_plan(&plan, chunk_start, chunk_policy);
        read_planned_chunk(
            &mut stream,
            &plan,
            chunk_start,
            chunk_end,
            channels,
            &mut md5,
            &mut chunk_samples,
        )?;
        let encoded = encode_chunk(config, &plan, chunk_start, chunk_end, &mut chunk_samples)?;
        processed_samples = write_encoded_chunk(
            &mut writer,
            encoded,
            processed_samples,
            spec.total_samples,
            chunk_start,
            plan.total_frames,
            progress,
        )?;
        chunk_start = chunk_end;
    }

    let extra_frames = stream.read_chunk(1, &mut Vec::new())?;
    if extra_frames != 0 {
        return Err(Error::Encode(
            "PCM stream produced more frames than declared in the spec".into(),
        ));
    }

    writer.set_streaminfo_md5(stream.finish_streaminfo_md5(md5)?);
    let (_, stream_info) = writer.finalize()?;
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
    chunk_samples: &mut Vec<i32>,
) -> Result<()>
where
    S: EncodePcmStream,
{
    let expected_frames = expected_frames_for_chunk(plan, chunk_start, chunk_end);
    let expected_samples = expected_frames
        .checked_mul(channels)
        .ok_or_else(|| Error::Encode("PCM chunk sample count overflows".into()))?;
    chunk_samples.clear();
    if chunk_samples.capacity() < expected_samples {
        chunk_samples.reserve(expected_samples - chunk_samples.capacity());
    }
    let read_frames = stream.read_chunk(expected_frames, chunk_samples)?;
    if read_frames != expected_frames {
        return Err(Error::Encode(format!(
            "PCM stream ended early: expected {expected_frames} frames, read {read_frames}"
        )));
    }
    if chunk_samples.len() != expected_samples {
        return Err(Error::Encode(format!(
            "PCM stream yielded {} samples for {expected_frames} frames across {channels} channels",
            chunk_samples.len()
        )));
    }
    stream.update_streaminfo_md5(md5, chunk_samples)?;
    Ok(())
}

fn chunk_end_for_plan(
    plan: &EncodePlan,
    chunk_start: usize,
    chunk_policy: EncodeChunkPolicy,
) -> usize {
    let remaining_frames = plan.total_frames - chunk_start;
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

    if chunk_end == plan.total_frames && remaining_frames > 1 {
        chunk_end = chunk_start + remaining_frames.div_ceil(2);
    }

    chunk_end
}

pub(crate) fn encode_chunk(
    config: &EncoderConfig,
    plan: &EncodePlan,
    chunk_start: usize,
    chunk_end: usize,
    chunk_samples: &mut Vec<i32>,
) -> Result<EncodedChunk> {
    let frame_count = chunk_end - chunk_start;
    let worker_count = config.threads.max(1).min(frame_count.max(1));
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
        progress.on_frame(ProgressSnapshot {
            processed_samples,
            total_samples,
            completed_frames: frame_index + 1,
            total_frames,
        })?;
    }
    Ok(processed_samples)
}
