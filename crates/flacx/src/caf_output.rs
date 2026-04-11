use std::io::Write;

use crate::{
    error::{Error, Result},
    input::{WavSpec, container_bits_from_valid_bits},
    md5::streaminfo_md5,
    metadata::WavMetadata,
    pcm::{PcmEnvelope, is_supported_channel_mask, ordinary_channel_mask},
};

const CAF_MAGIC: [u8; 4] = *b"caff";
const CAF_VERSION: u16 = 1;
const CAF_DESC_CHUNK_ID: [u8; 4] = *b"desc";
const CAF_DATA_CHUNK_ID: [u8; 4] = *b"data";
const CAF_CHAN_CHUNK_ID: [u8; 4] = *b"chan";
const CAF_INFO_CHUNK_ID: [u8; 4] = *b"info";
const CAF_MARK_CHUNK_ID: [u8; 4] = *b"mark";
const CAF_LPCM_FORMAT_ID: [u8; 4] = *b"lpcm";
const CAF_LAYOUT_TAG_USE_CHANNEL_BITMAP: u32 = 0x0001_0000;
const CAF_FORMAT_FLAG_IS_LITTLE_ENDIAN: u32 = 1 << 1;
const CAF_MARKER_TYPE_GENERIC: u32 = 0;
const CAF_INVALID_SMPTE_TIME: [u8; 8] = [0xFF; 8];
const CAF_MARKER_SIZE: usize = 28;

#[allow(dead_code)]
pub(crate) fn write_caf<W: Write>(
    writer: &mut W,
    spec: WavSpec,
    samples: &[i32],
    metadata: &WavMetadata,
) -> Result<[u8; 16]> {
    if !(1..=8).contains(&spec.channels) {
        return Err(Error::UnsupportedWav(format!(
            "only the ordinary 1..8 channel envelope is supported, found {} channels",
            spec.channels
        )));
    }
    if !matches!(spec.bits_per_sample, 4..=32) {
        return Err(Error::UnsupportedWav(format!(
            "only FLAC-native 4..32 valid bits/sample are supported, found {}",
            spec.bits_per_sample
        )));
    }
    if !samples.len().is_multiple_of(usize::from(spec.channels)) {
        return Err(Error::Decode(
            "decoded samples are not aligned to the channel count".into(),
        ));
    }

    let container_bits_per_sample = container_bits_from_valid_bits(u16::from(spec.bits_per_sample));
    if spec.bytes_per_sample * 8 != container_bits_per_sample {
        return Err(Error::UnsupportedWav(format!(
            "bytes/sample does not match the chosen container width for {} valid bits/sample",
            spec.bits_per_sample
        )));
    }

    let envelope = PcmEnvelope {
        channels: u16::from(spec.channels),
        valid_bits_per_sample: u16::from(spec.bits_per_sample),
        container_bits_per_sample,
        channel_mask: spec.channel_mask,
    };
    let streaminfo_md5 = streaminfo_md5(spec, samples)?;

    let data_bytes = u64::try_from(samples.len())
        .expect("sample slice length fits u64")
        .checked_mul(u64::from(container_bits_per_sample / 8))
        .ok_or_else(|| Error::UnsupportedWav("CAF data chunk overflows".into()))?;

    let channel_layout_chunk = caf_channel_layout_chunk(spec)?;
    let info_chunk = caf_info_chunk_payload(metadata)?;
    let mark_chunk = caf_mark_chunk_payload(metadata)?;

    write_header(writer)?;
    write_desc_chunk(writer, spec, container_bits_per_sample)?;
    if let Some(payload) = channel_layout_chunk {
        write_chunk(writer, CAF_CHAN_CHUNK_ID, &payload)?;
    }
    if let Some(payload) = info_chunk {
        write_chunk(writer, CAF_INFO_CHUNK_ID, &payload)?;
    }
    if let Some(payload) = mark_chunk {
        write_chunk(writer, CAF_MARK_CHUNK_ID, &payload)?;
    }
    write_data_chunk_header(writer, data_bytes)?;

    write_all_zero_edit_count(writer)?;
    write_sample_bytes(writer, samples, envelope)?;
    Ok(streaminfo_md5)
}

