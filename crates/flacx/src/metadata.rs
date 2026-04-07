use std::collections::BTreeMap;

use crate::input::ordinary_channel_mask;

pub(crate) const SEEKTABLE_BLOCK_TYPE: u8 = 3;
pub(crate) const SEEKTABLE_POINT_LEN: usize = 18;
pub(crate) const SEEKTABLE_PLACEHOLDER_SAMPLE_NUMBER: u64 = u64::MAX;
const VORBIS_COMMENT_BLOCK_TYPE: u8 = 4;
const CUESHEET_BLOCK_TYPE: u8 = 5;
const CUESHEET_LEADOUT_TRACK_NUMBER: u8 = 170;
const CUESHEET_HEADER_LEN: usize = 396;
const CUESHEET_TRACK_HEADER_LEN: usize = 36;
const CUESHEET_INDEX_LEN: usize = 12;
const WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY: &str = "WAVEFORMATEXTENSIBLE_CHANNEL_MASK";
pub(crate) const FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY: &str = "FLACX_CHANNEL_LAYOUT_PROVENANCE";
const FLACX_CHANNEL_LAYOUT_PROVENANCE_VALUE: &str = "1";
const MAX_RFC9639_CHANNEL_MASK: u32 = 0x0003_FFFF;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct EncodeMetadata {
    comments: Vec<VorbisComment>,
    cuesheet: Option<CueSheet>,
    channel_mask: Option<u32>,
    channel_layout_provenance: bool,
}

impl EncodeMetadata {
    pub(crate) fn set_channel_mask(&mut self, channels: u16, mask: u32) {
        if ordinary_channel_mask(channels).is_some_and(|ordinary| ordinary == mask) {
            return;
        }
        self.channel_mask = Some(mask);
        self.channel_layout_provenance = true;
    }

