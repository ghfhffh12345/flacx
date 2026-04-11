use std::io::Write;

use crate::{
    error::{Error, Result},
    input::{WavSpec, ordinary_channel_mask},
    md5::streaminfo_md5,
    metadata::WavMetadata,
    pcm::{PcmEnvelope, container_bits_from_valid_bits},
};

const FORM_ID: [u8; 4] = *b"FORM";
const AIFF_FORM_TYPE: [u8; 4] = *b"AIFF";
const AIFC_FORM_TYPE: [u8; 4] = *b"AIFC";
const COMM_CHUNK_ID: [u8; 4] = *b"COMM";
const FVER_CHUNK_ID: [u8; 4] = *b"FVER";
const MARK_CHUNK_ID: [u8; 4] = *b"MARK";
const SSND_CHUNK_ID: [u8; 4] = *b"SSND";
const AIFF_NAME_CHUNK_ID: [u8; 4] = *b"NAME";
const AIFF_AUTH_CHUNK_ID: [u8; 4] = *b"AUTH";
const AIFF_COPYRIGHT_CHUNK_ID: [u8; 4] = *b"(c) ";
const AIFF_ANNOTATION_CHUNK_ID: [u8; 4] = *b"ANNO";
const AIFC_NONE_COMPRESSION_TYPE: [u8; 4] = *b"NONE";
const AIFC_NONE_COMPRESSION_NAME: &[u8] = b"not compressed";
const AIFC_FVER_VERSION: u32 = 0xA280_5140;

/// Canonical AIFF/AIFC output families supported by the Stage 4 slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum AiffContainer {
    #[default]
    Auto,
    Aiff,
    AifcNone,
}

#[allow(dead_code)]
pub(crate) fn write_aiff_with_metadata<W: Write>(
    writer: &mut W,
    spec: WavSpec,
    samples: &[i32],
    metadata: &WavMetadata,
    container: AiffContainer,
) -> Result<()> {
    write_aiff_with_metadata_and_md5(writer, spec, samples, metadata, container).map(|_| ())
}

pub(crate) fn write_aiff_with_metadata_and_md5<W: Write>(
    writer: &mut W,
    spec: WavSpec,
    samples: &[i32],
    metadata: &WavMetadata,
    container: AiffContainer,
) -> Result<[u8; 16]> {
    let envelope = validate_aiff_output_shape(spec, samples)?;
    let container = resolve_container(container);
    let streaminfo_md5 = streaminfo_md5(spec, samples)?;
    let mut chunks = Vec::<([u8; 4], Vec<u8>)>::new();

    if matches!(container, AiffContainer::AifcNone) {
        chunks.push((FVER_CHUNK_ID, AIFC_FVER_VERSION.to_be_bytes().to_vec()));
    }
    chunks.push((COMM_CHUNK_ID, comm_chunk_payload(spec, envelope, container)));
    chunks.extend(aiff_metadata_chunks(metadata));
    if let Some(payload) = mark_chunk_payload(metadata) {
        chunks.push((MARK_CHUNK_ID, payload));
    }

    let data_bytes = encoded_sample_bytes_len(samples, envelope)?;
    let ssnd_payload_len = ssnd_chunk_payload_len(data_bytes);

    let form_type = match container {
        AiffContainer::Aiff | AiffContainer::Auto => AIFF_FORM_TYPE,
        AiffContainer::AifcNone => AIFC_FORM_TYPE,
    };
    let form_size = aiff_form_size(&chunks, ssnd_payload_len)?;
    write_aiff_header(writer, form_type, form_size)?;
    for (chunk_id, payload) in &chunks {
        write_aiff_chunk(writer, chunk_id, payload)?;
    }
    write_aiff_ssnd_chunk(writer, samples, envelope, ssnd_payload_len)?;
    Ok(streaminfo_md5)
}

pub(crate) struct AiffStreamWriter<W: Write> {
    writer: W,
    envelope: PcmEnvelope,
    pad_final: bool,
}