pub(crate) struct CafStreamWriter<W: Write> {
    writer: W,
    envelope: PcmEnvelope,
}

impl<W: Write> CafStreamWriter<W> {
    pub(crate) fn new(mut writer: W, spec: WavSpec, metadata: &WavMetadata) -> Result<Self> {
        if !(1..=8).contains(&spec.channels) {
            return Err(Error::UnsupportedWav(format!(
                "only the ordinary 1..8 channel envelope is supported, found {} channels",
                spec.channels
            )));
        }
        if !matches!(spec.bits_per_sample, 4..=32) {
            return Err(Error::UnsupportedWav(format!(
                "only FLAC-native 4..32 valid bits/sample are supported, found {}",
                spec.bits_per_sample
            )));
        }

        let container_bits_per_sample =
            container_bits_from_valid_bits(u16::from(spec.bits_per_sample));
        if spec.bytes_per_sample * 8 != container_bits_per_sample {
            return Err(Error::UnsupportedWav(format!(
                "bytes/sample does not match the chosen container width for {} valid bits/sample",
                spec.bits_per_sample
            )));
        }

        let envelope = PcmEnvelope {
            channels: u16::from(spec.channels),
            valid_bits_per_sample: u16::from(spec.bits_per_sample),
            container_bits_per_sample,
            channel_mask: spec.channel_mask,
        };
        let data_bytes = spec
            .total_samples
            .checked_mul(u64::from(spec.channels))
            .and_then(|count| count.checked_mul(u64::from(container_bits_per_sample / 8)))
            .ok_or_else(|| Error::UnsupportedWav("CAF data chunk overflows".into()))?;
        let channel_layout_chunk = caf_channel_layout_chunk(spec)?;
        let info_chunk = caf_info_chunk_payload(metadata)?;
        let mark_chunk = caf_mark_chunk_payload(metadata)?;

        write_header(&mut writer)?;
        write_desc_chunk(&mut writer, spec, container_bits_per_sample)?;
        if let Some(payload) = channel_layout_chunk {
            write_chunk(&mut writer, CAF_CHAN_CHUNK_ID, &payload)?;
        }
        if let Some(payload) = info_chunk {
            write_chunk(&mut writer, CAF_INFO_CHUNK_ID, &payload)?;
        }
        if let Some(payload) = mark_chunk {
            write_chunk(&mut writer, CAF_MARK_CHUNK_ID, &payload)?;
        }
        write_data_chunk_header(&mut writer, data_bytes)?;
        write_all_zero_edit_count(&mut writer)?;

        Ok(Self { writer, envelope })
    }

    pub(crate) fn write_samples(&mut self, samples: &[i32]) -> Result<()> {
        write_sample_bytes(&mut self.writer, samples, self.envelope)
    }

    pub(crate) fn finish(mut self) -> Result<W> {
        self.writer.flush()?;
        Ok(self.writer)
    }
}

fn write_header<W: Write>(writer: &mut W) -> Result<()> {
    writer.write_all(&CAF_MAGIC)?;
    writer.write_all(&CAF_VERSION.to_be_bytes())?;
    writer.write_all(&0u16.to_be_bytes())?;
    Ok(())
}

fn write_desc_chunk<W: Write>(
    writer: &mut W,
    spec: WavSpec,
    container_bits_per_sample: u16,
) -> Result<()> {
    let bytes_per_frame = u32::from(spec.channels)
        .checked_mul(u32::from(container_bits_per_sample / 8))
        .ok_or_else(|| Error::UnsupportedWav("CAF bytes/frame overflows".into()))?;

    let mut payload = Vec::with_capacity(32);
    payload.extend_from_slice(&(spec.sample_rate as f64).to_be_bytes());
    payload.extend_from_slice(&CAF_LPCM_FORMAT_ID);
    payload.extend_from_slice(&CAF_FORMAT_FLAG_IS_LITTLE_ENDIAN.to_be_bytes());
    payload.extend_from_slice(&bytes_per_frame.to_be_bytes());
    payload.extend_from_slice(&1u32.to_be_bytes());
    payload.extend_from_slice(&u32::from(spec.channels).to_be_bytes());
    payload.extend_from_slice(&u32::from(spec.bits_per_sample).to_be_bytes());

    write_chunk(writer, CAF_DESC_CHUNK_ID, &payload)
}

