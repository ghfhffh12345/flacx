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
    input::{EncodePcmStream, PcmSpec},
    metadata::Metadata,
    model::ChannelAssignment,
    pcm::{is_supported_channel_mask, ordinary_channel_mask},
    progress::NoProgress,
    stream_info::{MAX_STREAMINFO_SAMPLE_RATE, StreamInfo},
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
            FlacPcmStream::from_parts(self.reader, self.stream_info, self.spec),
        )
    }
}

#[derive(Debug)]
pub struct FlacPcmStream<R> {
    reader: R,
    stream_info: StreamInfo,
    spec: PcmSpec,
    next_frame_index: usize,
    next_sample_number: u64,
    threads: usize,
    pending_bytes: Vec<u8>,
    eof: bool,
}

impl<R> FlacPcmStream<R> {
    /// Start building a directly constructed FLAC PCM stream.
    #[must_use]
    pub fn builder(reader: R) -> FlacPcmStreamBuilder<R> {
        FlacPcmStreamBuilder::new(reader)
    }

    fn from_parts(reader: R, stream_info: StreamInfo, spec: PcmSpec) -> Self {
        Self {
            reader,
            stream_info,
            spec,
            next_frame_index: 0,
            next_sample_number: 0,
            threads: 1,
            pending_bytes: Vec::new(),
            eof: false,
        }
    }

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
        ))
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
    fn spec(&self) -> PcmSpec {
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
    use super::{FlacPcmStream, StreamInfo};
    use crate::model::ChannelAssignment;
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
}
