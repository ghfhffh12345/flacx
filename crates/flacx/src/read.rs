use std::{
    env,
    fs::OpenOptions,
    io::Write as _,
    io::{Read, Seek},
    sync::Arc,
    time::Instant,
};

use crate::{
    config::DecodeConfig,
    error::{Error, Result},
    input::{EncodePcmStream, WavSpec},
    metadata::DecodeMetadata,
    model::ChannelAssignment,
    progress::NoProgress,
    stream_info::StreamInfo,
};

const FLAC_MAGIC: &[u8; 4] = b"fLaC";
const STREAMINFO_BLOCK_TYPE: u8 = 0;
const FLAC_SYNC_CODE: u16 = 0b11_1111_1111_1110;
#[allow(dead_code)]
const FRAME_CHUNK_SIZE: usize = 128;
const FLAC_READ_CHUNK_SIZE: usize = 64 * 1024;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FlacReaderOptions {
    pub strict_seektable_validation: bool,
    pub strict_channel_mask_provenance: bool,
}

pub trait DecodePcmStream: EncodePcmStream {
    fn total_input_frames(&self) -> usize;
    fn completed_input_frames(&self) -> usize;
    fn stream_info(&self) -> StreamInfo;
    fn set_threads(&mut self, _threads: usize) {}
    fn take_decoded_samples(&mut self) -> Result<Option<(Vec<i32>, usize)>> {
        Ok(None)
    }
}

#[derive(Debug)]
pub struct FlacReader<R> {
    reader: R,
    frame_offset: u64,
    stream_info: StreamInfo,
    metadata: DecodeMetadata,
    spec: WavSpec,
}

impl<R: Read + Seek> FlacReader<R> {
    pub fn new(reader: R) -> Result<Self> {
        read_flac_reader_with_options(reader, FlacReaderOptions::default())
    }

    #[must_use]
    pub fn spec(&self) -> WavSpec {
        self.spec
    }

    #[must_use]
    pub fn metadata(&self) -> &DecodeMetadata {
        &self.metadata
    }

    #[must_use]
    pub fn stream_info(&self) -> StreamInfo {
        self.stream_info
    }

    pub fn into_pcm_stream(self) -> FlacPcmStream<R> {
        self.into_session_parts().3
    }

    pub(crate) fn into_session_parts(
        mut self,
    ) -> (DecodeMetadata, StreamInfo, WavSpec, FlacPcmStream<R>) {
        self.reader
            .seek(std::io::SeekFrom::Start(self.frame_offset))
            .expect("flac reader remains seekable through stream conversion");
        (
            self.metadata,
            self.stream_info,
            self.spec,
            FlacPcmStream {
                reader: self.reader,
                stream_info: self.stream_info,
                spec: self.spec,
                next_frame_index: 0,
                next_sample_number: 0,
                threads: 1,
                pending_bytes: Vec::new(),
                eof: false,
            },
        )
    }
}

#[derive(Debug)]
pub struct FlacPcmStream<R> {
    reader: R,
    stream_info: StreamInfo,
    spec: WavSpec,
    next_frame_index: usize,
    next_sample_number: u64,
    threads: usize,
    pending_bytes: Vec<u8>,
    eof: bool,
}

impl<R> FlacPcmStream<R> {
    #[must_use]
    pub fn spec(&self) -> WavSpec {
        self.spec
    }

    pub fn set_threads(&mut self, threads: usize) {
        self.threads = threads.max(1);
    }
}