fn write_data_chunk_header<W: Write>(writer: &mut W, data_bytes: u64) -> Result<()> {
    let payload_size = 4u64
        .checked_add(data_bytes)
        .ok_or_else(|| Error::UnsupportedWav("CAF data chunk overflows".into()))?;
    let payload_size = i64::try_from(payload_size)
        .map_err(|_| Error::UnsupportedWav("CAF data chunk exceeds signed size range".into()))?;

    writer.write_all(&CAF_DATA_CHUNK_ID)?;
    writer.write_all(&payload_size.to_be_bytes())?;
    Ok(())
}

fn write_all_zero_edit_count<W: Write>(writer: &mut W) -> Result<()> {
    writer.write_all(&0u32.to_be_bytes())?;
    Ok(())
}

fn caf_channel_layout_chunk(spec: WavSpec) -> Result<Option<Vec<u8>>> {
    let channels = u32::from(spec.channels);
    let mask = spec.channel_mask;

    if mask == 0 {
        return if channels <= 2 {
            Ok(None)
        } else {
            Err(Error::UnsupportedWav(format!(
                "CAF 3..8 channel outputs require a supported channel layout bitmap, found {mask:#010x}"
            )))
        };
    }

    if !is_supported_channel_mask(u16::from(spec.channels), mask) {
        return Err(Error::UnsupportedWav(format!(
            "channel mask {mask:#010x} is not supported for {} channels",
            spec.channels
        )));
    }
    if mask.count_ones() != channels {
        return Err(Error::UnsupportedWav(format!(
            "channel mask {mask:#010x} does not describe {} channels",
            spec.channels
        )));
    }

    if channels <= 2 && ordinary_channel_mask(u16::from(spec.channels)) == Some(mask) {
        return Ok(None);
    }

    let mut payload = Vec::with_capacity(12);
    payload.extend_from_slice(&CAF_LAYOUT_TAG_USE_CHANNEL_BITMAP.to_be_bytes());
    payload.extend_from_slice(&mask.to_be_bytes());
    payload.extend_from_slice(&0u32.to_be_bytes());
    Ok(Some(payload))
}

fn caf_info_chunk_payload(metadata: &WavMetadata) -> Result<Option<Vec<u8>>> {
    let Some(payload) = metadata.list_info_chunk_payload() else {
        return Ok(None);
    };

    let entries = parse_riff_info_payload(&payload)?;
    let mut projected: Vec<(String, String)> = Vec::new();
    for (chunk_id, value) in entries {
        let Some(key) = caf_info_key_for_riff_chunk_id(chunk_id, &value) else {
            continue;
        };

        if let Some(existing) = projected.iter_mut().find(|entry| entry.0 == key) {
            existing.1.push(',');
            existing.1.push_str(&value);
        } else {
            projected.push((key, value));
        }
    }

    if projected.is_empty() {
        return Ok(None);
    }

    let mut payload = Vec::new();
    payload.extend_from_slice(&(projected.len() as u32).to_be_bytes());
    for (key, value) in projected {
        payload.extend_from_slice(key.as_bytes());
        payload.push(0);
        payload.extend_from_slice(value.as_bytes());
        payload.push(0);
    }
    Ok(Some(payload))
}