impl<W: Write> AiffStreamWriter<W> {
    pub(crate) fn new(
        mut writer: W,
        spec: WavSpec,
        metadata: &WavMetadata,
        container: AiffContainer,
    ) -> Result<Self> {
        let envelope = validate_aiff_output_spec(spec)?;
        let container = resolve_container(container);
        let mut chunks = Vec::<([u8; 4], Vec<u8>)>::new();

        if matches!(container, AiffContainer::AifcNone) {
            chunks.push((FVER_CHUNK_ID, AIFC_FVER_VERSION.to_be_bytes().to_vec()));
        }
        chunks.push((COMM_CHUNK_ID, comm_chunk_payload(spec, envelope, container)));
        chunks.extend(aiff_metadata_chunks(metadata));
        if let Some(payload) = mark_chunk_payload(metadata) {
            chunks.push((MARK_CHUNK_ID, payload));
        }

        let frame_bytes =
            u64::from(envelope.channels) * u64::from(envelope.container_bits_per_sample / 8);
        let data_bytes = spec.total_samples.checked_mul(frame_bytes).ok_or_else(|| {
            Error::UnsupportedWav("AIFF/AIFC data section exceeds addressable range".into())
        })?;
        let ssnd_payload_len = ssnd_chunk_payload_len(data_bytes);
        let form_type = match container {
            AiffContainer::Aiff | AiffContainer::Auto => AIFF_FORM_TYPE,
            AiffContainer::AifcNone => AIFC_FORM_TYPE,
        };
        let form_size = aiff_form_size(&chunks, ssnd_payload_len)?;
        write_aiff_header(&mut writer, form_type, form_size)?;
        for (chunk_id, payload) in &chunks {
            write_aiff_chunk(&mut writer, chunk_id, payload)?;
        }
        writer.write_all(&SSND_CHUNK_ID)?;
        writer.write_all(
            &u32::try_from(ssnd_payload_len)
                .map_err(|_| Error::UnsupportedWav("AIFF SSND chunk exceeds 4 GiB".into()))?
                .to_be_bytes(),
        )?;
        writer.write_all(&0u32.to_be_bytes())?;
        writer.write_all(&0u32.to_be_bytes())?;

        Ok(Self {
            writer,
            envelope,
            pad_final: !ssnd_payload_len.is_multiple_of(2),
        })
    }

