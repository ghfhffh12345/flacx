use std::{
    io::{Read, Seek},
    sync::{Arc, Condvar, Mutex, mpsc},
};

use crate::{
    error::{Error, Result},
    input::{EncodePcmStream, PcmSpec},
    metadata::Metadata,
    model::ChannelAssignment,
    pcm::{is_supported_channel_mask, ordinary_channel_mask},
    stream_info::{MAX_STREAMINFO_SAMPLE_RATE, StreamInfo},
};

use self::{
    index::PushFrameOutcome,
    producer::{ProducerConfig, ProducerState},
    slab::DecodeSlabPlan,
};

const FLAC_MAGIC: &[u8; 4] = b"fLaC";
const STREAMINFO_BLOCK_TYPE: u8 = 0;
const FLAC_SYNC_CODE: u16 = 0b11_1111_1111_1110;
#[allow(dead_code)]
const FRAME_CHUNK_SIZE: usize = 128;
const FLAC_READ_CHUNK_SIZE: usize = 64 * 1024;
const DECODE_SLAB_MAX_INPUT_FRAMES: usize = 256;
const DECODE_SLAB_TARGET_PCM_FRAMES: usize = 1 << 20;
const DECODE_SLAB_MAX_INPUT_BYTES_FALLBACK: usize = FLAC_READ_CHUNK_SIZE * 4;
const DECODE_SESSION_QUEUE_DEPTH_MULTIPLIER: usize = 2;
const DECODE_SESSION_RESULT_BACKLOG_PER_WORKER: usize = 1;
const DECODE_SESSION_WINDOW_DEPTH: usize =
    DECODE_SESSION_QUEUE_DEPTH_MULTIPLIER + DECODE_SESSION_RESULT_BACKLOG_PER_WORKER + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameHeaderNumberKind {
    FrameNumber,
    SampleNumber,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameHeaderNumber {
    kind: FrameHeaderNumberKind,
    value: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
struct FrameIndex {
    header_number: FrameHeaderNumber,
    offset: usize,
    header_bytes_consumed: usize,
    bytes_consumed: usize,
    block_size: u16,
    bits_per_sample: u8,
    assignment: ChannelAssignment,
}

struct FrameChunkResult {
    start_index: usize,
    frame_count: usize,
    decoded_samples: Vec<i32>,
}

type SharedProducerProgress = Arc<(Mutex<ProducerProgressState>, Condvar)>;

#[derive(Debug, Default)]
struct ProducerProgressState {
    discovered_input_frames: usize,
    discovered_sample_number: u64,
    completed_input_frames: usize,
    producer_finished: bool,
    cancelled: bool,
    #[cfg(feature = "progress")]
    discovered_input_byte_totals: Vec<u64>,
}

struct DecodeSlabProducer<R> {
    reader: R,
    stream_info: StreamInfo,
    total_samples: u64,
    pending_bytes: Vec<u8>,
    pending_start: usize,
    producer: ProducerState,
    eof: bool,
    progress: SharedProducerProgress,
    discovered_input_frames: usize,
    discovered_sample_number: u64,
    #[cfg(feature = "progress")]
    discovered_input_bytes: u64,
}

struct ProducerExitGuard {
    progress: SharedProducerProgress,
}

impl ProducerExitGuard {
    fn new(progress: SharedProducerProgress) -> Self {
        Self { progress }
    }
}

impl Drop for ProducerExitGuard {
    fn drop(&mut self) {
        let (lock, condvar) = &*self.progress;
        let mut state = lock.lock().unwrap();
        state.producer_finished = true;
        condvar.notify_all();
    }
}

impl<R> DecodeSlabProducer<R> {
    fn inflight_slabs(&self) -> usize {
        self.producer.inflight_slabs()
    }

    fn into_background(
        self,
        reader: ChunkBackedReader,
    ) -> (R, DecodeSlabProducer<ChunkBackedReader>) {
        (
            self.reader,
            DecodeSlabProducer {
                reader,
                stream_info: self.stream_info,
                total_samples: self.total_samples,
                pending_bytes: self.pending_bytes,
                pending_start: self.pending_start,
                producer: self.producer,
                eof: self.eof,
                progress: self.progress,
                discovered_input_frames: self.discovered_input_frames,
                discovered_sample_number: self.discovered_sample_number,
                #[cfg(feature = "progress")]
                discovered_input_bytes: self.discovered_input_bytes,
            },
        )
    }
}

impl<R: Read> DecodeSlabProducer<R> {
    fn new(
        reader: R,
        stream_info: StreamInfo,
        total_samples: u64,
        producer: ProducerState,
        progress: SharedProducerProgress,
        #[cfg(feature = "progress")] frame_offset: u64,
    ) -> Self {
        Self {
            reader,
            stream_info,
            total_samples,
            pending_bytes: Vec::new(),
            pending_start: 0,
            producer,
            eof: false,
            progress,
            discovered_input_frames: 0,
            discovered_sample_number: 0,
            #[cfg(feature = "progress")]
            discovered_input_bytes: frame_offset,
        }
    }

    fn run(mut self, session_producer: session::SessionProducer) -> Result<()> {
        let _producer_exit = ProducerExitGuard::new(Arc::clone(&self.progress));
        loop {
            if !self.refresh_completed_input_progress() {
                break;
            }

            if let Some(plan) = self.read_next_slab_plan()? {
                if !session_producer.submit_tracked(&mut self.producer, plan)? {
                    break;
                }
                continue;
            }

            if self.eof || self.discovered_sample_number >= self.total_samples {
                break;
            }

            if !self.wait_for_completed_input_progress() {
                break;
            }
        }

        Ok(())
    }

    fn refresh_completed_input_progress(&mut self) -> bool {
        let (lock, _) = &*self.progress;
        let state = lock.lock().unwrap();
        let completed_input_frames = state.completed_input_frames;
        let cancelled = state.cancelled;
        drop(state);
        self.producer
            .retire_completed_input_frames(completed_input_frames);
        !cancelled
    }

    fn wait_for_completed_input_progress(&mut self) -> bool {
        let (lock, condvar) = &*self.progress;
        let mut state = lock.lock().unwrap();
        let completed_input_frames = state.completed_input_frames;
        while !state.cancelled && state.completed_input_frames == completed_input_frames {
            state = condvar.wait(state).unwrap();
        }
        let completed_input_frames = state.completed_input_frames;
        let cancelled = state.cancelled;
        drop(state);
        self.producer
            .retire_completed_input_frames(completed_input_frames);
        !cancelled
    }

    fn read_next_frame(&mut self) -> Result<Option<ParsedFrame>> {
        loop {
            match frame::scan_frame(
                &self.pending_bytes[self.pending_start..],
                self.stream_info,
                self.discovered_input_frames as u64,
                self.discovered_sample_number,
            ) {
                Ok(parsed) => return Ok(Some(parsed)),
                Err(Error::InvalidFlac("unexpected EOF while reading frames")) if !self.eof => {}
                Err(Error::Io(error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof && !self.eof => {}
                Err(error) => return Err(error),
            }

            let mut chunk = [0u8; FLAC_READ_CHUNK_SIZE];
            let read = self.reader.read(&mut chunk)?;
            if read == 0 {
                self.eof = true;
                if self.pending_bytes.is_empty() {
                    return Ok(None);
                }
                continue;
            }
            self.pending_bytes.extend_from_slice(&chunk[..read]);
        }
    }

    fn normalize_pending_bytes(&mut self) {
        if self.pending_start == self.pending_bytes.len() {
            self.pending_bytes.clear();
            self.pending_start = 0;
            return;
        }

        if self.pending_start != 0 {
            self.pending_bytes.drain(..self.pending_start);
            self.pending_start = 0;
        }
    }

    fn take_staged_slab_bytes(&mut self) -> Vec<u8> {
        let slab_end = self.pending_start;
        let remaining = self.pending_bytes.split_off(slab_end);
        let slab_bytes = std::mem::replace(&mut self.pending_bytes, remaining);
        self.pending_start = 0;
        slab_bytes
    }

    fn seal_staged_decode_slab_plan(&mut self, plan: DecodeSlabPlan) -> DecodeSlabPlan {
        plan.seal_bytes(self.take_staged_slab_bytes())
    }

    fn read_next_slab_plan(&mut self) -> Result<Option<DecodeSlabPlan>> {
        self.normalize_pending_bytes();
        while self.producer.has_capacity() && self.discovered_sample_number < self.total_samples {
            let Some(parsed) = self.read_next_frame()? else {
                break;
            };
            let frame_start = self.pending_start;
            let frame_end = frame_start + parsed.bytes_consumed;
            match self.producer.push_frame(parsed, frame_start)? {
                PushFrameOutcome::Pending => {
                    self.accept_indexed_frame(parsed, frame_end);
                }
                PushFrameOutcome::AcceptedAndSealed(plan) => {
                    self.accept_indexed_frame(parsed, frame_end);
                    return Ok(Some(self.seal_staged_decode_slab_plan(plan)));
                }
                PushFrameOutcome::SealedBeforeAdd(plan) => {
                    return Ok(Some(self.seal_staged_decode_slab_plan(plan)));
                }
            }
        }

        if self.discovered_sample_number >= self.total_samples || self.eof {
            if let Some(plan) = self.producer.finish() {
                return Ok(Some(self.seal_staged_decode_slab_plan(plan)));
            }
        }

        Ok(None)
    }

    fn accept_indexed_frame(&mut self, parsed: ParsedFrame, frame_end: usize) {
        self.discovered_input_frames += 1;
        self.discovered_sample_number += u64::from(parsed.block_size);
        #[cfg(feature = "progress")]
        {
            self.discovered_input_bytes = self
                .discovered_input_bytes
                .saturating_add(parsed.bytes_consumed as u64);
        }
        self.pending_start = frame_end;

        let (lock, condvar) = &*self.progress;
        let mut state = lock.lock().unwrap();
        state.discovered_input_frames = self.discovered_input_frames;
        state.discovered_sample_number = self.discovered_sample_number;
        #[cfg(feature = "progress")]
        state
            .discovered_input_byte_totals
            .push(self.discovered_input_bytes);
        condvar.notify_all();
    }
}

struct ChunkBackedReader {
    receiver: mpsc::Receiver<std::io::Result<Option<Vec<u8>>>>,
    chunk: Vec<u8>,
    chunk_start: usize,
    eof: bool,
}

impl ChunkBackedReader {
    fn new(receiver: mpsc::Receiver<std::io::Result<Option<Vec<u8>>>>) -> Self {
        Self {
            receiver,
            chunk: Vec::new(),
            chunk_start: 0,
            eof: false,
        }
    }
}

impl Read for ChunkBackedReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.eof {
            return Ok(0);
        }
        while self.chunk_start == self.chunk.len() {
            match self.receiver.recv() {
                Ok(Ok(Some(chunk))) => {
                    self.chunk = chunk;
                    self.chunk_start = 0;
                }
                Ok(Ok(None)) | Err(_) => {
                    self.eof = true;
                    return Ok(0);
                }
                Ok(Err(error)) => return Err(error),
            }
        }

        let read = buf.len().min(self.chunk.len() - self.chunk_start);
        buf[..read].copy_from_slice(&self.chunk[self.chunk_start..self.chunk_start + read]);
        self.chunk_start += read;
        if self.chunk_start == self.chunk.len() {
            self.chunk.clear();
            self.chunk_start = 0;
        }
        Ok(read)
    }
}

impl<R: std::fmt::Debug> std::fmt::Debug for FlacPcmStream<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlacPcmStream")
            .field("stream_info", &self.stream_info)
            .field("spec", &self.spec)
            .field("discovered_input_frames", &self.discovered_input_frames)
            .field("discovered_sample_number", &self.discovered_sample_number)
            .field("drained_input_frames", &self.completed_input_frames)
            .field("drained_pcm_frames", &self.drained_pcm_frames)
            .field("threads", &self.threads)
            .field(
                "pending_bytes_len",
                &self
                    .slab_producer
                    .as_ref()
                    .map_or(0, |producer| producer.pending_bytes.len()),
            )
            .field(
                "pending_start",
                &self
                    .slab_producer
                    .as_ref()
                    .map_or(0, |producer| producer.pending_start),
            )
            .field(
                "ready_slab_count",
                &self
                    .session
                    .as_ref()
                    .map_or(0, session::StreamingDecodeSession::ready_slab_count),
            )
            .field(
                "next_ready_slab_start_frame",
                &self.session.as_ref().map_or(
                    self.completed_input_frames,
                    session::StreamingDecodeSession::next_ready_slab_start_frame,
                ),
            )
            .field(
                "inflight_slabs",
                &self
                    .slab_producer
                    .as_ref()
                    .map_or(0, DecodeSlabProducer::inflight_slabs),
            )
            .field(
                "has_draining_slab",
                &self
                    .session
                    .as_ref()
                    .is_some_and(session::StreamingDecodeSession::has_draining_slab),
            )
            .field("has_streaming_session", &self.session.is_some())
            .field(
                "eof",
                &self
                    .slab_producer
                    .as_ref()
                    .is_none_or(|producer| producer.eof),
            )
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedFrame {
    header_number: FrameHeaderNumber,
    block_size: u16,
    bits_per_sample: u8,
    assignment: ChannelAssignment,
    header_bytes_consumed: usize,
    bytes_consumed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SubframeHeader {
    kind: u8,
    wasted_bits: usize,
    effective_bps: u8,
}

/// Options for [`FlacReader`] parsing and validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FlacReaderOptions {
    /// Reject malformed or non-conforming seektable metadata.
    pub strict_seektable_validation: bool,
    /// Require explicit provenance before restoring a non-ordinary channel mask.
    pub strict_channel_mask_provenance: bool,
}

/// Single-pass FLAC decode stream consumed by [`crate::Decoder`] and recompress flows.
pub trait DecodePcmStream: EncodePcmStream {
    /// Return the total number of FLAC frames expected from the input.
    fn total_input_frames(&self) -> usize;
    /// Return the number of FLAC frames decoded so far.
    fn completed_input_frames(&self) -> usize;
    /// Return the parsed STREAMINFO block for the input stream.
    fn stream_info(&self) -> StreamInfo;
    #[cfg(feature = "progress")]
    fn input_bytes_processed(&self) -> u64 {
        EncodePcmStream::input_bytes_processed(self)
    }
    /// Update the worker-thread count when the implementation supports it.
    fn set_threads(&mut self, _threads: usize) {}
    /// Optionally hand buffered decoded samples to the caller.
    fn take_decoded_samples(&mut self) -> Result<Option<(Vec<i32>, usize)>> {
        Ok(None)
    }
}

/// Owned decode-side handoff that keeps metadata and the PCM stream together.
///
/// `DecodeSource` is the decode counterpart to [`crate::EncodeSource`]. It is
/// usually created from [`FlacReader::into_decode_source`] and then passed to
/// [`crate::Decoder::decode_source`].
pub struct DecodeSource<S> {
    metadata: Metadata,
    stream: S,
}

impl<S> DecodeSource<S> {
    /// Create a new decode source from staged metadata and a PCM stream.
    #[must_use]
    pub fn new(metadata: Metadata, stream: S) -> Self {
        Self { metadata, stream }
    }

    /// Return the staged decode metadata.
    #[must_use]
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Return mutable access to the staged decode metadata.
    pub fn metadata_mut(&mut self) -> &mut Metadata {
        &mut self.metadata
    }

    /// Replace the staged metadata and return the updated source.
    #[must_use]
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Consume the source and return the metadata and stream.
    pub fn into_parts(self) -> (Metadata, S) {
        (self.metadata, self.stream)
    }
}

impl<S: DecodePcmStream> DecodeSource<S> {
    /// Return the PCM spec that will be decoded into an output container.
    #[must_use]
    pub fn spec(&self) -> PcmSpec {
        self.stream.spec()
    }
}

/// Reader façade for FLAC inputs.
///
/// `FlacReader` parses STREAMINFO plus user-visible metadata, exposes the
/// recovered [`PcmSpec`]-shaped stream description, and can then hand ownership
/// to either a decode or recompress source.
#[derive(Debug)]
pub struct FlacReader<R> {
    reader: R,
    frame_offset: u64,
    stream_info: StreamInfo,
    metadata: Metadata,
    spec: PcmSpec,
}

impl<R: Read + Seek> FlacReader<R> {
    /// Parse a FLAC stream with the default validation policy.
    pub fn new(reader: R) -> Result<Self> {
        read_flac_reader_with_options(reader, FlacReaderOptions::default())
    }

    /// Return the decoded PCM-facing stream description.
    #[must_use]
    pub fn spec(&self) -> PcmSpec {
        self.spec
    }

    /// Return the metadata recovered from FLAC metadata blocks.
    #[must_use]
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Return the parsed STREAMINFO block.
    #[must_use]
    pub fn stream_info(&self) -> StreamInfo {
        self.stream_info
    }

    /// Convert this reader into an owned decode source.
    pub fn into_decode_source(self) -> DecodeSource<impl DecodePcmStream> {
        let (metadata, _, _, stream) = self.into_session_parts();
        DecodeSource::new(metadata, stream)
    }

    /// Convert this reader into the owned FLAC recompress source.
    pub fn into_recompress_source(
        self,
    ) -> crate::recompress::FlacRecompressSource<impl DecodePcmStream> {
        crate::recompress::FlacRecompressSource::from_reader(self)
    }

    #[allow(dead_code)]
    pub(crate) fn into_pcm_stream(self) -> FlacPcmStream<R> {
        self.into_session_parts().3
    }

    pub(crate) fn into_session_parts(
        mut self,
    ) -> (Metadata, StreamInfo, PcmSpec, FlacPcmStream<R>) {
        self.reader
            .seek(std::io::SeekFrom::Start(self.frame_offset))
            .expect("flac reader remains seekable through stream conversion");
        (
            self.metadata,
            self.stream_info,
            self.spec,
            FlacPcmStream::from_parts(self.reader, self.stream_info, self.spec, self.frame_offset),
        )
    }
}

pub struct FlacPcmStream<R> {
    stream_info: StreamInfo,
    spec: PcmSpec,
    #[cfg(feature = "progress")]
    frame_offset: u64,
    discovered_input_frames: usize,
    discovered_sample_number: u64,
    completed_input_frames: usize,
    #[cfg(feature = "progress")]
    input_bytes_processed: u64,
    drained_pcm_frames: u64,
    threads: usize,
    reader: Option<R>,
    slab_producer: Option<DecodeSlabProducer<R>>,
    producer_progress: SharedProducerProgress,
    input_sender: Option<mpsc::SyncSender<std::io::Result<Option<Vec<u8>>>>>,
    session: Option<session::StreamingDecodeSession>,
    eof: bool,
}

impl<R> FlacPcmStream<R> {
    /// Start building a directly constructed FLAC PCM stream.
    #[must_use]
    pub fn builder(reader: R) -> FlacPcmStreamBuilder<R> {
        FlacPcmStreamBuilder::new(reader)
    }
}

impl<R: Read + Seek> FlacPcmStream<R> {
    fn from_parts(reader: R, stream_info: StreamInfo, spec: PcmSpec, frame_offset: u64) -> Self {
        #[cfg(not(feature = "progress"))]
        let _ = frame_offset;
        let producer_progress =
            Arc::new((Mutex::new(ProducerProgressState::default()), Condvar::new()));
        let producer = ProducerState::new(
            stream_info,
            ProducerConfig {
                target_pcm_frames_per_slab: DECODE_SLAB_TARGET_PCM_FRAMES,
                max_frames_per_slab: DECODE_SLAB_MAX_INPUT_FRAMES,
                max_bytes_per_slab: decode_slab_max_input_bytes(stream_info),
                max_slabs_ahead: 1,
            },
        );
        Self {
            stream_info,
            spec,
            #[cfg(feature = "progress")]
            frame_offset,
            discovered_input_frames: 0,
            discovered_sample_number: 0,
            completed_input_frames: 0,
            #[cfg(feature = "progress")]
            input_bytes_processed: 0,
            drained_pcm_frames: 0,
            threads: 1,
            reader: None,
            slab_producer: Some(DecodeSlabProducer::new(
                reader,
                stream_info,
                spec.total_samples,
                producer,
                Arc::clone(&producer_progress),
                #[cfg(feature = "progress")]
                frame_offset,
            )),
            producer_progress,
            input_sender: None,
            session: None,
            eof: false,
        }
    }
}

impl<R> FlacPcmStream<R> {
    #[must_use]
    pub fn spec(&self) -> PcmSpec {
        self.spec
    }

    /// Return the parsed or staged STREAMINFO block for this stream.
    #[must_use]
    pub fn stream_info(&self) -> StreamInfo {
        self.stream_info
    }

    pub fn set_threads(&mut self, threads: usize) {
        self.threads = threads.max(1);
        let window_limit = self.active_decode_window_limit();
        if let Some(producer) = self.slab_producer.as_mut() {
            producer.producer.set_max_slabs_ahead(window_limit);
        }
    }
}

/// Builder for directly constructing [`FlacPcmStream`] from a seekable FLAC
/// frame source plus explicit STREAMINFO-driven structural inputs.
#[derive(Debug)]
pub struct FlacPcmStreamBuilder<R> {
    reader: R,
    stream_info: Option<StreamInfo>,
    channel_mask: Option<u32>,
    frame_offset: Option<u64>,
}

impl<R> FlacPcmStreamBuilder<R> {
    #[must_use]
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            stream_info: None,
            channel_mask: None,
            frame_offset: None,
        }
    }

    /// Set the STREAMINFO block that acts as the sole structural authority for
    /// the direct FLAC stream.
    #[must_use]
    pub fn stream_info(mut self, stream_info: StreamInfo) -> Self {
        self.stream_info = Some(stream_info);
        self
    }

    /// Override the decoded PCM channel mask. When omitted, the ordinary mask
    /// for the STREAMINFO channel count is used.
    #[must_use]
    pub fn channel_mask(mut self, channel_mask: u32) -> Self {
        self.channel_mask = Some(channel_mask);
        self
    }

    /// Seek the reader to the first FLAC frame before building the stream.
    #[must_use]
    pub fn frame_offset(mut self, frame_offset: u64) -> Self {
        self.frame_offset = Some(frame_offset);
        self
    }
}