fn caf_mark_chunk_payload(metadata: &WavMetadata) -> Result<Option<Vec<u8>>> {
    let Some(payload) = metadata.cue_chunk_payload() else {
        return Ok(None);
    };

    let cue_points = parse_riff_cue_payload(&payload)?;
    if cue_points.is_empty() {
        return Ok(None);
    }

    let mut payload = Vec::with_capacity(4 + cue_points.len() * CAF_MARKER_SIZE);
    payload.extend_from_slice(&(cue_points.len() as u32).to_be_bytes());
    for (index, sample_offset) in cue_points.iter().enumerate() {
        payload.extend_from_slice(&CAF_MARKER_TYPE_GENERIC.to_be_bytes());
        payload.extend_from_slice(&(*sample_offset as f64).to_be_bytes());
        payload.extend_from_slice(&((index as u32) + 1).to_be_bytes());
        payload.extend_from_slice(&CAF_INVALID_SMPTE_TIME);
        payload.extend_from_slice(&0u32.to_be_bytes());
    }
    Ok(Some(payload))
}

fn parse_riff_info_payload(payload: &[u8]) -> Result<Vec<([u8; 4], String)>> {
    if payload.len() < 4 || &payload[..4] != b"INFO" {
        return Err(Error::InvalidWav("RIFF INFO payload is invalid"));
    }

    let mut cursor = 4usize;
    let mut entries = Vec::new();
    while cursor + 8 <= payload.len() {
        let chunk_id: [u8; 4] = payload[cursor..cursor + 4]
            .try_into()
            .expect("fixed info chunk id");
        let chunk_len = u32::from_le_bytes(
            payload[cursor + 4..cursor + 8]
                .try_into()
                .expect("fixed info chunk length"),
        ) as usize;
        cursor += 8;

        let end = cursor
            .checked_add(chunk_len)
            .ok_or(Error::InvalidWav("RIFF INFO payload length overflows"))?;
        if end > payload.len() {
            return Err(Error::InvalidWav("RIFF INFO payload is truncated"));
        }

        let value = String::from_utf8(payload[cursor..end].to_vec())
            .map_err(|_| Error::InvalidWav("RIFF INFO payload contains invalid UTF-8"))?;
        cursor = end;
        if !chunk_len.is_multiple_of(2) {
            if cursor >= payload.len() {
                return Err(Error::InvalidWav("RIFF INFO payload is truncated"));
            }
            cursor += 1;
        }

        entries.push((chunk_id, value));
    }

    if cursor != payload.len() {
        return Err(Error::InvalidWav("RIFF INFO payload has trailing bytes"));
    }

    Ok(entries)
}

fn parse_riff_cue_payload(payload: &[u8]) -> Result<Vec<u32>> {
    if payload.len() < 4 {
        return Err(Error::InvalidWav("RIFF cue payload is too short"));
    }

    let cue_count = u32::from_le_bytes(payload[..4].try_into().expect("fixed cue count")) as usize;
    let mut cursor = 4usize;
    let mut cue_points = Vec::with_capacity(cue_count);

    for _ in 0..cue_count {
        if cursor + 24 > payload.len() {
            return Err(Error::InvalidWav("RIFF cue payload is truncated"));
        }

        cursor += 8; // cue point id + position
        let chunk_id: [u8; 4] = payload[cursor..cursor + 4]
            .try_into()
            .expect("fixed cue chunk id");
        cursor += 4;
        if chunk_id != *b"data" {
            return Err(Error::InvalidWav(
                "RIFF cue payload references an unsupported chunk",
            ));
        }
        cursor += 8; // chunk start + block start
        let sample_offset = u32::from_le_bytes(
            payload[cursor..cursor + 4]
                .try_into()
                .expect("fixed cue sample offset"),
        );
        cursor += 4;
        cue_points.push(sample_offset);
    }

    if cursor != payload.len() {
        return Err(Error::InvalidWav("RIFF cue payload has trailing bytes"));
    }

    Ok(cue_points)
}