    pub(crate) fn write_samples(&mut self, samples: &[i32]) -> Result<()> {
        for &sample in samples {
            let mut buffer = Vec::with_capacity(4);
            append_aiff_encoded_sample(&mut buffer, sample, self.envelope)?;
            self.writer.write_all(&buffer)?;
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<W> {
        if self.pad_final {
            self.writer.write_all(&[0])?;
        }
        self.writer.flush()?;
        Ok(self.writer)
    }
}

fn resolve_container(container: AiffContainer) -> AiffContainer {
    match container {
        AiffContainer::Auto => AiffContainer::Aiff,
        other => other,
    }
}

fn validate_aiff_output_shape(spec: WavSpec, samples: &[i32]) -> Result<PcmEnvelope> {
    let envelope = validate_aiff_output_spec(spec)?;

    if !samples.len().is_multiple_of(usize::from(spec.channels)) {
        return Err(Error::Decode(
            "decoded samples are not aligned to the channel count".into(),
        ));
    }

    let frames = samples.len() / usize::from(spec.channels);
    if frames as u64 != spec.total_samples {
        return Err(Error::Decode(
            "decoded samples do not match the declared total sample count".into(),
        ));
    }

    Ok(envelope)
}

fn validate_aiff_output_spec(spec: WavSpec) -> Result<PcmEnvelope> {
    if !(1..=8).contains(&spec.channels) {
        return Err(Error::UnsupportedWav(format!(
            "AIFF/AIFC output only supports ordinary 1..8 channel PCM, found {} channels",
            spec.channels
        )));
    }

    let ordinary_mask = ordinary_channel_mask(u16::from(spec.channels)).ok_or_else(|| {
        Error::UnsupportedWav(format!(
            "no ordinary channel mask exists for {} channels",
            spec.channels
        ))
    })?;
    if spec.channel_mask != ordinary_mask {
        return Err(Error::UnsupportedWav(format!(
            "AIFF/AIFC output cannot preserve non-ordinary channel mask {:#010x} for {} channels",
            spec.channel_mask, spec.channels
        )));
    }

    if !(4..=32).contains(&spec.bits_per_sample) {
        return Err(Error::UnsupportedWav(format!(
            "only FLAC-native 4..32 valid bits/sample are supported, found {}",
            spec.bits_per_sample
        )));
    }

    let container_bits_per_sample = container_bits_from_valid_bits(u16::from(spec.bits_per_sample));
    if !matches!(container_bits_per_sample, 8 | 16 | 24 | 32) {
        return Err(Error::UnsupportedWav(format!(
            "AIFF/AIFC output only supports byte-aligned PCM containers, found {} bits/sample",
            container_bits_per_sample
        )));
    }
    if spec.bytes_per_sample * 8 != container_bits_per_sample {
        return Err(Error::UnsupportedWav(format!(
            "bytes/sample does not match the chosen container width for {} valid bits/sample",
            spec.bits_per_sample
        )));
    }
    if spec.total_samples > u64::from(u32::MAX) {
        return Err(Error::UnsupportedWav(
            "AIFF/AIFC output only supports up to 4,294,967,295 sample frames".into(),
        ));
    }

    Ok(PcmEnvelope {
        channels: u16::from(spec.channels),
        valid_bits_per_sample: u16::from(spec.bits_per_sample),
        container_bits_per_sample,
        channel_mask: spec.channel_mask,
    })
}

fn comm_chunk_payload(spec: WavSpec, envelope: PcmEnvelope, container: AiffContainer) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&u16::from(spec.channels).to_be_bytes());
    payload.extend_from_slice(
        &u32::try_from(spec.total_samples)
            .expect("validated total sample count")
            .to_be_bytes(),
    );
    payload.extend_from_slice(&u32::from(envelope.valid_bits_per_sample).to_be_bytes()[2..]);
    payload.extend_from_slice(&extended_float_bytes(spec.sample_rate));
    if matches!(container, AiffContainer::AifcNone) {
        payload.extend_from_slice(&AIFC_NONE_COMPRESSION_TYPE);
        payload.extend_from_slice(&pascal_string(AIFC_NONE_COMPRESSION_NAME));
    }
    payload
}

fn aiff_metadata_chunks(metadata: &WavMetadata) -> Vec<([u8; 4], Vec<u8>)> {
    let mut chunks = Vec::new();
    let Some(payload) = metadata.list_info_chunk_payload() else {
        return chunks;
    };

    let mut projection = AiffTextProjection::default();
    if let Some(entries) = parse_wav_info_chunk_payload(&payload) {
        for entry in entries {
            projection.ingest(entry);
        }
    }
    projection.into_chunks(&mut chunks);
    chunks
}

fn mark_chunk_payload(metadata: &WavMetadata) -> Option<Vec<u8>> {
    let payload = metadata.cue_chunk_payload()?;
    let cue_points = parse_wav_cue_chunk_payload(&payload)?;
    if cue_points.len() > usize::from(u16::MAX) {
        return None;
    }

    let mut mark = Vec::new();
    mark.extend_from_slice(&(cue_points.len() as u16).to_be_bytes());
    for (index, sample_offset) in cue_points.iter().enumerate() {
        mark.extend_from_slice(&((index + 1) as u16).to_be_bytes());
        mark.extend_from_slice(&sample_offset.to_be_bytes());
        mark.extend_from_slice(&pascal_string(b""));
    }
    Some(mark)
}