impl<R: Read + Seek> FlacPcmStreamBuilder<R> {
    pub fn build(mut self) -> Result<FlacPcmStream<R>> {
        let stream_info = self.stream_info.ok_or(Error::InvalidFlac(
            "direct FLAC stream construction requires STREAMINFO",
        ))?;
        validate_direct_stream_info(stream_info)?;
        if let Some(frame_offset) = self.frame_offset {
            self.reader.seek(std::io::SeekFrom::Start(frame_offset))?;
        }
        Ok(FlacPcmStream::from_parts(
            self.reader,
            stream_info,
            direct_spec_from_stream_info(stream_info, self.channel_mask)?,
            self.frame_offset.unwrap_or(0),
        ))
    }
}

impl<R> FlacPcmStream<R> {
    fn active_decode_window_limit(&self) -> usize {
        self.threads
            .max(1)
            .saturating_mul(DECODE_SESSION_WINDOW_DEPTH)
    }
}

impl<R> Drop for FlacPcmStream<R> {
    fn drop(&mut self) {
        self.input_sender.take();
        let (lock, condvar) = &*self.producer_progress;
        let mut state = lock.lock().unwrap();
        state.cancelled = true;
        condvar.notify_all();
    }
}

impl<R: Read + Seek> FlacPcmStream<R> {
    #[cfg(feature = "progress")]
    fn sync_completed_input_progress(&mut self, completed_input_frames: usize) {
        self.completed_input_frames = completed_input_frames;
        let (lock, condvar) = &*self.producer_progress;
        let mut state = lock.lock().unwrap();
        state.completed_input_frames = completed_input_frames;
        self.discovered_input_frames = state.discovered_input_frames;
        self.discovered_sample_number = state.discovered_sample_number;
        self.input_bytes_processed = completed_input_frames
            .checked_sub(1)
            .and_then(|index| state.discovered_input_byte_totals.get(index).copied())
            .unwrap_or(0);
        condvar.notify_all();
        drop(state);
    }