fn caf_info_key_for_riff_chunk_id(chunk_id: [u8; 4], value: &str) -> Option<String> {
    match &chunk_id {
        b"IART" => Some("artist".to_owned()),
        b"ICMT" => Some("comments".to_owned()),
        b"ICOP" => Some("copyright".to_owned()),
        b"ICRD" => {
            if value.trim().len() == 4 && value.chars().all(|ch| ch.is_ascii_digit()) {
                Some("year".to_owned())
            } else {
                Some("recorded date".to_owned())
            }
        }
        b"IGNR" => Some("genre".to_owned()),
        b"INAM" => Some("title".to_owned()),
        b"IPRD" => Some("album".to_owned()),
        b"ISFT" => Some("encoding application".to_owned()),
        b"ITRK" => Some("track number".to_owned()),
        _ => None,
    }
}

fn write_chunk<W: Write>(writer: &mut W, chunk_id: [u8; 4], payload: &[u8]) -> Result<()> {
    let payload_len = i64::try_from(payload.len())
        .map_err(|_| Error::UnsupportedWav("CAF chunk exceeds signed size range".into()))?;
    writer.write_all(&chunk_id)?;
    writer.write_all(&payload_len.to_be_bytes())?;
    writer.write_all(payload)?;
    Ok(())
}

fn write_sample_bytes<W: Write>(
    writer: &mut W,
    samples: &[i32],
    envelope: PcmEnvelope,
) -> Result<()> {
    for &sample in samples {
        let mut buffer = Vec::with_capacity(4);
        append_caf_encoded_sample(&mut buffer, sample, envelope)?;
        writer.write_all(&buffer)?;
    }
    Ok(())
}

fn append_caf_encoded_sample(
    buffer: &mut Vec<u8>,
    sample: i32,
    envelope: PcmEnvelope,
) -> Result<()> {
    let shift = envelope
        .container_bits_per_sample
        .checked_sub(envelope.valid_bits_per_sample)
        .ok_or(Error::InvalidWav(
            "valid bits cannot exceed container bits for encoding",
        ))? as u32;
    let sample = sample.checked_shl(shift).ok_or_else(|| {
        Error::UnsupportedWav(format!(
            "unsupported valid bits/container bits combination: {}/{}",
            envelope.valid_bits_per_sample, envelope.container_bits_per_sample
        ))
    })?;

    match envelope.container_bits_per_sample {
        8 => {
            let value = i8::try_from(sample)
                .map_err(|_| Error::UnsupportedWav("8-bit sample is out of range".into()))?;
            buffer.push(value as u8);
            Ok(())
        }
        16 => {
            let value = i16::try_from(sample)
                .map_err(|_| Error::UnsupportedWav("16-bit sample is out of range".into()))?;
            buffer.extend_from_slice(&value.to_le_bytes());
            Ok(())
        }
        24 => {
            if !(-8_388_608..=8_388_607).contains(&sample) {
                return Err(Error::UnsupportedWav(
                    "24-bit sample is out of range".into(),
                ));
            }
            buffer.extend_from_slice(&(sample as u32).to_le_bytes()[..3]);
            Ok(())
        }
        32 => {
            buffer.extend_from_slice(&sample.to_le_bytes());
            Ok(())
        }
        _ => Err(Error::UnsupportedWav(format!(
            "unsupported container bits/sample for encoder: {}",
            envelope.container_bits_per_sample
        ))),
    }
}

#[cfg(test)]
mod tests {
    use crate::{input::ordinary_channel_mask, metadata::WavMetadata};

    use super::{
        CAF_CHAN_CHUNK_ID, CAF_DATA_CHUNK_ID, CAF_DESC_CHUNK_ID, CAF_INFO_CHUNK_ID,
        CAF_LAYOUT_TAG_USE_CHANNEL_BITMAP, CAF_MARK_CHUNK_ID, write_caf,
    };

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

