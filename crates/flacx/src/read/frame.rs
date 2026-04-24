use super::{
    ChannelAssignment, Error, FLAC_SYNC_CODE, FRAME_CHUNK_SIZE, FrameChunkResult,
    FrameHeaderNumber, FrameHeaderNumberKind, FrameIndex, ParsedFrame, Result, StreamInfo,
    SubframeHeader,
};
use crate::{
    DecodeConfig,
    crc::{crc8, crc16},
    progress::ProgressSink,
    reconstruct::{
        append_fixed_residual, append_lpc_residual, interleave_channels_into, unfold_residual,
    },
};

#[cfg(feature = "progress")]
use crate::{input::container_bits_from_valid_bits, progress::emit_progress};
use bitstream_io::{BigEndian, BitRead, BitReader};
#[cfg(test)]
use std::{cell::Cell, sync::{Condvar, Mutex}};
use std::{
    collections::HashMap,
    io::{Cursor, Read},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
};

#[derive(Debug)]
pub(super) struct DecodeWorkChunk {
    pub(super) sequence: usize,
    pub(super) start_frame_index: usize,
    pub(super) start_sample_number: u64,
    pub(super) stream_info: StreamInfo,
    pub(super) bytes: Arc<[u8]>,
}

#[derive(Debug)]
pub(super) struct DecodedWorkChunk {
    pub(super) sequence: usize,
    pub(super) start_frame_index: usize,
    pub(super) frame_block_sizes: Vec<u16>,
    pub(super) decoded_samples: Vec<i32>,
}

#[cfg(test)]
std::thread_local! {
    static WORKER_SCAN_FRAME_COUNT: Cell<usize> = const { Cell::new(0) };
}

struct DecodedFrame {
    bytes_consumed: usize,
    block_size: u16,
}

struct WorkerScratch {
    channels: Vec<Vec<i32>>,
}

impl WorkerScratch {
    fn new() -> Self {
        Self {
            channels: Vec::new(),
        }
    }

    #[cfg(test)]
    fn new_with_counter(counter: Option<&Arc<AtomicUsize>>) -> Self {
        if let Some(counter) = counter {
            counter.fetch_add(1, Ordering::Relaxed);
        }
        Self::new()
    }

    fn prepare(&mut self, channel_count: usize, block_size: usize) {
        self.channels.resize_with(channel_count, Vec::new);
        for channel in &mut self.channels {
            channel.clear();
            channel.reserve(block_size);
        }
    }
}

pub(super) type DecodeWorkSlab = DecodeWorkChunk;
pub(super) type DecodedWorkSlab = DecodedWorkChunk;

pub(super) enum DecodeWorkerRecv {
    Empty,
    Slab(Result<DecodedWorkChunk>),
}

pub(super) struct FrameDecodeWorkerPool {
    worker_senders: Vec<SyncSender<DecodeWorkChunk>>,
    result_receiver: Receiver<Result<DecodedWorkChunk>>,
    worker_handles: Vec<thread::JoinHandle<()>>,
    next_worker: usize,
}

#[cfg(test)]
pub(super) type WorkerReceiveGate = Arc<(Mutex<bool>, Condvar)>;

#[cfg(test)]
pub(super) struct WorkerReceiveHold {
    gate: WorkerReceiveGate,
}

#[cfg(test)]
impl WorkerReceiveHold {
    pub(super) fn release(&self) {
        FrameDecodeWorkerPool::release_receive_gate(&self.gate);
    }
}

#[cfg(test)]
impl Drop for WorkerReceiveHold {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
fn wait_for_worker_receive_gate(gate: &WorkerReceiveGate) {
    let (lock, condvar) = &**gate;
    let mut open = lock.lock().unwrap();
    while !*open {
        open = condvar.wait(open).unwrap();
    }
}

impl FrameDecodeWorkerPool {
    pub(super) fn new(worker_count: usize, queue_depth: usize) -> Self {
        #[cfg(test)]
        {
            Self::new_with_receive_gate(worker_count, queue_depth, None)
        }

        #[cfg(not(test))]
        {
            Self::new_with_receive_gate(worker_count, queue_depth)
        }
    }

    #[cfg(test)]
    fn new_with_receive_gate(
        worker_count: usize,
        queue_depth: usize,
        receive_gate: Option<WorkerReceiveGate>,
    ) -> Self {
        Self::new_with_receive_gate_impl(worker_count, queue_depth, receive_gate)
    }

    #[cfg(not(test))]
    fn new_with_receive_gate(worker_count: usize, queue_depth: usize) -> Self {
        Self::new_with_receive_gate_impl(worker_count, queue_depth)
    }

    #[cfg(test)]
    fn new_with_receive_gate_impl(
        worker_count: usize,
        queue_depth: usize,
        receive_gate: Option<WorkerReceiveGate>,
    ) -> Self {
        Self::new_with_receive_gate_and_scratch_counter_impl(
            worker_count,
            queue_depth,
            receive_gate,
            None,
        )
    }

    #[cfg(test)]
    fn new_with_receive_gate_and_scratch_counter_impl(
        worker_count: usize,
        queue_depth: usize,
        receive_gate: Option<WorkerReceiveGate>,
        scratch_create_count: Option<Arc<AtomicUsize>>,
    ) -> Self {
        let queue_depth = queue_depth.max(1);
        let (result_sender, result_receiver) = mpsc::channel::<Result<DecodedWorkChunk>>();
        let mut worker_senders = Vec::with_capacity(worker_count);
        let mut worker_handles = Vec::with_capacity(worker_count);

        for _ in 0..worker_count.max(1) {
            let (sender, receiver) = mpsc::sync_channel::<DecodeWorkChunk>(queue_depth);
            let result_sender = result_sender.clone();
            let receive_gate = receive_gate.clone();
            let scratch_create_count = scratch_create_count.clone();
            worker_handles.push(thread::spawn(move || {
                let mut scratch = WorkerScratch::new_with_counter(scratch_create_count.as_ref());
                if let Some(gate) = receive_gate.as_ref() {
                    wait_for_worker_receive_gate(gate);
                }
                while let Ok(chunk) = receiver.recv() {
                    if result_sender
                        .send(decode_work_chunk_with_scratch(chunk, &mut scratch))
                        .is_err()
                    {
                        return;
                    }
                    if let Some(gate) = receive_gate.as_ref() {
                        wait_for_worker_receive_gate(gate);
                    }
                }
            }));
            worker_senders.push(sender);
        }

        drop(result_sender);

        Self {
            worker_senders,
            result_receiver,
            worker_handles,
            next_worker: 0,
        }
    }

