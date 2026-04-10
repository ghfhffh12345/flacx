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
    input::EncodeWavData,
    model::encode_frame,
    plan::{EncodePlan, FrameCodedNumberKind, summary_from_stream_info},
    progress::{ProgressSink, ProgressSnapshot},
    write::{EncodedFrame, FlacWriter, FrameHeaderNumber},
};

const FRAME_CHUNK_SIZE: usize = 32;

struct EncodedChunk {
    start_frame: usize,
    frames: Vec<EncodedFrame>,
}

pub(crate) fn encode_prepared<W, P>(
    config: &EncoderConfig,
    input: EncodeWavData,
    output: W,
    progress: &mut P,
) -> Result<EncodeSummary>
where
    W: Write + Seek,
    P: ProgressSink,
{
    let EncodeWavData {
        wav,
        metadata,
        streaminfo_md5,
    } = input;
    let plan = EncodePlan::new(wav.spec, config.clone())?;
    let mut stream_info = plan.stream_info();
    stream_info.md5 = streaminfo_md5;
    let has_preserved_bundle = metadata.has_preserved_bundle();
    let metadata_blocks = metadata.flac_blocks();
    let mut writer = FlacWriter::new(
        output,
        stream_info,
        &metadata_blocks,
        plan.total_frames,
        !has_preserved_bundle,
    )?;

    if plan.total_frames == 0 {
        let (_, stream_info) = writer.finalize()?;
        return Ok(summary_from_stream_info(stream_info, 0));
    }

    let total_frames = plan.total_frames;
    encode_frames(config, plan, wav.samples, &mut writer, progress)?;

    let (_, stream_info) = writer.finalize()?;
    Ok(summary_from_stream_info(stream_info, total_frames))
}

fn encode_frames<W, P>(
    config: &EncoderConfig,
    plan: EncodePlan,
    samples: Vec<i32>,
    writer: &mut FlacWriter<W>,
    progress: &mut P,
) -> Result<()>
where
    W: Write + Seek,
    P: ProgressSink,
{
    let worker_count = config.threads.max(1).min(plan.total_frames.max(1));
    let next_frame = Arc::new(AtomicUsize::new(0));
    let samples: Arc<[i32]> = Arc::from(samples);
    let channels = usize::from(plan.spec.channels);

    thread::scope(|scope| -> Result<()> {
        let (sender, receiver) = mpsc::channel();
        for _ in 0..worker_count {
            let sender = sender.clone();
            let next_frame = Arc::clone(&next_frame);
            let samples = Arc::clone(&samples);
            let channels_in_scope = channels;
            let plan = plan.clone();

            scope.spawn(move || {
                loop {
                    let chunk_start = next_frame.fetch_add(FRAME_CHUNK_SIZE, Ordering::Relaxed);
                    if chunk_start >= plan.total_frames {
                        break;
                    }
                    let chunk_end = (chunk_start + FRAME_CHUNK_SIZE).min(plan.total_frames);
                    let mut encoded_frames = Vec::with_capacity(chunk_end - chunk_start);

                    for frame_index in chunk_start..chunk_end {
                        let frame_plan = plan.frame(frame_index);
                        let frame_samples = usize::from(frame_plan.block_size);
                        let sample_start = usize::try_from(frame_plan.sample_offset)
                            .expect("sample offset fits in usize")
                            * channels_in_scope;
                        let sample_end = sample_start + frame_samples * channels_in_scope;
                        let frame = encode_frame(
                            &samples[sample_start..sample_end],
                            plan.spec.channels,
                            plan.spec.bits_per_sample,
                            plan.spec.sample_rate,
                            match frame_plan.coded_number_kind {
                                FrameCodedNumberKind::FrameNumber => {
                                    FrameHeaderNumber::Frame(frame_plan.coded_number)
                                }
                                FrameCodedNumberKind::SampleNumber => {
                                    FrameHeaderNumber::Sample(frame_plan.coded_number)
                                }
                            },
                            plan.profile,
                        );
                        match frame {
                            Ok(frame) => encoded_frames.push(frame),
                            Err(error) => {
                                let _ = sender.send(Err(error));
                                return;
                            }
                        }
                    }

                    if sender
                        .send(Ok(EncodedChunk {
                            start_frame: chunk_start,
                            frames: encoded_frames,
                        }))
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }

        drop(sender);
        let mut next_expected = 0usize;
        let mut processed_samples = 0u64;
        let mut pending: BTreeMap<usize, EncodedChunk> = BTreeMap::new();
        while next_expected < plan.total_frames {
            let encoded_chunk = receiver.recv().map_err(|_| {
                Error::Thread("frame worker channel closed before all frames were encoded".into())
            })??;
            if encoded_chunk.start_frame == next_expected {
                processed_samples = write_encoded_chunk(
                    writer,
                    encoded_chunk,
                    processed_samples,
                    plan.spec.total_samples,
                    &mut next_expected,
                    plan.total_frames,
                    progress,
                )?;
                while let Some(chunk) = pending.remove(&next_expected) {
                    processed_samples = write_encoded_chunk(
                        writer,
                        chunk,
                        processed_samples,
                        plan.spec.total_samples,
                        &mut next_expected,
                        plan.total_frames,
                        progress,
                    )?;
                }
            } else {
                pending.insert(encoded_chunk.start_frame, encoded_chunk);
            }
        }

        Ok(())
    })
}

fn write_encoded_frame<W, P>(
    writer: &mut FlacWriter<W>,
    frame: &EncodedFrame,
    frame_index: usize,
    processed_samples: u64,
    total_samples: u64,
    total_frames: usize,
    progress: &mut P,
) -> Result<u64>
where
    W: Write + Seek,
    P: ProgressSink,
{
    writer.write_frame(
        frame_index,
        processed_samples,
        frame.sample_count,
        &frame.bytes,
    )?;
    let processed_samples = processed_samples + u64::from(frame.sample_count);
    progress.on_frame(ProgressSnapshot {
        processed_samples,
        total_samples,
        completed_frames: frame_index + 1,
        total_frames,
    })?;
    Ok(processed_samples)
}

fn write_encoded_chunk<W, P>(
    writer: &mut FlacWriter<W>,
    chunk: EncodedChunk,
    mut processed_samples: u64,
    total_samples: u64,
    next_expected: &mut usize,
    total_frames: usize,
    progress: &mut P,
) -> Result<u64>
where
    W: Write + Seek,
    P: ProgressSink,
{
    for frame in chunk.frames {
        processed_samples = write_encoded_frame(
            writer,
            &frame,
            *next_expected,
            processed_samples,
            total_samples,
            total_frames,
            progress,
        )?;
        *next_expected += 1;
    }
    Ok(processed_samples)
}