    fn cuesheet_payload(track_offsets: &[u64], lead_out_offset: u64) -> Vec<u8> {
        let mut payload = vec![0u8; 128];
        payload.extend_from_slice(&0u64.to_be_bytes());
        payload.push(0);
        payload.extend_from_slice(&[0u8; 258]);
        payload.push((track_offsets.len() + 1) as u8);
        for (index, &offset) in track_offsets.iter().enumerate() {
            payload.extend_from_slice(&offset.to_be_bytes());
            payload.push((index + 1) as u8);
            payload.extend_from_slice(&[0u8; 12]);
            payload.push(0);
            payload.extend_from_slice(&[0u8; 13]);
            payload.push(1);
            payload.extend_from_slice(&0u64.to_be_bytes());
            payload.push(1);
            payload.extend_from_slice(&[0u8; 3]);
        }
        payload.extend_from_slice(&lead_out_offset.to_be_bytes());
        payload.push(170);
        payload.extend_from_slice(&[0u8; 12]);
        payload.push(0);
        payload.extend_from_slice(&[0u8; 13]);
        payload.push(0);
        payload
    }

    fn parse_caf_chunks(bytes: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        assert_eq!(&bytes[..4], b"caff");
        assert_eq!(u16::from_be_bytes(bytes[4..6].try_into().unwrap()), 1);
        let mut cursor = 8usize;
        let mut chunks = Vec::new();
        while cursor < bytes.len() {
            let chunk_id: [u8; 4] = bytes[cursor..cursor + 4].try_into().unwrap();
            let size = i64::from_be_bytes(bytes[cursor + 4..cursor + 12].try_into().unwrap());
            assert!(size >= 0);
            cursor += 12;
            let size = size as usize;
            chunks.push((chunk_id, bytes[cursor..cursor + size].to_vec()));
            cursor += size;
        }
        chunks
    }