    #[cfg(not(test))]
    fn new_with_receive_gate_impl(worker_count: usize, queue_depth: usize) -> Self {
        let queue_depth = queue_depth.max(1);
        let (result_sender, result_receiver) = mpsc::channel::<Result<DecodedWorkChunk>>();
        let mut worker_senders = Vec::with_capacity(worker_count);
        let mut worker_handles = Vec::with_capacity(worker_count);

        for _ in 0..worker_count.max(1) {
            let (sender, receiver) = mpsc::sync_channel::<DecodeWorkChunk>(queue_depth);
            let result_sender = result_sender.clone();
            worker_handles.push(thread::spawn(move || {
                let mut scratch = WorkerScratch::new();
                while let Ok(chunk) = receiver.recv() {
                    if result_sender
                        .send(decode_work_chunk_with_scratch(chunk, &mut scratch))
                        .is_err()
                    {
                        return;
                    }
                }
            }));
            worker_senders.push(sender);
        }

        drop(result_sender);

        Self {
            worker_senders,
            result_receiver,
            worker_handles,
            next_worker: 0,
        }
    }

    #[cfg(test)]
    pub(super) fn new_with_blocked_receives(
        worker_count: usize,
        queue_depth: usize,
    ) -> (Self, WorkerReceiveHold) {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        (
            Self::new_with_receive_gate(worker_count, queue_depth, Some(Arc::clone(&gate))),
            WorkerReceiveHold { gate },
        )
    }

    pub(super) fn submit(&mut self, slab: DecodeWorkChunk) -> Result<()> {
        let sender = &self.worker_senders[self.next_worker % self.worker_senders.len()];
        self.next_worker = self.next_worker.wrapping_add(1);
        sender
            .send(slab)
            .map_err(|_| Error::Thread("decode worker channel closed unexpectedly".into()))
    }

