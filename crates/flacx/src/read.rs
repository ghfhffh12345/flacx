use std::{
    collections::BTreeMap,
    io::{Cursor, Read, Seek},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

use bitstream_io::{BigEndian, BitRead, BitReader};

use crate::{
    config::DecodeConfig,
    crc::{crc8, crc16},
    error::{Error, Result},
    input::{WavData, WavSpec},
    model::ChannelAssignment,
    progress::{NoProgress, ProgressSink, ProgressSnapshot},
    reconstruct::{interleave_channels, restore_fixed, restore_lpc, unfold_residual},
    stream_info::StreamInfo,
};

const FLAC_MAGIC: &[u8; 4] = b"fLaC";
const STREAMINFO_BLOCK_TYPE: u8 = 0;
const MAX_SUPPORTED_BLOCK_SIZE: u16 = 16_384;
const FLAC_SYNC_CODE: u16 = 0b11_1111_1111_1110;
const FRAME_CHUNK_SIZE: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameIndex {
    frame_number: u64,
    offset: usize,
    bytes_consumed: usize,
    block_size: u16,
}

struct FrameResult {
    frame_index: usize,
    result: Result<Vec<i32>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedFrame {
    frame_number: u64,
    block_size: u16,
    bits_per_sample: u8,
    assignment: ChannelAssignment,
    bytes_consumed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SubframeHeader {
    kind: u8,
    wasted_bits: usize,
    effective_bps: u8,
}

#[allow(dead_code)]
pub(crate) fn read_flac<R: Read + Seek>(reader: R) -> Result<(WavData, StreamInfo, usize)> {
    let mut progress = NoProgress;
    read_flac_with_config(reader, DecodeConfig::default(), &mut progress)
}

pub(crate) fn read_flac_with_config<R, P>(
    mut reader: R,
    config: DecodeConfig,
    progress: &mut P,
) -> Result<(WavData, StreamInfo, usize)>
where
    R: Read + Seek,
    P: ProgressSink,
{
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    let (stream_info, frame_offset) = parse_metadata(&bytes)?;

    if !(1..=2).contains(&stream_info.channels) {
        return Err(Error::UnsupportedFlac(format!(
            "only mono/stereo decode is supported, found {} channels",
            stream_info.channels
        )));
    }
    if !matches!(stream_info.bits_per_sample, 16 | 24) {
        return Err(Error::UnsupportedFlac(format!(
            "only 16-bit and 24-bit decode is supported, found {} bits/sample",
            stream_info.bits_per_sample
        )));
    }
    if stream_info.max_block_size > MAX_SUPPORTED_BLOCK_SIZE {
        return Err(Error::UnsupportedFlac(format!(
            "block sizes above {MAX_SUPPORTED_BLOCK_SIZE} are out of scope"
        )));
    }

    let expected_frames = stream_info.total_samples as usize;
    let total_output_samples = expected_frames * usize::from(stream_info.channels);
    let wav_spec = WavSpec {
        sample_rate: stream_info.sample_rate,
        channels: stream_info.channels,
        bits_per_sample: stream_info.bits_per_sample,
        total_samples: stream_info.total_samples,
        bytes_per_sample: u16::from(stream_info.bits_per_sample / 8),
    };

    if expected_frames == 0 {
        return Ok((
            WavData {
                spec: wav_spec,
                samples: Vec::new(),
            },
            stream_info,
            0,
        ));
    }

    let frames = index_frames(&bytes, frame_offset, stream_info)?;
    let frame_count = frames.len();
    let bytes: Arc<[u8]> = Arc::from(bytes);
    let frames: Arc<[FrameIndex]> = Arc::from(frames);
    let mut samples = Vec::with_capacity(total_output_samples);
    decode_frames_parallel(bytes, frames, stream_info, config, progress, &mut samples)?;

    if samples.len() != total_output_samples {
        return Err(Error::Decode(format!(
            "decoded sample count mismatch: expected {total_output_samples}, got {}",
            samples.len()
        )));
    }

    Ok((
        WavData {
            spec: wav_spec,
            samples,
        },
        stream_info,
        frame_count,
    ))
}

fn index_frames(
    bytes: &[u8],
    frame_offset: usize,
    stream_info: StreamInfo,
) -> Result<Vec<FrameIndex>> {
    let mut expected_frame_number = 0u64;
    let mut processed_samples = 0usize;
    let mut cursor = frame_offset;
    let mut frames = Vec::new();

    while processed_samples < stream_info.total_samples as usize {
        let frame = scan_frame(&bytes[cursor..], stream_info, expected_frame_number)?;
        frames.push(FrameIndex {
            frame_number: frame.frame_number,
            offset: cursor,
            bytes_consumed: frame.bytes_consumed,
            block_size: frame.block_size,
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

fn decode_frames_parallel<P>(
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
    if frames.is_empty() {
        return Ok(());
    }

    let worker_count = config.threads.max(1).min(frames.len());
    let next_chunk = Arc::new(AtomicUsize::new(0));

    thread::scope(|scope| -> Result<()> {
        let (sender, receiver) = mpsc::channel::<FrameResult>();

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

                    for frame_index in chunk_start..chunk_end {
                        let frame = &frames[frame_index];
                        let frame_bytes = &bytes[frame.offset..frame.offset + frame.bytes_consumed];
                        let result = decode_frame(frame_bytes, stream_info, frame.frame_number)
                            .map(|frame| frame.samples);

                        if sender
                            .send(FrameResult {
                                frame_index,
                                result,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            });
        }

        drop(sender);

        let mut next_expected = 0usize;
        let mut processed_samples = 0u64;
        let mut pending: BTreeMap<usize, Result<Vec<i32>>> = BTreeMap::new();

        while next_expected < frames.len() {
            if let Some(result) = pending.remove(&next_expected) {
                processed_samples = process_frame_result(
                    samples,
                    result,
                    frames[next_expected].block_size,
                    processed_samples,
                    stream_info.total_samples,
                    next_expected + 1,
                    frames.len(),
                    progress,
                )?;
                next_expected += 1;
                continue;
            }

            let frame_result = receiver.recv().map_err(|_| {
                Error::Thread("frame worker channel closed before all frames were decoded".into())
            })?;
            pending.insert(frame_result.frame_index, frame_result.result);

            while let Some(result) = pending.remove(&next_expected) {
                processed_samples = process_frame_result(
                    samples,
                    result,
                    frames[next_expected].block_size,
                    processed_samples,
                    stream_info.total_samples,
                    next_expected + 1,
                    frames.len(),
                    progress,
                )?;
                next_expected += 1;
            }
        }

        Ok(())
    })
}

fn process_frame_result<P>(
    samples: &mut Vec<i32>,
    result: Result<Vec<i32>>,
    block_size: u16,
    processed_samples: u64,
    total_samples: u64,
    completed_frames: usize,
    total_frames: usize,
    progress: &mut P,
) -> Result<u64>
where
    P: ProgressSink,
{
    let frame_samples = result?;
    samples.extend(frame_samples);
    let processed_samples = processed_samples + u64::from(block_size);
    progress.on_frame(ProgressSnapshot {
        processed_samples,
        total_samples,
        completed_frames,
        total_frames,
    })?;
    Ok(processed_samples)
}

fn scan_frame(
    bytes: &[u8],
    stream_info: StreamInfo,
    expected_frame_number: u64,
) -> Result<ParsedFrame> {
    let parsed = parse_frame(bytes, stream_info, expected_frame_number)?;
    let mut reader = BitReader::endian(Cursor::new(&bytes[parsed.bytes_consumed..]), BigEndian);

    for bits_per_channel in channel_bits_per_sample(parsed.assignment, parsed.bits_per_sample)
        .into_iter()
        .take(channel_count(parsed.assignment))
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
        bytes_consumed: footer_start + 2,
        ..parsed
    })
}

fn parse_frame(
    bytes: &[u8],
    stream_info: StreamInfo,
    expected_frame_number: u64,
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
    if reader.read_bit()? {
        return Err(Error::UnsupportedFlac(
            "variable-blocksize frames are out of scope".into(),
        ));
    }

    let block_size_bits: u8 = reader.read_unsigned_var(4)?;
    let sample_rate_bits: u8 = reader.read_unsigned_var(4)?;
    let assignment_bits: u8 = reader.read_unsigned_var(4)?;
    let bits_per_sample_bits: u8 = reader.read_unsigned_var(3)?;
    if reader.read_bit()? {
        return Err(Error::InvalidFlac("frame header reserved bit must be zero"));
    }

    reader.byte_align();
    let (frame_number, utf8_len) = decode_utf8_number(reader.aligned_reader())?;
    if frame_number != expected_frame_number {
        return Err(Error::Decode(format!(
            "expected frame number {expected_frame_number}, found {frame_number}"
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
        frame_number,
        block_size,
        bits_per_sample,
        assignment,
        bytes_consumed: header_end + 1,
    })
}

struct DecodedFrame {
    samples: Vec<i32>,
}

fn decode_frame(
    bytes: &[u8],
    stream_info: StreamInfo,
    expected_frame_number: u64,
) -> Result<DecodedFrame> {
    let parsed = parse_frame(bytes, stream_info, expected_frame_number)?;
    let mut reader = BitReader::endian(Cursor::new(&bytes[parsed.bytes_consumed..]), BigEndian);

    let subframe_bps = channel_bits_per_sample(parsed.assignment, parsed.bits_per_sample);
    let mut channels = Vec::with_capacity(channel_count(parsed.assignment));
    for bits_per_channel in subframe_bps
        .into_iter()
        .take(channel_count(parsed.assignment))
    {
        channels.push(decode_subframe(
            &mut reader,
            bits_per_channel,
            parsed.block_size,
        )?);
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

    Ok(DecodedFrame {
        samples: interleave_channels(parsed.assignment, &channels)?,
    })
}

fn decode_subframe<R: Read>(
    reader: &mut BitReader<R, BigEndian>,
    bits_per_sample: u8,
    block_size: u16,
) -> Result<Vec<i32>> {
    let header = parse_subframe_header(reader, bits_per_sample)?;

    let mut samples = match header.kind {
        0b000000 => {
            vec![read_signed_sample(reader, header.effective_bps)?; usize::from(block_size)]
        }
        0b000001 => {
            let mut samples = Vec::with_capacity(usize::from(block_size));
            for _ in 0..block_size {
                samples.push(read_signed_sample(reader, header.effective_bps)?);
            }
            samples
        }
        0b001000..=0b001100 => {
            let order = header.kind - 0b001000;
            let warmup = read_warmup(reader, header.effective_bps, order)?;
            let residuals = read_residual(reader, block_size, order)?;
            restore_fixed(order, warmup, residuals)?
        }
        0b100000..=0b111111 => {
            let order = header.kind - 0b100000 + 1;
            let warmup = read_warmup(reader, header.effective_bps, order)?;
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
            let residuals = read_residual(reader, block_size, order)?;
            restore_lpc(order, warmup, shift, &coefficients, residuals)?
        }
        _ => {
            return Err(Error::UnsupportedFlac(format!(
                "subframe type {kind:#08b} is out of scope",
                kind = header.kind
            )));
        }
    };

    if header.wasted_bits > 0 {
        for sample in &mut samples {
            *sample = i32::try_from(i64::from(*sample) << header.wasted_bits)
                .map_err(|_| Error::Decode("wasted-bit restoration overflowed".into()))?;
        }
    }

    Ok(samples)
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

fn read_residual<R: Read>(
    reader: &mut BitReader<R, BigEndian>,
    block_size: u16,
    predictor_order: u8,
) -> Result<Vec<i32>> {
    let mut residuals = Vec::with_capacity(usize::from(block_size) - usize::from(predictor_order));
    visit_residuals(reader, block_size, predictor_order, |residual| {
        residuals.push(residual);
        Ok(())
    })?;
    Ok(residuals)
}

fn skip_residual<R: Read>(
    reader: &mut BitReader<R, BigEndian>,
    block_size: u16,
    predictor_order: u8,
) -> Result<()> {
    visit_residuals(reader, block_size, predictor_order, |_| Ok(()))
}

fn read_warmup<R: Read>(
    reader: &mut BitReader<R, BigEndian>,
    bits_per_sample: u8,
    order: u8,
) -> Result<Vec<i32>> {
    let mut warmup = Vec::with_capacity(usize::from(order));
    for _ in 0..order {
        warmup.push(read_signed_sample(reader, bits_per_sample)?);
    }
    Ok(warmup)
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

fn decode_bits_per_sample(code: u8, stream_bps: u8) -> Result<u8> {
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

fn decode_channel_assignment(code: u8) -> Result<ChannelAssignment> {
    match code {
        0b0000 => Ok(ChannelAssignment::IndependentMono),
        0b0001 => Ok(ChannelAssignment::IndependentStereo),
        0b1000 => Ok(ChannelAssignment::LeftSide),
        0b1001 => Ok(ChannelAssignment::SideRight),
        0b1010 => Ok(ChannelAssignment::MidSide),
        0b0010..=0b0111 | 0b1011..=0b1111 => Err(Error::UnsupportedFlac(format!(
            "channel assignment {code:#06b} is out of scope"
        ))),
        _ => unreachable!(),
    }
}

fn channel_bits_per_sample(assignment: ChannelAssignment, bits_per_sample: u8) -> [u8; 2] {
    match assignment {
        ChannelAssignment::IndependentMono => [bits_per_sample, bits_per_sample],
        ChannelAssignment::IndependentStereo => [bits_per_sample, bits_per_sample],
        ChannelAssignment::LeftSide => [bits_per_sample, bits_per_sample + 1],
        ChannelAssignment::SideRight => [bits_per_sample + 1, bits_per_sample],
        ChannelAssignment::MidSide => [bits_per_sample, bits_per_sample + 1],
    }
}

fn channel_count(assignment: ChannelAssignment) -> usize {
    match assignment {
        ChannelAssignment::IndependentMono => 1,
        _ => 2,
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

fn parse_metadata(bytes: &[u8]) -> Result<(StreamInfo, usize)> {
    if bytes.len() < 8 {
        return Err(Error::InvalidFlac("file is too short"));
    }
    if &bytes[..4] != FLAC_MAGIC {
        return Err(Error::InvalidFlac("expected fLaC stream marker"));
    }

    let mut offset = 4usize;
    let mut saw_streaminfo = false;
    let mut stream_info = None;
    loop {
        if offset + 4 > bytes.len() {
            return Err(Error::InvalidFlac("metadata block header is truncated"));
        }
        let header = bytes[offset];
        let is_last = header & 0x80 != 0;
        let block_type = header & 0x7f;
        let block_len =
            u32::from_be_bytes([0, bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
                as usize;
        offset += 4;
        if offset + block_len > bytes.len() {
            return Err(Error::InvalidFlac("metadata block body is truncated"));
        }

        if !saw_streaminfo {
            if block_type != STREAMINFO_BLOCK_TYPE || block_len != 34 {
                return Err(Error::InvalidFlac(
                    "first metadata block must be a 34-byte STREAMINFO block",
                ));
            }
            let mut raw = [0u8; 34];
            raw.copy_from_slice(&bytes[offset..offset + 34]);
            stream_info = Some(StreamInfo::from_bytes(raw));
            saw_streaminfo = true;
        }

        offset += block_len;
        if is_last {
            break;
        }
    }

    Ok((
        stream_info.ok_or(Error::InvalidFlac("missing STREAMINFO block"))?,
        offset,
    ))
}