    fn parse_info_entries(payload: &[u8]) -> Vec<(String, String)> {
        let mut cursor = 0usize;
        let count = u32::from_be_bytes(payload[cursor..cursor + 4].try_into().unwrap()) as usize;
        cursor += 4;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let key_end = payload[cursor..]
                .iter()
                .position(|&byte| byte == 0)
                .map(|offset| cursor + offset)
                .unwrap();
            let key = String::from_utf8(payload[cursor..key_end].to_vec()).unwrap();
            cursor = key_end + 1;
            let value_end = payload[cursor..]
                .iter()
                .position(|&byte| byte == 0)
                .map(|offset| cursor + offset)
                .unwrap();
            let value = String::from_utf8(payload[cursor..value_end].to_vec()).unwrap();
            cursor = value_end + 1;
            entries.push((key, value));
        }
        entries
    }

    #[test]
    fn writes_minimal_caf_pcm_with_md5() {
        let spec = super::WavSpec {
            sample_rate: 44_100,
            channels: 2,
            bits_per_sample: 16,
            total_samples: 2,
            bytes_per_sample: 2,
            channel_mask: ordinary_channel_mask(2u16).unwrap(),
        };
        let samples = [1, -2, 3, -4];
        let mut caf = Vec::new();

        let md5 = write_caf(&mut caf, spec, &samples, &WavMetadata::default()).unwrap();
        let chunks = parse_caf_chunks(&caf);

        assert_eq!(
            chunks.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![CAF_DESC_CHUNK_ID, CAF_DATA_CHUNK_ID]
        );
        assert_eq!(md5.len(), 16);
        assert_eq!(
            u32::from_be_bytes(chunks[0].1[16..20].try_into().unwrap()),
            4
        );
        assert_eq!(
            u32::from_be_bytes(chunks[0].1[24..28].try_into().unwrap()),
            2
        );
        assert_eq!(chunks[1].1[..4], [0, 0, 0, 0]);
        assert_eq!(chunks[1].1[4..], [1, 0, 254, 255, 3, 0, 252, 255]);
    }

    #[test]
    fn writes_signed_8bit_pcm_without_wav_bias() {
        let spec = super::WavSpec {
            sample_rate: 48_000,
            channels: 1,
            bits_per_sample: 8,
            total_samples: 3,
            bytes_per_sample: 1,
            channel_mask: ordinary_channel_mask(1u16).unwrap(),
        };
        let samples = [-128, 0, 127];
        let mut caf = Vec::new();

        write_caf(&mut caf, spec, &samples, &WavMetadata::default()).unwrap();
        let chunks = parse_caf_chunks(&caf);

        assert_eq!(chunks[1].1[..4], [0, 0, 0, 0]);
        assert_eq!(chunks[1].1[4..], [0x80, 0x00, 0x7f]);
    }

    #[test]
    fn projects_info_and_mark_metadata_without_strings_dependency() {
        let spec = super::WavSpec {
            sample_rate: 44_100,
            channels: 2,
            bits_per_sample: 16,
            total_samples: 4,
            bytes_per_sample: 2,
            channel_mask: ordinary_channel_mask(2u16).unwrap(),
        };
        let samples = [1, -2, 3, -4, 5, -6, 7, -8];
        let mut metadata = WavMetadata::default();
        metadata
            .ingest_flac_metadata_block(
                4,
                &vorbis_comment_payload(&[
                    ("TITLE", "Example Title"),
                    ("ARTIST", "Example Artist"),
                    ("COMMENT", "One"),
                    ("COMMENT", "Two"),
                ]),
                4,
                2,
            )
            .unwrap();
        metadata
            .ingest_flac_metadata_block(5, &cuesheet_payload(&[1, 3], 4), 4, 2)
            .unwrap();

        let mut caf = Vec::new();
        write_caf(&mut caf, spec, &samples, &metadata).unwrap();
        let chunks = parse_caf_chunks(&caf);

        assert_eq!(
            chunks.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![
                CAF_DESC_CHUNK_ID,
                CAF_INFO_CHUNK_ID,
                CAF_MARK_CHUNK_ID,
                CAF_DATA_CHUNK_ID
            ]
        );
        let info_entries = parse_info_entries(&chunks[1].1);
        assert!(info_entries.contains(&(String::from("title"), String::from("Example Title"))));
        assert!(info_entries.contains(&(String::from("artist"), String::from("Example Artist"))));
        assert!(info_entries.contains(&(String::from("comments"), String::from("One,Two"))));
        assert_eq!(u32::from_be_bytes(chunks[2].1[0..4].try_into().unwrap()), 2);
        assert_eq!(
            f64::from_be_bytes(chunks[2].1[8..16].try_into().unwrap()),
            1.0
        );
        assert_eq!(
            f64::from_be_bytes(chunks[2].1[36..44].try_into().unwrap()),
            3.0
        );
    }

    #[test]
    fn emits_channel_layout_for_non_ordinary_masks() {
        let spec = super::WavSpec {
            sample_rate: 48_000,
            channels: 4,
            bits_per_sample: 16,
            total_samples: 2,
            bytes_per_sample: 2,
            channel_mask: 0x0001_2104,
        };
        let samples = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut caf = Vec::new();

        write_caf(&mut caf, spec, &samples, &WavMetadata::default()).unwrap();
        let chunks = parse_caf_chunks(&caf);

        assert_eq!(
            chunks.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![CAF_DESC_CHUNK_ID, CAF_CHAN_CHUNK_ID, CAF_DATA_CHUNK_ID]
        );
        assert_eq!(
            u32::from_be_bytes(chunks[1].1[0..4].try_into().unwrap()),
            CAF_LAYOUT_TAG_USE_CHANNEL_BITMAP
        );
        assert_eq!(
            u32::from_be_bytes(chunks[1].1[4..8].try_into().unwrap()),
            0x0001_2104
        );
        assert_eq!(
            u32::from_be_bytes(chunks[1].1[8..12].try_into().unwrap()),
            0
        );
    }

    #[test]
    fn rejects_unsupported_channel_layouts() {
        let spec = super::WavSpec {
            sample_rate: 48_000,
            channels: 4,
            bits_per_sample: 16,
            total_samples: 2,
            bytes_per_sample: 2,
            channel_mask: 0x0000_0003,
        };
        let samples = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut caf = Vec::new();

        let error = write_caf(&mut caf, spec, &samples, &WavMetadata::default()).unwrap_err();
        assert!(error.to_string().contains("channel mask"));
    }
}
