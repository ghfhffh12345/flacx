mod blocks;
mod draft;

use crate::input::ordinary_channel_mask;

pub(crate) use blocks::{
    CueSheetBlock, FlacMetadataBlock, PreservedMetadataBundle, VorbisCommentBlock,
};
#[cfg(test)]
pub(crate) use draft::parse_cuesheet_tracks;
pub(crate) use draft::{
    MetadataDraft, append_chunk_payload, append_u32_le, cue_points_from_cuesheet_payload,
    format_channel_mask, normalize_cuesheet, parse_channel_layout_provenance_comment,
    parse_channel_mask_comment, raw_vorbis_comment_entry, split_vorbis_comment_entry,
    wav_info_chunk_id_for_vorbis_key,
};

pub(crate) const SEEKTABLE_BLOCK_TYPE: u8 = 3;
pub(crate) const SEEKTABLE_POINT_LEN: usize = 18;
pub(crate) const SEEKTABLE_PLACEHOLDER_SAMPLE_NUMBER: u64 = u64::MAX;
const APPLICATION_BLOCK_TYPE: u8 = 2;
const PADDING_BLOCK_TYPE: u8 = 1;
const VORBIS_COMMENT_BLOCK_TYPE: u8 = 4;
const CUESHEET_BLOCK_TYPE: u8 = 5;
const PICTURE_BLOCK_TYPE: u8 = 6;
const CUESHEET_LEADOUT_TRACK_NUMBER: u8 = 170;
const CUESHEET_HEADER_LEN: usize = 396;
const CUESHEET_TRACK_HEADER_LEN: usize = 36;
const CUESHEET_INDEX_LEN: usize = 12;
pub(crate) const FXMD_CHUNK_ID: [u8; 4] = *b"fxmd";
const FXMD_MAGIC: [u8; 4] = *b"fxmd";
const FXMD_VERSION: u16 = 1;
const FXMD_HEADER_FLAGS: u16 = 1;
const WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY: &str = "WAVEFORMATEXTENSIBLE_CHANNEL_MASK";
pub(crate) const FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY: &str = "FLACX_CHANNEL_LAYOUT_PROVENANCE";
const FLACX_CHANNEL_LAYOUT_PROVENANCE_VALUE: &str = "1";
const MAX_RFC9639_CHANNEL_MASK: u32 = 0x0003_FFFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FxmdChunkPolicy {
    pub(crate) capture: bool,
    pub(crate) strict: bool,
}

