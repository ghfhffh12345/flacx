use std::collections::BTreeMap;

use super::draft::{
    append_u32_le, read_bytes, read_bytes_strict, read_u32_le, read_u32_le_strict, read_utf8_entry,
};
use super::{
    APPLICATION_BLOCK_TYPE, CUESHEET_BLOCK_TYPE, CUESHEET_LEADOUT_TRACK_NUMBER, FXMD_HEADER_FLAGS,
    FXMD_MAGIC, FXMD_VERSION, PADDING_BLOCK_TYPE, PICTURE_BLOCK_TYPE, SEEKTABLE_BLOCK_TYPE,
    VORBIS_COMMENT_BLOCK_TYPE, validate_seektable_payload,
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PreservedMetadataBundle {
    records: Vec<PreservedMetadataRecord>,
}

impl PreservedMetadataBundle {
    pub(crate) fn flac_blocks(&self) -> Vec<FlacMetadataBlock> {
        self.records
            .iter()
            .map(PreservedMetadataRecord::to_flac_metadata_block)
            .collect()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub(crate) fn ingest_flac_block(
        &mut self,
        block_type: u8,
        payload: &[u8],
    ) -> crate::error::Result<()> {
        if block_type == 0 {
            return Ok(());
        }
        validate_preserved_block_payload(block_type, payload)
            .map_err(crate::error::Error::InvalidFlac)?;
        self.records.push(PreservedMetadataRecord {
            block_type,
            payload: payload.to_vec(),
        });
        Ok(())
    }

    pub(crate) fn fxmd_chunk_payload(&self) -> Option<Vec<u8>> {
        if self.records.is_empty() {
            return None;
        }

        let mut payload = Vec::new();
        payload.extend_from_slice(&FXMD_MAGIC);
        payload.extend_from_slice(&FXMD_VERSION.to_le_bytes());
        payload.extend_from_slice(&FXMD_HEADER_FLAGS.to_le_bytes());

        let mut blob_indices = BTreeMap::<Vec<u8>, u32>::new();
        let mut blobs = Vec::<Vec<u8>>::new();
        let mut records = Vec::<(u8, u8, u16, u32, u32)>::with_capacity(self.records.len());
        for (ordinal, record) in self.records.iter().enumerate() {
            let blob_index = if let Some(index) = blob_indices.get(&record.payload) {
                *index
            } else {
                let index = blobs.len() as u32;
                blobs.push(record.payload.clone());
                blob_indices.insert(record.payload.clone(), index);
                index
            };
            records.push((record.block_type, 0, 0, ordinal as u32, blob_index));
        }

        payload.extend_from_slice(&(blobs.len() as u32).to_le_bytes());
        payload.extend_from_slice(&(records.len() as u32).to_le_bytes());
        for blob in &blobs {
            payload.extend_from_slice(&(blob.len() as u32).to_le_bytes());
            payload.extend_from_slice(blob);
        }
        for (block_type, flags, reserved, ordinal, blob_index) in records {
            payload.push(block_type);
            payload.push(flags);
            payload.extend_from_slice(&reserved.to_le_bytes());
            payload.extend_from_slice(&ordinal.to_le_bytes());
            payload.extend_from_slice(&blob_index.to_le_bytes());
        }
        Some(payload)
    }

    pub(crate) fn from_fxmd_payload(payload: &[u8]) -> crate::error::Result<Self> {
        if payload.len() < 16 {
            return Err(crate::error::Error::InvalidPcmContainer(
                "fxmd payload is too short",
            ));
        }
        let mut cursor = 0usize;
        if payload[cursor..cursor + 4] != FXMD_MAGIC {
            return Err(crate::error::Error::InvalidPcmContainer(
                "fxmd payload magic is invalid",
            ));
        }
        cursor += 4;
        let version = u16::from_le_bytes(
            payload[cursor..cursor + 2]
                .try_into()
                .expect("fxmd version slice"),
        );
        cursor += 2;
        if version != FXMD_VERSION {
            return Err(crate::error::Error::InvalidPcmContainer(
                "fxmd payload version is unsupported",
            ));
        }
        let flags = u16::from_le_bytes(
            payload[cursor..cursor + 2]
                .try_into()
                .expect("fxmd flags slice"),
        );
        cursor += 2;
        if flags != FXMD_HEADER_FLAGS {
            return Err(crate::error::Error::InvalidPcmContainer(
                "fxmd payload flags are unsupported",
            ));
        }
        let blob_count = read_u32_le_strict(payload, &mut cursor, "fxmd blob count is truncated")?;
        let record_count =
            read_u32_le_strict(payload, &mut cursor, "fxmd record count is truncated")?;
        let mut blobs = Vec::with_capacity(blob_count as usize);
        for _ in 0..blob_count {
            let blob_len =
                read_u32_le_strict(payload, &mut cursor, "fxmd blob length is truncated")? as usize;
            let blob = read_bytes_strict(
                payload,
                &mut cursor,
                blob_len,
                "fxmd blob bytes are truncated",
            )?;
            blobs.push(blob);
        }

        let mut records = Vec::with_capacity(record_count as usize);
        let mut previous_ordinal = None;
        for _ in 0..record_count {
            if cursor + 12 > payload.len() {
                return Err(crate::error::Error::InvalidPcmContainer(
                    "fxmd record entry is truncated",
                ));
            }
            let block_type = payload[cursor];
            cursor += 1;
            let _flags = payload[cursor];
            cursor += 1;
            cursor += 2; // reserved
            let ordinal = u32::from_le_bytes(
                payload[cursor..cursor + 4]
                    .try_into()
                    .expect("fxmd ordinal slice"),
            );
            cursor += 4;
            let blob_index = u32::from_le_bytes(
                payload[cursor..cursor + 4]
                    .try_into()
                    .expect("fxmd blob index slice"),
            );
            cursor += 4;

            if previous_ordinal.is_some_and(|prev| ordinal < prev) {
                return Err(crate::error::Error::InvalidPcmContainer(
                    "fxmd record ordinals must be ascending",
                ));
            }
            previous_ordinal = Some(ordinal);
            let blob =
                blobs
                    .get(blob_index as usize)
                    .ok_or(crate::error::Error::InvalidPcmContainer(
                        "fxmd blob index is out of range",
                    ))?;
            validate_preserved_block_payload(block_type, blob)
                .map_err(crate::error::Error::InvalidPcmContainer)?;
            records.push(PreservedMetadataRecord {
                block_type,
                payload: blob.clone(),
            });
        }
        if cursor != payload.len() {
            return Err(crate::error::Error::InvalidPcmContainer(
                "fxmd payload has trailing bytes",
            ));
        }

        Ok(Self { records })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreservedMetadataRecord {
    block_type: u8,
    payload: Vec<u8>,
}

impl PreservedMetadataRecord {
    fn to_flac_metadata_block(&self) -> FlacMetadataBlock {
        match self.block_type {
            SEEKTABLE_BLOCK_TYPE => {
                FlacMetadataBlock::SeekTable(SeekTableBlock::new(&self.payload))
            }
            APPLICATION_BLOCK_TYPE => {
                FlacMetadataBlock::Application(ApplicationBlock::new(&self.payload))
            }
            PADDING_BLOCK_TYPE => FlacMetadataBlock::Padding(PaddingBlock::new(self.payload.len())),
            VORBIS_COMMENT_BLOCK_TYPE => FlacMetadataBlock::VorbisComment(
                VorbisCommentBlock::from_flac_payload(&self.payload)
                    .expect("preserved vorbis comment payload previously validated"),
            ),
            CUESHEET_BLOCK_TYPE => {
                FlacMetadataBlock::CueSheet(CueSheetBlock::from_raw_payload(&self.payload))
            }
            PICTURE_BLOCK_TYPE => FlacMetadataBlock::Picture(PictureBlock::new(&self.payload)),
            _ => FlacMetadataBlock::Opaque {
                block_type: self.block_type,
                payload: self.payload.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FlacMetadataBlock {
    SeekTable(SeekTableBlock),
    Application(ApplicationBlock),
    Padding(PaddingBlock),
    VorbisComment(VorbisCommentBlock),
    CueSheet(CueSheetBlock),
    Picture(PictureBlock),
    Opaque { block_type: u8, payload: Vec<u8> },
}

impl FlacMetadataBlock {
    pub(crate) fn block_type(&self) -> u8 {
        match self {
            Self::SeekTable(_) => SEEKTABLE_BLOCK_TYPE,
            Self::Application(_) => APPLICATION_BLOCK_TYPE,
            Self::Padding(_) => PADDING_BLOCK_TYPE,
            Self::VorbisComment(_) => VORBIS_COMMENT_BLOCK_TYPE,
            Self::CueSheet(_) => CUESHEET_BLOCK_TYPE,
            Self::Picture(_) => PICTURE_BLOCK_TYPE,
            Self::Opaque { block_type, .. } => *block_type,
        }
    }

    pub(crate) fn payload(&self) -> Vec<u8> {
        match self {
            Self::SeekTable(block) => block.payload(),
            Self::Application(block) => block.payload(),
            Self::Padding(block) => block.payload(),
            Self::VorbisComment(block) => block.payload(),
            Self::CueSheet(cuesheet) => cuesheet.payload(),
            Self::Picture(block) => block.payload(),
            Self::Opaque { payload, .. } => payload.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeekTableBlock {
    raw_payload: Vec<u8>,
}

impl SeekTableBlock {
    pub(crate) fn new(payload: &[u8]) -> Self {
        Self {
            raw_payload: payload.to_vec(),
        }
    }

    pub(crate) fn payload(&self) -> Vec<u8> {
        self.raw_payload.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApplicationBlock {
    raw_payload: Vec<u8>,
}

impl ApplicationBlock {
    pub(crate) fn new(payload: &[u8]) -> Self {
        Self {
            raw_payload: payload.to_vec(),
        }
    }

    pub(crate) fn payload(&self) -> Vec<u8> {
        self.raw_payload.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaddingBlock {
    len: usize,
}

impl PaddingBlock {
    pub(crate) fn new(len: usize) -> Self {
        Self { len }
    }

    pub(crate) fn payload(&self) -> Vec<u8> {
        vec![0u8; self.len]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PictureBlock {
    raw_payload: Vec<u8>,
}

impl PictureBlock {
    pub(crate) fn new(payload: &[u8]) -> Self {
        Self {
            raw_payload: payload.to_vec(),
        }
    }

    pub(crate) fn payload(&self) -> Vec<u8> {
        self.raw_payload.clone()
    }
}

fn validate_preserved_block_payload(
    block_type: u8,
    payload: &[u8],
) -> std::result::Result<(), &'static str> {
    match block_type {
        SEEKTABLE_BLOCK_TYPE => {
            validate_seektable_payload(payload).map_err(|_| "seektable payload is invalid")
        }
        APPLICATION_BLOCK_TYPE => {
            if payload.len() < 4 {
                return Err("application payload must contain a 4-byte application id");
            }
            Ok(())
        }
        PADDING_BLOCK_TYPE => {
            if payload.iter().any(|&byte| byte != 0) {
                return Err("padding payload must contain only zero bytes");
            }
            Ok(())
        }
        VORBIS_COMMENT_BLOCK_TYPE => VorbisCommentBlock::from_flac_payload(payload)
            .ok_or("vorbis comment payload is invalid")
            .map(|_| ()),
        CUESHEET_BLOCK_TYPE => super::draft::parse_cuesheet_tracks(payload)
            .map_err(|_| "cuesheet payload is invalid")
            .map(|_| ()),
        PICTURE_BLOCK_TYPE => validate_picture_payload(payload),
        0 | 127 => Err("unsupported preserved metadata block type"),
        _ => Ok(()),
    }
}

fn validate_picture_payload(payload: &[u8]) -> std::result::Result<(), &'static str> {
    let mut cursor = 0usize;
    let _picture_type = read_u32_le(payload, &mut cursor).ok_or("picture type is truncated")?;
    let mime_len =
        read_u32_le(payload, &mut cursor).ok_or("picture MIME length is truncated")? as usize;
    let _mime =
        read_bytes(payload, &mut cursor, mime_len).ok_or("picture MIME bytes are truncated")?;
    let description_len = read_u32_le(payload, &mut cursor)
        .ok_or("picture description length is truncated")? as usize;
    let _description = read_bytes(payload, &mut cursor, description_len)
        .ok_or("picture description bytes are truncated")?;
    let _width = read_u32_le(payload, &mut cursor).ok_or("picture width is truncated")?;
    let _height = read_u32_le(payload, &mut cursor).ok_or("picture height is truncated")?;
    let _depth = read_u32_le(payload, &mut cursor).ok_or("picture depth is truncated")?;
    let _colors = read_u32_le(payload, &mut cursor).ok_or("picture color count is truncated")?;
    let picture_data_len =
        read_u32_le(payload, &mut cursor).ok_or("picture data length is truncated")? as usize;
    let _picture_data = read_bytes(payload, &mut cursor, picture_data_len)
        .ok_or("picture data bytes are truncated")?;
    if cursor != payload.len() {
        return Err("picture payload has trailing bytes");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VorbisCommentBlock {
    pub(crate) vendor: String,
    pub(crate) entries: Vec<String>,
}

impl VorbisCommentBlock {
    pub(crate) fn new(vendor: String, entries: Vec<String>) -> Self {
        Self { vendor, entries }
    }

    pub(crate) fn from_flac_payload(payload: &[u8]) -> Option<Self> {
        let mut cursor = 0usize;
        let vendor_len = read_u32_le(payload, &mut cursor)? as usize;
        let vendor = read_utf8_entry(payload, &mut cursor, vendor_len)?;
        let comment_count = read_u32_le(payload, &mut cursor)? as usize;
        let mut entries = Vec::with_capacity(comment_count);
        for _ in 0..comment_count {
            let entry_len = read_u32_le(payload, &mut cursor)? as usize;
            let entry = read_utf8_entry(payload, &mut cursor, entry_len)?;
            entries.push(entry);
        }
        if cursor != payload.len() {
            return None;
        }
        Some(Self { vendor, entries })
    }

    pub(crate) fn payload(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        append_u32_le(&mut payload, self.vendor.len() as u32);
        payload.extend_from_slice(self.vendor.as_bytes());
        append_u32_le(&mut payload, self.entries.len() as u32);
        for entry in &self.entries {
            append_u32_le(&mut payload, entry.len() as u32);
            payload.extend_from_slice(entry.as_bytes());
        }
        payload
    }

    pub(crate) fn push_entry(&mut self, entry: String) {
        self.entries.push(entry);
    }

    pub(crate) fn entries(&self) -> &[String] {
        &self.entries
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CueSheetBlock {
    raw_payload: Vec<u8>,
}

impl CueSheetBlock {
    pub(crate) fn from_raw_payload(payload: &[u8]) -> Self {
        Self {
            raw_payload: payload.to_vec(),
        }
    }

    pub(crate) fn from_projection(cuesheet: &CueSheet) -> Self {
        Self {
            raw_payload: cuesheet.payload(),
        }
    }

    pub(crate) fn payload(&self) -> Vec<u8> {
        self.raw_payload.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CueSheet {
    pub(crate) track_offsets: Vec<u64>,
    pub(crate) lead_out_offset: u64,
}

impl CueSheet {
    pub(crate) fn payload(&self) -> Vec<u8> {
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