    #[cfg(not(feature = "progress"))]
    fn sync_completed_input_progress(&mut self, completed_input_frames: usize) {
        self.completed_input_frames = completed_input_frames;
        let (lock, condvar) = &*self.producer_progress;
        let mut state = lock.lock().unwrap();
        state.completed_input_frames = completed_input_frames;
        self.discovered_input_frames = state.discovered_input_frames;
        self.discovered_sample_number = state.discovered_sample_number;
        condvar.notify_all();
        drop(state);
    }

    fn ensure_streaming_session(&mut self) {
        if self.session.is_some() {
            return;
        }
        let slab_producer = self
            .slab_producer
            .take()
            .expect("stream owns a slab producer until the background session starts");
        let (input_sender, input_receiver) =
            mpsc::sync_channel(self.active_decode_window_limit().max(1));
        let (reader, slab_producer) =
            slab_producer.into_background(ChunkBackedReader::new(input_receiver));
        self.reader = Some(reader);
        self.input_sender = Some(input_sender);
        self.session = Some(session::StreamingDecodeSession::spawn_with_producer(
            self.threads.max(1),
            self.active_decode_window_limit(),
            move |session_producer| slab_producer.run(session_producer),
        ));
    }

    fn read_next_slab_plan(&mut self) -> Result<Option<DecodeSlabPlan>> {
        self.slab_producer
            .as_mut()
            .expect("slab producer is available before the background session starts")
            .read_next_slab_plan()
    }