    pub(crate) fn flac_blocks(&self) -> Vec<FlacMetadataBlock> {
        let mut blocks = Vec::new();
        let mut comments = self.comments.clone();
        if let Some(channel_mask) = self.channel_mask {
            comments.push(VorbisComment {
                key: WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY.to_owned(),
                value: format_channel_mask(channel_mask),
            });
        }
        if self.channel_layout_provenance {
            comments.push(VorbisComment {
                key: FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY.to_owned(),
                value: FLACX_CHANNEL_LAYOUT_PROVENANCE_VALUE.to_owned(),
            });
        }
        if !comments.is_empty() {
            blocks.push(FlacMetadataBlock::VorbisComment(VorbisCommentBlock {
                vendor: format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
                comments,
            }));
        }
        if let Some(cuesheet) = &self.cuesheet {
            blocks.push(FlacMetadataBlock::CueSheet(cuesheet.clone()));
        }
        blocks
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct WavMetadata {
    info_entries: Vec<WavInfoEntry>,
    cue_points: Vec<u32>,
    channel_mask: Option<u32>,
    channel_layout_provenance: bool,
}

impl WavMetadata {
    pub(crate) fn is_empty(&self) -> bool {
        self.info_entries.is_empty() && self.cue_points.is_empty()
    }

    pub(crate) fn channel_mask(&self) -> Option<u32> {
        self.channel_mask
    }

    pub(crate) fn has_channel_layout_provenance(&self) -> bool {
        self.channel_layout_provenance
    }

    pub(crate) fn ingest_flac_metadata_block(
        &mut self,
        block_type: u8,
        payload: &[u8],
        total_samples: u64,
        channels: u8,
    ) -> crate::error::Result<()> {
        match block_type {
            VORBIS_COMMENT_BLOCK_TYPE => self.ingest_vorbis_comment_payload(payload, channels)?,
            CUESHEET_BLOCK_TYPE => self.ingest_cuesheet_payload(payload, total_samples),
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn list_info_chunk_payload(&self) -> Option<Vec<u8>> {
        if self.info_entries.is_empty() {
            return None;
        }

        let mut payload = b"INFO".to_vec();
        for entry in &self.info_entries {
            append_chunk_payload(&mut payload, &entry.chunk_id, entry.value.as_bytes());
        }
        Some(payload)
    }

    pub(crate) fn cue_chunk_payload(&self) -> Option<Vec<u8>> {
        if self.cue_points.is_empty() {
            return None;
        }

        let mut payload = Vec::new();
        append_u32_le(&mut payload, self.cue_points.len() as u32);
        for (index, &sample_offset) in self.cue_points.iter().enumerate() {
            append_u32_le(&mut payload, index as u32);
            append_u32_le(&mut payload, 0);
            payload.extend_from_slice(b"data");
            append_u32_le(&mut payload, 0);
            append_u32_le(&mut payload, 0);
            append_u32_le(&mut payload, sample_offset);
        }
        Some(payload)
    }

    fn ingest_vorbis_comment_payload(
        &mut self,
        payload: &[u8],
        channels: u8,
    ) -> crate::error::Result<()> {
        let mut cursor = 0usize;
        let Some(vendor_len) = read_u32_le(payload, &mut cursor) else {
            return Ok(());
        };
        let vendor_len = vendor_len as usize;
        if cursor + vendor_len > payload.len() {
            return Ok(());
        }
        cursor += vendor_len;

        let Some(comment_count) = read_u32_le(payload, &mut cursor) else {
            return Ok(());
        };
        for _ in 0..comment_count {
            let Some(comment_len) = read_u32_le(payload, &mut cursor) else {
                return Ok(());
            };
            let comment_len = comment_len as usize;
            if cursor + comment_len > payload.len() {
                return Ok(());
            }
            let entry = &payload[cursor..cursor + comment_len];
            cursor += comment_len;

            let Some(comment) = String::from_utf8(entry.to_vec()).ok() else {
                continue;
            };
            let Some((key, value)) = comment.split_once('=') else {
                continue;
            };
            if key.eq_ignore_ascii_case(WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY) {
                self.channel_mask = Some(parse_channel_mask_comment(value, channels)?);
                continue;
            }
            if key.eq_ignore_ascii_case(FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY) {
                self.channel_layout_provenance = parse_channel_layout_provenance_comment(value)?;
                continue;
            }
            let Some(chunk_id) = wav_info_chunk_id_for_vorbis_key(key) else {
                continue;
            };
            if value.is_empty() {
                continue;
            }
            self.info_entries.push(WavInfoEntry {
                chunk_id,
                value: value.to_owned(),
            });
        }
        Ok(())
    }

    fn ingest_cuesheet_payload(&mut self, payload: &[u8], total_samples: u64) {
        if payload.len() < CUESHEET_HEADER_LEN {
            return;
        }

        let track_count = payload[CUESHEET_HEADER_LEN - 1] as usize;
        let mut cursor = CUESHEET_HEADER_LEN;
        for _ in 0..track_count {
            if cursor + CUESHEET_TRACK_HEADER_LEN > payload.len() {
                return;
            }

            let track_offset = u64::from_be_bytes(
                payload[cursor..cursor + 8]
                    .try_into()
                    .expect("fixed cuesheet track offset slice"),
            );
            let track_number = payload[cursor + 8];
            let index_count = payload[cursor + 35] as usize;
            cursor += CUESHEET_TRACK_HEADER_LEN;

            let mut index01_offset = None;
            for _ in 0..index_count {
                if cursor + CUESHEET_INDEX_LEN > payload.len() {
                    return;
                }
                let index_offset = u64::from_be_bytes(
                    payload[cursor..cursor + 8]
                        .try_into()
                        .expect("fixed cuesheet index offset slice"),
                );
                let index_number = payload[cursor + 8];
                if index_number == 1 && index01_offset.is_none() {
                    index01_offset = Some(index_offset);
                }
                cursor += CUESHEET_INDEX_LEN;
            }

            if track_number == CUESHEET_LEADOUT_TRACK_NUMBER {
                continue;
            }

            let cue_offset = track_offset + index01_offset.unwrap_or(0);
            let Some(cue_offset) = normalize_cue_offset(cue_offset, total_samples) else {
                continue;
            };
            if !self.cue_points.contains(&cue_offset) {
                self.cue_points.push(cue_offset);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeekPoint {
    pub(crate) sample_number: u64,
    pub(crate) frame_offset: u64,
    pub(crate) sample_count: u16,
}

impl SeekPoint {
    pub(crate) fn payload(points: &[Self]) -> Vec<u8> {
        let mut payload = Vec::with_capacity(points.len() * SEEKTABLE_POINT_LEN);
        for point in points {
            payload.extend_from_slice(&point.sample_number.to_be_bytes());
            payload.extend_from_slice(&point.frame_offset.to_be_bytes());
            payload.extend_from_slice(&point.sample_count.to_be_bytes());
        }
        payload
    }
}

pub(crate) fn validate_seektable_payload(payload: &[u8]) -> crate::error::Result<()> {
    if !payload.len().is_multiple_of(SEEKTABLE_POINT_LEN) {
        return Err(crate::error::Error::InvalidFlac(
            "seektable payload length must be a multiple of 18 bytes",
        ));
    }

    let mut previous_sample_number = None;
    let mut saw_placeholder = false;

    for chunk in payload.chunks_exact(SEEKTABLE_POINT_LEN) {
        let sample_number = u64::from_be_bytes(
            chunk[..8]
                .try_into()
                .expect("seektable sample number slice length"),
        );

        if sample_number == SEEKTABLE_PLACEHOLDER_SAMPLE_NUMBER {
            saw_placeholder = true;
            continue;
        }

        if saw_placeholder {
            return Err(crate::error::Error::InvalidFlac(
                "seektable placeholder points must appear at the end of the table",
            ));
        }

        if let Some(previous) = previous_sample_number {
            if sample_number < previous {
                return Err(crate::error::Error::InvalidFlac(
                    "seektable sample numbers must be in ascending order",
                ));
            }
            if sample_number == previous {
                return Err(crate::error::Error::InvalidFlac(
                    "seektable sample numbers must be unique",
                ));
            }
        }

        previous_sample_number = Some(sample_number);
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FlacMetadataBlock {
    VorbisComment(VorbisCommentBlock),
    CueSheet(CueSheet),
}

impl FlacMetadataBlock {
    pub(crate) fn block_type(&self) -> u8 {
        match self {
            Self::VorbisComment(_) => VORBIS_COMMENT_BLOCK_TYPE,
            Self::CueSheet(_) => CUESHEET_BLOCK_TYPE,
        }
    }

    pub(crate) fn payload(&self) -> Vec<u8> {
        match self {
            Self::VorbisComment(block) => block.payload(),
            Self::CueSheet(cuesheet) => cuesheet.payload(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VorbisCommentBlock {
    vendor: String,
    comments: Vec<VorbisComment>,
}

impl VorbisCommentBlock {
    fn payload(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        append_u32_le(&mut payload, self.vendor.len() as u32);
        payload.extend_from_slice(self.vendor.as_bytes());
        append_u32_le(&mut payload, self.comments.len() as u32);
        for comment in &self.comments {
            let entry = format!("{}={}", comment.key, comment.value);
            append_u32_le(&mut payload, entry.len() as u32);
            payload.extend_from_slice(entry.as_bytes());
        }
        payload
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VorbisComment {
    key: String,
    value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CueSheet {
    track_offsets: Vec<u64>,
    lead_out_offset: u64,
}

impl CueSheet {
    fn payload(&self) -> Vec<u8> {
        let mut payload = vec![0u8; 128];
        payload.extend_from_slice(&0u64.to_be_bytes());
        payload.push(0);
        payload.extend_from_slice(&[0u8; 258]);
        payload.push((self.track_offsets.len() + 1) as u8);
        for (index, &offset) in self.track_offsets.iter().enumerate() {
            write_cuesheet_track(&mut payload, offset, (index + 1) as u8);
        }
        write_leadout_track(&mut payload, self.lead_out_offset);
        payload
    }
}

fn write_cuesheet_track(payload: &mut Vec<u8>, offset: u64, track_number: u8) {
    payload.extend_from_slice(&offset.to_be_bytes());
    payload.push(track_number);
    payload.extend_from_slice(&[0u8; 12]);
    payload.push(0);
    payload.extend_from_slice(&[0u8; 13]);
    payload.push(1);
    payload.extend_from_slice(&0u64.to_be_bytes());
    payload.push(1);
    payload.extend_from_slice(&[0u8; 3]);
}

fn write_leadout_track(payload: &mut Vec<u8>, offset: u64) {
    payload.extend_from_slice(&offset.to_be_bytes());
    payload.push(CUESHEET_LEADOUT_TRACK_NUMBER);
    payload.extend_from_slice(&[0u8; 12]);
    payload.push(0);
    payload.extend_from_slice(&[0u8; 13]);
    payload.push(0);
}

#[derive(Debug, Default)]
pub(crate) struct MetadataDraft {
    comments_by_key: BTreeMap<String, Vec<String>>,
    cue_points: Vec<u64>,
}

impl MetadataDraft {
    pub(crate) fn ingest_chunk(&mut self, chunk_id: [u8; 4], payload: &[u8]) {
        match &chunk_id {
            b"LIST" => self.ingest_list_chunk(payload),
            b"cue " => self.ingest_cue_chunk(payload),
            _ => {}
        }
    }

    pub(crate) fn finish(self, total_samples: u64) -> EncodeMetadata {
        let mut comments = Vec::new();
        for (key, values) in self.comments_by_key {
            for value in values {
                comments.push(VorbisComment {
                    key: key.clone(),
                    value,
                });
            }
        }

        EncodeMetadata {
            comments,
            cuesheet: normalize_cuesheet(self.cue_points, total_samples),
            channel_mask: None,
            channel_layout_provenance: false,
        }
    }

    fn ingest_list_chunk(&mut self, payload: &[u8]) {
        if payload.len() < 4 || &payload[..4] != b"INFO" {
            return;
        }

        let mut offset = 4usize;
        while offset + 8 <= payload.len() {
            let chunk_id: [u8; 4] = payload[offset..offset + 4]
                .try_into()
                .expect("fixed chunk identifier slice");
            let chunk_size = u32::from_le_bytes(
                payload[offset + 4..offset + 8]
                    .try_into()
                    .expect("fixed chunk size slice"),
            ) as usize;
            offset += 8;

            if offset + chunk_size > payload.len() {
                return;
            }

            if let Some((key, value)) =
                normalize_info_entry(chunk_id, &payload[offset..offset + chunk_size])
            {
                self.comments_by_key.entry(key).or_default().push(value);
            }

            offset += chunk_size;
            if !chunk_size.is_multiple_of(2) {
                if offset == payload.len() {
                    return;
                }
                offset += 1;
            }
        }
    }

    fn ingest_cue_chunk(&mut self, payload: &[u8]) {
        if payload.len() < 4 {
            return;
        }

        let cue_count = u32::from_le_bytes(payload[..4].try_into().expect("fixed cue count slice"));
        let required_len = 4usize.saturating_add(cue_count as usize * 24);
        if payload.len() < required_len {
            return;
        }

        let mut offset = 4usize;
        for _ in 0..cue_count {
            if &payload[offset + 8..offset + 12] == b"data" {
                let sample_offset = u32::from_le_bytes(
                    payload[offset + 20..offset + 24]
                        .try_into()
                        .expect("fixed cue sample offset slice"),
                );
                self.cue_points.push(u64::from(sample_offset));
            }
            offset += 24;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WavInfoEntry {
    chunk_id: [u8; 4],
    value: String,
}

fn normalize_info_entry(chunk_id: [u8; 4], payload: &[u8]) -> Option<(String, String)> {
    let key = match &chunk_id {
        b"IART" => "ARTIST",
        b"ICMT" => "COMMENT",
        b"ICOP" => "COPYRIGHT",
        b"ICRD" => "DATE",
        b"IGNR" => "GENRE",
        b"INAM" => "TITLE",
        b"IPRD" => "ALBUM",
        b"ISFT" => "ENCODER",
        b"ITRK" => "TRACKNUMBER",
        _ => return None,
    };

    let value = decode_text_payload(payload)?;
    Some((key.to_owned(), value))
}

fn decode_text_payload(payload: &[u8]) -> Option<String> {
    let end = payload
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(payload.len());
    let value = String::from_utf8(payload[..end].to_vec()).ok()?;
    if value.is_empty() { None } else { Some(value) }
}

fn normalize_cuesheet(mut cue_points: Vec<u64>, total_samples: u64) -> Option<CueSheet> {
    cue_points.retain(|offset| *offset < total_samples);
    cue_points.sort_unstable();
    cue_points.dedup();
    if cue_points.is_empty() || cue_points.len() > 99 {
        return None;
    }

    Some(CueSheet {
        track_offsets: cue_points,
        lead_out_offset: total_samples,
    })
}

fn wav_info_chunk_id_for_vorbis_key(key: &str) -> Option<[u8; 4]> {
    match key.to_ascii_uppercase().as_str() {
        "ARTIST" => Some(*b"IART"),
        "COMMENT" => Some(*b"ICMT"),
        "COPYRIGHT" => Some(*b"ICOP"),
        "DATE" => Some(*b"ICRD"),
        "GENRE" => Some(*b"IGNR"),
        "TITLE" => Some(*b"INAM"),
        "ALBUM" => Some(*b"IPRD"),
        "ENCODER" => Some(*b"ISFT"),
        "TRACKNUMBER" => Some(*b"ITRK"),
        _ => None,
    }
}

fn normalize_cue_offset(offset: u64, total_samples: u64) -> Option<u32> {
    if offset >= total_samples || offset > u64::from(u32::MAX) {
        return None;
    }
    Some(offset as u32)
}

fn format_channel_mask(mask: u32) -> String {
    format!("0x{mask:08x}")
}

fn parse_channel_mask_comment(value: &str, channels: u8) -> crate::error::Result<u32> {
    let value = value.trim();
    let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    else {
        return Err(crate::error::Error::UnsupportedFlac(format!(
            "invalid {WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY} value `{value}`"
        )));
    };

    let mask = u32::from_str_radix(hex, 16).map_err(|_| {
        crate::error::Error::UnsupportedFlac(format!(
            "invalid {WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY} value `{value}`"
        ))
    })?;
    if mask & !MAX_RFC9639_CHANNEL_MASK != 0 {
        return Err(crate::error::Error::UnsupportedFlac(format!(
            "{WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY} {mask:#010x} uses unsupported speaker bits"
        )));
    }
    if mask.count_ones() > u32::from(channels) {
        return Err(crate::error::Error::UnsupportedFlac(format!(
            "{WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY} {mask:#010x} names more speakers than the stream has channels"
        )));
    }
    Ok(mask)
}

fn parse_channel_layout_provenance_comment(value: &str) -> crate::error::Result<bool> {
    if value.trim() == FLACX_CHANNEL_LAYOUT_PROVENANCE_VALUE {
        Ok(true)
    } else {
        Err(crate::error::Error::UnsupportedFlac(format!(
            "invalid {FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY} value `{value}`"
        )))
    }
}

fn read_u32_le(bytes: &[u8], cursor: &mut usize) -> Option<u32> {
    if *cursor + 4 > bytes.len() {
        return None;
    }
    let value = u32::from_le_bytes(bytes[*cursor..*cursor + 4].try_into().ok()?);
    *cursor += 4;
    Some(value)
}

fn append_u32_le(buffer: &mut Vec<u8>, value: u32) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

fn append_chunk_payload(buffer: &mut Vec<u8>, id: &[u8; 4], payload: &[u8]) {
    buffer.extend_from_slice(id);
    append_u32_le(buffer, payload.len() as u32);
    buffer.extend_from_slice(payload);
    if !payload.len().is_multiple_of(2) {
        buffer.push(0);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY, FlacMetadataBlock, MetadataDraft,
        SEEKTABLE_PLACEHOLDER_SAMPLE_NUMBER, SeekPoint, WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY,
        WavMetadata, validate_seektable_payload,
    };

    fn info_list_chunk(entries: &[([u8; 4], &[u8])]) -> Vec<u8> {
        let mut payload = b"INFO".to_vec();
        for (id, value) in entries {
            payload.extend_from_slice(id);
            payload.extend_from_slice(&(value.len() as u32).to_le_bytes());
            payload.extend_from_slice(value);
            if value.len() % 2 != 0 {
                payload.push(0);
            }
        }
        payload
    }

    fn cue_chunk(offsets: &[u32]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&(offsets.len() as u32).to_le_bytes());
        for (index, offset) in offsets.iter().enumerate() {
            payload.extend_from_slice(&(index as u32).to_le_bytes());
            payload.extend_from_slice(&0u32.to_le_bytes());
            payload.extend_from_slice(b"data");
            payload.extend_from_slice(&0u32.to_le_bytes());
            payload.extend_from_slice(&0u32.to_le_bytes());
            payload.extend_from_slice(&offset.to_le_bytes());
        }
        payload
    }

    fn vorbis_comment_payload(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (key, value) in entries {
            let entry = format!("{key}={value}");
            payload.extend_from_slice(&(entry.len() as u32).to_le_bytes());
            payload.extend_from_slice(entry.as_bytes());
        }
        payload
    }

    fn cuesheet_payload(track_specs: &[(&[(u64, u8)], u64)], lead_out_offset: u64) -> Vec<u8> {
        let mut payload = vec![0u8; 128];
        payload.extend_from_slice(&0u64.to_be_bytes());
        payload.push(0);
        payload.extend_from_slice(&[0u8; 258]);
        payload.push((track_specs.len() + 1) as u8);
        for (index, (indices, offset)) in track_specs.iter().enumerate() {
            payload.extend_from_slice(&offset.to_be_bytes());
            payload.push((index + 1) as u8);
            payload.extend_from_slice(&[0u8; 12]);
            payload.push(0);
            payload.extend_from_slice(&[0u8; 13]);
            payload.push(indices.len() as u8);
            for &(index_offset, index_number) in *indices {
                payload.extend_from_slice(&index_offset.to_be_bytes());
                payload.push(index_number);
                payload.extend_from_slice(&[0u8; 3]);
            }
        }
        payload.extend_from_slice(&lead_out_offset.to_be_bytes());
        payload.push(170);
        payload.extend_from_slice(&[0u8; 12]);
        payload.push(0);
        payload.extend_from_slice(&[0u8; 13]);
        payload.push(0);
        payload
    }

    fn seektable_payload(entries: &[(u64, u64, u16)]) -> Vec<u8> {
        SeekPoint::payload(
            &entries
                .iter()
                .map(|&(sample_number, frame_offset, sample_count)| SeekPoint {
                    sample_number,
                    frame_offset,
                    sample_count,
                })
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn normalizes_info_chunks_into_stable_vorbis_comments() {
        let mut draft = MetadataDraft::default();
        draft.ingest_chunk(
            *b"LIST",
            &info_list_chunk(&[
                (*b"IART", b"Example Artist"),
                (*b"INAM", b"Song Title"),
                (*b"IZZZ", b"ignored"),
                (*b"IART", b"Guest Artist"),
            ]),
        );

        let blocks = draft.finish(8_000).flac_blocks();
        let FlacMetadataBlock::VorbisComment(block) = &blocks[0] else {
            panic!("expected vorbis comments");
        };

        let rendered: Vec<String> = block
            .comments
            .iter()
            .map(|comment| format!("{}={}", comment.key, comment.value))
            .collect();
        assert_eq!(
            rendered,
            vec![
                "ARTIST=Example Artist",
                "ARTIST=Guest Artist",
                "TITLE=Song Title",
            ]
        );
    }

    #[test]
    fn drops_invalid_utf8_text_metadata() {
        let mut draft = MetadataDraft::default();
        draft.ingest_chunk(
            *b"LIST",
            &info_list_chunk(&[(*b"IART", &[0xff, 0xfe, 0xfd])]),
        );

        assert!(draft.finish(8_000).flac_blocks().is_empty());
    }

    #[test]
    fn normalizes_representable_cue_points_into_cuesheet_tracks() {
        let mut draft = MetadataDraft::default();
        draft.ingest_chunk(*b"cue ", &cue_chunk(&[4_000, 1_000, 4_000]));

        let blocks = draft.finish(6_000).flac_blocks();
        let FlacMetadataBlock::CueSheet(cuesheet) = &blocks[0] else {
            panic!("expected cuesheet block");
        };
        assert_eq!(cuesheet.track_offsets, vec![1_000, 4_000]);
        assert_eq!(cuesheet.lead_out_offset, 6_000);
    }

    #[test]
    fn drops_cue_points_that_cannot_be_represented() {
        let mut draft = MetadataDraft::default();
        draft.ingest_chunk(*b"cue ", &cue_chunk(&[4_000]));

        assert!(draft.finish(4_000).flac_blocks().is_empty());
    }

    #[test]
    fn restores_vorbis_comments_into_supported_wav_info_entries() {
        let mut metadata = WavMetadata::default();
        metadata
            .ingest_flac_metadata_block(
                4,
                &vorbis_comment_payload(&[
                    ("artist", "Example Artist"),
                    ("TITLE", "Example Title"),
                    ("UNKNOWN", "ignored"),
                    ("COMMENT", ""),
                ]),
                8_000,
                1,
            )
            .unwrap();

        let payload = metadata.list_info_chunk_payload().unwrap();
        assert!(payload.starts_with(b"INFO"));
        assert!(payload.windows(4).any(|window| window == b"IART"));
        assert!(payload.windows(4).any(|window| window == b"INAM"));
        assert!(!payload.windows(4).any(|window| window == b"ICMT"));
    }

    #[test]
    fn restores_cuesheet_tracks_preferring_index_01_offsets() {
        let mut metadata = WavMetadata::default();
        metadata
            .ingest_flac_metadata_block(
                5,
                &cuesheet_payload(&[(&[(10, 0), (20, 1)], 1_000), (&[], 4_000)], 8_000),
                8_000,
                1,
            )
            .unwrap();

        assert_eq!(metadata.cue_points, vec![1_020, 4_000]);
    }

    #[test]
    fn drops_out_of_range_or_duplicate_cuesheet_points() {
        let mut metadata = WavMetadata::default();
        metadata
            .ingest_flac_metadata_block(
                5,
                &cuesheet_payload(
                    &[(&[(0, 1)], 7_999), (&[(0, 1)], 7_999), (&[(0, 1)], 8_000)],
                    8_100,
                ),
                8_000,
                1,
            )
            .unwrap();

        assert_eq!(metadata.cue_points, vec![7_999]);
    }

    #[test]
    fn emits_channel_mask_comment_for_non_ordinary_layouts() {
        let mut draft = MetadataDraft::default();
        draft.ingest_chunk(*b"LIST", &info_list_chunk(&[(*b"INAM", b"Example Title")]));
        let mut metadata = draft.finish(4_096);
        metadata.set_channel_mask(4, 0x0001_2104);

        let blocks = metadata.flac_blocks();
        let FlacMetadataBlock::VorbisComment(block) = &blocks[0] else {
            panic!("expected vorbis comments");
        };

        let rendered: Vec<String> = block
            .comments
            .iter()
            .map(|comment| format!("{}={}", comment.key, comment.value))
            .collect();
        assert!(rendered.contains(&"TITLE=Example Title".to_string()));
        assert!(rendered.contains(&format!(
            "{WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY}=0x00012104"
        )));
        assert!(rendered.contains(&format!("{FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY}=1")));
    }

    #[test]
    fn accepts_valid_seektable_payload() {
        validate_seektable_payload(&seektable_payload(&[(0, 0, 128), (128, 512, 128)])).unwrap();
    }

    #[test]
    fn rejects_seektable_payloads_with_invalid_length() {
        let error = validate_seektable_payload(&[0u8; 17]).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("seektable payload length must be a multiple of 18 bytes")
        );
    }

    #[test]
    fn rejects_non_ascending_seektable_points() {
        let error = validate_seektable_payload(&seektable_payload(&[(128, 512, 128), (64, 0, 64)]))
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("seektable sample numbers must be in ascending order")
        );
    }

    #[test]
    fn rejects_duplicate_seektable_sample_numbers() {
        let error = validate_seektable_payload(&seektable_payload(&[(64, 0, 64), (64, 64, 64)]))
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("seektable sample numbers must be unique")
        );
    }

    #[test]
    fn rejects_seektable_placeholders_before_real_points() {
        let error = validate_seektable_payload(&seektable_payload(&[
            (SEEKTABLE_PLACEHOLDER_SAMPLE_NUMBER, 0, 0),
            (64, 64, 64),
        ]))
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("seektable placeholder points must appear at the end of the table")
        );
    }

    #[test]
    fn skips_channel_mask_comment_for_ordinary_layouts() {
        let mut metadata = MetadataDraft::default().finish(4_096);
        metadata.set_channel_mask(4, 0x0033);

        assert!(metadata.flac_blocks().is_empty());
    }

    #[test]
    fn parses_channel_mask_comment_case_insensitively_with_padded_hex() {
        let mut metadata = WavMetadata::default();
        metadata
            .ingest_flac_metadata_block(
                4,
                &vorbis_comment_payload(&[("waveformatextensible_channel_mask", "0X00012104")]),
                8_000,
                4,
            )
            .unwrap();

        assert_eq!(metadata.channel_mask(), Some(0x0001_2104));
    }

    #[test]
    fn parses_zero_channel_mask_value() {
        let mut metadata = WavMetadata::default();
        metadata
            .ingest_flac_metadata_block(
                4,
                &vorbis_comment_payload(&[(WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY, "0x0")]),
                8_000,
                2,
            )
            .unwrap();

        assert_eq!(metadata.channel_mask(), Some(0));
    }

    #[test]
    fn parses_private_layout_provenance_marker() {
        let mut metadata = WavMetadata::default();
        metadata
            .ingest_flac_metadata_block(
                4,
                &vorbis_comment_payload(&[(FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY, "1")]),
                8_000,
                4,
            )
            .unwrap();

        assert!(metadata.has_channel_layout_provenance());
    }

    #[test]
    fn rejects_channel_mask_comment_with_unsupported_speaker_bits() {
        let mut metadata = WavMetadata::default();
        let error = metadata
            .ingest_flac_metadata_block(
                4,
                &vorbis_comment_payload(&[(WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY, "0x40000")]),
                8_000,
                4,
            )
            .unwrap_err();

        assert!(error.to_string().contains("unsupported speaker bits"));
    }

    #[test]
    fn rejects_invalid_private_layout_provenance_marker() {
        let mut metadata = WavMetadata::default();
        let error = metadata
            .ingest_flac_metadata_block(
                4,
                &vorbis_comment_payload(&[(FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY, "v2")]),
                8_000,
                4,
            )
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains(FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY)
        );
    }
}
