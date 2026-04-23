use std::{
    collections::VecDeque,
    io::{Cursor, Read},
    sync::Arc,
};

use bitstream_io::{BigEndian, BitRead, BitReader};

use crate::{
    crc::crc8,
    error::{Error, Result},
    stream_info::StreamInfo,
};

use super::{FLAC_SYNC_CODE, FrameHeaderNumber, FrameHeaderNumberKind, ParsedFrame};

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
            return Ok(ChunkStep::Pending);
        }
        self.buffered_bytes.extend_from_slice(bytes);
        if let Some(chunk) = self.ready_chunks.pop_front() {
            return Ok(ChunkStep::Sealed(chunk));
        }
        self.scan_available_frames(false)
    }

    pub(super) fn finish(&mut self) -> Result<ChunkStep> {
        if let Some(chunk) = self.ready_chunks.pop_front() {
            return Ok(ChunkStep::Sealed(chunk));
        }
        if self.finished {
            return Ok(ChunkStep::Pending);
        }
        let step = self.scan_available_frames(true)?;
        if matches!(step, ChunkStep::Pending) {
            self.finished = true;
        }
        Ok(step)
    }

    fn scan_available_frames(&mut self, eof: bool) -> Result<ChunkStep> {
        loop {
            if let Some(chunk) = self.ready_chunks.pop_front() {
                return Ok(ChunkStep::Sealed(chunk));
            }

            if self.current_frame.is_none() {
                if self.buffered_bytes.is_empty() {
                    return Ok(self.finish_or_pending(eof));
                }

                match parse_frame_header(
                    &self.buffered_bytes,
                    self.stream_info,
                    self.next_frame_index as u64,
                    self.next_sample_number,
                ) {
                    Ok(parsed) => {
                        self.current_frame = Some(ScannedFrame {
                            start_offset: 0,
                            parsed,
                        });
                    }
                    Err(error) if is_incomplete_header(&error) && !eof => {
                        return Ok(ChunkStep::Pending);
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
                self.current_frame = None;
                self.accept_frame(current, self.buffered_bytes.len());
                return Ok(self.finish_or_pending(true));
            }

            return Ok(ChunkStep::Pending);
        }
    }

    fn finish_or_pending(&mut self, eof: bool) -> ChunkStep {
        if let Some(chunk) = self.ready_chunks.pop_front() {
            return ChunkStep::Sealed(chunk);
        }

        if eof {
            if let Some(chunk) = self.seal_pending_chunk() {
                return ChunkStep::Sealed(chunk);
            }
            self.finished = true;
        }

        ChunkStep::Pending
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
    while offset < bytes.len() {
        match parse_frame_header(
            &bytes[offset..],
            stream_info,
            expected_frame_number,
            expected_sample_number,
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

fn is_incomplete_header(error: &Error) -> bool {
    match error {
        Error::Io(inner) => inner.kind() == std::io::ErrorKind::UnexpectedEof,
        Error::InvalidFlac(message) => *message == "unexpected EOF while reading frames",
        _ => false,
    }
}

fn parse_frame_header(
    bytes: &[u8],
    stream_info: StreamInfo,
    expected_frame_number: u64,
    expected_sample_number: u64,
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

    let expected_number = match header_number.kind {
        FrameHeaderNumberKind::FrameNumber => expected_frame_number,
        FrameHeaderNumberKind::SampleNumber => expected_sample_number,
    };
    if header_number.value != expected_number {
        return Err(Error::Decode(format!(
            "expected {}, found {}",
            expected_number, header_number.value
        )));
    }

    let block_size = decode_block_size(block_size_bits, reader.aligned_reader())?;
    let sample_rate = decode_sample_rate(
        sample_rate_bits,
        reader.aligned_reader(),
        stream_info.sample_rate,
    )?;
    let bits_per_sample =
        super::frame::decode_bits_per_sample(bits_per_sample_bits, stream_info.bits_per_sample)?;
    let assignment = super::frame::decode_channel_assignment(assignment_bits)?;

    let header_end = 4usize
        .saturating_add(utf8_len)
        .saturating_add(block_size_extra_len(block_size_bits))
        .saturating_add(sample_rate_extra_len(sample_rate_bits));
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
    use crate::{
        crc::crc8,
        read::chunk::{ChunkScanner, ChunkScannerConfig, ChunkStep},
        stream_info::StreamInfo,
    };

    fn stream_info() -> StreamInfo {
        StreamInfo {
            min_block_size: 16,
            max_block_size: 16,
            min_frame_size: 8,
            max_frame_size: 64,
            sample_rate: 44_100,
            channels: 1,
            bits_per_sample: 16,
            total_samples: 64,
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
            stream_info(),
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
    fn push_bytes_buffers_new_input_before_returning_a_queued_chunk() {
        let mut initial = frame_bytes(0, 5);
        initial.extend(frame_bytes(1, 6));
        initial.extend(frame_bytes(2, 7));
        initial.extend(frame_bytes(3, 8));
        initial.extend(frame_bytes(4, 9));
        let trailing = frame_bytes(5, 10);

        let mut scanner = ChunkScanner::new(
            stream_info(),
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
            ChunkStep::Pending => panic!("expected buffered trailing input to survive queued return"),
        }
    }
}