    pub(super) fn try_submit(
        &mut self,
        slab: DecodeWorkChunk,
    ) -> std::result::Result<(), mpsc::TrySendError<DecodeWorkChunk>> {
        let sender = &self.worker_senders[self.next_worker % self.worker_senders.len()];
        match sender.try_send(slab) {
            Ok(()) => {
                self.next_worker = self.next_worker.wrapping_add(1);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn try_recv(&self) -> DecodeWorkerRecv {
        match self.result_receiver.try_recv() {
            Ok(slab) => DecodeWorkerRecv::Slab(slab),
            Err(mpsc::TryRecvError::Empty) => DecodeWorkerRecv::Empty,
            Err(mpsc::TryRecvError::Disconnected) => DecodeWorkerRecv::Slab(Err(Error::Thread(
                "decode worker result channel closed unexpectedly".into(),
            ))),
        }
    }

    pub(super) fn recv(&self) -> Result<DecodedWorkChunk> {
        self.result_receiver
            .recv()
            .map_err(|_| Error::Thread("decode worker result channel closed unexpectedly".into()))?
    }

    #[cfg(test)]
    pub(super) fn release_receive_gate(gate: &WorkerReceiveGate) {
        let (lock, condvar) = &**gate;
        *lock.lock().unwrap() = true;
        condvar.notify_all();
    }
}

impl Drop for FrameDecodeWorkerPool {
    fn drop(&mut self) {
        self.worker_senders.clear();
        for handle in self.worker_handles.drain(..) {
            let _ = handle.join();
        }
    }
}

pub(super) fn decode_work_chunk(chunk: DecodeWorkChunk) -> Result<DecodedWorkChunk> {
    let mut scratch = WorkerScratch::new();
    decode_work_chunk_with_scratch(chunk, &mut scratch)
}

fn decode_work_chunk_with_scratch(
    chunk: DecodeWorkChunk,
    scratch: &mut WorkerScratch,
) -> Result<DecodedWorkChunk> {
    let mut decoded_samples = Vec::new();
    let mut decoded_block_sizes = Vec::new();
    let mut cursor = 0usize;
    let mut expected_sample_number = chunk.start_sample_number;

    while cursor < chunk.bytes.len() {
        let decoded = decode_frame_into(
            &chunk.bytes[cursor..],
            chunk.stream_info,
            (chunk.start_frame_index + decoded_block_sizes.len()) as u64,
            expected_sample_number,
            &mut decoded_samples,
            scratch,
        )?;
        cursor += decoded.bytes_consumed;
        expected_sample_number =
            expected_sample_number.saturating_add(u64::from(decoded.block_size));
        decoded_block_sizes.push(decoded.block_size);
    }

    Ok(DecodedWorkChunk {
        sequence: chunk.sequence,
        start_frame_index: chunk.start_frame_index,
        frame_block_sizes: decoded_block_sizes,
        decoded_samples,
    })
}

pub(super) fn decode_work_slab(slab: DecodeWorkSlab) -> Result<DecodedWorkSlab> {
    decode_work_chunk(slab)
}

#[allow(dead_code)]
pub(super) fn index_frames(
    bytes: &[u8],
    frame_offset: usize,
    stream_info: StreamInfo,
) -> Result<Vec<FrameIndex>> {
    let mut expected_frame_number = 0u64;
    let mut processed_samples = 0usize;
    let mut cursor = frame_offset;
    let mut frames = Vec::new();

    while processed_samples < stream_info.total_samples as usize {
        let frame = scan_frame(
            &bytes[cursor..],
            stream_info,
            expected_frame_number,
            processed_samples as u64,
        )?;
        frames.push(FrameIndex {
            header_number: frame.header_number,
            offset: cursor,
            header_bytes_consumed: frame.header_bytes_consumed,
            bytes_consumed: frame.bytes_consumed,
            block_size: frame.block_size,
            bits_per_sample: frame.bits_per_sample,
            assignment: frame.assignment,
        });
        cursor += frame.bytes_consumed;
        processed_samples += usize::from(frame.block_size);
        expected_frame_number += 1;
    }

    if processed_samples != stream_info.total_samples as usize {
        return Err(Error::Decode(format!(
            "decoded sample count mismatch: expected {}, got {processed_samples}",
            stream_info.total_samples
        )));
    }

    Ok(frames)
}

#[allow(dead_code)]
pub(super) fn decode_frames_parallel<P>(
    bytes: Arc<[u8]>,
    frames: Arc<[FrameIndex]>,
    stream_info: StreamInfo,
    config: DecodeConfig,
    progress: &mut P,
    samples: &mut Vec<i32>,
) -> Result<()>
where
    P: ProgressSink,
{
    #[cfg(not(feature = "progress"))]
    let _ = stream_info;
    if frames.is_empty() {
        return Ok(());
    }

    samples.reserve(total_interleaved_sample_count(&frames));
    let worker_count = config.threads().max(1).min(frames.len());
    if worker_count == 1 || frames.len() <= FRAME_CHUNK_SIZE {
        let mut processed_samples = 0u64;
        #[cfg(feature = "progress")]
        let mut input_bytes_read = 0u64;
        #[cfg(feature = "progress")]
        let total_frames = frames.len();
        let indexed_total_samples = frames
            .iter()
            .map(|frame| u64::from(frame.block_size))
            .sum::<u64>();
        // The frame index is the authoritative decode schedule here, so the
        // single-threaded fast path should validate against its summed block
        // sizes instead of assuming STREAMINFO's advertised total still matches.
        #[cfg(feature = "progress")]
        for (frame_offset, frame) in frames.iter().enumerate() {
            let frame_bytes = &bytes[frame.offset..frame.offset + frame.bytes_consumed];
            decode_frame_samples_into(frame_bytes, frame, samples)?;
            processed_samples += u64::from(frame.block_size);
            input_bytes_read = input_bytes_read.saturating_add(frame.bytes_consumed as u64);
            emit_progress!(
                progress,
                crate::progress::ProgressSnapshot {
                    processed_samples,
                    total_samples: stream_info.total_samples,
                    completed_frames: frame_offset + 1,
                    total_frames,
                    input_bytes_read,
                    output_bytes_written: pcm_output_bytes(stream_info, processed_samples),
                }
            )?;
        }
        #[cfg(not(feature = "progress"))]
        for frame in frames.iter() {
            let frame_bytes = &bytes[frame.offset..frame.offset + frame.bytes_consumed];
            decode_frame_samples_into(frame_bytes, frame, samples)?;
            processed_samples += u64::from(frame.block_size);
        }
        debug_assert_eq!(processed_samples, indexed_total_samples);
        return Ok(());
    }

    let next_chunk = Arc::new(AtomicUsize::new(0));

    thread::scope(|scope| -> Result<()> {
        let (sender, receiver) = mpsc::channel::<Result<FrameChunkResult>>();

        for _ in 0..worker_count {
            let sender = sender.clone();
            let next_chunk = Arc::clone(&next_chunk);
            let bytes = Arc::clone(&bytes);
            let frames = Arc::clone(&frames);

            scope.spawn(move || {
                loop {
                    let chunk_start = next_chunk.fetch_add(FRAME_CHUNK_SIZE, Ordering::Relaxed);
                    if chunk_start >= frames.len() {
                        break;
                    }
                    let chunk_end = (chunk_start + FRAME_CHUNK_SIZE).min(frames.len());

                    let mut decoded_samples = Vec::with_capacity(total_interleaved_sample_count(
                        &frames[chunk_start..chunk_end],
                    ));
                    for frame_index in chunk_start..chunk_end {
                        let frame = &frames[frame_index];
                        let frame_bytes = &bytes[frame.offset..frame.offset + frame.bytes_consumed];
                        if let Err(error) =
                            decode_frame_samples_into(frame_bytes, frame, &mut decoded_samples)
                        {
                            let _ = sender.send(Err(error));
                            return;
                        }
                    }

                    if sender
                        .send(Ok(FrameChunkResult {
                            start_index: chunk_start,
                            frame_count: chunk_end - chunk_start,
                            decoded_samples,
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
        #[cfg(feature = "progress")]
        let mut input_bytes_read = 0u64;
        let mut pending: HashMap<usize, FrameChunkResult> = HashMap::new();

        while next_expected < frames.len() {
            if let Some(chunk_result) = pending.remove(&next_expected) {
                let chunk_len = chunk_result.frame_count;
                #[cfg(feature = "progress")]
                {
                    processed_samples = process_frame_chunk_result(
                        samples,
                        chunk_result,
                        &frames[next_expected..next_expected + chunk_len],
                        processed_samples,
                        &mut input_bytes_read,
                        progress,
                        FrameProgressWindow {
                            stream_info,
                            start_index: next_expected,
                            total_frames: frames.len(),
                        },
                    )?;
                }
                #[cfg(not(feature = "progress"))]
                {
                    processed_samples = process_frame_chunk_result(
                        samples,
                        chunk_result,
                        &frames[next_expected..next_expected + chunk_len],
                        processed_samples,
                        progress,
                    )?;
                }
                next_expected += chunk_len;
                continue;
            }

            let frame_chunk = receiver.recv().map_err(|_| {
                Error::Thread("frame worker channel closed before all frames were decoded".into())
            })??;
            pending.insert(frame_chunk.start_index, frame_chunk);

            while let Some(chunk_result) = pending.remove(&next_expected) {
                let chunk_len = chunk_result.frame_count;
                #[cfg(feature = "progress")]
                {
                    processed_samples = process_frame_chunk_result(
                        samples,
                        chunk_result,
                        &frames[next_expected..next_expected + chunk_len],
                        processed_samples,
                        &mut input_bytes_read,
                        progress,
                        FrameProgressWindow {
                            stream_info,
                            start_index: next_expected,
                            total_frames: frames.len(),
                        },
                    )?;
                }
                #[cfg(not(feature = "progress"))]
                {
                    processed_samples = process_frame_chunk_result(
                        samples,
                        chunk_result,
                        &frames[next_expected..next_expected + chunk_len],
                        processed_samples,
                        progress,
                    )?;
                }
                next_expected += chunk_len;
            }
        }

        Ok(())
    })
}

#[cfg(feature = "progress")]
fn process_frame_chunk_result<P>(
    samples: &mut Vec<i32>,
    chunk: FrameChunkResult,
    frames: &[FrameIndex],
    mut processed_samples: u64,
    input_bytes_read: &mut u64,
    progress: &mut P,
    progress_window: FrameProgressWindow,
) -> Result<u64>
where
    P: ProgressSink,
{
    samples.extend(chunk.decoded_samples);
    for (frame_offset, frame) in frames.iter().enumerate() {
        processed_samples += u64::from(frame.block_size);
        *input_bytes_read = (*input_bytes_read).saturating_add(frame.bytes_consumed as u64);
        emit_progress!(
            progress,
            crate::progress::ProgressSnapshot {
                processed_samples,
                total_samples: progress_window.stream_info.total_samples,
                completed_frames: progress_window.start_index + frame_offset + 1,
                total_frames: progress_window.total_frames,
                input_bytes_read: *input_bytes_read,
                output_bytes_written: pcm_output_bytes(
                    progress_window.stream_info,
                    processed_samples,
                ),
            }
        )?;
    }
    Ok(processed_samples)
}

#[cfg(not(feature = "progress"))]
fn process_frame_chunk_result<P>(
    samples: &mut Vec<i32>,
    chunk: FrameChunkResult,
    frames: &[FrameIndex],
    mut processed_samples: u64,
    _progress: &mut P,
) -> Result<u64>
where
    P: ProgressSink,
{
    samples.extend(chunk.decoded_samples);
    for frame in frames {
        processed_samples += u64::from(frame.block_size);
    }
    Ok(processed_samples)
}

#[cfg(feature = "progress")]
#[derive(Clone, Copy)]
struct FrameProgressWindow {
    stream_info: StreamInfo,
    start_index: usize,
    total_frames: usize,
}

fn total_interleaved_sample_count(frames: &[FrameIndex]) -> usize {
    frames
        .iter()
        .map(|frame| usize::from(frame.block_size) * frame.assignment.channel_count())
        .sum()
}

#[cfg(feature = "progress")]
fn pcm_output_bytes(stream_info: StreamInfo, processed_samples: u64) -> u64 {
    let bytes_per_sample = u64::from(container_bits_from_valid_bits(u16::from(
        stream_info.bits_per_sample,
    ))) / 8;
    processed_samples
        .saturating_mul(u64::from(stream_info.channels))
        .saturating_mul(bytes_per_sample)
}

pub(super) fn scan_frame(
    bytes: &[u8],
    stream_info: StreamInfo,
    expected_frame_number: u64,
    expected_sample_number: u64,
) -> Result<ParsedFrame> {
    #[cfg(test)]
    WORKER_SCAN_FRAME_COUNT.with(|count| count.set(count.get() + 1));
    let parsed = parse_frame_header(
        bytes,
        stream_info,
        expected_frame_number,
        expected_sample_number,
        None,
    )?;
    let mut reader = BitReader::endian(Cursor::new(&bytes[parsed.bytes_consumed..]), BigEndian);

    for bits_per_channel in channel_bits_per_sample(parsed.assignment, parsed.bits_per_sample)
        .into_iter()
        .take(parsed.assignment.channel_count())
    {
        skip_subframe(&mut reader, bits_per_channel, parsed.block_size)?;
    }

    reader.byte_align();
    let footer_pos = reader.aligned_reader().position() as usize;
    let footer_start = parsed.bytes_consumed + footer_pos;
    let expected_crc = u16::from_be_bytes([
        read_exact_byte(reader.aligned_reader())?,
        read_exact_byte(reader.aligned_reader())?,
    ]);
    if crc16(&bytes[..footer_start]) != expected_crc {
        return Err(Error::InvalidFlac("frame footer CRC16 mismatch"));
    }

    Ok(ParsedFrame {
        header_bytes_consumed: parsed.header_bytes_consumed,
        bytes_consumed: footer_start + 2,
        ..parsed
    })
}

pub(super) fn parse_frame_header(
    bytes: &[u8],
    stream_info: StreamInfo,
    expected_frame_number: u64,
    expected_sample_number: u64,
    expected_kind: Option<FrameHeaderNumberKind>,
) -> Result<ParsedFrame> {
    if bytes.len() < 2 {
        return Err(Error::InvalidFlac("unexpected EOF while reading frames"));
    }

    let mut reader = BitReader::endian(Cursor::new(bytes), BigEndian);
    let sync_code: u16 = reader.read_unsigned_var(14)?;
    if sync_code != FLAC_SYNC_CODE {
        return Err(Error::InvalidFlac("invalid frame sync code"));
    }
    if reader.read_bit()? {
        return Err(Error::InvalidFlac("frame header reserved bit must be zero"));
    }
    let is_variable_blocksize = reader.read_bit()?;

    let block_size_bits: u8 = reader.read_unsigned_var(4)?;
    let sample_rate_bits: u8 = reader.read_unsigned_var(4)?;
    let assignment_bits: u8 = reader.read_unsigned_var(4)?;
    let bits_per_sample_bits: u8 = reader.read_unsigned_var(3)?;
    if reader.read_bit()? {
        return Err(Error::InvalidFlac("frame header reserved bit must be zero"));
    }

    reader.byte_align();
    let (coded_number, utf8_len) = decode_utf8_number(reader.aligned_reader())?;
    let header_number = if is_variable_blocksize {
        FrameHeaderNumber {
            kind: FrameHeaderNumberKind::SampleNumber,
            value: coded_number,
        }
    } else {
        FrameHeaderNumber {
            kind: FrameHeaderNumberKind::FrameNumber,
            value: coded_number,
        }
    };

    if let Some(expected_kind) = expected_kind
        && header_number.kind != expected_kind
    {
        return Err(Error::Decode(format!(
            "expected {}-coded frame header, found {}-coded frame header",
            expected_kind.label(),
            header_number.kind.label()
        )));
    }

    let expected_number = match header_number.kind {
        FrameHeaderNumberKind::FrameNumber => expected_frame_number,
        FrameHeaderNumberKind::SampleNumber => expected_sample_number,
    };
    if header_number.value != expected_number {
        return Err(Error::Decode(format!(
            "expected {} {expected_number}, found {}",
            header_number.kind.label(),
            header_number.value
        )));
    }

    let block_size = decode_block_size(block_size_bits, reader.aligned_reader())?;
    let sample_rate = decode_sample_rate(
        sample_rate_bits,
        reader.aligned_reader(),
        stream_info.sample_rate,
    )?;
    let bits_per_sample =
        decode_bits_per_sample(bits_per_sample_bits, stream_info.bits_per_sample)?;
    let assignment = decode_channel_assignment(assignment_bits)?;

    let header_end = 4usize
        + utf8_len
        + block_size_extra_len(block_size_bits)
        + sample_rate_extra_len(sample_rate_bits);
    let header_crc = read_exact_byte(reader.aligned_reader())?;
    if crc8(&bytes[..header_end]) != header_crc {
        return Err(Error::InvalidFlac("frame header CRC8 mismatch"));
    }

    if sample_rate != stream_info.sample_rate {
        return Err(Error::UnsupportedFlac(format!(
            "sample rate changed mid-stream: expected {}, found {sample_rate}",
            stream_info.sample_rate
        )));
    }

    Ok(ParsedFrame {
        header_number,
        block_size,
        bits_per_sample,
        assignment,
        header_bytes_consumed: header_end + 1,
        bytes_consumed: header_end + 1,
    })
}

fn decode_frame_into(
    bytes: &[u8],
    stream_info: StreamInfo,
    expected_frame_number: u64,
    expected_sample_number: u64,
    output: &mut Vec<i32>,
    scratch: &mut WorkerScratch,
) -> Result<DecodedFrame> {
    let parsed = parse_frame_header(
        bytes,
        stream_info,
        expected_frame_number,
        expected_sample_number,
        None,
    )?;
    let mut reader = BitReader::endian(
        Cursor::new(&bytes[parsed.header_bytes_consumed..]),
        BigEndian,
    );
    let subframe_bps = channel_bits_per_sample(parsed.assignment, parsed.bits_per_sample);
    scratch.prepare(
        parsed.assignment.channel_count(),
        usize::from(parsed.block_size),
    );

    for (channel, bits_per_channel) in scratch.channels.iter_mut().zip(subframe_bps.into_iter()) {
        decode_subframe_into(&mut reader, bits_per_channel, parsed.block_size, channel)?;
    }

    interleave_channels_into(parsed.assignment, &scratch.channels, output)?;

    reader.byte_align();
    let footer_pos = reader.aligned_reader().position() as usize;
    let footer_start = parsed.header_bytes_consumed + footer_pos;
    let expected_crc = u16::from_be_bytes([
        read_exact_byte(reader.aligned_reader())?,
        read_exact_byte(reader.aligned_reader())?,
    ]);
    if crc16(&bytes[..footer_start]) != expected_crc {
        return Err(Error::InvalidFlac("frame footer CRC16 mismatch"));
    }

    Ok(DecodedFrame {
        bytes_consumed: footer_start + 2,
        block_size: parsed.block_size,
    })
}

fn decode_frame_samples_into(
    bytes: &[u8],
    frame: &FrameIndex,
    output: &mut Vec<i32>,
) -> Result<()> {
    let mut reader = BitReader::endian(
        Cursor::new(&bytes[frame.header_bytes_consumed..]),
        BigEndian,
    );
    let subframe_bps = channel_bits_per_sample(frame.assignment, frame.bits_per_sample);
    let mut channels = Vec::with_capacity(frame.assignment.channel_count());
    for bits_per_channel in subframe_bps
        .into_iter()
        .take(frame.assignment.channel_count())
    {
        channels.push(decode_subframe(
            &mut reader,
            bits_per_channel,
            frame.block_size,
        )?);
    }
    interleave_channels_into(frame.assignment, &channels, output)
}

impl FrameHeaderNumberKind {
    fn label(self) -> &'static str {
        match self {
            Self::FrameNumber => "frame number",
            Self::SampleNumber => "sample number",
        }
    }
}

fn decode_subframe<R: Read>(
    reader: &mut BitReader<R, BigEndian>,
    bits_per_sample: u8,
    block_size: u16,
) -> Result<Vec<i32>> {
    let mut samples = Vec::with_capacity(usize::from(block_size));
    decode_subframe_into(reader, bits_per_sample, block_size, &mut samples)?;
    Ok(samples)
}

fn decode_subframe_into<R: Read>(
    reader: &mut BitReader<R, BigEndian>,
    bits_per_sample: u8,
    block_size: u16,
    output: &mut Vec<i32>,
) -> Result<()> {
    let header = parse_subframe_header(reader, bits_per_sample)?;
    output.clear();

    match header.kind {
        0b000000 => {
            let sample = read_signed_sample(reader, header.effective_bps)?;
            output.resize(usize::from(block_size), sample);
        }
        0b000001 => {
            for _ in 0..block_size {
                output.push(read_signed_sample(reader, header.effective_bps)?);
            }
        }
        0b001000..=0b001100 => {
            let order = header.kind - 0b001000;
            read_warmup_into(reader, header.effective_bps, order, output)?;
            visit_residuals(reader, block_size, order, |residual| {
                append_fixed_residual(output, order, residual)
            })?;
        }
        0b100000..=0b111111 => {
            let order = header.kind - 0b100000 + 1;
            read_warmup_into(reader, header.effective_bps, order, output)?;
            let precision_minus_one: u8 = reader.read_unsigned_var(4)?;
            if precision_minus_one == 0b1111 {
                return Err(Error::UnsupportedFlac(
                    "LPC precision escape code is out of scope".into(),
                ));
            }
            let shift: i8 = reader.read_signed_var(5)?;
            let precision = precision_minus_one + 1;
            let mut coefficients = Vec::with_capacity(usize::from(order));
            for _ in 0..order {
                coefficients.push(reader.read_signed_var::<i16>(u32::from(precision))?);
            }
            visit_residuals(reader, block_size, order, |residual| {
                append_lpc_residual(output, shift, &coefficients, residual)
            })?;
        }
        _ => {
            return Err(Error::UnsupportedFlac(format!(
                "subframe type {kind:#08b} is out of scope",
                kind = header.kind
            )));
        }
    }

    if header.wasted_bits > 0 {
        for sample in output {
            *sample = i32::try_from(i64::from(*sample) << header.wasted_bits)
                .map_err(|_| Error::Decode("wasted-bit restoration overflowed".into()))?;
        }
    }

    Ok(())
}

fn skip_subframe<R: Read>(
    reader: &mut BitReader<R, BigEndian>,
    bits_per_sample: u8,
    block_size: u16,
) -> Result<()> {
    let header = parse_subframe_header(reader, bits_per_sample)?;

    match header.kind {
        0b000000 => {
            let _ = read_signed_sample(reader, header.effective_bps)?;
        }
        0b000001 => {
            for _ in 0..block_size {
                let _ = read_signed_sample(reader, header.effective_bps)?;
            }
        }
        0b001000..=0b001100 => {
            let order = header.kind - 0b001000;
            for _ in 0..order {
                let _ = read_signed_sample(reader, header.effective_bps)?;
            }
            skip_residual(reader, block_size, order)?;
        }
        0b100000..=0b111111 => {
            let order = header.kind - 0b100000 + 1;
            for _ in 0..order {
                let _ = read_signed_sample(reader, header.effective_bps)?;
            }
            let precision_minus_one: u8 = reader.read_unsigned_var(4)?;
            if precision_minus_one == 0b1111 {
                return Err(Error::UnsupportedFlac(
                    "LPC precision escape code is out of scope".into(),
                ));
            }
            let _shift: i8 = reader.read_signed_var(5)?;
            let precision = precision_minus_one + 1;
            for _ in 0..order {
                let _ = reader.read_signed_var::<i16>(u32::from(precision))?;
            }
            skip_residual(reader, block_size, order)?;
        }
        _ => {
            return Err(Error::UnsupportedFlac(format!(
                "subframe type {kind:#08b} is out of scope",
                kind = header.kind
            )));
        }
    }

    Ok(())
}

fn parse_subframe_header<R: Read>(
    reader: &mut BitReader<R, BigEndian>,
    bits_per_sample: u8,
) -> Result<SubframeHeader> {
    if reader.read_bit()? {
        return Err(Error::InvalidFlac("subframe padding bit must be zero"));
    }
    let kind: u8 = reader.read_unsigned_var(6)?;
    let wasted_bits = if reader.read_bit()? {
        reader.read_unary::<1>()? as usize + 1
    } else {
        0
    };
    let effective_bps = bits_per_sample
        .checked_sub(wasted_bits as u8)
        .ok_or_else(|| Error::UnsupportedFlac("subframe wasted bits exceed bit depth".into()))?;

    Ok(SubframeHeader {
        kind,
        wasted_bits,
        effective_bps,
    })
}

fn visit_residuals<R, F>(
    reader: &mut BitReader<R, BigEndian>,
    block_size: u16,
    predictor_order: u8,
    mut visit: F,
) -> Result<()>
where
    R: Read,
    F: FnMut(i32) -> Result<()>,
{
    let method: u8 = reader.read_unsigned_var(2)?;
    let parameter_bits = match method {
        0b00 => 4,
        0b01 => 5,
        _ => {
            return Err(Error::UnsupportedFlac(format!(
                "residual coding method {method:#04b} is out of scope"
            )));
        }
    };
    let escape_code = if parameter_bits == 4 {
        0b1111
    } else {
        0b1_1111
    };
    let partition_order: u8 = reader.read_unsigned_var(4)?;
    let partition_count = 1usize << usize::from(partition_order);
    let partition_len = usize::from(block_size) >> usize::from(partition_order);
    if partition_len == 0 {
        return Err(Error::InvalidFlac("residual partition length is zero"));
    }

    for partition in 0..partition_count {
        let warmup = if partition == 0 {
            usize::from(predictor_order)
        } else {
            0
        };
        if partition_len < warmup {
            return Err(Error::InvalidFlac(
                "residual partition is shorter than the predictor order",
            ));
        }
        let residual_count = partition_len - warmup;
        let parameter: u8 = reader.read_unsigned_var(parameter_bits)?;
        if parameter == escape_code {
            let bits: u8 = reader.read_unsigned_var(5)?;
            for _ in 0..residual_count {
                let residual = if bits == 0 {
                    0
                } else {
                    reader.read_signed_var::<i32>(u32::from(bits))?
                };
                visit(residual)?;
            }
        } else {
            for _ in 0..residual_count {
                let quotient = reader.read_unary::<1>()?;
                let remainder = if parameter == 0 {
                    0
                } else {
                    reader.read_unsigned_var::<u32>(u32::from(parameter))?
                };
                visit(unfold_residual(
                    (quotient << u32::from(parameter)) | remainder,
                ))?;
            }
        }
    }

    Ok(())
}

fn skip_residual<R: Read>(
    reader: &mut BitReader<R, BigEndian>,
    block_size: u16,
    predictor_order: u8,
) -> Result<()> {
    visit_residuals(reader, block_size, predictor_order, |_| Ok(()))
}

fn read_warmup_into<R: Read>(
    reader: &mut BitReader<R, BigEndian>,
    bits_per_sample: u8,
    order: u8,
    output: &mut Vec<i32>,
) -> Result<()> {
    for _ in 0..order {
        output.push(read_signed_sample(reader, bits_per_sample)?);
    }
    Ok(())
}

fn decode_block_size<R: Read>(code: u8, reader: &mut R) -> Result<u16> {
    Ok(match code {
        0b0000 => return Err(Error::InvalidFlac("reserved block-size code encountered")),
        0b0001 => 192,
        0b0010 => 576,
        0b0011 => 1152,
        0b0100 => 2304,
        0b0101 => 4608,
        0b0110 => u16::from(read_exact_byte(reader)?) + 1,
        0b0111 => u16::from_be_bytes([read_exact_byte(reader)?, read_exact_byte(reader)?]) + 1,
        0b1000 => 256,
        0b1001 => 512,
        0b1010 => 1024,
        0b1011 => 2048,
        0b1100 => 4096,
        0b1101 => 8192,
        0b1110 => 16384,
        0b1111 => 32768,
        _ => unreachable!(),
    })
}

fn block_size_extra_len(code: u8) -> usize {
    match code {
        0b0110 => 1,
        0b0111 => 2,
        _ => 0,
    }
}

fn decode_sample_rate<R: Read>(code: u8, reader: &mut R, stream_rate: u32) -> Result<u32> {
    Ok(match code {
        0b0000 => stream_rate,
        0b0001 => 88_200,
        0b0010 => 176_400,
        0b0011 => 192_000,
        0b0100 => 8_000,
        0b0101 => 16_000,
        0b0110 => 22_050,
        0b0111 => 24_000,
        0b1000 => 32_000,
        0b1001 => 44_100,
        0b1010 => 48_000,
        0b1011 => 96_000,
        0b1100 => u32::from(read_exact_byte(reader)?) * 1000,
        0b1101 => u32::from(u16::from_be_bytes([
            read_exact_byte(reader)?,
            read_exact_byte(reader)?,
        ])),
        0b1110 => {
            u32::from(u16::from_be_bytes([
                read_exact_byte(reader)?,
                read_exact_byte(reader)?,
            ])) * 10
        }
        0b1111 => {
            return Err(Error::UnsupportedFlac(
                "sample-rate code 0b1111 is out of scope".into(),
            ));
        }
        _ => unreachable!(),
    })
}

fn sample_rate_extra_len(code: u8) -> usize {
    match code {
        0b1100 => 1,
        0b1101 | 0b1110 => 2,
        _ => 0,
    }
}

pub(super) fn decode_bits_per_sample(code: u8, stream_bps: u8) -> Result<u8> {
    match code {
        0b000 => Ok(stream_bps),
        0b001 => Ok(8),
        0b010 => Ok(12),
        0b011 => Err(Error::InvalidFlac(
            "reserved bits-per-sample code encountered",
        )),
        0b100 => Ok(16),
        0b101 => Ok(20),
        0b110 => Ok(24),
        0b111 => Ok(32),
        _ => unreachable!(),
    }
}

pub(super) fn decode_channel_assignment(code: u8) -> Result<ChannelAssignment> {
    match code {
        0b0000..=0b0111 => Ok(ChannelAssignment::Independent(code + 1)),
        0b1000 => Ok(ChannelAssignment::LeftSide),
        0b1001 => Ok(ChannelAssignment::SideRight),
        0b1010 => Ok(ChannelAssignment::MidSide),
        0b1011..=0b1111 => Err(Error::UnsupportedFlac(format!(
            "channel assignment {code:#06b} is out of scope"
        ))),
        _ => unreachable!(),
    }
}

pub(super) fn channel_bits_per_sample(
    assignment: ChannelAssignment,
    bits_per_sample: u8,
) -> Vec<u8> {
    match assignment {
        ChannelAssignment::Independent(channels) => vec![bits_per_sample; usize::from(channels)],
        ChannelAssignment::LeftSide => vec![bits_per_sample, bits_per_sample + 1],
        ChannelAssignment::SideRight => vec![bits_per_sample + 1, bits_per_sample],
        ChannelAssignment::MidSide => vec![bits_per_sample, bits_per_sample + 1],
    }
}

fn read_signed_sample<R: Read>(
    reader: &mut BitReader<R, BigEndian>,
    bits_per_sample: u8,
) -> Result<i32> {
    Ok(reader.read_signed_var(u32::from(bits_per_sample))?)
}

fn read_exact_byte<R: Read>(reader: &mut R) -> Result<u8> {
    let mut byte = [0u8; 1];
    reader.read_exact(&mut byte)?;
    Ok(byte[0])
}

fn decode_utf8_number<R: Read>(reader: &mut R) -> Result<(u64, usize)> {
    let first = read_exact_byte(reader)?;
    let (mut value, additional) = match first {
        0x00..=0x7f => (u64::from(first), 0usize),
        0xc0..=0xdf => (u64::from(first & 0x1f), 1usize),
        0xe0..=0xef => (u64::from(first & 0x0f), 2usize),
        0xf0..=0xf7 => (u64::from(first & 0x07), 3usize),
        0xf8..=0xfb => (u64::from(first & 0x03), 4usize),
        0xfc..=0xfd => (u64::from(first & 0x01), 5usize),
        0xfe => (0, 6usize),
        _ => return Err(Error::InvalidFlac("invalid UTF-8-like frame number prefix")),
    };

    for _ in 0..additional {
        let continuation = read_exact_byte(reader)?;
        if continuation & 0b1100_0000 != 0b1000_0000 {
            return Err(Error::InvalidFlac(
                "invalid UTF-8-like frame number continuation byte",
            ));
        }
        value = (value << 6) | u64::from(continuation & 0b0011_1111);
    }

    Ok((value, additional + 1))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        EncoderConfig,
        convenience::encode_bytes_with_config,
        crc::{crc8, crc16},
    };

    use super::{
        DecodeWorkChunk, Error, FrameDecodeWorkerPool, StreamInfo, WORKER_SCAN_FRAME_COUNT,
        decode_work_chunk, index_frames,
    };

    fn wav_bytes_from_i16_samples(samples: &[i16]) -> Vec<u8> {
        let channels = 1u16;
        let sample_rate = 44_100u32;
        let bits_per_sample = 16u16;
        let block_align = channels * (bits_per_sample / 8);
        let byte_rate = sample_rate * u32::from(block_align);
        let data_bytes = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect::<Vec<_>>();
        let chunk_size = 36 + data_bytes.len() as u32;

        let mut wav = Vec::with_capacity(44 + data_bytes.len());
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&chunk_size.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&bits_per_sample.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data_bytes.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data_bytes);
        wav
    }

    fn encoded_fixture() -> (Vec<u8>, StreamInfo, usize, Vec<super::FrameIndex>) {
        let samples = (0i16..48).collect::<Vec<_>>();
        let wav = wav_bytes_from_i16_samples(&samples);
        let flac = encode_bytes_with_config(&EncoderConfig::default().with_block_size(16), &wav)
            .expect("encode test fixture");
        let (stream_info, _, frame_offset) =
            crate::read::metadata::parse_metadata(&flac, false).expect("parse metadata");
        let frames = index_frames(&flac, frame_offset, stream_info).expect("index fixture frames");
        (flac, stream_info, frame_offset, frames)
    }

    fn stream_info_with_total_samples(total_samples: u64) -> StreamInfo {
        StreamInfo {
            min_block_size: 16,
            max_block_size: 16,
            min_frame_size: 8,
            max_frame_size: 64,
            sample_rate: 44_100,
            channels: 1,
            bits_per_sample: 16,
            total_samples,
            md5: [0; 16],
        }
    }

    fn valid_test_chunk_bytes() -> Vec<u8> {
        let (bytes, _stream_info, _frame_offset, frames) = encoded_fixture();
        let frame = &frames[0];
        bytes[frame.offset..frame.offset + frame.bytes_consumed].to_vec()
    }

    fn reset_worker_decode_test_counters() {
        WORKER_SCAN_FRAME_COUNT.with(|count| count.set(0));
    }

    fn build_chunk(frame_count: usize) -> DecodeWorkChunk {
        let (bytes, stream_info, _frame_offset, frames) = encoded_fixture();
        let chunk_frames = &frames[..frame_count];
        let start = chunk_frames
            .first()
            .expect("chunk fixture has at least one frame")
            .offset;
        let end = chunk_frames
            .last()
            .expect("chunk fixture has at least one frame")
            .offset
            + chunk_frames
                .last()
                .expect("chunk fixture has at least one frame")
                .bytes_consumed;
        DecodeWorkChunk {
            sequence: 0,
            start_frame_index: 0,
            start_sample_number: 0,
            stream_info,
            bytes: Arc::from(bytes[start..end].to_vec()),
        }
    }

    fn rewrite_frame_as_sample_number_coded(
        frame_bytes: &mut [u8],
        header_bytes_consumed: usize,
        sample_number: u8,
    ) {
        frame_bytes[1] |= 0x01;
        frame_bytes[4] = sample_number;
        let header_crc_pos = header_bytes_consumed - 1;
        frame_bytes[header_crc_pos] = crc8(&frame_bytes[..header_crc_pos]);
        let footer_crc_pos = frame_bytes.len() - 2;
        let footer_crc = crc16(&frame_bytes[..footer_crc_pos]).to_be_bytes();
        frame_bytes[footer_crc_pos..].copy_from_slice(&footer_crc);
    }

    #[test]
    fn decode_work_chunk_rejects_bad_crc_in_single_worker_pass() {
        let mut bytes = valid_test_chunk_bytes();
        *bytes.last_mut().unwrap() ^= 0x01;

        let error = decode_work_chunk(DecodeWorkChunk {
            sequence: 0,
            start_frame_index: 0,
            start_sample_number: 0,
            stream_info: stream_info_with_total_samples(16),
            bytes: bytes.into(),
        })
        .unwrap_err();

        assert!(error.to_string().contains("CRC"));
    }

    #[test]
    fn decode_work_chunk_decodes_complete_frame_payload_without_scan_frame_roundtrip() {
        let chunk = DecodeWorkChunk {
            sequence: 0,
            start_frame_index: 0,
            start_sample_number: 0,
            stream_info: stream_info_with_total_samples(16),
            bytes: valid_test_chunk_bytes().into(),
        };
        reset_worker_decode_test_counters();

        let decoded = decode_work_chunk(chunk).unwrap();

        assert_eq!(decoded.start_frame_index, 0);
        assert_eq!(decoded.frame_block_sizes, vec![16]);
        assert!(!decoded.decoded_samples.is_empty());
        assert_eq!(WORKER_SCAN_FRAME_COUNT.with(|count| count.get()), 0);
    }

    #[test]
    fn frame_decode_worker_pool_reuses_worker_scratch_across_chunks() {
        let scratch_create_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut pool = FrameDecodeWorkerPool::new_with_receive_gate_and_scratch_counter_impl(
            1,
            2,
            None,
            Some(Arc::clone(&scratch_create_count)),
        );

        for sequence in 0..2 {
            pool.submit(DecodeWorkChunk {
                sequence,
                start_frame_index: 0,
                start_sample_number: 0,
                stream_info: stream_info_with_total_samples(16),
                bytes: valid_test_chunk_bytes().into(),
            })
            .unwrap();
        }

        for _ in 0..2 {
            let decoded = pool.recv().unwrap();
            assert_eq!(decoded.frame_block_sizes, vec![16]);
            assert!(!decoded.decoded_samples.is_empty());
        }

        assert_eq!(scratch_create_count.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn worker_decode_rejects_bad_crc_in_compressed_chunk() {
        let mut chunk = build_chunk(1);
        let bytes = Arc::make_mut(&mut chunk.bytes);
        let corrupt_index = bytes.len() - 3;
        bytes[corrupt_index] ^= 0x01;

        let error = decode_work_chunk(chunk).expect_err("chunk decode should fail on CRC mismatch");
        assert!(matches!(
            error,
            Error::InvalidFlac("frame footer CRC16 mismatch")
        ));
    }

    #[test]
    fn worker_decode_rejects_wrong_sample_number_progression_in_compressed_chunk() {
        let mut chunk = build_chunk(2);
        let header_sizes = {
            let (_stream_info, _, _frame_offset, frames) = encoded_fixture();
            vec![
                frames[0].header_bytes_consumed,
                frames[1].header_bytes_consumed,
            ]
        };
        let first_frame_len = {
            let (_bytes, _stream_info, _frame_offset, frames) = encoded_fixture();
            frames[0].bytes_consumed
        };

        let bytes = Arc::make_mut(&mut chunk.bytes);
        rewrite_frame_as_sample_number_coded(&mut bytes[..first_frame_len], header_sizes[0], 0);
        rewrite_frame_as_sample_number_coded(&mut bytes[first_frame_len..], header_sizes[1], 15);

        let error =
            decode_work_chunk(chunk).expect_err("chunk decode should fail on sample progression");
        assert!(matches!(
            error,
            Error::Decode(message) if message == "expected sample number 16, found 15"
        ));
    }
}
