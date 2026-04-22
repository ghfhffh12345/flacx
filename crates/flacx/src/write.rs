use std::io::{self, Seek, SeekFrom, Write};

use crate::{
    metadata::{FlacMetadataBlock, SEEKTABLE_BLOCK_TYPE, SEEKTABLE_POINT_LEN, SeekPoint},
    stream_info::StreamInfo,
};

mod frame;

pub(crate) use frame::{EncodedFrame, FrameHeaderNumber, serialize_frame};
#[cfg(test)]
pub(crate) use frame::{
    bit_depth_bits, channel_assignment_bits, encode_frame_header, encode_utf8_number,
    sample_rate_is_representable,
};

const MAX_METADATA_PAYLOAD_LEN: usize = 0x00ff_ffff;
const MAX_SEEKTABLE_POINTS: usize = MAX_METADATA_PAYLOAD_LEN / SEEKTABLE_POINT_LEN;
const STREAMINFO_LENGTH: u32 = 34;
pub(crate) struct FlacWriter<W: Seek + Write> {
    writer: W,
    position: u64,
    stream_info: StreamInfo,
    streaminfo_offset: u64,
    seektable: Option<SeekTableReservation>,
}

impl<W: Seek + Write> FlacWriter<W> {
    pub(crate) fn new(
        mut writer: W,
        stream_info: StreamInfo,
        metadata_blocks: &[FlacMetadataBlock],
        total_frames: usize,
        allow_implicit_seektable: bool,
    ) -> io::Result<Self> {
        let mut position = writer.stream_position()?;
        let write_counted = |writer: &mut W, position: &mut u64, bytes: &[u8]| {
            writer.write_all(bytes)?;
            *position = position.saturating_add(bytes.len() as u64);
            Ok::<(), io::Error>(())
        };
        let has_explicit_seektable = metadata_blocks
            .iter()
            .any(|block| matches!(block, FlacMetadataBlock::SeekTable(_)));
        let seektable_selection = if has_explicit_seektable || !allow_implicit_seektable {
            None
        } else {
            SeekTableSelection::for_total_frames(total_frames)
        };
        write_counted(&mut writer, &mut position, b"fLaC")?;
        write_counted(
            &mut writer,
            &mut position,
            &metadata_block_header(
                0,
                metadata_blocks.is_empty() && seektable_selection.is_none(),
                STREAMINFO_LENGTH,
            )?,
        )?;
        let streaminfo_offset = position;
        write_counted(&mut writer, &mut position, &stream_info.to_bytes())?;
        let mut seektable_payload_offset = None;
        if let Some(selection) = &seektable_selection {
            write_counted(
                &mut writer,
                &mut position,
                &metadata_block_header(
                    SEEKTABLE_BLOCK_TYPE,
                    metadata_blocks.is_empty(),
                    selection.payload_len,
                )?,
            )?;
            let payload_offset = position;
            write_counted(
                &mut writer,
                &mut position,
                &vec![0u8; selection.payload_len as usize],
            )?;
            seektable_payload_offset = Some(payload_offset);
        }
        for (index, block) in metadata_blocks.iter().enumerate() {
            let payload = block.payload();
            write_counted(
                &mut writer,
                &mut position,
                &metadata_block_header(
                    block.block_type(),
                    index + 1 == metadata_blocks.len(),
                    payload.len() as u32,
                )?,
            )?;
            write_counted(&mut writer, &mut position, &payload)?;
        }
        let first_frame_header_offset = position;

        Ok(Self {
            writer,
            position,
            stream_info,
            streaminfo_offset,
            seektable: seektable_selection.map(|selection| {
                SeekTableReservation::new(
                    selection.selected_frame_indices,
                    seektable_payload_offset.expect("seektable payload offset exists"),
                    first_frame_header_offset,
                )
            }),
        })
    }

