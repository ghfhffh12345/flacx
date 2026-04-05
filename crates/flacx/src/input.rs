use std::io::{Read, Seek, SeekFrom};

use crate::error::{Error, Result};
use crate::metadata::{EncodeMetadata, MetadataDraft};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavSpec {
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
    pub total_samples: u64,
    pub bytes_per_sample: u16,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FormatChunk {
    format_tag: u16,
    channels: u16,
    sample_rate: u32,
    byte_rate: u32,
    block_align: u16,
    bits_per_sample: u16,
}

#[allow(dead_code)]
pub fn read_wav<R: Read + Seek>(mut reader: R) -> Result<WavData> {
    Ok(read_wav_internal(&mut reader, false)?.wav)
}

pub(crate) fn read_wav_for_encode<R: Read + Seek>(mut reader: R) -> Result<EncodeWavData> {
    read_wav_internal(&mut reader, true)
}

fn read_wav_internal<R: Read + Seek>(
    reader: &mut R,
    capture_metadata: bool,
) -> Result<EncodeWavData> {
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

        match &chunk_header[..4] {
            b"fmt " => {
                format = Some(read_format_chunk(reader, chunk_size)?);
            }
            b"data" => {
                data_offset = Some(chunk_start);
                data_size = Some(chunk_size);
                reader.seek(SeekFrom::Current(chunk_size as i64))?;
            }
            b"LIST" | b"cue " if capture_metadata => {
                let payload = read_chunk_payload(reader, chunk_size)?;
                metadata_draft.ingest_chunk(
                    chunk_header[..4].try_into().expect("fixed chunk id"),
                    &payload,
                );
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

    validate_format(format)?;

    let expected_block_align = format.channels * (format.bits_per_sample / 8);
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

    reader.seek(SeekFrom::Start(data_offset))?;
    let mut data = vec![0u8; data_size as usize];
    reader.read_exact(&mut data)?;

    let total_samples = u64::from(data_size / u32::from(format.block_align));
    let samples = decode_samples(&data, format.bits_per_sample)?;

    let wav = WavData {
        spec: WavSpec {
            sample_rate: format.sample_rate,
            channels: format.channels as u8,
            bits_per_sample: format.bits_per_sample as u8,
            total_samples,
            bytes_per_sample: format.bits_per_sample / 8,
        },
        samples,
    };

    Ok(EncodeWavData {
        metadata: if capture_metadata {
            metadata_draft.finish(total_samples)
        } else {
            EncodeMetadata::default()
        },
        wav,
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
    let bits_per_sample = read_u16_le(reader)?;

    if chunk_size > 16 {
        let mut discard = vec![0u8; (chunk_size - 16) as usize];
        reader.read_exact(&mut discard)?;
    }

    Ok(FormatChunk {
        format_tag,
        channels,
        sample_rate,
        byte_rate,
        block_align,
        bits_per_sample,
    })
}

fn validate_format(format: FormatChunk) -> Result<()> {
    if format.format_tag != 1 {
        return Err(Error::UnsupportedWav(format!(
            "only PCM format tag 1 is supported, found {}",
            format.format_tag
        )));
    }

    if !(1..=2).contains(&format.channels) {
        return Err(Error::UnsupportedWav(format!(
            "only mono/stereo input is supported, found {} channels",
            format.channels
        )));
    }

    if !matches!(format.bits_per_sample, 16 | 24) {
        return Err(Error::UnsupportedWav(format!(
            "only 16-bit and 24-bit PCM are supported, found {} bits/sample",
            format.bits_per_sample
        )));
    }

    if format.sample_rate == 0 {
        return Err(Error::UnsupportedWav("sample rate 0 is not allowed".into()));
    }

    let expected_byte_rate =
        format.sample_rate * u32::from(format.channels) * u32::from(format.bits_per_sample / 8);
    if format.byte_rate != expected_byte_rate {
        return Err(Error::InvalidWav(
            "fmt byte rate does not match the PCM payload shape",
        ));
    }

    Ok(())
}

fn decode_samples(data: &[u8], bits_per_sample: u16) -> Result<Vec<i32>> {
    match bits_per_sample {
        16 => Ok(data
            .chunks_exact(2)
            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]) as i32)
            .collect()),
        24 => Ok(data
            .chunks_exact(3)
            .map(|chunk| {
                let mut value =
                    i32::from(chunk[0]) | (i32::from(chunk[1]) << 8) | (i32::from(chunk[2]) << 16);
                if value & 0x0080_0000 != 0 {
                    value |= !0x00ff_ffff;
                }
                value
            })
            .collect()),
        _ => Err(Error::UnsupportedWav(format!(
            "unsupported bits/sample for decoder: {bits_per_sample}"
        ))),
    }
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

    use crate::metadata::FlacMetadataBlock;

    use super::{WavData, WavSpec, read_wav, read_wav_for_encode};

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

    fn append_chunk(bytes: &mut Vec<u8>, id: &[u8; 4], payload: &[u8]) {
        bytes.extend_from_slice(id);
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(payload);
        if payload.len() % 2 != 0 {
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
    fn rejects_non_stereo_or_mono_input() {
        let error = read_wav(Cursor::new(pcm_wav_bytes(16, 3, 44_100, &[0; 9]))).unwrap_err();
        assert!(error.to_string().contains("mono/stereo"));
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

        let parsed = read_wav_for_encode(Cursor::new(wav)).unwrap();
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

        let parsed = read_wav_for_encode(Cursor::new(wav)).unwrap();
        assert!(parsed.metadata.flac_blocks().is_empty());
        assert_eq!(parsed.wav.samples, vec![0, 1, 2, 3]);
    }
}