fn parse_wav_info_chunk_payload(payload: &[u8]) -> Option<Vec<WavInfoEntry>> {
    if payload.len() < 4 || &payload[..4] != b"INFO" {
        return None;
    }

    let mut entries = Vec::new();
    let mut cursor = 4usize;
    while cursor + 8 <= payload.len() {
        let chunk_id = payload[cursor..cursor + 4].try_into().ok()?;
        let length = u32::from_le_bytes(payload[cursor + 4..cursor + 8].try_into().ok()?) as usize;
        cursor += 8;
        let end = cursor.checked_add(length)?;
        if end > payload.len() {
            return None;
        }
        let value = payload[cursor..end].to_vec();
        entries.push(WavInfoEntry { chunk_id, value });
        cursor = end;
        if !length.is_multiple_of(2) {
            cursor = cursor.checked_add(1)?;
        }
    }

    Some(entries)
}

fn parse_wav_cue_chunk_payload(payload: &[u8]) -> Option<Vec<u32>> {
    if payload.len() < 4 {
        return None;
    }

    let count = u32::from_le_bytes(payload[..4].try_into().ok()?) as usize;
    let mut cursor = 4usize;
    let mut cue_points = Vec::with_capacity(count);
    for _ in 0..count {
        if cursor + 24 > payload.len() {
            return None;
        }
        cursor += 4; // cue id
        cursor += 4; // cue position
        if payload[cursor..cursor + 4] != *b"data" {
            return None;
        }
        cursor += 4;
        cursor += 4; // chunk start
        cursor += 4; // block start
        let sample_offset = u32::from_le_bytes(payload[cursor..cursor + 4].try_into().ok()?);
        cursor += 4;
        cue_points.push(sample_offset);
    }

    Some(cue_points)
}

fn aiff_form_size(chunks: &[([u8; 4], Vec<u8>)], ssnd_payload_len: u64) -> Result<u32> {
    let mut total = 4u64;
    for (_, payload) in chunks {
        total = total
            .checked_add(aiff_chunk_serialized_size(payload.len()))
            .ok_or_else(|| {
                Error::UnsupportedWav("AIFF output size exceeds supported range".into())
            })?;
    }
    total = total
        .checked_add(aiff_chunk_serialized_size(
            usize::try_from(ssnd_payload_len).map_err(|_| {
                Error::UnsupportedWav("AIFF output size exceeds supported range".into())
            })?,
        ))
        .ok_or_else(|| Error::UnsupportedWav("AIFF output size exceeds supported range".into()))?;
    u32::try_from(total).map_err(|_| Error::UnsupportedWav("AIFF output exceeds 4 GiB".into()))
}

fn aiff_chunk_serialized_size(payload_len: usize) -> u64 {
    let payload_len = payload_len as u64;
    8 + payload_len + (payload_len % 2)
}

fn encoded_sample_bytes_len(samples: &[i32], envelope: PcmEnvelope) -> Result<u64> {
    let bytes_per_sample = u64::from(envelope.container_bits_per_sample / 8);
    samples
        .len()
        .checked_mul(bytes_per_sample as usize)
        .map(|len| len as u64)
        .ok_or_else(|| Error::UnsupportedWav("AIFF audio payload exceeds supported range".into()))
}

fn ssnd_chunk_payload_len(data_bytes: u64) -> u64 {
    8 + data_bytes
}

fn write_aiff_header<W: Write>(writer: &mut W, form_type: [u8; 4], form_size: u32) -> Result<()> {
    writer.write_all(&FORM_ID)?;
    writer.write_all(&form_size.to_be_bytes())?;
    writer.write_all(&form_type)?;
    Ok(())
}

fn write_aiff_chunk<W: Write>(writer: &mut W, chunk_id: &[u8; 4], payload: &[u8]) -> Result<()> {
    writer.write_all(chunk_id)?;
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(payload)?;
    if !payload.len().is_multiple_of(2) {
        writer.write_all(&[0])?;
    }
    Ok(())
}