    pub(crate) fn write_frame(
        &mut self,
        frame_index: usize,
        sample_offset: u64,
        sample_count: u16,
        frame: &[u8],
    ) -> io::Result<()> {
        if let Some(seektable) = &mut self.seektable {
            seektable.record_frame(frame_index, sample_offset, self.position, sample_count);
        }
        self.stream_info.update_frame_size(frame.len() as u32);
        self.write_all_counted(frame)
    }

    pub(crate) fn set_streaminfo_md5(&mut self, md5: [u8; 16]) {
        self.stream_info.md5 = md5;
    }

    #[cfg(feature = "progress")]
    pub(crate) fn bytes_written(&self) -> u64 {
        self.position
    }

    pub(crate) fn finalize(mut self) -> io::Result<(W, StreamInfo)> {
        let end_position = self.position;
        if let Some(seektable) = &self.seektable {
            let payload = seektable.payload()?;
            self.seek_counted(SeekFrom::Start(seektable.payload_offset))?;
            self.write_all_counted(&payload)?;
            self.seek_counted(SeekFrom::Start(end_position))?;
        }
        self.seek_counted(SeekFrom::Start(self.streaminfo_offset))?;
        self.write_all_counted(&self.stream_info.to_bytes())?;
        self.seek_counted(SeekFrom::Start(end_position))?;
        self.writer.flush()?;
        Ok((self.writer, self.stream_info))
    }

    fn write_all_counted(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)?;
        self.position = self.position.saturating_add(bytes.len() as u64);
        Ok(())
    }

    fn seek_counted(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.position = self.writer.seek(position)?;
        Ok(self.position)
    }
}

fn metadata_block_header(block_type: u8, is_last: bool, payload_len: u32) -> io::Result<[u8; 4]> {
    if payload_len > MAX_METADATA_PAYLOAD_LEN as u32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "metadata block payload exceeds FLAC u24 length limit",
        ));
    }

    let [_, b1, b2, b3] = payload_len.to_be_bytes();
    Ok([
        if is_last {
            0x80 | (block_type & 0x7f)
        } else {
            block_type & 0x7f
        },
        b1,
        b2,
        b3,
    ])
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SeekTableSelection {
    selected_frame_indices: Vec<usize>,
    payload_len: u32,
}

