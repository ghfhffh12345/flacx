use std::{collections::VecDeque, sync::Arc};

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    error::{Error, Result},
    stream_info::StreamInfo,
};

use super::ParsedFrame;

#[cfg(test)]
static FIND_NEXT_FRAME_START_PARSE_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ChunkScannerConfig {
    pub(super) target_pcm_frames_per_chunk: usize,
    pub(super) max_frames_per_chunk: usize,
    pub(super) max_bytes_per_chunk: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CompressedDecodeChunk {
    pub(super) sequence: usize,
    pub(super) start_frame_index: usize,
    pub(super) start_sample_number: u64,
    pub(super) frame_block_sizes: Vec<u16>,
    pub(super) frame_byte_lengths: Vec<usize>,
    pub(super) bytes: Arc<[u8]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ChunkStep {
    Pending,
    Sealed(CompressedDecodeChunk),
}

#[derive(Debug)]
pub(super) struct ChunkScanner {
    stream_info: StreamInfo,
    config: ChunkScannerConfig,
    buffered_bytes: Vec<u8>,
    current_frame: Option<ScannedFrame>,
    pending_chunk: PendingChunk,
    ready_chunks: VecDeque<CompressedDecodeChunk>,
    next_sequence: usize,
    next_frame_index: usize,
    next_sample_number: u64,
    finished: bool,
}

impl ChunkScanner {
    pub(super) fn new(stream_info: StreamInfo, config: ChunkScannerConfig) -> Self {
        Self {
            stream_info,
            config,
            buffered_bytes: Vec::new(),
            current_frame: None,
            pending_chunk: PendingChunk::default(),
            ready_chunks: VecDeque::new(),
            next_sequence: 0,
            next_frame_index: 0,
            next_sample_number: 0,
            finished: false,
        }
    }

    pub(super) fn push_bytes(&mut self, bytes: &[u8]) -> Result<ChunkStep> {
        if self.finished {
            if bytes.is_empty() {
                return Ok(ChunkStep::Pending);
            }
            return Err(Error::Decode(
                "cannot push additional input after chunk scanner is finished".into(),
            ));
        }

        self.buffered_bytes.extend_from_slice(bytes);
        self.scan_available_frames(false)?;
        Ok(self
            .ready_chunks
            .pop_front()
            .map_or(ChunkStep::Pending, ChunkStep::Sealed))
    }

    pub(super) fn finish(&mut self) -> Result<ChunkStep> {
        if self.finished {
            return Ok(ChunkStep::Pending);
        }

        self.scan_available_frames(true)?;
        Ok(self
            .ready_chunks
            .pop_front()
            .map_or(ChunkStep::Pending, ChunkStep::Sealed))
    }

    pub(super) fn take_ready_chunk(&mut self) -> Option<CompressedDecodeChunk> {
        self.ready_chunks.pop_front()
    }

    pub(super) fn requeue_ready_chunk_front(&mut self, chunk: CompressedDecodeChunk) {
        self.ready_chunks.push_front(chunk);
    }

    pub(super) fn ready_chunk_count(&self) -> usize {
        self.ready_chunks.len()
    }

    pub(super) fn buffered_bytes_len(&self) -> usize {
        self.buffered_bytes.len()
    }

    pub(super) fn is_finished(&self) -> bool {
        self.finished
    }

    fn scan_available_frames(&mut self, eof: bool) -> Result<()> {
        loop {
            if self.next_sample_number >= self.stream_info.total_samples {
                if let Some(chunk) = self.seal_pending_chunk() {
                    self.ready_chunks.push_back(chunk);
                }
                self.finished = true;
                return Ok(());
            }

            if self.current_frame.is_none() {
                if self.buffered_bytes.is_empty() {
                    if eof {
                        if let Some(chunk) = self.seal_pending_chunk() {
                            self.ready_chunks.push_back(chunk);
                        } else {
                            self.finished = true;
                        }
                    }
                    return Ok(());
                }

                match super::frame::parse_frame_header(
                    &self.buffered_bytes,
                    self.stream_info,
                    self.next_frame_index as u64,
                    self.next_sample_number,
                    None,
                ) {
                    Ok(parsed) => {
                        self.current_frame = Some(ScannedFrame {
                            start_offset: 0,
                            parsed,
                        });
                    }
                    Err(error) if is_incomplete_header(&error) && !eof => {
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                }
            }

            if self.should_seal_before_scanning_current_frame() {
                let chunk = self
                    .seal_pending_chunk()
                    .expect("pending chunk exists before scanning the next frame");
                self.ready_chunks.push_back(chunk);
                continue;
            }

            let current = self
                .current_frame
                .expect("scanner populates the current frame before boundary search");
            let next = find_next_frame_start(
                &self.buffered_bytes,
                current.start_offset + current.parsed.header_bytes_consumed,
                self.stream_info,
                self.next_frame_index as u64 + 1,
                self.next_sample_number + u64::from(current.parsed.block_size),
            )?;

            if let Some(next_frame) = next {
                self.current_frame = Some(next_frame);
                self.accept_frame(current, next_frame.start_offset);
                continue;
            }

            if eof {
                let current_end = if self
                    .next_sample_number
                    .saturating_add(u64::from(current.parsed.block_size))
                    >= self.stream_info.total_samples
                {
                    match super::frame::scan_frame(
                        &self.buffered_bytes[current.start_offset..],
                        self.stream_info,
                        self.next_frame_index as u64,
                        self.next_sample_number,
                    ) {
                        Ok(parsed) => current.start_offset + parsed.bytes_consumed,
                        Err(_) => self.buffered_bytes.len(),
                    }
                } else {
                    self.buffered_bytes.len()
                };
                self.current_frame = None;
                self.accept_frame(current, current_end);
                if let Some(chunk) = self.seal_pending_chunk() {
                    self.ready_chunks.push_back(chunk);
                } else {
                    self.finished = true;
                }
                return Ok(());
            }

            return Ok(());
        }
    }

    fn accept_frame(&mut self, frame: ScannedFrame, frame_end: usize) {
        let frame_len = frame_end.saturating_sub(frame.start_offset);
        if self.should_seal_before_adding(frame.parsed, frame_len) {
            let chunk = self
                .seal_pending_chunk()
                .expect("pending chunk exists before a pre-add seal");
            self.ready_chunks.push_back(chunk);
        }

        self.append_frame(frame.parsed, frame_len);
        if self.should_seal_after_adding() {
            let chunk = self
                .seal_pending_chunk()
                .expect("pending chunk exists before a post-add seal");
            self.ready_chunks.push_back(chunk);
        }
    }

    fn append_frame(&mut self, parsed: ParsedFrame, frame_len: usize) {
        if self.pending_chunk.frame_block_sizes.is_empty() {
            self.pending_chunk.start_frame_index = self.next_frame_index;
            self.pending_chunk.start_sample_number = self.next_sample_number;
        }

        self.pending_chunk.frame_block_sizes.push(parsed.block_size);
        self.pending_chunk.frame_byte_lengths.push(frame_len);
        self.pending_chunk.pcm_frames = self
            .pending_chunk
            .pcm_frames
            .saturating_add(usize::from(parsed.block_size));
        self.pending_chunk.bytes_len = self.pending_chunk.bytes_len.saturating_add(frame_len);
        self.next_frame_index += 1;
        self.next_sample_number = self
            .next_sample_number
            .saturating_add(u64::from(parsed.block_size));
    }

    fn should_seal_before_adding(&self, parsed: ParsedFrame, frame_len: usize) -> bool {
        !self.pending_chunk.frame_block_sizes.is_empty()
            && (self
                .pending_chunk
                .pcm_frames
                .saturating_add(usize::from(parsed.block_size))
                > self.config.target_pcm_frames_per_chunk
                || self.pending_chunk.bytes_len.saturating_add(frame_len)
                    > self.config.max_bytes_per_chunk)
    }

    fn should_seal_after_adding(&self) -> bool {
        !self.pending_chunk.frame_block_sizes.is_empty()
            && (self.pending_chunk.pcm_frames >= self.config.target_pcm_frames_per_chunk
                || self.pending_chunk.frame_block_sizes.len() >= self.config.max_frames_per_chunk
                || self.pending_chunk.bytes_len >= self.config.max_bytes_per_chunk)
    }

    fn should_seal_before_scanning_current_frame(&self) -> bool {
        let Some(current) = self.current_frame else {
            return false;
        };

        !self.pending_chunk.frame_block_sizes.is_empty()
            && self
                .pending_chunk
                .pcm_frames
                .saturating_add(usize::from(current.parsed.block_size))
                > self.config.target_pcm_frames_per_chunk
    }

    fn seal_pending_chunk(&mut self) -> Option<CompressedDecodeChunk> {
        if self.pending_chunk.frame_block_sizes.is_empty() {
            return None;
        }

        let bytes_len = self.pending_chunk.bytes_len;
        let chunk = CompressedDecodeChunk {
            sequence: self.next_sequence,
            start_frame_index: self.pending_chunk.start_frame_index,
            start_sample_number: self.pending_chunk.start_sample_number,
            frame_block_sizes: std::mem::take(&mut self.pending_chunk.frame_block_sizes),
            frame_byte_lengths: std::mem::take(&mut self.pending_chunk.frame_byte_lengths),
            bytes: Arc::from(self.buffered_bytes.drain(..bytes_len).collect::<Vec<_>>()),
        };
        self.next_sequence += 1;
        self.pending_chunk.pcm_frames = 0;
        self.pending_chunk.bytes_len = 0;
        if let Some(frame) = self.current_frame.as_mut() {
            frame.start_offset = frame.start_offset.saturating_sub(bytes_len);
        }
        Some(chunk)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScannedFrame {
    start_offset: usize,
    parsed: ParsedFrame,
}

#[derive(Debug, Default)]
struct PendingChunk {
    start_frame_index: usize,
    start_sample_number: u64,
    frame_block_sizes: Vec<u16>,
    frame_byte_lengths: Vec<usize>,
    pcm_frames: usize,
    bytes_len: usize,
}

fn find_next_frame_start(
    bytes: &[u8],
    search_from: usize,
    stream_info: StreamInfo,
    expected_frame_number: u64,
    expected_sample_number: u64,
) -> Result<Option<ScannedFrame>> {
    let mut offset = search_from;
    while offset + 1 < bytes.len() {
        if bytes[offset] != 0xff || (bytes[offset + 1] & 0b1111_1110) != 0b1111_1000 {
            offset += 1;
            continue;
        }

        match parse_frame_header_for_scan(
            &bytes[offset..],
            stream_info,
            expected_frame_number,
            expected_sample_number,
            None,
        ) {
            Ok(parsed) => {
                return Ok(Some(ScannedFrame {
                    start_offset: offset,
                    parsed,
                }));
            }
            Err(error) if is_incomplete_header(&error) => return Ok(None),
            Err(_) => offset += 1,
        }
    }
    Ok(None)
}

fn parse_frame_header_for_scan(
    bytes: &[u8],
    stream_info: StreamInfo,
    expected_frame_number: u64,
    expected_sample_number: u64,
    expected_kind: Option<super::FrameHeaderNumberKind>,
) -> Result<ParsedFrame> {
    #[cfg(test)]
    FIND_NEXT_FRAME_START_PARSE_ATTEMPTS.fetch_add(1, Ordering::Relaxed);

    super::frame::parse_frame_header(
        bytes,
        stream_info,
        expected_frame_number,
        expected_sample_number,
        expected_kind,
    )
}

#[cfg(test)]
fn reset_find_next_frame_start_parse_attempts() {
    FIND_NEXT_FRAME_START_PARSE_ATTEMPTS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
fn find_next_frame_start_parse_attempts() -> usize {
    FIND_NEXT_FRAME_START_PARSE_ATTEMPTS.load(Ordering::Relaxed)
}

fn is_incomplete_header(error: &Error) -> bool {
    match error {
        Error::Io(inner) => inner.kind() == std::io::ErrorKind::UnexpectedEof,
        Error::InvalidFlac(message) => *message == "unexpected EOF while reading frames",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        crc::crc8,
        error::Error,
        read::chunk::{ChunkScanner, ChunkScannerConfig, ChunkStep},
        stream_info::StreamInfo,
    };

    fn stream_info() -> StreamInfo {
        stream_info_with_total_samples(64)
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

    fn frame_header(frame_number: u8, block_size: u8) -> Vec<u8> {
        let mut header = vec![0xff, 0xf8, 0x60, 0x08, frame_number, block_size - 1];
        header.push(crc8(&header));
        header
    }

    fn frame_bytes(frame_number: u8, payload_len: usize) -> Vec<u8> {
        let mut bytes = frame_header(frame_number, 16);
        bytes.extend(std::iter::repeat_n(0x40 + frame_number, payload_len));
        bytes
    }

    fn valid_test_frame_bytes() -> Vec<u8> {
        frame_bytes(0, 5)
    }

    #[test]
    fn find_next_frame_start_skips_non_sync_bytes_without_reparsing_every_offset() {
        let stream_info = stream_info_with_total_samples(16);
        let mut bytes = vec![0x00, 0x11, 0x22];
        bytes.extend_from_slice(&valid_test_frame_bytes());
        super::reset_find_next_frame_start_parse_attempts();

        let next = super::find_next_frame_start(bytes.as_slice(), 0, stream_info, 0, 0).unwrap();

        assert!(matches!(next, Some(frame) if frame.start_offset == 3));
        assert_eq!(super::find_next_frame_start_parse_attempts(), 1);
    }

    #[test]
    fn find_next_frame_start_does_not_parse_when_no_sync_candidate_exists() {
        super::reset_find_next_frame_start_parse_attempts();

        let next = super::find_next_frame_start(
            &[0x00, 0x11, 0x22, 0x33, 0x44],
            0,
            stream_info_with_total_samples(16),
            0,
            0,
        )
        .unwrap();

        assert!(next.is_none());
        assert_eq!(super::find_next_frame_start_parse_attempts(), 0);
    }

    #[test]
    fn split_input_seals_at_the_next_frame_boundary() {
        let first_frame = frame_bytes(0, 5);
        let second_frame = frame_bytes(1, 7);
        let split_point = first_frame.len() + 3;
        let mut joined = first_frame.clone();
        joined.extend_from_slice(&second_frame);

        let mut scanner = ChunkScanner::new(
            stream_info(),
            ChunkScannerConfig {
                target_pcm_frames_per_chunk: 24,
                max_frames_per_chunk: 8,
                max_bytes_per_chunk: 128,
            },
        );

        assert_eq!(
            scanner.push_bytes(&joined[..split_point]).unwrap(),
            ChunkStep::Pending
        );

        match scanner.push_bytes(&joined[split_point..]).unwrap() {
            ChunkStep::Sealed(chunk) => {
                assert_eq!(chunk.sequence, 0);
                assert_eq!(chunk.start_frame_index, 0);
                assert_eq!(chunk.start_sample_number, 0);
                assert_eq!(chunk.frame_block_sizes, vec![16]);
                assert_eq!(chunk.bytes.as_ref(), first_frame.as_slice());
            }
            ChunkStep::Pending => panic!("expected the first chunk to seal at the next boundary"),
        }

        match scanner.finish().unwrap() {
            ChunkStep::Sealed(chunk) => {
                assert_eq!(chunk.sequence, 1);
                assert_eq!(chunk.start_frame_index, 1);
                assert_eq!(chunk.start_sample_number, 16);
                assert_eq!(chunk.frame_block_sizes, vec![16]);
                assert_eq!(chunk.bytes.as_ref(), second_frame.as_slice());
            }
            ChunkStep::Pending => panic!("expected finish to flush the trailing chunk"),
        }
    }

    #[test]
    fn finish_flushes_a_partial_final_chunk_without_full_frame_crc_validation() {
        let trailing_frame = frame_bytes(0, 5);
        let mut scanner = ChunkScanner::new(
            stream_info(),
            ChunkScannerConfig {
                target_pcm_frames_per_chunk: 128,
                max_frames_per_chunk: 8,
                max_bytes_per_chunk: 128,
            },
        );

        assert_eq!(scanner.push_bytes(&trailing_frame).unwrap(), ChunkStep::Pending);

        match scanner.finish().unwrap() {
            ChunkStep::Sealed(chunk) => {
                assert_eq!(chunk.sequence, 0);
                assert_eq!(chunk.start_frame_index, 0);
                assert_eq!(chunk.start_sample_number, 0);
                assert_eq!(chunk.frame_block_sizes, vec![16]);
                assert_eq!(chunk.bytes.as_ref(), trailing_frame.as_slice());
            }
            ChunkStep::Pending => panic!("expected finish to flush the final partial chunk"),
        }
    }

    #[test]
    fn seals_when_the_frame_count_budget_is_reached() {
        let mut joined = frame_bytes(0, 5);
        joined.extend(frame_bytes(1, 6));
        joined.extend(frame_bytes(2, 7));

        let mut scanner = ChunkScanner::new(
            stream_info_with_total_samples(96),
            ChunkScannerConfig {
                target_pcm_frames_per_chunk: 128,
                max_frames_per_chunk: 2,
                max_bytes_per_chunk: 128,
            },
        );

        match scanner.push_bytes(&joined).unwrap() {
            ChunkStep::Sealed(chunk) => {
                assert_eq!(chunk.sequence, 0);
                assert_eq!(chunk.start_frame_index, 0);
                assert_eq!(chunk.start_sample_number, 0);
                assert_eq!(chunk.frame_block_sizes, vec![16, 16]);
                assert_eq!(chunk.bytes.as_ref(), &joined[..25]);
            }
            ChunkStep::Pending => panic!("expected max-frames budget to seal a chunk"),
        }

        match scanner.finish().unwrap() {
            ChunkStep::Sealed(chunk) => {
                assert_eq!(chunk.sequence, 1);
                assert_eq!(chunk.start_frame_index, 2);
                assert_eq!(chunk.start_sample_number, 32);
                assert_eq!(chunk.frame_block_sizes, vec![16]);
                assert_eq!(chunk.bytes.as_ref(), &joined[25..]);
            }
            ChunkStep::Pending => panic!("expected finish to flush the trailing chunk"),
        }
    }

    #[test]
    fn seals_when_the_byte_budget_is_reached() {
        let mut joined = frame_bytes(0, 5);
        joined.extend(frame_bytes(1, 6));
        joined.extend(frame_bytes(2, 7));

        let mut scanner = ChunkScanner::new(
            stream_info(),
            ChunkScannerConfig {
                target_pcm_frames_per_chunk: 128,
                max_frames_per_chunk: 8,
                max_bytes_per_chunk: 25,
            },
        );

        match scanner.push_bytes(&joined).unwrap() {
            ChunkStep::Sealed(chunk) => {
                assert_eq!(chunk.sequence, 0);
                assert_eq!(chunk.start_frame_index, 0);
                assert_eq!(chunk.start_sample_number, 0);
                assert_eq!(chunk.frame_block_sizes, vec![16, 16]);
                assert_eq!(chunk.bytes.as_ref(), &joined[..25]);
            }
            ChunkStep::Pending => panic!("expected max-bytes budget to seal a chunk"),
        }

        match scanner.finish().unwrap() {
            ChunkStep::Sealed(chunk) => {
                assert_eq!(chunk.sequence, 1);
                assert_eq!(chunk.start_frame_index, 2);
                assert_eq!(chunk.start_sample_number, 32);
                assert_eq!(chunk.frame_block_sizes, vec![16]);
                assert_eq!(chunk.bytes.as_ref(), &joined[25..]);
            }
            ChunkStep::Pending => panic!("expected finish to flush the trailing chunk"),
        }
    }

    #[test]
    fn push_bytes_buffers_new_input_before_returning_a_queued_chunk() {
        let mut initial = frame_bytes(0, 5);
        initial.extend(frame_bytes(1, 6));
        initial.extend(frame_bytes(2, 7));
        initial.extend(frame_bytes(3, 8));
        initial.extend(frame_bytes(4, 9));
        let trailing = frame_bytes(5, 10);

        let mut scanner = ChunkScanner::new(
            stream_info_with_total_samples(96),
            ChunkScannerConfig {
                target_pcm_frames_per_chunk: 128,
                max_frames_per_chunk: 2,
                max_bytes_per_chunk: 128,
            },
        );

        match scanner.push_bytes(&initial).unwrap() {
            ChunkStep::Sealed(chunk) => {
                assert_eq!(chunk.sequence, 0);
                assert_eq!(chunk.start_frame_index, 0);
                assert_eq!(chunk.start_sample_number, 0);
                assert_eq!(chunk.frame_block_sizes, vec![16, 16]);
                assert_eq!(chunk.bytes.as_ref(), &initial[..25]);
            }
            ChunkStep::Pending => panic!("expected first push to queue and return the first chunk"),
        }

        match scanner.push_bytes(&trailing).unwrap() {
            ChunkStep::Sealed(chunk) => {
                assert_eq!(chunk.sequence, 1);
                assert_eq!(chunk.start_frame_index, 2);
                assert_eq!(chunk.start_sample_number, 32);
                assert_eq!(chunk.frame_block_sizes, vec![16, 16]);
                assert_eq!(chunk.bytes.as_ref(), &initial[25..54]);
            }
            ChunkStep::Pending => {
                panic!("expected queued chunk to be returned while buffering new trailing input")
            }
        }

        match scanner.finish().unwrap() {
            ChunkStep::Sealed(chunk) => {
                assert_eq!(chunk.sequence, 2);
                assert_eq!(chunk.start_frame_index, 4);
                assert_eq!(chunk.start_sample_number, 64);
                assert_eq!(chunk.frame_block_sizes, vec![16, 16]);
                let mut expected = initial[54..].to_vec();
                expected.extend_from_slice(&trailing);
                assert_eq!(chunk.bytes.as_ref(), expected.as_slice());
            }
            ChunkStep::Pending => {
                panic!("expected buffered trailing input to survive queued return")
            }
        }
    }

    #[test]
    fn push_bytes_rejects_non_empty_input_after_finish() {
        let mut scanner = ChunkScanner::new(
            stream_info(),
            ChunkScannerConfig {
                target_pcm_frames_per_chunk: 128,
                max_frames_per_chunk: 8,
                max_bytes_per_chunk: 128,
            },
        );

        assert_eq!(scanner.finish().unwrap(), ChunkStep::Pending);

        let error = scanner.push_bytes(&frame_bytes(0, 5)).unwrap_err();
        assert!(matches!(
            error,
            Error::Decode(message) if message == "cannot push additional input after chunk scanner is finished"
        ));
    }
}