fn write_aiff_ssnd_chunk<W: Write>(
    writer: &mut W,
    samples: &[i32],
    envelope: PcmEnvelope,
    ssnd_payload_len: u64,
) -> Result<()> {
    writer.write_all(&SSND_CHUNK_ID)?;
    writer.write_all(
        &u32::try_from(ssnd_payload_len)
            .map_err(|_| Error::UnsupportedWav("AIFF SSND chunk exceeds 4 GiB".into()))?
            .to_be_bytes(),
    )?;
    writer.write_all(&0u32.to_be_bytes())?;
    writer.write_all(&0u32.to_be_bytes())?;
    for &sample in samples {
        let mut buffer = Vec::with_capacity(4);
        append_aiff_encoded_sample(&mut buffer, sample, envelope)?;
        writer.write_all(&buffer)?;
    }
    if !ssnd_payload_len.is_multiple_of(2) {
        writer.write_all(&[0])?;
    }
    Ok(())
}

fn append_aiff_encoded_sample(
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
            buffer.extend_from_slice(&value.to_be_bytes());
            Ok(())
        }
        24 => {
            if !(-8_388_608..=8_388_607).contains(&sample) {
                return Err(Error::UnsupportedWav(
                    "24-bit sample is out of range".into(),
                ));
            }
            buffer.extend_from_slice(&sample.to_be_bytes()[1..]);
            Ok(())
        }
        32 => {
            buffer.extend_from_slice(&sample.to_be_bytes());
            Ok(())
        }
        _ => Err(Error::UnsupportedWav(format!(
            "unsupported container bits/sample for encoder: {}",
            envelope.container_bits_per_sample
        ))),
    }
}

fn extended_float_bytes(sample_rate: u32) -> [u8; 10] {
    if sample_rate == 0 {
        return [0; 10];
    }

    let exponent = 16_383u16 + (u32::BITS - 1 - sample_rate.leading_zeros()) as u16;
    let shift = sample_rate.leading_zeros() + 32;
    let mantissa = u64::from(sample_rate) << shift;
    let mut bytes = [0u8; 10];
    bytes[..2].copy_from_slice(&exponent.to_be_bytes());
    bytes[2..].copy_from_slice(&mantissa.to_be_bytes());
    bytes
}

fn pascal_string(bytes: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(bytes.len() + 2);
    payload.push(bytes.len() as u8);
    payload.extend_from_slice(bytes);
    if payload.len().is_multiple_of(2) {
        return payload;
    }
    payload.push(0);
    payload
}

fn aiff_text_chunk_id_for_wav_info_chunk(chunk_id: [u8; 4]) -> Option<[u8; 4]> {
    match &chunk_id {
        b"INAM" => Some(AIFF_NAME_CHUNK_ID),
        b"IART" => Some(AIFF_AUTH_CHUNK_ID),
        b"ICOP" => Some(AIFF_COPYRIGHT_CHUNK_ID),
        b"ICMT" => Some(AIFF_ANNOTATION_CHUNK_ID),
        _ => None,
    }
}

#[derive(Default)]
struct AiffTextProjection {
    name: Option<Vec<u8>>,
    author: Option<Vec<u8>>,
    copyright: Option<Vec<u8>>,
    annotations: Vec<Vec<u8>>,
}

impl AiffTextProjection {
    fn ingest(&mut self, entry: WavInfoEntry) {
        let Some(chunk_id) = aiff_text_chunk_id_for_wav_info_chunk(entry.chunk_id) else {
            return;
        };
        if chunk_id == AIFF_NAME_CHUNK_ID {
            self.name.get_or_insert(entry.value);
        } else if chunk_id == AIFF_AUTH_CHUNK_ID {
            self.author.get_or_insert(entry.value);
        } else if chunk_id == AIFF_COPYRIGHT_CHUNK_ID {
            self.copyright.get_or_insert(entry.value);
        } else if chunk_id == AIFF_ANNOTATION_CHUNK_ID {
            self.annotations.push(entry.value);
        } else {
            unreachable!("bounded AIFF text projection");
        };
    }