impl<R: Read + Seek> FlacPcmStream<R> {
    fn read_next_frame_bytes(&mut self) -> Result<Option<(ParsedFrame, Vec<u8>)>> {
        loop {
            match frame::scan_frame(
                &self.pending_bytes,
                self.stream_info,
                self.next_frame_index as u64,
                self.next_sample_number,
            ) {
                Ok(parsed) => {
                    let bytes = self
                        .pending_bytes
                        .drain(..parsed.bytes_consumed)
                        .collect::<Vec<_>>();
                    return Ok(Some((parsed, bytes)));
                }
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
}

impl<R: Read + Seek> EncodePcmStream for FlacPcmStream<R> {
    fn spec(&self) -> WavSpec {
        self.spec
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize> {
        if self.next_sample_number >= self.spec.total_samples || max_frames == 0 {
            return Ok(0);
        }

        let mut total_pcm_frames = 0usize;
        let mut batch_bytes = Vec::new();
        let mut frames = Vec::new();
        while total_pcm_frames < max_frames && self.next_sample_number < self.spec.total_samples {
            let Some((parsed, frame_bytes)) = self.read_next_frame_bytes()? else {
                break;
            };
            let block_size = parsed.block_size;
            if total_pcm_frames > 0 && total_pcm_frames + usize::from(block_size) > max_frames {
                self.pending_bytes.splice(0..0, frame_bytes);
                break;
            }

            let offset = batch_bytes.len();
            batch_bytes.extend_from_slice(&frame_bytes);
            frames.push(FrameIndex {
                header_number: parsed.header_number,
                offset,
                header_bytes_consumed: parsed.header_bytes_consumed,
                bytes_consumed: frame_bytes.len(),
                block_size,
                bits_per_sample: parsed.bits_per_sample,
                assignment: parsed.assignment,
            });
            total_pcm_frames += usize::from(block_size);
            self.next_frame_index += 1;
            self.next_sample_number += u64::from(block_size);
        }

        if frames.is_empty() {
            return Ok(0);
        }

        let mut progress = NoProgress;
        frame::decode_frames_parallel(
            Arc::from(batch_bytes),
            Arc::from(frames),
            self.stream_info,
            DecodeConfig::default().with_threads(self.threads),
            &mut progress,
            output,
        )?;
        Ok(total_pcm_frames)
    }
}

impl<R: Read + Seek> DecodePcmStream for FlacPcmStream<R> {
    fn total_input_frames(&self) -> usize {
        0
    }

    fn completed_input_frames(&self) -> usize {
        self.next_frame_index
    }

    fn stream_info(&self) -> StreamInfo {
        self.stream_info
    }

    fn set_threads(&mut self, threads: usize) {
        self.threads = threads.max(1);
    }

    fn take_decoded_samples(&mut self) -> Result<Option<(Vec<i32>, usize)>> {
        if self.next_frame_index != 0 || !self.pending_bytes.is_empty() {
            return Ok(None);
        }

        let profile_path = env::var_os("FLACX_DECODE_PROFILE").map(std::path::PathBuf::from);
        let profile_start = Instant::now();
        let mut bytes = Vec::new();
        let read_start = Instant::now();
        self.reader.read_to_end(&mut bytes)?;
        let read_elapsed = read_start.elapsed();
        self.eof = true;
        if bytes.is_empty() {
            if let Some(path) = profile_path
                && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path)
            {
                let _ = writeln!(
                    file,
                    "event=decode_phase\tphase=read_to_end\telapsed_seconds={:.9}",
                    read_elapsed.as_secs_f64()
                );
            }
            return Ok(Some((Vec::new(), 0)));
        }

        let index_start = Instant::now();
        let frames = frame::index_frames(&bytes, 0, self.stream_info)?;
        let index_elapsed = index_start.elapsed();
        let frame_count = frames.len();
        let total_output_samples = usize::try_from(self.spec.total_samples)
            .unwrap_or(0)
            .saturating_mul(usize::from(self.spec.channels));
        let mut samples = Vec::with_capacity(total_output_samples);
        let mut progress = NoProgress;
        let decode_start = Instant::now();
        frame::decode_frames_parallel(
            Arc::from(bytes),
            Arc::from(frames),
            self.stream_info,
            DecodeConfig::default().with_threads(self.threads),
            &mut progress,
            &mut samples,
        )?;
        let decode_elapsed = decode_start.elapsed();
        if let Some(path) = profile_path
            && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path)
        {
            let _ = writeln!(
                file,
                "event=decode_phase\tphase=read_to_end\telapsed_seconds={:.9}",
                read_elapsed.as_secs_f64()
            );
            let _ = writeln!(
                file,
                "event=decode_phase\tphase=index_frames\telapsed_seconds={:.9}",
                index_elapsed.as_secs_f64()
            );
            let _ = writeln!(
                file,
                "event=decode_phase\tphase=decode_frames_parallel\telapsed_seconds={:.9}",
                decode_elapsed.as_secs_f64()
            );
            let _ = writeln!(
                file,
                "event=decode_phase\tphase=total_take_decoded_samples\telapsed_seconds={:.9}",
                profile_start.elapsed().as_secs_f64()
            );
        }
        self.next_frame_index = frame_count;
        self.next_sample_number = self.spec.total_samples;
        Ok(Some((samples, frame_count)))
    }
}

pub fn read_flac_reader<R: Read + Seek>(reader: R) -> Result<FlacReader<R>> {
    read_flac_reader_with_options(reader, FlacReaderOptions::default())
}

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

mod frame;
mod metadata;

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
    metadata: &DecodeMetadata,
    strict_channel_mask_provenance: bool,
) -> Result<WavSpec> {
    let channel_mask = resolve_channel_mask(
        stream_info.channels,
        metadata,
        strict_channel_mask_provenance,
    )?;
    Ok(WavSpec {
        sample_rate: stream_info.sample_rate,
        channels: stream_info.channels,
        bits_per_sample: stream_info.bits_per_sample,
        total_samples: stream_info.total_samples,
        bytes_per_sample: u16::from(stream_info.bits_per_sample.div_ceil(8)),
        channel_mask,
    })
}

#[cfg(test)]
mod tests {
    use super::frame::{
        channel_bits_per_sample, decode_bits_per_sample, decode_channel_assignment,
    };
    use super::metadata::requires_channel_layout_provenance;
    use crate::model::ChannelAssignment;

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
}
