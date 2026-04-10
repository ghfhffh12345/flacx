use std::{
    io::{Read, Seek},
    sync::Arc,
};

use crate::{
    config::DecodeConfig,
    error::{Error, Result},
    input::{WavData, WavSpec},
    metadata::WavMetadata,
    model::ChannelAssignment,
    progress::{NoProgress, ProgressSink},
    stream_info::StreamInfo,
};

const FLAC_MAGIC: &[u8; 4] = b"fLaC";
const STREAMINFO_BLOCK_TYPE: u8 = 0;
const FLAC_SYNC_CODE: u16 = 0b11_1111_1111_1110;
const FRAME_CHUNK_SIZE: usize = 32;

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
struct FrameIndex {
    header_number: FrameHeaderNumber,
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
    header_number: FrameHeaderNumber,
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

pub(crate) struct DecodedFlacData {
    pub(crate) wav: WavData,
    pub(crate) metadata: WavMetadata,
    pub(crate) stream_info: StreamInfo,
    pub(crate) frame_count: usize,
}

#[allow(dead_code)]
pub(crate) fn read_flac<R: Read + Seek>(reader: R) -> Result<(WavData, StreamInfo, usize)> {
    let mut progress = NoProgress;
    let decoded = read_flac_for_decode(reader, DecodeConfig::default(), &mut progress)?;
    Ok((decoded.wav, decoded.stream_info, decoded.frame_count))
}

pub(crate) fn read_flac_for_decode<R, P>(
    mut reader: R,
    config: DecodeConfig,
    progress: &mut P,
) -> Result<DecodedFlacData>
where
    R: Read + Seek,
    P: ProgressSink,
{
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    let (stream_info, metadata, frame_offset) =
        parse_metadata(&bytes, config.strict_seektable_validation)?;

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

    let expected_frames = stream_info.total_samples as usize;
    let total_output_samples = expected_frames * usize::from(stream_info.channels);
    let channel_mask = resolve_channel_mask(
        stream_info.channels,
        &metadata,
        config.strict_channel_mask_provenance,
    )?;
    let wav_spec = WavSpec {
        sample_rate: stream_info.sample_rate,
        channels: stream_info.channels,
        bits_per_sample: stream_info.bits_per_sample,
        total_samples: stream_info.total_samples,
        bytes_per_sample: u16::from(stream_info.bits_per_sample.div_ceil(8)),
        channel_mask,
    };

    if expected_frames == 0 {
        return Ok(DecodedFlacData {
            wav: WavData {
                spec: wav_spec,
                samples: Vec::new(),
            },
            metadata,
            stream_info,
            frame_count: 0,
        });
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
    Ok(DecodedFlacData {
        wav: WavData {
            spec: wav_spec,
            samples,
        },
        metadata,
        stream_info,
        frame_count,
    })
}

mod frame;
mod metadata;

use frame::{decode_frames_parallel, index_frames};
pub use metadata::inspect_flac_total_samples;
use metadata::{parse_metadata, resolve_channel_mask};

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