    fn into_chunks(self, chunks: &mut Vec<([u8; 4], Vec<u8>)>) {
        if let Some(value) = self.name {
            chunks.push((AIFF_NAME_CHUNK_ID, value));
        }
        if let Some(value) = self.author {
            chunks.push((AIFF_AUTH_CHUNK_ID, value));
        }
        if let Some(value) = self.copyright {
            chunks.push((AIFF_COPYRIGHT_CHUNK_ID, value));
        }
        for value in self.annotations {
            chunks.push((AIFF_ANNOTATION_CHUNK_ID, value));
        }
    }
}

#[derive(Debug)]
struct WavInfoEntry {
    chunk_id: [u8; 4],
    value: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use crate::{
        input::{WavSpec, ordinary_channel_mask},
        metadata::WavMetadata,
    };

    use super::{
        AIFC_FORM_TYPE, AIFC_NONE_COMPRESSION_TYPE, AIFF_ANNOTATION_CHUNK_ID, AIFF_AUTH_CHUNK_ID,
        AIFF_COPYRIGHT_CHUNK_ID, AIFF_FORM_TYPE, AIFF_NAME_CHUNK_ID, AiffContainer,
        aiff_text_chunk_id_for_wav_info_chunk, mark_chunk_payload, pascal_string,
        write_aiff_with_metadata_and_md5,
    };