    fn seal_staged_decode_slab_plan(&mut self, plan: DecodeSlabPlan) -> DecodeSlabPlan {
        self.slab_producer
            .as_mut()
            .expect("slab producer is available before the background session starts")
            .seal_staged_decode_slab_plan(plan)
    }

    fn pump_input_chunk(&mut self) -> Result<bool> {
        if self.eof {
            return Ok(false);
        }
        let Some(reader) = self.reader.as_mut() else {
            return Ok(false);
        };
        let Some(sender) = self.input_sender.as_ref() else {
            return Ok(false);
        };

        let mut chunk = vec![0u8; FLAC_READ_CHUNK_SIZE];
        match reader.read(&mut chunk) {
            Ok(0) => {
                self.eof = true;
                if let Some(sender) = self.input_sender.take() {
                    if sender.send(Ok(None)).is_err() {
                        self.wait_for_producer_progress();
                        if !self.producer_finished() {
                            return Err(Error::Thread(
                                "decode producer input channel closed unexpectedly".into(),
                            ));
                        }
                    }
                }
                Ok(false)
            }
            Ok(read) => {
                chunk.truncate(read);
                if sender.send(Ok(Some(chunk))).is_err() {
                    self.wait_for_producer_progress();
                    if !self.producer_finished() {
                        return Err(Error::Thread(
                            "decode producer input channel closed unexpectedly".into(),
                        ));
                    }
                    return Ok(false);
                }
                Ok(true)
            }
            Err(error) => {
                if let Some(sender) = self.input_sender.take() {
                    let forwarded = std::io::Error::new(error.kind(), error.to_string());
                    let _ = sender.send(Err(forwarded));
                }
                Err(error.into())
            }
        }
    }