impl FxmdChunkPolicy {
    pub(crate) const IGNORE: Self = Self {
        capture: false,
        strict: false,
    };
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
/// Semantic metadata captured from container inputs or staged for output.
///
/// `Metadata` is the type to reach for when you want to inspect or edit comment
/// fields, cue points, or channel-layout preservation information before an
/// encode, decode, or recompress operation.
pub struct Metadata {
    preserved: PreservedMetadataBundle,
    vorbis_comment: Option<VorbisCommentBlock>,
    cue_points: Vec<u32>,
    channel_mask: Option<u32>,
    channel_layout_provenance: bool,
}

impl Metadata {
    /// Create an empty semantic metadata value for direct source construction.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the normalized comment entries currently stored on this metadata.
    #[must_use]
    pub fn comments(&self) -> Vec<(&str, &str)> {
        self.vorbis_comment
            .as_ref()
            .map(|block| {
                block
                    .entries()
                    .iter()
                    .filter_map(|entry| split_vorbis_comment_entry(entry))
                    .filter(|(key, _)| {
                        !key.eq_ignore_ascii_case(WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY)
                            && !key.eq_ignore_ascii_case(FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Append one semantic comment entry.
    pub fn add_comment<K, V>(&mut self, key: K, value: V)
    where
        K: AsRef<str>,
        V: AsRef<str>,
    {
        self.ensure_semantic_editable();
        self.vorbis_comment_mut()
            .push_entry(raw_vorbis_comment_entry(key.as_ref(), value.as_ref()));
    }

    /// Replace all comment values for one key.
    pub fn set_comments<I, S>(&mut self, key: &str, values: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.ensure_semantic_editable();
        let preserved = self
            .comments()
            .into_iter()
            .filter(|(existing_key, _)| !existing_key.eq_ignore_ascii_case(key))
            .map(|(existing_key, value)| raw_vorbis_comment_entry(existing_key, value))
            .collect::<Vec<_>>();
        let mut block = VorbisCommentBlock::new(
            format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
            preserved,
        );
        for value in values {
            block.push_entry(raw_vorbis_comment_entry(key, value.as_ref()));
        }
        self.vorbis_comment = if block.entries().is_empty() {
            None
        } else {
            Some(block)
        };
    }

    /// Remove all comment values for one key and return the number removed.
    pub fn remove_comments(&mut self, key: &str) -> usize {
        let removed = self
            .comments()
            .into_iter()
            .filter(|(existing_key, _)| existing_key.eq_ignore_ascii_case(key))
            .count();
        if removed == 0 {
            return 0;
        }
        self.set_comments::<Vec<&str>, &str>(key, Vec::new());
        removed
    }

    /// Return the representable cue-point view currently stored on this metadata.
    #[must_use]
    pub fn cue_points(&self) -> &[u32] {
        &self.cue_points
    }

    /// Replace the semantic cue-point list.
    pub fn set_cue_points<I>(&mut self, cue_points: I)
    where
        I: IntoIterator<Item = u32>,
    {
        self.ensure_semantic_editable();
        self.cue_points = cue_points.into_iter().collect();
    }

    /// Clear the semantic cue-point list.
    pub fn clear_cue_points(&mut self) {
        self.ensure_semantic_editable();
        self.cue_points.clear();
    }

    /// Return the explicit semantic channel mask when present.
    #[must_use]
    pub fn channel_mask(&self) -> Option<u32> {
        self.channel_mask
    }

    /// Set an explicit semantic channel mask.
    pub fn set_channel_mask(&mut self, mask: u32) {
        self.ensure_semantic_editable();
        self.channel_mask = Some(mask);
        self.channel_layout_provenance = true;
    }

    /// Clear any explicit semantic channel mask.
    pub fn clear_channel_mask(&mut self) {
        self.ensure_semantic_editable();
        self.channel_mask = None;
    }

    /// Return whether the metadata records explicit channel-layout provenance.
    #[must_use]
    pub fn has_channel_layout_provenance(&self) -> bool {
        self.channel_layout_provenance
    }

    /// Control the semantic channel-layout provenance flag.
    pub fn set_channel_layout_provenance(&mut self, value: bool) {
        self.ensure_semantic_editable();
        self.channel_layout_provenance = value;
    }

    pub(crate) fn has_preserved_bundle(&self) -> bool {
        !self.preserved.is_empty()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.preserved.is_empty() && self.vorbis_comment.is_none() && self.cue_points.is_empty()
    }

    pub(crate) fn set_channel_mask_for_channels(&mut self, channels: u16, mask: u32) {
        if self.has_preserved_bundle() {
            return;
        }
        if ordinary_channel_mask(channels).is_some_and(|ordinary| ordinary == mask) {
            return;
        }
        self.channel_mask = Some(mask);
        self.channel_layout_provenance = true;
    }

    pub(crate) fn flac_blocks(&self, total_samples: u64) -> Vec<FlacMetadataBlock> {
        if !self.preserved.is_empty() {
            return self.preserved.flac_blocks();
        }
        let mut blocks = Vec::new();
        let mut vorbis_comment = self.vorbis_comment.clone();
        if let Some(channel_mask) = self.channel_mask {
            if let Some(block) = &mut vorbis_comment {
                block.push_entry(raw_vorbis_comment_entry(
                    WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY,
                    &format_channel_mask(channel_mask),
                ));
            } else {
                vorbis_comment = Some(VorbisCommentBlock::new(
                    format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
                    vec![raw_vorbis_comment_entry(
                        WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY,
                        &format_channel_mask(channel_mask),
                    )],
                ));
            }
        }
        if self.channel_layout_provenance {
            if let Some(block) = &mut vorbis_comment {
                block.push_entry(raw_vorbis_comment_entry(
                    FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY,
                    FLACX_CHANNEL_LAYOUT_PROVENANCE_VALUE,
                ));
            } else {
                vorbis_comment = Some(VorbisCommentBlock::new(
                    format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
                    vec![raw_vorbis_comment_entry(
                        FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY,
                        FLACX_CHANNEL_LAYOUT_PROVENANCE_VALUE,
                    )],
                ));
            }
        }
        if let Some(block) = vorbis_comment {
            blocks.push(FlacMetadataBlock::VorbisComment(block));
        }
        if let Some(cuesheet) = normalize_cuesheet(
            self.cue_points.iter().copied().map(u64::from).collect(),
            total_samples,
        ) {
            blocks.push(FlacMetadataBlock::CueSheet(CueSheetBlock::from_projection(
                &cuesheet,
            )));
        }
        blocks
    }

    pub(crate) fn ingest_flac_metadata_block(
        &mut self,
        block_type: u8,
        payload: &[u8],
        total_samples: u64,
        channels: u8,
    ) -> crate::error::Result<()> {
        self.preserved.ingest_flac_block(block_type, payload)?;
        match block_type {
            SEEKTABLE_BLOCK_TYPE => {}
            PADDING_BLOCK_TYPE => {}
            APPLICATION_BLOCK_TYPE => {}
            VORBIS_COMMENT_BLOCK_TYPE => self.ingest_vorbis_comment_payload(payload, channels)?,
            CUESHEET_BLOCK_TYPE => self.ingest_cuesheet_payload(payload, total_samples),
            PICTURE_BLOCK_TYPE => {}
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn unified_chunk_payload(&self) -> Option<Vec<u8>> {
        self.preserved.fxmd_chunk_payload()
    }

    pub(crate) fn list_info_chunk_payload(&self) -> Option<Vec<u8>> {
        let mut payload = b"INFO".to_vec();
        let mut count = 0usize;
        if let Some(block) = &self.vorbis_comment {
            for entry in block.entries() {
                let Some((key, value)) = split_vorbis_comment_entry(entry) else {
                    continue;
                };
                let Some(chunk_id) = wav_info_chunk_id_for_vorbis_key(key) else {
                    continue;
                };
                if value.is_empty() {
                    continue;
                }
                append_chunk_payload(&mut payload, &chunk_id, value.as_bytes());
                count += 1;
            }
        }
        (count > 0).then_some(payload)
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
        let Some(block) = VorbisCommentBlock::from_flac_payload(payload) else {
            return Ok(());
        };
        self.vorbis_comment = Some(block.clone());
        for entry in block.entries() {
            let Some((key, value)) = split_vorbis_comment_entry(entry) else {
                continue;
            };
            if key.eq_ignore_ascii_case(WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY) {
                self.channel_mask = Some(parse_channel_mask_comment(value, channels)?);
                continue;
            }
            if key.eq_ignore_ascii_case(FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY) {
                self.channel_layout_provenance = parse_channel_layout_provenance_comment(value)?;
            }
        }
        Ok(())
    }

    fn ingest_cuesheet_payload(&mut self, payload: &[u8], total_samples: u64) {
        self.cue_points = cue_points_from_cuesheet_payload(payload, total_samples);
    }

    fn ensure_semantic_editable(&mut self) {
        if !self.preserved.is_empty() {
            self.preserved = PreservedMetadataBundle::default();
        }
    }

    fn vorbis_comment_mut(&mut self) -> &mut VorbisCommentBlock {
        self.vorbis_comment.get_or_insert_with(|| {
            VorbisCommentBlock::new(
                format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
                Vec::new(),
            )
        })
    }
}

pub(crate) type WavMetadata = Metadata;

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

#[cfg(test)]
mod tests {
    use super::{
        APPLICATION_BLOCK_TYPE, FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY, FXMD_VERSION,
        FlacMetadataBlock, FxmdChunkPolicy, MetadataDraft, PICTURE_BLOCK_TYPE,
        PreservedMetadataBundle, SEEKTABLE_PLACEHOLDER_SAMPLE_NUMBER, SeekPoint,
        WAVEFORMATEXTENSIBLE_CHANNEL_MASK_KEY, WavMetadata, parse_cuesheet_tracks,
        validate_seektable_payload,
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

    fn application_payload(bytes: &[u8]) -> Vec<u8> {
        let mut payload = b"TEST".to_vec();
        payload.extend_from_slice(bytes);
        payload
    }

    fn picture_payload(bytes: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&3u32.to_le_bytes());
        payload.extend_from_slice(&9u32.to_le_bytes());
        payload.extend_from_slice(b"image/png");
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&24u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        payload.extend_from_slice(bytes);
        payload
    }

    #[test]
    fn normalizes_info_chunks_into_stable_vorbis_comments() {
        let mut draft = MetadataDraft::default();
        draft
            .ingest_chunk(
                *b"LIST",
                &info_list_chunk(&[
                    (*b"IART", b"Example Artist"),
                    (*b"INAM", b"Song Title"),
                    (*b"IZZZ", b"ignored"),
                    (*b"IART", b"Guest Artist"),
                ]),
                FxmdChunkPolicy::IGNORE,
            )
            .unwrap();

        let blocks = draft.finish(8_000).flac_blocks(8_000);
        let FlacMetadataBlock::VorbisComment(block) = &blocks[0] else {
            panic!("expected vorbis comments");
        };

        let rendered = block.entries.clone();
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
        draft
            .ingest_chunk(
                *b"LIST",
                &info_list_chunk(&[(*b"IART", &[0xff, 0xfe, 0xfd])]),
                FxmdChunkPolicy::IGNORE,
            )
            .unwrap();

        assert!(draft.finish(8_000).flac_blocks(8_000).is_empty());
    }

    #[test]
    fn normalizes_representable_cue_points_into_cuesheet_tracks() {
        let mut draft = MetadataDraft::default();
        draft
            .ingest_chunk(
                *b"cue ",
                &cue_chunk(&[4_000, 1_000, 4_000]),
                FxmdChunkPolicy::IGNORE,
            )
            .unwrap();

        let blocks = draft.finish(6_000).flac_blocks(6_000);
        let FlacMetadataBlock::CueSheet(cuesheet) = &blocks[0] else {
            panic!("expected cuesheet block");
        };
        let tracks = parse_cuesheet_tracks(&cuesheet.payload()).unwrap();
        assert_eq!(tracks.len(), 3);
        assert_eq!(tracks[0].offset, 1_000);
        assert_eq!(tracks[1].offset, 4_000);
        assert!(tracks[2].is_lead_out);
    }

    #[test]
    fn drops_cue_points_that_cannot_be_represented() {
        let mut draft = MetadataDraft::default();
        draft
            .ingest_chunk(*b"cue ", &cue_chunk(&[4_000]), FxmdChunkPolicy::IGNORE)
            .unwrap();

        assert!(draft.finish(4_000).flac_blocks(4_000).is_empty());
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
        draft
            .ingest_chunk(
                *b"LIST",
                &info_list_chunk(&[(*b"INAM", b"Example Title")]),
                FxmdChunkPolicy::IGNORE,
            )
            .unwrap();
        let mut metadata = draft.finish(4_096);
        metadata.set_channel_mask_for_channels(4, 0x0001_2104);

        let blocks = metadata.flac_blocks(4_096);
        let FlacMetadataBlock::VorbisComment(block) = &blocks[0] else {
            panic!("expected vorbis comments");
        };

        let rendered = block.entries.clone();
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
        metadata.set_channel_mask_for_channels(4, 0x0033);

        assert!(metadata.flac_blocks(4_096).is_empty());
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
    fn wav_metadata_with_preserved_bundle_converts_back_to_encode_metadata() {
        let mut metadata = WavMetadata::default();
        metadata
            .ingest_flac_metadata_block(2, &application_payload(b"opaque-app"), 8_000, 1)
            .unwrap();

        let blocks = metadata.flac_blocks(8_000);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].block_type(), 2);
        assert_eq!(blocks[0].payload(), application_payload(b"opaque-app"));
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

    #[test]
    fn canonical_fxmd_v1_round_trips_exact_preserved_payloads() {
        let mut bundle = PreservedMetadataBundle::default();
        bundle
            .ingest_flac_block(APPLICATION_BLOCK_TYPE, &application_payload(b"opaque-app"))
            .unwrap();
        bundle
            .ingest_flac_block(PICTURE_BLOCK_TYPE, &picture_payload(b"\x89PNGexact"))
            .unwrap();

        let payload = bundle.fxmd_chunk_payload().unwrap();
        let restored = PreservedMetadataBundle::from_fxmd_payload(&payload).unwrap();

        assert_eq!(payload[4..6], FXMD_VERSION.to_le_bytes());
        assert_eq!(restored, bundle);
    }

    #[test]
    fn rejects_unsupported_fxmd_versions() {
        let mut bundle = PreservedMetadataBundle::default();
        bundle
            .ingest_flac_block(APPLICATION_BLOCK_TYPE, &application_payload(b"opaque-app"))
            .unwrap();
        let mut payload = bundle.fxmd_chunk_payload().unwrap();
        payload[4..6].copy_from_slice(&2u16.to_le_bytes());

        let error = PreservedMetadataBundle::from_fxmd_payload(&payload).unwrap_err();

        assert!(error.to_string().contains("version is unsupported"));
    }

    #[test]
    fn rejects_unsupported_fxmd_header_flags() {
        let mut bundle = PreservedMetadataBundle::default();
        bundle
            .ingest_flac_block(APPLICATION_BLOCK_TYPE, &application_payload(b"opaque-app"))
            .unwrap();
        let mut payload = bundle.fxmd_chunk_payload().unwrap();
        payload[6..8].copy_from_slice(&0u16.to_le_bytes());

        let error = PreservedMetadataBundle::from_fxmd_payload(&payload).unwrap_err();

        assert!(error.to_string().contains("flags are unsupported"));
    }
}
