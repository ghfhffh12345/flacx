use std::collections::BTreeMap;

use super::blocks::{CueSheet, PreservedMetadataBundle, VorbisCommentBlock};
use super::{
    CUESHEET_HEADER_LEN, CUESHEET_INDEX_LEN, CUESHEET_LEADOUT_TRACK_NUMBER,
    CUESHEET_TRACK_HEADER_LEN, FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY,
    FLACX_CHANNEL_LAYOUT_PROVENANCE_VALUE, FXMD_CHUNK_ID, FxmdChunkPolicy,
    MAX_RFC9639_CHANNEL_MASK, Metadata, WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY,
};

#[derive(Debug, Default)]
pub(crate) struct MetadataDraft {
    preserved: Option<PreservedMetadataBundle>,
    comments_by_key: BTreeMap<String, Vec<String>>,
    cue_points: Vec<u64>,
}

impl MetadataDraft {
    pub(crate) fn ingest_chunk(
        &mut self,
        chunk_id: [u8; 4],
        payload: &[u8],
        fxmd_policy: FxmdChunkPolicy,
    ) -> crate::error::Result<()> {
        if chunk_id == FXMD_CHUNK_ID {
            self.ingest_fxmd_chunk(payload, fxmd_policy)?;
            return Ok(());
        }

        if self.preserved.is_some() {
            return Ok(());
        }
        match &chunk_id {
            b"LIST" => self.ingest_list_chunk(payload),
            b"cue " => self.ingest_cue_chunk(payload),
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn finish(self, _total_samples: u64) -> Metadata {
        if let Some(preserved) = self.preserved {
            return Metadata {
                preserved,
                vorbis_comment: None,
                cue_points: Vec::new(),
                channel_mask: None,
                channel_layout_provenance: false,
            };
        }
        let vorbis_comment = if self.comments_by_key.is_empty() {
            None
        } else {
            let mut entries = Vec::new();
            for (key, values) in self.comments_by_key {
                for value in values {
                    entries.push(raw_vorbis_comment_entry(&key, &value));
                }
            }
            Some(VorbisCommentBlock::new(
                format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
                entries,
            ))
        };

        Metadata {
            preserved: PreservedMetadataBundle::default(),
            vorbis_comment,
            cue_points: self
                .cue_points
                .into_iter()
                .filter_map(|point| u32::try_from(point).ok())
                .collect(),
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

    fn ingest_fxmd_chunk(
        &mut self,
        payload: &[u8],
        fxmd_policy: FxmdChunkPolicy,
    ) -> crate::error::Result<()> {
        if !fxmd_policy.capture {
            return Ok(());
        }

        if self.preserved.is_some() {
            if fxmd_policy.strict {
                return Err(crate::error::Error::InvalidPcmContainer(
                    "duplicate fxmd chunk is not allowed",
                ));
            }
            return Ok(());
        }

        match PreservedMetadataBundle::from_fxmd_payload(payload) {
            Ok(bundle) => {
                self.preserved = Some(bundle);
            }
            Err(error) if fxmd_policy.strict => return Err(error),
            Err(_) => {}
        }
        Ok(())
    }
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

pub(crate) fn raw_vorbis_comment_entry(key: &str, value: &str) -> String {
    format!("{key}={value}")
}

pub(crate) fn split_vorbis_comment_entry(entry: &str) -> Option<(&str, &str)> {
    let (key, value) = entry.split_once('=')?;
    if key.is_empty() {
        None
    } else {
        Some((key, value))
    }
}

fn decode_text_payload(payload: &[u8]) -> Option<String> {
    let end = payload
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(payload.len());
    let value = String::from_utf8(payload[..end].to_vec()).ok()?;
    if value.is_empty() { None } else { Some(value) }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CueSheetIndexPoint {
    pub(crate) offset: u64,
    pub(crate) number: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CueSheetTrackProjection {
    pub(crate) offset: u64,
    pub(crate) number: u8,
    pub(crate) is_lead_out: bool,
    pub(crate) index_points: Vec<CueSheetIndexPoint>,
}

pub(crate) fn parse_cuesheet_tracks(
    payload: &[u8],
) -> crate::error::Result<Vec<CueSheetTrackProjection>> {
    if payload.len() < CUESHEET_HEADER_LEN {
        return Err(crate::error::Error::InvalidPcmContainer(
            "cuesheet payload header is truncated",
        ));
    }

    let track_count = payload[CUESHEET_HEADER_LEN - 1] as usize;
    if track_count == 0 {
        return Err(crate::error::Error::InvalidPcmContainer(
            "cuesheet payload must include a lead-out track",
        ));
    }

    let mut cursor = CUESHEET_HEADER_LEN;
    let mut tracks = Vec::with_capacity(track_count);
    for track_index in 0..track_count {
        if cursor + CUESHEET_TRACK_HEADER_LEN > payload.len() {
            return Err(crate::error::Error::InvalidPcmContainer(
                "cuesheet track header is truncated",
            ));
        }

        let track_offset = u64::from_be_bytes(
            payload[cursor..cursor + 8]
                .try_into()
                .expect("fixed cuesheet track offset slice"),
        );
        let track_number = payload[cursor + 8];
        if track_number == 0 {
            return Err(crate::error::Error::InvalidPcmContainer(
                "cuesheet track number must be non-zero",
            ));
        }
        if tracks
            .iter()
            .any(|track: &CueSheetTrackProjection| track.number == track_number)
        {
            return Err(crate::error::Error::InvalidPcmContainer(
                "cuesheet track numbers must be unique",
            ));
        }

        let is_lead_out = track_index + 1 == track_count;
        let index_count = payload[cursor + 35] as usize;
        cursor += CUESHEET_TRACK_HEADER_LEN;

        if is_lead_out {
            if track_number != CUESHEET_LEADOUT_TRACK_NUMBER && track_number != 255 {
                return Err(crate::error::Error::InvalidPcmContainer(
                    "cuesheet lead-out track number is invalid",
                ));
            }
            if index_count != 0 {
                return Err(crate::error::Error::InvalidPcmContainer(
                    "cuesheet lead-out track must not contain index points",
                ));
            }
        }

        let mut index_points: Vec<CueSheetIndexPoint> = Vec::with_capacity(index_count);
        for index_position in 0..index_count {
            if cursor + CUESHEET_INDEX_LEN > payload.len() {
                return Err(crate::error::Error::InvalidPcmContainer(
                    "cuesheet index point is truncated",
                ));
            }

            let index_offset = u64::from_be_bytes(
                payload[cursor..cursor + 8]
                    .try_into()
                    .expect("fixed cuesheet index offset slice"),
            );
            let index_number = payload[cursor + 8];
            if index_position == 0 && !matches!(index_number, 0 | 1) {
                return Err(crate::error::Error::InvalidPcmContainer(
                    "cuesheet track must start at index 0 or 1",
                ));
            }
            if let Some(previous) = index_points.last()
                && index_number <= previous.number
            {
                return Err(crate::error::Error::InvalidPcmContainer(
                    "cuesheet index numbers must be strictly increasing",
                ));
            }
            index_points.push(CueSheetIndexPoint {
                offset: index_offset,
                number: index_number,
            });
            cursor += CUESHEET_INDEX_LEN;
        }

        tracks.push(CueSheetTrackProjection {
            offset: track_offset,
            number: track_number,
            is_lead_out,
            index_points,
        });
    }

    if cursor != payload.len() {
        return Err(crate::error::Error::InvalidPcmContainer(
            "cuesheet payload has trailing bytes",
        ));
    }

    Ok(tracks)
}

pub(crate) fn cue_points_from_cuesheet_payload(payload: &[u8], total_samples: u64) -> Vec<u32> {
    let Ok(tracks) = parse_cuesheet_tracks(payload) else {
        return Vec::new();
    };

    let mut cue_points = Vec::new();
    for track in tracks.into_iter().filter(|track| !track.is_lead_out) {
        let cue_offset = track
            .index_points
            .iter()
            .find(|index| index.number == 1)
            .map_or(track.offset, |index| track.offset + index.offset);
        let Some(cue_offset) = normalize_cue_offset(cue_offset, total_samples) else {
            continue;
        };
        if !cue_points.contains(&cue_offset) {
            cue_points.push(cue_offset);
        }
    }
    cue_points
}

pub(crate) fn normalize_cuesheet(mut cue_points: Vec<u64>, total_samples: u64) -> Option<CueSheet> {
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

pub(crate) fn wav_info_chunk_id_for_vorbis_key(key: &str) -> Option<[u8; 4]> {
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

pub(crate) fn format_channel_mask(mask: u32) -> String {
    format!("0x{mask:08x}")
}

pub(crate) fn parse_channel_mask_comment(value: &str, channels: u8) -> crate::error::Result<u32> {
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

pub(crate) fn parse_channel_layout_provenance_comment(value: &str) -> crate::error::Result<bool> {
    if value.trim() == FLACX_CHANNEL_LAYOUT_PROVENANCE_VALUE {
        Ok(true)
    } else {
        Err(crate::error::Error::UnsupportedFlac(format!(
            "invalid {FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY} value `{value}`"
        )))
    }
}

pub(crate) fn read_u32_le(bytes: &[u8], cursor: &mut usize) -> Option<u32> {
    if *cursor + 4 > bytes.len() {
        return None;
    }
    let value = u32::from_le_bytes(bytes[*cursor..*cursor + 4].try_into().ok()?);
    *cursor += 4;
    Some(value)
}

pub(crate) fn read_bytes<'a>(bytes: &'a [u8], cursor: &mut usize, len: usize) -> Option<&'a [u8]> {
    let end = cursor.checked_add(len)?;
    if end > bytes.len() {
        return None;
    }
    let slice = &bytes[*cursor..end];
    *cursor = end;
    Some(slice)
}

pub(crate) fn read_u32_le_strict(
    bytes: &[u8],
    cursor: &mut usize,
    truncated_message: &'static str,
) -> crate::error::Result<u32> {
    read_u32_le(bytes, cursor).ok_or(crate::error::Error::InvalidPcmContainer(truncated_message))
}

pub(crate) fn read_utf8_entry(bytes: &[u8], cursor: &mut usize, len: usize) -> Option<String> {
    let end = cursor.checked_add(len)?;
    if end > bytes.len() {
        return None;
    }
    let entry = String::from_utf8(bytes[*cursor..end].to_vec()).ok()?;
    *cursor = end;
    Some(entry)
}

pub(crate) fn read_bytes_strict(
    bytes: &[u8],
    cursor: &mut usize,
    len: usize,
    truncated_message: &'static str,
) -> crate::error::Result<Vec<u8>> {
    let end = cursor
        .checked_add(len)
        .ok_or(crate::error::Error::InvalidPcmContainer(truncated_message))?;
    if end > bytes.len() {
        return Err(crate::error::Error::InvalidPcmContainer(truncated_message));
    }
    let entry = bytes[*cursor..end].to_vec();
    *cursor = end;
    Ok(entry)
}

pub(crate) fn append_u32_le(buffer: &mut Vec<u8>, value: u32) {
    buffer.extend_from_slice(&value.to_le_bytes());
}

pub(crate) fn append_chunk_payload(buffer: &mut Vec<u8>, id: &[u8; 4], payload: &[u8]) {
    buffer.extend_from_slice(id);
    append_u32_le(buffer, payload.len() as u32);
    buffer.extend_from_slice(payload);
    if !payload.len().is_multiple_of(2) {
        buffer.push(0);
    }
}