    fn producer_finished(&self) -> bool {
        self.producer_progress.0.lock().unwrap().producer_finished
    }

    fn wait_for_producer_progress(&self) {
        let (lock, condvar) = &*self.producer_progress;
        let mut state = lock.lock().unwrap();
        let discovered_input_frames = state.discovered_input_frames;
        while !state.cancelled
            && !state.producer_finished
            && state.discovered_input_frames == discovered_input_frames
        {
            state = condvar.wait(state).unwrap();
        }
    }

    fn collect_ready_slabs(&mut self) -> Result<()> {
        let Some(session) = self.session.as_mut() else {
            return Ok(());
        };
        session.collect_ready_slabs()?;
        let completed_input_frames = session.completed_input_frames();
        self.sync_completed_input_progress(completed_input_frames);
        Ok(())
    }

    fn wait_for_ready_slab(&mut self) -> Result<bool> {
        let Some(session) = self.session.as_mut() else {
            return Ok(false);
        };
        if !session.wait_for_ready_slab()? {
            // The session can report exhaustion before the queued producer
            // error is drained; re-collect once before telling the caller
            // there is nothing left to wait on.
            session.collect_ready_slabs()?;
            let completed_input_frames = session.completed_input_frames();
            self.sync_completed_input_progress(completed_input_frames);
            return Ok(false);
        }
        let completed_input_frames = session.completed_input_frames();
        self.sync_completed_input_progress(completed_input_frames);
        Ok(true)
    }