impl SeekTableSelection {
    fn for_total_frames(total_frames: usize) -> Option<Self> {
        if total_frames == 0 {
            return None;
        }

        let selected_frame_indices = selected_seektable_frame_indices(total_frames);
        Some(Self {
            payload_len: (selected_frame_indices.len() * SEEKTABLE_POINT_LEN) as u32,
            selected_frame_indices,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SeekTableReservation {
    selected_frame_indices: Vec<usize>,
    next_selected_frame: usize,
    payload_offset: u64,
    first_frame_header_offset: u64,
    points: Vec<SeekPoint>,
}

impl SeekTableReservation {
    fn new(
        selected_frame_indices: Vec<usize>,
        payload_offset: u64,
        first_frame_header_offset: u64,
    ) -> Self {
        Self {
            points: Vec::with_capacity(selected_frame_indices.len()),
            selected_frame_indices,
            next_selected_frame: 0,
            payload_offset,
            first_frame_header_offset,
        }
    }

    fn record_frame(
        &mut self,
        frame_index: usize,
        sample_number: u64,
        absolute_frame_offset: u64,
        sample_count: u16,
    ) {
        if self
            .selected_frame_indices
            .get(self.next_selected_frame)
            .copied()
            != Some(frame_index)
        {
            return;
        }

        self.points.push(SeekPoint {
            sample_number,
            frame_offset: absolute_frame_offset - self.first_frame_header_offset,
            sample_count,
        });
        self.next_selected_frame += 1;
    }

    fn payload(&self) -> io::Result<Vec<u8>> {
        if self.points.len() != self.selected_frame_indices.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "seektable reservation did not capture every selected frame",
            ));
        }
        Ok(SeekPoint::payload(&self.points))
    }
}

fn selected_seektable_frame_indices(total_frames: usize) -> Vec<usize> {
    if total_frames == 0 {
        return Vec::new();
    }

    let selected_count = total_frames.min(MAX_SEEKTABLE_POINTS);
    if selected_count == total_frames {
        return (0..total_frames).collect();
    }

    if selected_count == 1 {
        return vec![0];
    }

    (0..selected_count)
        .map(|slot| {
            ((slot as u128 * (total_frames - 1) as u128) / (selected_count - 1) as u128) as usize
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{
        FlacWriter, FrameHeaderNumber, MAX_SEEKTABLE_POINTS, SEEKTABLE_BLOCK_TYPE, bit_depth_bits,
        channel_assignment_bits, encode_frame_header, encode_utf8_number, metadata_block_header,
        sample_rate_is_representable, selected_seektable_frame_indices,
    };
    use crate::{
        level::LevelProfile,
        metadata::{FlacMetadataBlock, FxmdChunkPolicy, MetadataDraft},
        model::{ChannelAssignment, encode_frame},
        stream_info::StreamInfo,
    };

    #[test]
    fn sample_rate_representation_matches_streamable_header_limits() {
        assert!(!sample_rate_is_representable(0));
        assert!(sample_rate_is_representable(44_100));
        assert!(sample_rate_is_representable(50_000));
        assert!(sample_rate_is_representable(65_000));
        assert!(sample_rate_is_representable(65_350));
        assert!(!sample_rate_is_representable(700_001));
    }

    #[test]
    fn frame_header_falls_back_to_streaminfo_sample_rate_code_when_needed() {
        let header = encode_frame_header(
            4_096,
            700_001,
            ChannelAssignment::Independent(1),
            16,
            FrameHeaderNumber::Frame(0),
        )
        .unwrap();

        let sample_rate_bits = header[2] & 0b1111;
        assert_eq!(sample_rate_bits, 0b0000);
    }

    #[test]
    fn utf8_like_frame_numbers_match_rfc_ranges() {
        assert_eq!(encode_utf8_number(0).unwrap().as_slice(), &[0x00]);
        assert_eq!(encode_utf8_number(0x7f).unwrap().as_slice(), &[0x7f]);
        assert_eq!(encode_utf8_number(0x80).unwrap().as_slice(), &[0xc2, 0x80]);
        assert_eq!(
            encode_utf8_number(0x800).unwrap().as_slice(),
            &[0xe0, 0xa0, 0x80]
        );
        assert_eq!(
            encode_utf8_number(0x1f_ffff).unwrap().as_slice(),
            &[0xf7, 0xbf, 0xbf, 0xbf]
        );
    }

    #[test]
    fn independent_assignment_bits_match_rfc_table() {
        assert_eq!(
            channel_assignment_bits(ChannelAssignment::Independent(1)),
            0b0000
        );
        assert_eq!(
            channel_assignment_bits(ChannelAssignment::Independent(2)),
            0b0001
        );
        assert_eq!(
            channel_assignment_bits(ChannelAssignment::Independent(3)),
            0b0010
        );
        assert_eq!(
            channel_assignment_bits(ChannelAssignment::Independent(8)),
            0b0111
        );
        assert_eq!(channel_assignment_bits(ChannelAssignment::LeftSide), 0b1000);
        assert_eq!(
            channel_assignment_bits(ChannelAssignment::SideRight),
            0b1001
        );
        assert_eq!(channel_assignment_bits(ChannelAssignment::MidSide), 0b1010);
    }

    #[test]
    fn frame_header_uses_supported_independent_assignment_bits() {
        let mut interleaved = Vec::new();
        for sample in 0..32i32 {
            interleaved.push(sample * 16);
            interleaved.push(sample * 16 + (sample & 1));
            interleaved.push(sample * 16 + 2);
        }
        let profile = LevelProfile::new(256, 4, 12, 4, true, true);
        let encoded = encode_frame(
            &interleaved,
            3,
            16,
            44_100,
            FrameHeaderNumber::Frame(0),
            profile,
        )
        .unwrap();
        let assignment = (encoded.bytes[3] >> 4) & 0x0f;
        assert_eq!(assignment, 0b0010);
    }

    #[test]
    fn frame_header_uses_supported_stereo_assignment_bits() {
        let mut interleaved = Vec::new();
        for sample in 0..32i32 {
            interleaved.push(sample * 16);
            interleaved.push(sample * 16 + (sample & 1));
        }
        let profile = LevelProfile::new(256, 4, 12, 4, true, true);
        let encoded = encode_frame(
            &interleaved,
            2,
            16,
            44_100,
            FrameHeaderNumber::Frame(0),
            profile,
        )
        .unwrap();
        let assignment = (encoded.bytes[3] >> 4) & 0x0f;
        assert!(matches!(assignment, 0b0001 | 0b1000 | 0b1001 | 0b1010));
    }

    #[test]
    fn bit_depth_codes_fall_back_to_streaminfo_for_non_explicit_depths() {
        assert_eq!(bit_depth_bits(4).unwrap(), 0b000);
        assert_eq!(bit_depth_bits(8).unwrap(), 0b001);
        assert_eq!(bit_depth_bits(11).unwrap(), 0b000);
        assert_eq!(bit_depth_bits(12).unwrap(), 0b010);
        assert_eq!(bit_depth_bits(16).unwrap(), 0b100);
        assert_eq!(bit_depth_bits(20).unwrap(), 0b101);
        assert_eq!(bit_depth_bits(24).unwrap(), 0b110);
        assert_eq!(bit_depth_bits(32).unwrap(), 0b111);
    }

    fn parse_metadata_blocks(flac: &[u8]) -> Vec<(bool, u8, Vec<u8>)> {
        assert_eq!(&flac[..4], b"fLaC");
        let mut offset = 4usize;
        let mut blocks = Vec::new();
        loop {
            let header = flac[offset];
            let is_last = header & 0x80 != 0;
            let block_type = header & 0x7f;
            let payload_len =
                u32::from_be_bytes([0, flac[offset + 1], flac[offset + 2], flac[offset + 3]])
                    as usize;
            offset += 4;
            blocks.push((
                is_last,
                block_type,
                flac[offset..offset + payload_len].to_vec(),
            ));
            offset += payload_len;
            if is_last {
                return blocks;
            }
        }
    }

    fn parse_seektable_entries(payload: &[u8]) -> Vec<(u64, u64, u16)> {
        payload
            .chunks_exact(18)
            .map(|chunk| {
                (
                    u64::from_be_bytes(chunk[..8].try_into().unwrap()),
                    u64::from_be_bytes(chunk[8..16].try_into().unwrap()),
                    u16::from_be_bytes(chunk[16..18].try_into().unwrap()),
                )
            })
            .collect()
    }

    #[test]
    fn metadata_header_uses_flac_u24_layout() {
        assert_eq!(
            metadata_block_header(4, false, 34).unwrap(),
            [0x04, 0, 0, 34]
        );
        assert_eq!(
            metadata_block_header(5, true, 513).unwrap(),
            [0x85, 0, 2, 1]
        );
    }

    #[test]
    fn writer_marks_streaminfo_as_last_when_no_optional_metadata_exists() {
        let stream_info = StreamInfo::new(44_100, 2, 16, 128, [0u8; 16]);
        let writer = Cursor::new(Vec::new());
        let (writer, _) = FlacWriter::new(writer, stream_info, &[], 0, true)
            .unwrap()
            .finalize()
            .unwrap();
        let blocks = parse_metadata_blocks(&writer.into_inner());

        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].0);
        assert_eq!(blocks[0].1, 0);
    }

    #[test]
    fn writer_emits_optional_metadata_before_frames_with_correct_last_flag() {
        let mut draft = MetadataDraft::default();
        let info_chunk = {
            let mut chunk = b"INFO".to_vec();
            chunk.extend_from_slice(b"IART");
            chunk.extend_from_slice(&6u32.to_le_bytes());
            chunk.extend_from_slice(b"Artist");
            chunk
        };
        draft
            .ingest_chunk(*b"LIST", &info_chunk, FxmdChunkPolicy::IGNORE)
            .unwrap();
        draft
            .ingest_chunk(
                *b"cue ",
                &[
                    1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, b'd', b'a', b't', b'a', 0, 0, 0, 0, 0, 0,
                    0, 0, 16, 0, 0, 0,
                ],
                FxmdChunkPolicy::IGNORE,
            )
            .unwrap();
        let metadata_blocks = draft.finish(64).flac_blocks(64);
        let stream_info = StreamInfo::new(44_100, 2, 16, 64, [0u8; 16]);
        let writer = Cursor::new(Vec::new());
        let mut writer = FlacWriter::new(writer, stream_info, &metadata_blocks, 1, true).unwrap();
        writer.write_frame(0, 0, 64, &[0xAA]).unwrap();
        let (writer, _) = writer.finalize().unwrap();
        let blocks = parse_metadata_blocks(&writer.into_inner());

        assert_eq!(
            blocks.iter().map(|block| block.1).collect::<Vec<_>>(),
            vec![0, SEEKTABLE_BLOCK_TYPE, 4, 5]
        );
        assert_eq!(
            blocks.iter().map(|block| block.0).collect::<Vec<_>>(),
            vec![false, false, false, true]
        );
    }

    #[test]
    fn writer_uses_only_native_metadata_block_types_for_preserved_wav_metadata() {
        let mut draft = MetadataDraft::default();
        draft
            .ingest_chunk(
                *b"LIST",
                &[
                    b'I', b'N', b'F', b'O', b'I', b'N', b'A', b'M', 5, 0, 0, 0, b'T', b'i', b't',
                    b'l', b'e', 0,
                ],
                FxmdChunkPolicy::IGNORE,
            )
            .unwrap();
        let blocks = draft.finish(32).flac_blocks(32);

        assert!(matches!(&blocks[..], [FlacMetadataBlock::VorbisComment(_)]));
    }

    #[test]
    fn writer_backpatches_seektable_entries_from_written_frame_layout() {
        let stream_info = StreamInfo::new(44_100, 1, 16, 48, [0u8; 16]);
        let writer = Cursor::new(Vec::new());
        let mut writer = FlacWriter::new(writer, stream_info, &[], 3, true).unwrap();

        writer.write_frame(0, 0, 16, &[0xAA; 4]).unwrap();
        writer.write_frame(1, 16, 24, &[0xBB; 7]).unwrap();
        writer.write_frame(2, 40, 8, &[0xCC; 2]).unwrap();

        let (writer, _) = writer.finalize().unwrap();
        let blocks = parse_metadata_blocks(&writer.into_inner());
        let seektable = blocks
            .iter()
            .find(|(_, block_type, _)| *block_type == SEEKTABLE_BLOCK_TYPE)
            .expect("seektable block present");

        assert_eq!(
            parse_seektable_entries(&seektable.2),
            vec![(0, 0, 16), (16, 4, 24), (40, 11, 8)]
        );
    }

    #[test]
    fn seektable_subsampling_keeps_first_and_last_frame_indices_when_oversized() {
        let indices = selected_seektable_frame_indices(MAX_SEEKTABLE_POINTS + 17);

        assert_eq!(indices.len(), MAX_SEEKTABLE_POINTS);
        assert_eq!(indices.first().copied(), Some(0));
        assert_eq!(indices.last().copied(), Some(MAX_SEEKTABLE_POINTS + 16));
    }
}