    fn cue_payload(points: &[u32]) -> Vec<u8> {
        let mut payload = vec![0u8; 128];
        payload.extend_from_slice(&0u64.to_be_bytes());
        payload.push(0);
        payload.extend_from_slice(&[0u8; 258]);
        payload.push((points.len() + 1) as u8);
        for (index, &point) in points.iter().enumerate() {
            payload.extend_from_slice(&u64::from(point).to_be_bytes());
            payload.push((index + 1) as u8);
            payload.extend_from_slice(&[0u8; 12]);
            payload.push(0);
            payload.extend_from_slice(&[0u8; 13]);
            payload.push(0);
        }
        payload
            .extend_from_slice(&(u64::from(points.last().copied().unwrap_or(0)) + 1).to_be_bytes());
        payload.push(170);
        payload.extend_from_slice(&[0u8; 12]);
        payload.push(0);
        payload.extend_from_slice(&[0u8; 13]);
        payload.push(0);
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

    #[test]
    fn maps_wav_info_chunks_to_aiff_text_chunks() {
        assert_eq!(
            aiff_text_chunk_id_for_wav_info_chunk(*b"INAM"),
            Some(AIFF_NAME_CHUNK_ID)
        );
        assert_eq!(
            aiff_text_chunk_id_for_wav_info_chunk(*b"IART"),
            Some(AIFF_AUTH_CHUNK_ID)
        );
        assert_eq!(
            aiff_text_chunk_id_for_wav_info_chunk(*b"ICOP"),
            Some(AIFF_COPYRIGHT_CHUNK_ID)
        );
        assert_eq!(
            aiff_text_chunk_id_for_wav_info_chunk(*b"ICMT"),
            Some(AIFF_ANNOTATION_CHUNK_ID)
        );
    }

    #[test]
    fn pascal_strings_are_even_length() {
        assert_eq!(pascal_string(b""), vec![0, 0]);
        assert_eq!(pascal_string(b"abc"), vec![3, b'a', b'b', b'c']);
        assert_eq!(pascal_string(b"abcd"), vec![4, b'a', b'b', b'c', b'd', 0]);
    }

    #[test]
    fn project_cues_into_mark_chunk() {
        let mut metadata = WavMetadata::default();
        metadata
            .ingest_flac_metadata_block(5, &cue_payload(&[0, 2]), 4, 2)
            .unwrap();

        let payload = mark_chunk_payload(&metadata).expect("mark chunk");
        assert_eq!(u16::from_be_bytes(payload[..2].try_into().unwrap()), 2);
        assert_eq!(u16::from_be_bytes(payload[2..4].try_into().unwrap()), 1);
        assert_eq!(u32::from_be_bytes(payload[4..8].try_into().unwrap()), 0);
        assert_eq!(u16::from_be_bytes(payload[10..12].try_into().unwrap()), 2);
        assert_eq!(u32::from_be_bytes(payload[12..16].try_into().unwrap()), 2);
    }

    #[test]
    fn writes_canonical_aiff_with_text_and_markers() {
        let spec = WavSpec {
            sample_rate: 44_100,
            channels: 2,
            bits_per_sample: 16,
            total_samples: 2,
            bytes_per_sample: 2,
            channel_mask: ordinary_channel_mask(2).unwrap(),
        };
        let samples = [1, -2, 3, -4];
        let mut metadata = WavMetadata::default();
        metadata
            .ingest_flac_metadata_block(
                4,
                &vorbis_comment_payload(&[
                    ("TITLE", "Example"),
                    ("ARTIST", "Artist"),
                    ("COPYRIGHT", "2026"),
                    ("COMMENT", "Comment"),
                ]),
                4,
                2,
            )
            .unwrap();
        metadata
            .ingest_flac_metadata_block(5, &cue_payload(&[1]), 4, 2)
            .unwrap();

        let mut out = Vec::new();
        let md5 = write_aiff_with_metadata_and_md5(
            &mut out,
            spec,
            &samples,
            &metadata,
            AiffContainer::Aiff,
        )
        .unwrap();

        assert_eq!(&out[..4], b"FORM");
        assert_eq!(&out[8..12], &AIFF_FORM_TYPE);
        assert_eq!(md5.len(), 16);
        assert!(out.windows(4).any(|window| window == b"NAME"));
        assert!(out.windows(4).any(|window| window == b"AUTH"));
        assert!(out.windows(4).any(|window| window == b"(c) "));
        assert!(out.windows(4).any(|window| window == b"ANNO"));
        assert!(out.windows(4).any(|window| window == b"MARK"));
        assert!(out.windows(4).any(|window| window == b"SSND"));
    }

    #[test]
    fn writes_canonical_aifc_none_with_fver() {
        let spec = WavSpec {
            sample_rate: 48_000,
            channels: 1,
            bits_per_sample: 24,
            total_samples: 1,
            bytes_per_sample: 3,
            channel_mask: ordinary_channel_mask(1).unwrap(),
        };
        let samples = [0x123456];
        let metadata = WavMetadata::default();
        let mut out = Vec::new();

        write_aiff_with_metadata_and_md5(
            &mut out,
            spec,
            &samples,
            &metadata,
            AiffContainer::AifcNone,
        )
        .unwrap();

        assert_eq!(&out[..4], b"FORM");
        assert_eq!(&out[8..12], &AIFC_FORM_TYPE);
        assert!(out.windows(4).any(|window| window == b"FVER"));
        assert!(out.windows(4).any(|window| window == b"NONE"));
        assert!(
            out.windows(4)
                .any(|window| window == AIFC_NONE_COMPRESSION_TYPE)
        );
    }

    #[test]
    fn accepts_ordinary_multichannel_output_shapes() {
        let spec = WavSpec {
            sample_rate: 48_000,
            channels: 4,
            bits_per_sample: 16,
            total_samples: 2,
            bytes_per_sample: 2,
            channel_mask: ordinary_channel_mask(4).unwrap(),
        };
        let samples = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut out = Vec::new();

        write_aiff_with_metadata_and_md5(
            &mut out,
            spec,
            &samples,
            &WavMetadata::default(),
            AiffContainer::Aiff,
        )
        .unwrap();

        assert_eq!(&out[..4], b"FORM");
        assert_eq!(&out[8..12], &AIFF_FORM_TYPE);
    }
}