    fn drain_ready_output(&mut self, max_frames: usize, output: &mut Vec<i32>) -> usize {
        let Some(session) = self.session.as_mut() else {
            return 0;
        };
        let channels = usize::from(self.spec.channels);
        let (drained_frames, _) = session.drain_into(max_frames, channels, output);
        let completed_input_frames = session.completed_input_frames();
        self.drained_pcm_frames += drained_frames as u64;
        self.sync_completed_input_progress(completed_input_frames);
        drained_frames
    }
}

impl<R: Read + Seek> EncodePcmStream for FlacPcmStream<R> {
    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize> {
        profile::begin_decode_profile_session_for_current_thread(
            self.threads.max(1),
            self.active_decode_window_limit(),
            DECODE_SLAB_TARGET_PCM_FRAMES,
        );
        if max_frames == 0 {
            return Ok(0);
        }
        self.ensure_streaming_session();
        if self.producer_finished()
            && self.drained_pcm_frames >= self.spec.total_samples
            && self
                .session
                .as_ref()
                .is_some_and(session::StreamingDecodeSession::is_idle)
        {
            return Ok(0);
        }

        let mut total_pcm_frames = 0usize;
        while total_pcm_frames < max_frames {
            total_pcm_frames += self.drain_ready_output(max_frames - total_pcm_frames, output);
            if total_pcm_frames == max_frames {
                break;
            }

            self.collect_ready_slabs()?;
            total_pcm_frames += self.drain_ready_output(max_frames - total_pcm_frames, output);
            if total_pcm_frames == max_frames {
                break;
            }
            if self.producer_finished()
                && self
                    .session
                    .as_ref()
                    .is_some_and(session::StreamingDecodeSession::is_idle)
            {
                break;
            }

            if self.session.as_ref().is_none_or(|session| {
                session.ready_slab_count() == 0 && !session.has_draining_slab()
            }) {
                if self.pump_input_chunk()? {
                    continue;
                }
                if !self.wait_for_ready_slab()? {
                    if self.eof && self.producer_finished() {
                        break;
                    }
                    self.wait_for_producer_progress();
                }
            }
        }

        Ok(total_pcm_frames)
    }
}

impl<R: Read + Seek> DecodePcmStream for FlacPcmStream<R> {
    fn total_input_frames(&self) -> usize {
        0
    }

    fn completed_input_frames(&self) -> usize {
        self.completed_input_frames
    }

    fn stream_info(&self) -> StreamInfo {
        self.stream_info
    }

    #[cfg(feature = "progress")]
    fn input_bytes_processed(&self) -> u64 {
        self.input_bytes_processed
    }

    fn set_threads(&mut self, threads: usize) {
        FlacPcmStream::set_threads(self, threads);
    }

    fn take_decoded_samples(&mut self) -> Result<Option<(Vec<i32>, usize)>> {
        Ok(None)
    }
}

fn decode_slab_max_input_bytes(stream_info: StreamInfo) -> usize {
    let advertised_max_frame_size =
        usize::try_from(stream_info.max_frame_size).unwrap_or(usize::MAX);
    let derived_max_frame_size = advertised_max_frame_size
        .checked_mul(DECODE_SLAB_MAX_INPUT_FRAMES)
        .unwrap_or(usize::MAX);
    if advertised_max_frame_size == 0 {
        DECODE_SLAB_MAX_INPUT_BYTES_FALLBACK
    } else {
        derived_max_frame_size.max(DECODE_SLAB_MAX_INPUT_BYTES_FALLBACK)
    }
}

/// Parse a FLAC stream into a reusable [`FlacReader`].
pub fn read_flac_reader<R: Read + Seek>(reader: R) -> Result<FlacReader<R>> {
    read_flac_reader_with_options(reader, FlacReaderOptions::default())
}

/// Parse a FLAC stream into a reusable [`FlacReader`] with explicit validation options.
pub fn read_flac_reader_with_options<R: Read + Seek>(
    mut reader: R,
    options: FlacReaderOptions,
) -> Result<FlacReader<R>> {
    let (stream_info, metadata, frame_offset) =
        metadata::parse_metadata_from_reader(&mut reader, options.strict_seektable_validation)?;
    validate_stream_info(stream_info)?;
    let spec = spec_from_stream_info(
        stream_info,
        &metadata,
        options.strict_channel_mask_provenance,
    )?;
    Ok(FlacReader {
        reader,
        frame_offset,
        stream_info,
        metadata,
        spec,
    })
}

mod chunk;
mod frame;
mod index;
mod metadata;
mod producer;
mod profile;
mod session;
mod slab;

