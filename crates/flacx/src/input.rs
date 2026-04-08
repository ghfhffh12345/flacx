use std::{
    io::{Read, Seek, SeekFrom},
    sync::Arc,
    thread,
};

use crate::config::EncoderConfig;
use crate::metadata::{EncodeMetadata, FXMD_CHUNK_ID, FxmdChunkPolicy, MetadataDraft};
use crate::{
    error::{Error, Result},
    md5::digest_bytes,
};

const PCM_SUBFORMAT_GUID: [u8; 16] = [
    0x01, 0x00, 0x00, 0x00, // PCM subformat
    0x00, 0x00, 0x10, 0x00, // GUID data2/data3
    0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71, // GUID data4
];
const EXTENSIBLE_FMT_CHUNK_SIZE: u32 = 40;
const MAX_RFC9639_CHANNEL_MASK: u32 = 0x0003_FFFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavSpec {
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
    pub total_samples: u64,
    pub bytes_per_sample: u16,
    pub channel_mask: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WavData {
    pub spec: WavSpec,
    pub samples: Vec<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EncodeWavData {
    pub(crate) wav: WavData,
    pub(crate) metadata: EncodeMetadata,
    pub(crate) streaminfo_md5: [u8; 16],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FormatChunk {
    format_tag: u16,
    channels: u16,
    sample_rate: u32,
    byte_rate: u32,
    block_align: u16,
    container_bits_per_sample: u16,
    valid_bits_per_sample: u16,
    channel_mask: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PcmEnvelope {
    pub(crate) channels: u16,
    pub(crate) valid_bits_per_sample: u16,
    pub(crate) container_bits_per_sample: u16,
    pub(crate) channel_mask: u32,
}

pub(crate) fn ordinary_channel_mask(channels: u16) -> Option<u32> {
    match channels {
        1 => Some(0x0004),
        2 => Some(0x0003),
        3 => Some(0x0007),
        4 => Some(0x0033),
        5 => Some(0x0037),
        6 => Some(0x003F),
        7 => Some(0x070F),
        8 => Some(0x063F),
        _ => None,
    }
}

#[allow(dead_code)]
pub fn read_wav<R: Read + Seek>(mut reader: R) -> Result<WavData> {
    Ok(read_wav_internal(&mut reader, false, FxmdChunkPolicy::IGNORE)?.wav)
}

/// Inspect a WAV stream and return its total sample count.
///
/// This helper reads only the container metadata needed for sample counts.
/// It is useful for preflight checks and progress planning.
///
/// # Example
///
/// ```no_run
/// use flacx::inspect_wav_total_samples;
/// use std::fs::File;
///
/// let total_samples = inspect_wav_total_samples(File::open("input.wav").unwrap()).unwrap();
/// assert!(total_samples > 0);
/// ```
pub fn inspect_wav_total_samples<R: Read + Seek>(mut reader: R) -> Result<u64> {
    Ok(parse_wav_layout(&mut reader, false, FxmdChunkPolicy::IGNORE)?.total_samples)
}

pub(crate) fn read_wav_for_encode_with_config<R: Read + Seek>(
    mut reader: R,
    config: &EncoderConfig,
) -> Result<EncodeWavData> {
    read_wav_internal(
        &mut reader,
        true,
        FxmdChunkPolicy {
            capture: config.capture_fxmd,
            strict: config.strict_fxmd_validation,
        },
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedWavLayout {
    format: FormatChunk,
    envelope: PcmEnvelope,
    data_offset: u64,
    data_size: u32,
    total_samples: u64,
    metadata: EncodeMetadata,
}

fn parse_wav_layout<R: Read + Seek>(
    reader: &mut R,
    capture_metadata: bool,
    fxmd_policy: FxmdChunkPolicy,
) -> Result<ParsedWavLayout> {
    let mut chunk_id = [0u8; 4];
    reader.read_exact(&mut chunk_id)?;

    if &chunk_id == b"RF64" {
        return Err(Error::UnsupportedWav("RF64 is out of scope for v1".into()));
    }

    if &chunk_id != b"RIFF" {
        return Err(Error::InvalidWav("expected RIFF header"));
    }

    let _riff_size = read_u32_le(reader)?;
    reader.read_exact(&mut chunk_id)?;
    if &chunk_id != b"WAVE" {
        return Err(Error::InvalidWav("expected WAVE signature"));
    }

    let mut format = None;
    let mut data_offset = None;
    let mut data_size = None;
    let mut metadata_draft = MetadataDraft::default();

    loop {
        let mut chunk_header = [0u8; 8];
        match reader.read_exact(&mut chunk_header) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error.into()),
        }

        let chunk_size =
            u32::from_le_bytes(chunk_header[4..8].try_into().expect("fixed chunk header"));
        let chunk_start = reader.stream_position()?;

        let chunk_id: [u8; 4] = chunk_header[..4].try_into().expect("fixed chunk id");
        match &chunk_id {
            b"fmt " => {
                format = Some(read_format_chunk(reader, chunk_size)?);
            }
            b"data" => {
                data_offset = Some(chunk_start);
                data_size = Some(chunk_size);
                reader.seek(SeekFrom::Current(chunk_size as i64))?;
            }
            id if capture_metadata && is_captured_metadata_chunk(*id) => {
                let payload = read_chunk_payload(reader, chunk_size)?;
                metadata_draft.ingest_chunk(chunk_id, &payload, fxmd_policy)?;
            }
            _ => {
                reader.seek(SeekFrom::Current(chunk_size as i64))?;
            }
        }

        if chunk_size % 2 != 0 {
            reader.seek(SeekFrom::Current(1))?;
        }
    }

    let format = format.ok_or(Error::InvalidWav("missing fmt chunk"))?;
    let data_offset = data_offset.ok_or(Error::InvalidWav("missing data chunk"))?;
    let data_size = data_size.ok_or(Error::InvalidWav("missing data size"))?;

    let envelope = validate_format(format)?;

    let expected_block_align = envelope.channels * (envelope.container_bits_per_sample / 8);
    if format.block_align != expected_block_align {
        return Err(Error::InvalidWav(
            "fmt block alignment does not match channels * bytes/sample",
        ));
    }

    let block_align = u32::from(format.block_align);
    if block_align == 0 {
        return Err(Error::InvalidWav("fmt block alignment must be non-zero"));
    }

    if data_size % block_align != 0 {
        return Err(Error::InvalidWav(
            "data chunk is not aligned to the sample frame size",
        ));
    }

    let total_samples = u64::from(data_size / u32::from(format.block_align));

    Ok(ParsedWavLayout {
        format,
        envelope,
        data_offset,
        data_size,
        total_samples,
        metadata: if capture_metadata {
            metadata_draft.finish(total_samples)
        } else {
            EncodeMetadata::default()
        },
    })
}

fn read_wav_internal<R: Read + Seek>(
    reader: &mut R,
    capture_metadata: bool,
    fxmd_policy: FxmdChunkPolicy,
) -> Result<EncodeWavData> {
    let layout = parse_wav_layout(reader, capture_metadata, fxmd_policy)?;
    reader.seek(SeekFrom::Start(layout.data_offset))?;
    let mut data = vec![0u8; layout.data_size as usize];
    reader.read_exact(&mut data)?;
    let data: Arc<[u8]> = Arc::from(data);
    let md5_input = Arc::clone(&data);
    let md5_worker = thread::spawn(move || digest_bytes(&md5_input));
    let samples = decode_samples(&data, layout.envelope)?;
    let streaminfo_md5 = md5_worker
        .join()
        .map_err(|_| Error::Thread("streaminfo md5 worker panicked".into()))?;

    let wav = WavData {
        spec: WavSpec {
            sample_rate: layout.format.sample_rate,
            channels: layout.format.channels as u8,
            bits_per_sample: layout.envelope.valid_bits_per_sample as u8,
            total_samples: layout.total_samples,
            bytes_per_sample: layout.envelope.container_bits_per_sample / 8,
            channel_mask: layout.envelope.channel_mask,
        },
        samples,
    };
    let mut metadata = layout.metadata;
    if should_preserve_channel_mask(layout.format.channels, layout.envelope.channel_mask) {
        metadata.set_channel_mask(layout.format.channels, layout.envelope.channel_mask);
    }

    Ok(EncodeWavData {
        wav,
        metadata,
        streaminfo_md5,
    })
}

fn read_format_chunk<R: Read>(reader: &mut R, chunk_size: u32) -> Result<FormatChunk> {
    if chunk_size < 16 {
        return Err(Error::InvalidWav("fmt chunk is too short"));
    }

    let format_tag = read_u16_le(reader)?;
    let channels = read_u16_le(reader)?;
    let sample_rate = read_u32_le(reader)?;
    let byte_rate = read_u32_le(reader)?;
    let block_align = read_u16_le(reader)?;
    let container_bits_per_sample = read_u16_le(reader)?;

    if format_tag == 0xFFFE {
        if chunk_size < EXTENSIBLE_FMT_CHUNK_SIZE {
            return Err(Error::InvalidWav(
                "WAVEFORMATEXTENSIBLE fmt chunk is too short",
            ));
        }

        let cb_size = read_u16_le(reader)?;
        if cb_size < 22 {
            return Err(Error::InvalidWav(
                "WAVEFORMATEXTENSIBLE extension is too short",
            ));
        }
        let valid_bits_per_sample = read_u16_le(reader)?;
        let channel_mask = read_u32_le(reader)?;
        let mut subformat = [0u8; 16];
        reader.read_exact(&mut subformat)?;
        if subformat != PCM_SUBFORMAT_GUID {
            return Err(Error::UnsupportedWav(
                "only WAVEFORMATEXTENSIBLE PCM subformat is supported".into(),
            ));
        }

        let extra_bytes = usize::from(cb_size.saturating_sub(22))
            + (chunk_size as usize).saturating_sub(EXTENSIBLE_FMT_CHUNK_SIZE as usize);
        if extra_bytes > 0 {
            let mut discard = vec![0u8; extra_bytes];
            reader.read_exact(&mut discard)?;
        }

        Ok(FormatChunk {
            format_tag,
            channels,
            sample_rate,
            byte_rate,
            block_align,
            container_bits_per_sample,
            valid_bits_per_sample,
            channel_mask,
        })
    } else {
        let mut discard = vec![0u8; (chunk_size - 16) as usize];
        reader.read_exact(&mut discard)?;
        Ok(FormatChunk {
            format_tag,
            channels,
            sample_rate,
            byte_rate,
            block_align,
            container_bits_per_sample,
            valid_bits_per_sample: container_bits_per_sample,
            channel_mask: ordinary_channel_mask(channels).unwrap_or(0),
        })
    }
}

fn validate_format(format: FormatChunk) -> Result<PcmEnvelope> {
    if format.sample_rate == 0 {
        return Err(Error::UnsupportedWav("sample rate 0 is not allowed".into()));
    }

    let container_bits_per_sample = format.container_bits_per_sample;
    if !matches!(container_bits_per_sample, 8 | 16 | 24 | 32) {
        return Err(Error::UnsupportedWav(format!(
            "only byte-aligned PCM containers are supported, found {container_bits_per_sample} bits/sample"
        )));
    }

    let envelope = if format.format_tag == 1 {
        if !(1..=2).contains(&format.channels) {
            return Err(Error::UnsupportedWav(format!(
                "canonical PCM tag 1 only supports exact mono/stereo (1..2 channel) cases, found {} channels",
                format.channels
            )));
        }
        if format.valid_bits_per_sample != container_bits_per_sample {
            return Err(Error::UnsupportedWav(
                "canonical PCM requires valid bits to match container bits".into(),
            ));
        }

        PcmEnvelope {
            channels: format.channels,
            valid_bits_per_sample: format.valid_bits_per_sample,
            container_bits_per_sample,
            channel_mask: ordinary_channel_mask(format.channels).ok_or_else(|| {
                Error::UnsupportedWav(format!(
                    "no ordinary mask exists for {} channels",
                    format.channels
                ))
            })?,
        }
    } else if format.format_tag == 0xFFFE {
        if !(1..=8).contains(&format.channels) {
            return Err(Error::UnsupportedWav(format!(
                "WAVEFORMATEXTENSIBLE input only supports 1..8 channel layouts, found {} channels",
                format.channels
            )));
        }
        if format.valid_bits_per_sample < 4 || format.valid_bits_per_sample > 32 {
            return Err(Error::UnsupportedWav(format!(
                "valid bits must be in the FLAC-native 4..32 range, found {}",
                format.valid_bits_per_sample
            )));
        }
        if format.valid_bits_per_sample > container_bits_per_sample {
            return Err(Error::UnsupportedWav(format!(
                "valid bits cannot exceed container bits ({} > {})",
                format.valid_bits_per_sample, container_bits_per_sample
            )));
        }

        if !is_supported_channel_mask(format.channels, format.channel_mask) {
            return Err(Error::UnsupportedWav(format!(
                "channel mask {:#010x} is not supported for {} channels",
                format.channel_mask, format.channels
            )));
        }

        PcmEnvelope {
            channels: format.channels,
            valid_bits_per_sample: format.valid_bits_per_sample,
            container_bits_per_sample,
            channel_mask: format.channel_mask,
        }
    } else {
        return Err(Error::UnsupportedWav(format!(
            "only PCM format tag 1 and WAVEFORMATEXTENSIBLE PCM are supported, found {}",
            format.format_tag
        )));
    };

    let expected_byte_rate =
        format.sample_rate * u32::from(format.channels) * u32::from(container_bits_per_sample / 8);
    if format.byte_rate != expected_byte_rate {
        return Err(Error::InvalidWav(
            "fmt byte rate does not match the PCM payload shape",
        ));
    }

    Ok(envelope)
}

fn should_preserve_channel_mask(channels: u16, mask: u32) -> bool {
    ordinary_channel_mask(channels) != Some(mask)
}

fn is_captured_metadata_chunk(chunk_id: [u8; 4]) -> bool {
    matches!(&chunk_id, b"LIST" | b"cue " | &FXMD_CHUNK_ID)
}

fn is_supported_channel_mask(channels: u16, mask: u32) -> bool {
    if mask & !MAX_RFC9639_CHANNEL_MASK != 0 {
        return false;
    }
    mask.count_ones() <= u32::from(channels)
}

fn decode_samples(data: &[u8], envelope: PcmEnvelope) -> Result<Vec<i32>> {
    let shift = envelope
        .container_bits_per_sample
        .checked_sub(envelope.valid_bits_per_sample)
        .ok_or(Error::InvalidWav(
            "valid bits cannot exceed container bits for decoding",
        ))? as u32;

    match envelope.container_bits_per_sample {
        8 => {
            let bias = 1i32 << (envelope.valid_bits_per_sample - 1);
            Ok(data
                .iter()
                .map(|&byte| (i32::from(byte) >> shift) - bias)
                .collect())
        }
        16 => Ok(data
            .chunks_exact(2)
            .map(|chunk| {
                let value = i16::from_le_bytes([chunk[0], chunk[1]]) as i32;
                if shift == 0 { value } else { value >> shift }
            })
            .collect()),
        24 => Ok(data
            .chunks_exact(3)
            .map(|chunk| {
                let mut value =
                    i32::from(chunk[0]) | (i32::from(chunk[1]) << 8) | (i32::from(chunk[2]) << 16);
                if value & 0x0080_0000 != 0 {
                    value |= !0x00ff_ffff;
                }
                if shift == 0 { value } else { value >> shift }
            })
            .collect()),
        32 => Ok(data
            .chunks_exact(4)
            .map(|chunk| {
                let value = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                if shift == 0 { value } else { value >> shift }
            })
            .collect()),
        _ => Err(Error::UnsupportedWav(format!(
            "unsupported container bits/sample for decoder: {}",
            envelope.container_bits_per_sample
        ))),
    }
}

pub(crate) fn container_bits_from_valid_bits(valid_bits: u16) -> u16 {
    match valid_bits {
        0..=8 => 8,
        9..=16 => 16,
        17..=24 => 24,
        25..=32 => 32,
        _ => valid_bits.div_ceil(8) * 8,
    }
}

pub(crate) fn append_encoded_sample(
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

    match envelope.container_bits_per_sample {
        8 => {
            let bias = 1i32 << (envelope.valid_bits_per_sample - 1);
            let stored = (i64::from(sample) + i64::from(bias))
                .checked_shl(shift)
                .ok_or_else(|| Error::UnsupportedWav("8-bit sample is out of range".into()))?;
            let stored = u8::try_from(stored)
                .map_err(|_| Error::UnsupportedWav("8-bit sample is out of range".into()))?;
            buffer.push(stored);
            Ok(())
        }
        16 => {
            let stored =
                i16::try_from(i64::from(sample).checked_shl(shift).ok_or_else(|| {
                    Error::UnsupportedWav("16-bit sample is out of range".into())
                })?)
                .map_err(|_| Error::UnsupportedWav("16-bit sample is out of range".into()))?;
            buffer.extend_from_slice(&stored.to_le_bytes());
            Ok(())
        }
        24 => {
            let stored =
                i32::try_from(i64::from(sample).checked_shl(shift).ok_or_else(|| {
                    Error::UnsupportedWav("24-bit sample is out of range".into())
                })?)
                .map_err(|_| Error::UnsupportedWav("24-bit sample is out of range".into()))?;
            let value = stored as u32;
            buffer.extend_from_slice(&[
                (value & 0xff) as u8,
                ((value >> 8) & 0xff) as u8,
                ((value >> 16) & 0xff) as u8,
            ]);
            Ok(())
        }
        32 => {
            let stored =
                i32::try_from(i64::from(sample).checked_shl(shift).ok_or_else(|| {
                    Error::UnsupportedWav("32-bit sample is out of range".into())
                })?)
                .map_err(|_| Error::UnsupportedWav("32-bit sample is out of range".into()))?;
            buffer.extend_from_slice(&stored.to_le_bytes());
            Ok(())
        }
        _ => Err(Error::UnsupportedWav(format!(
            "unsupported container bits/sample for encoder: {}",
            envelope.container_bits_per_sample
        ))),
    }?;

    Ok(())
}

fn read_chunk_payload<R: Read>(reader: &mut R, chunk_size: u32) -> Result<Vec<u8>> {
    let mut payload = vec![0u8; chunk_size as usize];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

fn read_u16_le<R: Read>(reader: &mut R) -> Result<u16> {
    let mut bytes = [0u8; 2];
    reader.read_exact(&mut bytes)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32_le<R: Read>(reader: &mut R) -> Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use crate::{config::EncoderConfig, metadata::FlacMetadataBlock};

    use super::{
        WavData, WavSpec, ordinary_channel_mask, read_wav, read_wav_for_encode_with_config,
    };

    fn pcm_wav_bytes(
        bits_per_sample: u16,
        channels: u16,
        sample_rate: u32,
        samples: &[i32],
    ) -> Vec<u8> {
        let bytes_per_sample = usize::from(bits_per_sample / 8);
        let block_align = usize::from(channels) * bytes_per_sample;
        let data_bytes = samples.len() * bytes_per_sample;
        let riff_size = 4 + (8 + 16) + (8 + data_bytes);

        let mut bytes = Vec::with_capacity(12 + 8 + 16 + 8 + data_bytes);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(riff_size as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");

        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * block_align as u32).to_le_bytes());
        bytes.extend_from_slice(&(block_align as u16).to_le_bytes());
        bytes.extend_from_slice(&bits_per_sample.to_le_bytes());

        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(data_bytes as u32).to_le_bytes());
        match bits_per_sample {
            16 => {
                for &sample in samples {
                    bytes.extend_from_slice(&(sample as i16).to_le_bytes());
                }
            }
            24 => {
                for &sample in samples {
                    let value = sample as u32;
                    bytes.extend_from_slice(&[
                        (value & 0xff) as u8,
                        ((value >> 8) & 0xff) as u8,
                        ((value >> 16) & 0xff) as u8,
                    ]);
                }
            }
            _ => unreachable!(),
        }

        bytes
    }

    fn extensible_pcm_wav_bytes(
        valid_bits_per_sample: u16,
        container_bits_per_sample: u16,
        channels: u16,
        sample_rate: u32,
        channel_mask: u32,
        samples: &[i32],
    ) -> Vec<u8> {
        let bytes_per_sample = usize::from(container_bits_per_sample / 8);
        let block_align = usize::from(channels) * bytes_per_sample;
        let data_bytes = samples.len() * bytes_per_sample;
        let riff_size = 4 + (8 + super::EXTENSIBLE_FMT_CHUNK_SIZE as usize) + (8 + data_bytes);

        let mut bytes =
            Vec::with_capacity(12 + 8 + super::EXTENSIBLE_FMT_CHUNK_SIZE as usize + 8 + data_bytes);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(riff_size as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");

        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&super::EXTENSIBLE_FMT_CHUNK_SIZE.to_le_bytes());
        bytes.extend_from_slice(&0xFFFEu16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * block_align as u32).to_le_bytes());
        bytes.extend_from_slice(&(block_align as u16).to_le_bytes());
        bytes.extend_from_slice(&container_bits_per_sample.to_le_bytes());
        bytes.extend_from_slice(&22u16.to_le_bytes());
        bytes.extend_from_slice(&valid_bits_per_sample.to_le_bytes());
        bytes.extend_from_slice(&channel_mask.to_le_bytes());
        bytes.extend_from_slice(&super::PCM_SUBFORMAT_GUID);

        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(data_bytes as u32).to_le_bytes());
        match container_bits_per_sample {
            16 => {
                for &sample in samples {
                    bytes.extend_from_slice(&(sample as i16).to_le_bytes());
                }
            }
            24 => {
                for &sample in samples {
                    let value = sample as u32;
                    bytes.extend_from_slice(&[
                        (value & 0xff) as u8,
                        ((value >> 8) & 0xff) as u8,
                        ((value >> 16) & 0xff) as u8,
                    ]);
                }
            }
            _ => unreachable!(),
        }

        bytes
    }

    fn append_chunk(bytes: &mut Vec<u8>, id: &[u8; 4], payload: &[u8]) {
        bytes.extend_from_slice(id);
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(payload);
        if !payload.len().is_multiple_of(2) {
            bytes.push(0);
        }
    }

    fn update_riff_size(bytes: &mut [u8]) {
        let riff_size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&riff_size.to_le_bytes());
    }

    fn with_chunks(mut wav: Vec<u8>, chunks: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
        let data_chunk_offset = wav
            .windows(4)
            .position(|window| window == b"data")
            .expect("data chunk present");
        let mut suffix = wav.split_off(data_chunk_offset);
        for (id, payload) in chunks {
            append_chunk(&mut wav, id, payload);
        }
        wav.append(&mut suffix);
        update_riff_size(&mut wav);
        wav
    }

    fn info_list_chunk(entries: &[([u8; 4], &[u8])]) -> Vec<u8> {
        let mut payload = b"INFO".to_vec();
        for (id, value) in entries {
            append_chunk(&mut payload, id, value);
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

    fn invalid_fxmd_chunk() -> Vec<u8> {
        vec![0x66, 0x78, 0x6d]
    }

    #[test]
    fn parses_16bit_pcm_wav() {
        let samples = [0, -1_000, 1_000, -2_000];
        let wav = read_wav(Cursor::new(pcm_wav_bytes(16, 2, 44_100, &samples))).unwrap();
        assert_eq!(
            wav,
            WavData {
                spec: WavSpec {
                    sample_rate: 44_100,
                    channels: 2,
                    bits_per_sample: 16,
                    total_samples: 2,
                    bytes_per_sample: 2,
                    channel_mask: ordinary_channel_mask(2u16).unwrap(),
                },
                samples: samples.to_vec(),
            }
        );
    }

    #[test]
    fn rejects_rf64() {
        let mut bytes = b"RF64".to_vec();
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        let error = read_wav(Cursor::new(bytes)).unwrap_err();
        assert!(error.to_string().contains("RF64"));
    }

    #[test]
    fn rejects_non_pcm_format_tag() {
        let mut bytes = pcm_wav_bytes(16, 1, 44_100, &[0, 1, 2, 3]);
        bytes[20] = 3;
        let error = read_wav(Cursor::new(bytes)).unwrap_err();
        assert!(error.to_string().contains("only PCM"));
    }

    #[test]
    fn rejects_non_extensible_multichannel_pcm_tag1_input() {
        let error = read_wav(Cursor::new(pcm_wav_bytes(16, 3, 44_100, &[0; 9]))).unwrap_err();
        assert!(error.to_string().contains("exact mono/stereo"));
    }

    #[test]
    fn accepts_non_ordinary_extensible_channel_masks() {
        let wav = read_wav(Cursor::new(extensible_pcm_wav_bytes(
            16,
            16,
            4,
            48_000,
            0x0001_2104,
            &[1, 2, 3, 4, 5, 6, 7, 8],
        )))
        .unwrap();

        assert_eq!(wav.spec.channel_mask, 0x0001_2104);
        assert_eq!(wav.spec.channels, 4);
    }

    #[test]
    fn accepts_zero_extensible_channel_mask() {
        let wav = read_wav(Cursor::new(extensible_pcm_wav_bytes(
            16,
            16,
            2,
            44_100,
            0,
            &[1, -1, 2, -2],
        )))
        .unwrap();

        assert_eq!(wav.spec.channel_mask, 0);
        assert_eq!(wav.spec.channels, 2);
    }

    #[test]
    fn rejects_extensible_channel_masks_with_unsupported_speaker_bits() {
        let error = read_wav(Cursor::new(extensible_pcm_wav_bytes(
            16,
            16,
            4,
            48_000,
            0x0004_0000,
            &[0; 8],
        )))
        .unwrap_err();

        assert!(error.to_string().contains("channel mask"));
    }

    #[test]
    fn rejects_zero_sample_rate() {
        let mut bytes = pcm_wav_bytes(16, 1, 44_100, &[0, 1, 2, 3]);
        bytes[24..28].copy_from_slice(&0u32.to_le_bytes());
        bytes[28..32].copy_from_slice(&0u32.to_le_bytes());
        let error = read_wav(Cursor::new(bytes)).unwrap_err();
        assert!(error.to_string().contains("sample rate 0"));
    }

    #[test]
    fn rejects_zero_block_align_without_panicking() {
        let mut bytes = pcm_wav_bytes(16, 1, 44_100, &[0, 1, 2, 3]);
        bytes[32..34].copy_from_slice(&0u16.to_le_bytes());
        let error = read_wav(Cursor::new(bytes)).unwrap_err();
        assert!(error.to_string().contains("block alignment"));
    }

    #[test]
    fn read_wav_remains_audio_only_when_metadata_chunks_exist() {
        let wav = with_chunks(
            pcm_wav_bytes(16, 1, 44_100, &[0, 1, 2, 3]),
            &[(*b"LIST", info_list_chunk(&[(*b"IART", b"Example Artist")]))],
        );

        let parsed = read_wav(Cursor::new(wav)).unwrap();
        assert_eq!(parsed.spec.total_samples, 4);
        assert_eq!(parsed.samples, vec![0, 1, 2, 3]);
    }

    #[test]
    fn read_wav_for_encode_captures_info_and_cue_metadata() {
        let wav = with_chunks(
            pcm_wav_bytes(16, 1, 44_100, &[0, 1, 2, 3]),
            &[
                (*b"LIST", info_list_chunk(&[(*b"IART", b"Example Artist")])),
                (*b"cue ", cue_chunk(&[0, 2])),
            ],
        );

        let parsed =
            read_wav_for_encode_with_config(Cursor::new(wav), &EncoderConfig::default()).unwrap();
        let blocks = parsed.metadata.flac_blocks();
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], FlacMetadataBlock::VorbisComment(_)));
        assert!(matches!(&blocks[1], FlacMetadataBlock::CueSheet(_)));
    }

    #[test]
    fn ignores_malformed_metadata_chunks_without_rejecting_audio() {
        let mut malformed_list = b"INFO".to_vec();
        malformed_list.extend_from_slice(b"IART");
        malformed_list.extend_from_slice(&99u32.to_le_bytes());
        malformed_list.extend_from_slice(b"too-short");
        let wav = with_chunks(
            pcm_wav_bytes(16, 1, 44_100, &[0, 1, 2, 3]),
            &[(*b"LIST", malformed_list)],
        );

        let parsed =
            read_wav_for_encode_with_config(Cursor::new(wav), &EncoderConfig::default()).unwrap();
        assert!(parsed.metadata.flac_blocks().is_empty());
        assert_eq!(parsed.wav.samples, vec![0, 1, 2, 3]);
    }

    #[test]
    fn read_wav_for_encode_leniently_ignores_invalid_fxmd_chunks() {
        let wav = with_chunks(
            pcm_wav_bytes(16, 1, 44_100, &[0, 1, 2, 3]),
            &[
                (*b"fxmd", invalid_fxmd_chunk()),
                (*b"LIST", info_list_chunk(&[(*b"IART", b"Example Artist")])),
                (*b"cue ", cue_chunk(&[0, 2])),
            ],
        );

        let parsed = read_wav_for_encode_with_config(
            Cursor::new(wav),
            &EncoderConfig::default().with_strict_fxmd_validation(false),
        )
        .unwrap();
        let blocks = parsed.metadata.flac_blocks();
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], FlacMetadataBlock::VorbisComment(_)));
        assert!(matches!(&blocks[1], FlacMetadataBlock::CueSheet(_)));
    }

    #[test]
    fn read_wav_for_encode_can_ignore_fxmd_chunks_entirely() {
        let wav = with_chunks(
            pcm_wav_bytes(16, 1, 44_100, &[0, 1, 2, 3]),
            &[
                (*b"fxmd", invalid_fxmd_chunk()),
                (*b"LIST", info_list_chunk(&[(*b"IART", b"Example Artist")])),
                (*b"cue ", cue_chunk(&[0, 2])),
            ],
        );

        let parsed = read_wav_for_encode_with_config(
            Cursor::new(wav),
            &EncoderConfig::default()
                .with_capture_fxmd(false)
                .with_strict_fxmd_validation(false),
        )
        .unwrap();
        let blocks = parsed.metadata.flac_blocks();
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], FlacMetadataBlock::VorbisComment(_)));
        assert!(matches!(&blocks[1], FlacMetadataBlock::CueSheet(_)));
    }
}
