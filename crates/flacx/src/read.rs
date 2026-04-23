use std::{
    collections::VecDeque,
    io::{Read, Seek},
    sync::Arc,
};

use crate::{
    error::{Error, Result},
    input::{EncodePcmStream, PcmSpec},
    metadata::Metadata,
    model::ChannelAssignment,
    pcm::{is_supported_channel_mask, ordinary_channel_mask},
    stream_info::{MAX_STREAMINFO_SAMPLE_RATE, StreamInfo},
};

use self::slab::DecodeSlabPlan;

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
            .field("buffered_input_bytes", &self.chunk_scanner.buffered_bytes_len())
            .field("queued_chunk_count", &self.chunk_scanner.ready_chunk_count())
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
                "has_draining_slab",
                &self
                    .session
                    .as_ref()
                    .is_some_and(session::StreamingDecodeSession::has_draining_slab),
            )
            .field("has_streaming_session", &self.session.is_some())
            .field("eof", &self.eof)
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
    reader: R,
    chunk_scanner: chunk::ChunkScanner,
    submitted_frame_byte_lengths: VecDeque<usize>,
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
            reader,
            chunk_scanner: chunk::ChunkScanner::new(
                stream_info,
                chunk::ChunkScannerConfig {
                    target_pcm_frames_per_chunk: DECODE_SLAB_TARGET_PCM_FRAMES,
                    max_frames_per_chunk: DECODE_SLAB_MAX_INPUT_FRAMES,
                    max_bytes_per_chunk: decode_slab_max_input_bytes(stream_info),
                },
            ),
            submitted_frame_byte_lengths: VecDeque::new(),
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
        let window_depth_limit = self.active_decode_window_limit();
        if let Some(session) = self.session.as_mut() {
            session.set_window_depth_limit(window_depth_limit);
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

impl<R: Read + Seek> FlacPcmStream<R> {
    #[cfg(feature = "progress")]
    fn sync_completed_input_progress(&mut self, completed_input_frames: usize) {
        let completed_now = completed_input_frames.saturating_sub(self.completed_input_frames);
        if completed_now > 0 {
            if self.completed_input_frames == 0 {
                self.input_bytes_processed =
                    self.input_bytes_processed.saturating_add(self.frame_offset);
            }
            for _ in 0..completed_now {
                let Some(frame_bytes) = self.submitted_frame_byte_lengths.pop_front() else {
                    break;
                };
                self.input_bytes_processed = self
                    .input_bytes_processed
                    .saturating_add(frame_bytes as u64);
            }
        }
        self.completed_input_frames = completed_input_frames;
    }

    #[cfg(not(feature = "progress"))]
    fn sync_completed_input_progress(&mut self, completed_input_frames: usize) {
        self.completed_input_frames = completed_input_frames;
    }

    fn ensure_streaming_session(&mut self) {
        if self.session.is_some() {
            return;
        }
        self.session = Some(session::StreamingDecodeSession::spawn(
            self.threads.max(1),
            self.active_decode_window_limit(),
        ));
    }

    fn submit_scanned_chunk(&mut self, chunk: chunk::CompressedDecodeChunk) -> Result<bool> {
        if self
            .session
            .as_ref()
            .is_none_or(|session| !session.has_submit_capacity())
        {
            self.chunk_scanner.requeue_ready_chunk_front(chunk);
            return Ok(false);
        }
        let discovered_input_frames = chunk.frame_block_sizes.len();
        let discovered_sample_number = chunk
            .frame_block_sizes
                .iter()
                .map(|&block_size| u64::from(block_size))
                .sum::<u64>();
        self.submit_chunk(chunk)?;
        self.discovered_input_frames = self
            .discovered_input_frames
            .saturating_add(discovered_input_frames);
        self.discovered_sample_number = self
            .discovered_sample_number
            .saturating_add(discovered_sample_number);
        Ok(true)
    }

    fn dispatch_scanner_step(&mut self, step: chunk::ChunkStep) -> Result<bool> {
        let mut dispatched_any = false;
        if let chunk::ChunkStep::Sealed(chunk) = step {
            dispatched_any |= self.submit_scanned_chunk(chunk)?;
        }
        while self
            .session
            .as_ref()
            .is_some_and(session::StreamingDecodeSession::has_submit_capacity)
        {
            let Some(chunk) = self.chunk_scanner.take_ready_chunk() else {
                break;
            };
            dispatched_any |= self.submit_scanned_chunk(chunk)?;
        }
        Ok(dispatched_any)
    }

    fn read_next_input_chunk(&mut self) -> Result<bool> {
        if self.chunk_scanner.is_finished() {
            return Ok(false);
        }

        if self.eof {
            let step = self.chunk_scanner.finish()?;
            let dispatched = self.dispatch_scanner_step(step)?;
            return Ok(dispatched || self.chunk_scanner.ready_chunk_count() > 0);
        }

        let mut chunk = vec![0u8; FLAC_READ_CHUNK_SIZE];
        match self.reader.read(&mut chunk) {
            Ok(0) => {
                self.eof = true;
                let step = self.chunk_scanner.finish()?;
                let dispatched = self.dispatch_scanner_step(step)?;
                Ok(dispatched || self.chunk_scanner.ready_chunk_count() > 0)
            }
            Ok(read) => {
                chunk.truncate(read);
                let step = self.chunk_scanner.push_bytes(&chunk)?;
                let _ = self.dispatch_scanner_step(step)?;
                Ok(true)
            }
            Err(error) => Err(error.into()),
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

    fn submit_scanner_ready_chunks(&mut self) -> Result<bool> {
        self.dispatch_scanner_step(chunk::ChunkStep::Pending)
    }

    fn submit_chunk(&mut self, chunk: chunk::CompressedDecodeChunk) -> Result<()> {
        let plan = DecodeSlabPlan {
            sequence: chunk.sequence,
            start_frame_index: chunk.start_frame_index,
            start_sample_number: chunk.start_sample_number,
            stream_info: self.stream_info,
            frame_block_sizes: chunk.frame_block_sizes,
            bytes: Arc::clone(&chunk.bytes),
            frames: Arc::from(Vec::<FrameIndex>::new()),
        };
        self.session
            .as_mut()
            .expect("streaming decode session is available before chunk submission")
            .submit(plan)?;
        self.submitted_frame_byte_lengths
            .extend(chunk.frame_byte_lengths);
        Ok(())
    }

    fn decode_is_exhausted(&self) -> bool {
        self.discovered_sample_number >= self.spec.total_samples
            && self.chunk_scanner.ready_chunk_count() == 0
            && self.drained_pcm_frames >= self.spec.total_samples
            && self
                .session
                .as_ref()
                .is_some_and(session::StreamingDecodeSession::is_idle)
    }

    fn decode_is_stalled_at_eof(&self) -> bool {
        self.eof
            && self.chunk_scanner.is_finished()
            && self.chunk_scanner.ready_chunk_count() == 0
            && self
                .session
                .as_ref()
                .is_some_and(session::StreamingDecodeSession::is_idle)
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
            decode_slab_max_input_bytes(self.stream_info),
        );
        if max_frames == 0 {
            return Ok(0);
        }
        self.ensure_streaming_session();
        if self.decode_is_exhausted() {
            return Ok(0);
        }

        let mut total_pcm_frames = 0usize;
        while total_pcm_frames < max_frames {
            total_pcm_frames += self.drain_ready_output(max_frames - total_pcm_frames, output);
            if total_pcm_frames == max_frames || self.decode_is_exhausted() {
                break;
            }

            self.collect_ready_slabs()?;
            total_pcm_frames += self.drain_ready_output(max_frames - total_pcm_frames, output);
            if total_pcm_frames == max_frames || self.decode_is_exhausted() {
                break;
            }

            if self.submit_scanner_ready_chunks()? {
                continue;
            }

            if self
                .session
                .as_ref()
                .is_some_and(|session| !session.has_submit_capacity())
            {
                if !self.wait_for_ready_slab()? && self.decode_is_exhausted() {
                    break;
                }
                continue;
            }

            if self.read_next_input_chunk()? {
                continue;
            }

            if !self.wait_for_ready_slab()? && self.decode_is_exhausted() {
                break;
            }

            if self.decode_is_stalled_at_eof() {
                return Err(Error::InvalidFlac("unexpected EOF while reading frames"));
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
mod metadata;
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
    use std::fs;
    use std::sync::Arc;
    use std::time::Duration;

    use super::frame::{
        channel_bits_per_sample, decode_bits_per_sample, decode_channel_assignment,
    };
    use super::metadata::requires_channel_layout_provenance;
    use super::{FlacPcmStream, StreamInfo};
    use crate::input::EncodePcmStream;
    use crate::model::ChannelAssignment;
    use crate::read::DecodePcmStream;
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

    fn direct_stream_info() -> StreamInfo {
        StreamInfo {
            sample_rate: 44_100,
            channels: 1,
            bits_per_sample: 16,
            total_samples: 16,
            md5: [0; 16],
            min_block_size: 16,
            max_block_size: 16,
            min_frame_size: 32,
            max_frame_size: 32,
        }
    }

    fn current_process_thread_count() -> usize {
        fs::read_dir("/proc/self/task")
            .expect("linux thread list should be readable in tests")
            .count()
    }

    fn wait_for_thread_count_at_least(expected_minimum: usize) -> usize {
        let start = std::time::Instant::now();
        loop {
            let count = current_process_thread_count();
            if count >= expected_minimum || start.elapsed() > Duration::from_secs(1) {
                return count;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn live_set_threads_reconfigures_streaming_session_window_limit() {
        let stream_info = direct_stream_info();
        let mut stream = FlacPcmStream::builder(Cursor::new(Vec::<u8>::new()))
            .stream_info(stream_info)
            .build()
            .unwrap();
        let chunk = super::chunk::CompressedDecodeChunk {
            sequence: 0,
            start_frame_index: 0,
            start_sample_number: 0,
            frame_block_sizes: vec![16],
            frame_byte_lengths: vec![32],
            bytes: Arc::from(vec![0u8; 32]),
        };

        stream.ensure_streaming_session();
        let initial_window_limit = stream
            .session
            .as_ref()
            .unwrap()
            .window_depth_limit_for_tests();
        assert!(stream.submit_scanned_chunk(chunk).is_ok());
        assert!(
            stream
                .session
                .as_ref()
                .unwrap()
                .outstanding_window_slabs_for_tests()
                > 0
        );

        stream.set_threads(4);

        assert_eq!(stream.threads, 4);
        assert_eq!(initial_window_limit, super::DECODE_SESSION_WINDOW_DEPTH);
        assert_eq!(
            stream
                .session
                .as_ref()
                .unwrap()
                .window_depth_limit_for_tests(),
            4 * super::DECODE_SESSION_WINDOW_DEPTH
        );
    }

    #[test]
    fn submit_scanned_chunk_only_advances_discovery_after_accepted_submission() {
        let stream_info = direct_stream_info();
        let mut stream = FlacPcmStream::builder(Cursor::new(Vec::<u8>::new()))
            .stream_info(stream_info)
            .build()
            .unwrap();
        stream.session = Some(super::session::StreamingDecodeSession::broken_for_submit_failure());
        let chunk = super::chunk::CompressedDecodeChunk {
            sequence: 0,
            start_frame_index: 0,
            start_sample_number: 0,
            frame_block_sizes: vec![16],
            frame_byte_lengths: vec![32],
            bytes: Arc::from(vec![0u8; 32]),
        };

        let error = stream.submit_scanned_chunk(chunk).unwrap_err();

        assert!(matches!(error, crate::Error::Thread(_)));
        assert_eq!(stream.discovered_input_frames, 0);
        assert_eq!(stream.discovered_sample_number, 0);
        assert!(stream.submitted_frame_byte_lengths.is_empty());
    }

    #[test]
    fn read_chunk_drives_worker_submission_without_coordinator_backpressure_regression() {
        let (stream_info, chunks) = fixture_ready_chunks(2);
        let expected_frames = usize::try_from(stream_info.total_samples).unwrap();
        let mut chunks = chunks.into_iter();
        let mut stream = FlacPcmStream::builder(Cursor::new(Vec::<u8>::new()))
            .stream_info(stream_info)
            .build()
            .unwrap();
        let output_channels = usize::from(stream.spec.channels);
        let mut output = Vec::new();
        let before = current_process_thread_count();

        stream.set_threads(2);
        stream.ensure_streaming_session();
        let after = wait_for_thread_count_at_least(before + 2);
        stream
            .session
            .as_mut()
            .unwrap()
            .set_window_depth_limit(1);

        assert_eq!(
            after,
            before + 2,
            "read path should create only worker threads when the streaming session starts"
        );

        assert!(stream.submit_scanned_chunk(chunks.next().unwrap()).unwrap());
        assert!(!stream.submit_scanned_chunk(chunks.next().unwrap()).unwrap());

        let queued_before = stream.chunk_scanner.ready_chunk_count();
        let completed_before = stream.completed_input_frames();

        let frames = stream.read_chunk(expected_frames, &mut output).unwrap();

        assert!(queued_before > 0);
        assert_eq!(completed_before, 0);
        assert_eq!(frames, expected_frames);
        assert_eq!(output.len(), frames * output_channels);
        assert_eq!(stream.chunk_scanner.ready_chunk_count(), 0);
    }

    fn fixture_ready_chunks(count: usize) -> (StreamInfo, Vec<super::chunk::CompressedDecodeChunk>) {
        let fixture_path = workspace_fixture_dir("test-flacs").join("case1/test01.flac");
        let bytes = std::fs::read(fixture_path).unwrap();
        let (mut stream_info, _, frame_offset) =
            super::metadata::parse_metadata(&bytes, false).unwrap();
        let mut start_sample_number = 0u64;
        let mut total_samples = 0u64;
        let chunks = super::frame::index_frames(&bytes, frame_offset, stream_info)
            .unwrap()
            .into_iter()
            .take(count)
            .enumerate()
            .map(|(sequence, frame)| {
                let frame_bytes = bytes[frame.offset..frame.offset + frame.bytes_consumed].to_vec();
                total_samples = total_samples.saturating_add(u64::from(frame.block_size));
                let chunk = super::chunk::CompressedDecodeChunk {
                    sequence,
                    start_frame_index: sequence,
                    start_sample_number,
                    frame_block_sizes: vec![frame.block_size],
                    frame_byte_lengths: vec![frame.bytes_consumed],
                    bytes: Arc::from(frame_bytes),
                };
                start_sample_number =
                    start_sample_number.saturating_add(u64::from(frame.block_size));
                chunk
            })
            .collect();
        stream_info.total_samples = total_samples;
        (stream_info, chunks)
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