pub(crate) fn set_decode_profile_path_for_current_thread(path: Option<std::path::PathBuf>) {
    profile::set_decode_profile_path_for_current_thread(path);
}

pub(crate) fn clear_decode_profile_session_for_current_thread() {
    profile::clear_decode_profile_session_for_current_thread();
}

pub(crate) fn hand_out_decode_output_pcm_frames_for_current_thread(pcm_frames: usize) {
    profile::hand_out_pcm_frames_for_current_thread(pcm_frames);
}

pub(crate) fn release_ordered_decode_output_for_current_thread() {
    profile::release_decode_output_buffer_for_current_thread();
}

pub(crate) fn finish_successful_decode_profile_for_current_thread() {
    profile::finish_successful_decode_profile_for_current_thread();
}

pub use metadata::inspect_flac_total_samples;
use metadata::resolve_channel_mask;

fn validate_stream_info(stream_info: StreamInfo) -> Result<()> {
    if !(1..=8).contains(&stream_info.channels) {
        return Err(Error::UnsupportedFlac(format!(
            "only independent 1..8 channel decode is supported, found {} channels",
            stream_info.channels
        )));
    }
    if !(4..=32).contains(&stream_info.bits_per_sample) {
        return Err(Error::UnsupportedFlac(format!(
            "only FLAC-native 4..32-bit decode is supported, found {} bits/sample",
            stream_info.bits_per_sample
        )));
    }
    Ok(())
}

fn spec_from_stream_info(
    stream_info: StreamInfo,
    metadata: &Metadata,
    strict_channel_mask_provenance: bool,
) -> Result<PcmSpec> {
    let channel_mask = resolve_channel_mask(
        stream_info.channels,
        metadata,
        strict_channel_mask_provenance,
    )?;
    Ok(PcmSpec {
        sample_rate: stream_info.sample_rate,
        channels: stream_info.channels,
        bits_per_sample: stream_info.bits_per_sample,
        total_samples: stream_info.total_samples,
        bytes_per_sample: u16::from(stream_info.bits_per_sample.div_ceil(8)),
        channel_mask,
    })
}

fn direct_spec_from_stream_info(
    stream_info: StreamInfo,
    channel_mask: Option<u32>,
) -> Result<PcmSpec> {
    let channel_mask = match channel_mask {
        Some(channel_mask) => channel_mask,
        None => ordinary_channel_mask(u16::from(stream_info.channels)).ok_or_else(|| {
            Error::UnsupportedFlac(format!(
                "no ordinary channel mask exists for {} channels",
                stream_info.channels
            ))
        })?,
    };
    if !is_supported_channel_mask(u16::from(stream_info.channels), channel_mask) {
        return Err(Error::UnsupportedFlac(format!(
            "channel mask {channel_mask:#010x} is not supported for {} channels",
            stream_info.channels
        )));
    }
    Ok(PcmSpec {
        sample_rate: stream_info.sample_rate,
        channels: stream_info.channels,
        bits_per_sample: stream_info.bits_per_sample,
        total_samples: stream_info.total_samples,
        bytes_per_sample: u16::from(stream_info.bits_per_sample.div_ceil(8)),
        channel_mask,
    })
}

fn validate_direct_stream_info(stream_info: StreamInfo) -> Result<()> {
    if !(1..=8).contains(&stream_info.channels) {
        return Err(Error::UnsupportedFlac(format!(
            "FLAC direct streams only support 1..8 channels, found {}",
            stream_info.channels
        )));
    }
    if !(4..=32).contains(&stream_info.bits_per_sample) {
        return Err(Error::UnsupportedFlac(format!(
            "FLAC direct streams only support 4..32 bits/sample, found {}",
            stream_info.bits_per_sample
        )));
    }
    if stream_info.sample_rate > MAX_STREAMINFO_SAMPLE_RATE {
        return Err(Error::UnsupportedFlac(format!(
            "FLAC sample rate {} exceeds STREAMINFO limits",
            stream_info.sample_rate
        )));
    }
    if stream_info.total_samples > 0x0f_ff_ff_ff_ff {
        return Err(Error::UnsupportedFlac(format!(
            "FLAC total samples {} exceed STREAMINFO limits",
            stream_info.total_samples
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::frame::{
        channel_bits_per_sample, decode_bits_per_sample, decode_channel_assignment,
    };
    use super::metadata::requires_channel_layout_provenance;
    use super::{
        FlacPcmStream, StreamInfo,
        producer::{ProducerConfig, ProducerState},
    };
    use crate::model::ChannelAssignment;
    use crate::{EncoderConfig, convenience::encode_bytes_with_config};
    use std::io::Cursor;

    #[test]
    fn decodes_independent_channel_assignments_for_one_to_eight_channels() {
        for (code, channels) in (0u8..=7).zip(1u8..=8) {
            assert_eq!(
                decode_channel_assignment(code).unwrap(),
                ChannelAssignment::Independent(channels)
            );
        }
    }

    #[test]
    fn preserves_stereo_only_decorrelation_modes() {
        assert_eq!(
            decode_channel_assignment(0b1000).unwrap(),
            ChannelAssignment::LeftSide
        );
        assert_eq!(
            decode_channel_assignment(0b1001).unwrap(),
            ChannelAssignment::SideRight
        );
        assert_eq!(
            decode_channel_assignment(0b1010).unwrap(),
            ChannelAssignment::MidSide
        );
    }

    #[test]
    fn bit_depth_code_zero_uses_streaminfo_depth_for_broader_flac_native_range() {
        for depth in [4u8, 5, 7, 9, 11, 13, 15, 17, 19, 21, 23, 25, 31] {
            assert_eq!(decode_bits_per_sample(0b000, depth).unwrap(), depth);
        }
        assert_eq!(decode_bits_per_sample(0b001, 4).unwrap(), 8);
        assert_eq!(decode_bits_per_sample(0b010, 4).unwrap(), 12);
        assert_eq!(decode_bits_per_sample(0b100, 4).unwrap(), 16);
        assert_eq!(decode_bits_per_sample(0b101, 4).unwrap(), 20);
        assert_eq!(decode_bits_per_sample(0b110, 4).unwrap(), 24);
        assert_eq!(decode_bits_per_sample(0b111, 4).unwrap(), 32);
    }

    #[test]
    fn channel_bits_expand_for_independent_multichannel_assignments() {
        assert_eq!(
            channel_bits_per_sample(ChannelAssignment::Independent(3), 16),
            vec![16, 16, 16]
        );
        assert_eq!(
            channel_bits_per_sample(ChannelAssignment::MidSide, 16),
            vec![16, 17]
        );
    }

    #[test]
    fn channel_layout_provenance_is_only_required_for_explicit_mask_restore() {
        assert!(!requires_channel_layout_provenance(2, None));
        assert!(requires_channel_layout_provenance(2, Some(0)));
        assert!(requires_channel_layout_provenance(4, Some(0x0001_2104)));
    }

    #[test]
    fn direct_flac_builder_rejects_invalid_streaminfo() {
        let stream_info = StreamInfo {
            sample_rate: 44_100,
            channels: 0,
            bits_per_sample: 16,
            total_samples: 16,
            md5: [0; 16],
            min_block_size: 0,
            max_block_size: 0,
            min_frame_size: 0,
            max_frame_size: 0,
        };
        let error = FlacPcmStream::builder(Cursor::new(Vec::<u8>::new()))
            .stream_info(stream_info)
            .build()
            .unwrap_err();
        assert!(error.to_string().contains("1..8"));
    }

    #[test]
    fn seal_staged_decode_slab_plan_consumes_staged_bytes_and_frame_metadata() {
        let stream_info = StreamInfo {
            sample_rate: 44_100,
            channels: 2,
            bits_per_sample: 16,
            total_samples: 4_096,
            md5: [0; 16],
            min_block_size: 4_096,
            max_block_size: 4_096,
            min_frame_size: 0,
            max_frame_size: 0,
        };
        let mut stream = FlacPcmStream::builder(Cursor::new(Vec::<u8>::new()))
            .stream_info(stream_info)
            .build()
            .unwrap();
        let frames = vec![super::FrameIndex {
            header_number: super::FrameHeaderNumber {
                kind: super::FrameHeaderNumberKind::FrameNumber,
                value: 7,
            },
            offset: 0,
            header_bytes_consumed: 2,
            bytes_consumed: 3,
            block_size: 2,
            bits_per_sample: 16,
            assignment: crate::model::ChannelAssignment::Independent(0),
        }];
        let producer = stream.slab_producer.as_mut().unwrap();
        producer.pending_bytes = vec![1, 2, 3, 4, 5];
        producer.pending_start = 3;

        let plan = stream.seal_staged_decode_slab_plan(super::slab::DecodeSlabPlan::new(
            3,
            7,
            14,
            stream_info,
            frames.clone(),
        ));

        assert_eq!(plan.sequence, 3);
        assert_eq!(plan.start_frame_index, 7);
        assert_eq!(plan.start_sample_number, 14);
        assert_eq!(plan.frame_block_sizes, vec![2]);
        assert_eq!(plan.bytes.as_ref(), &[1, 2, 3]);
        assert_eq!(plan.frames.as_ref(), frames.as_slice());
        let producer = stream.slab_producer.as_ref().unwrap();
        assert_eq!(producer.pending_bytes, vec![4, 5]);
        assert_eq!(producer.pending_start, 0);
    }

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

    #[test]
    fn read_next_slab_plan_preserves_frame_bytes_across_pre_add_seal_boundaries() {
        let samples = (0i16..48).collect::<Vec<_>>();
        let wav = wav_bytes_from_i16_samples(&samples);
        let flac = encode_bytes_with_config(&EncoderConfig::default().with_block_size(16), &wav)
            .expect("encode test flac");
        let mut stream = super::read_flac_reader(Cursor::new(flac))
            .expect("parse encoded flac")
            .into_pcm_stream();
        stream.slab_producer.as_mut().unwrap().producer = ProducerState::new(
            stream.stream_info,
            ProducerConfig {
                target_pcm_frames_per_slab: 24,
                max_frames_per_slab: 4,
                max_bytes_per_slab: usize::MAX,
                max_slabs_ahead: 4,
            },
        );

        let first = stream
            .read_next_slab_plan()
            .expect("first slab plan result")
            .expect("first slab plan");
        assert_eq!(first.start_frame_index, 0);
        assert_eq!(first.frame_block_sizes, vec![16]);
        let first_packet =
            super::frame::decode_work_packet(first.into()).expect("decode first slab plan");
        assert_eq!(
            first_packet.decoded_samples,
            (0..16).map(i32::from).collect::<Vec<_>>()
        );

        let second = stream
            .read_next_slab_plan()
            .expect("second slab plan result")
            .expect("second slab plan");
        assert_eq!(second.start_frame_index, 1);
        assert_eq!(second.frame_block_sizes, vec![16]);
        let second_packet =
            super::frame::decode_work_packet(second.into()).expect("decode second slab plan");
        assert_eq!(
            second_packet.decoded_samples,
            (16..32).map(i32::from).collect::<Vec<_>>()
        );
    }
}
