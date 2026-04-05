use std::io::Write;

use crate::{
    error::{Error, Result},
    input::WavSpec,
    metadata::WavMetadata,
};

const FMT_CHUNK_SIZE: u32 = 16;

#[allow(dead_code)]
pub(crate) fn write_wav<W: Write>(writer: &mut W, spec: WavSpec, samples: &[i32]) -> Result<()> {
    write_wav_with_metadata(writer, spec, samples, &WavMetadata::default())
}

pub(crate) fn write_wav_with_metadata<W: Write>(
    writer: &mut W,
    spec: WavSpec,
    samples: &[i32],
    metadata: &WavMetadata,
) -> Result<()> {
    if !(1..=2).contains(&spec.channels) {
        return Err(Error::UnsupportedWav(format!(
            "only mono/stereo output is supported, found {} channels",
            spec.channels
        )));
    }
    if !matches!(spec.bits_per_sample, 16 | 24) {
        return Err(Error::UnsupportedWav(format!(
            "only 16-bit and 24-bit PCM are supported, found {} bits/sample",
            spec.bits_per_sample
        )));
    }
    if samples.len() % usize::from(spec.channels) != 0 {
        return Err(Error::Decode(
            "decoded samples are not aligned to the channel count".into(),
        ));
    }

    let bytes_per_sample = usize::from(spec.bits_per_sample / 8);
    let block_align = usize::from(spec.channels) * bytes_per_sample;
    let data_bytes = samples.len() * bytes_per_sample;
    let metadata_bytes = wav_metadata_bytes(metadata);
    let riff_size = 4 + (8 + FMT_CHUNK_SIZE as usize) + metadata_bytes.len() + (8 + data_bytes);

    writer.write_all(b"RIFF")?;
    writer.write_all(&(riff_size as u32).to_le_bytes())?;
    writer.write_all(b"WAVE")?;

    writer.write_all(b"fmt ")?;
    writer.write_all(&FMT_CHUNK_SIZE.to_le_bytes())?;
    writer.write_all(&1u16.to_le_bytes())?;
    writer.write_all(&(u16::from(spec.channels)).to_le_bytes())?;
    writer.write_all(&spec.sample_rate.to_le_bytes())?;
    writer.write_all(&(spec.sample_rate * block_align as u32).to_le_bytes())?;
    writer.write_all(&(block_align as u16).to_le_bytes())?;
    writer.write_all(&(u16::from(spec.bits_per_sample)).to_le_bytes())?;

    writer.write_all(&metadata_bytes)?;

    writer.write_all(b"data")?;
    writer.write_all(&(data_bytes as u32).to_le_bytes())?;

    match spec.bits_per_sample {
        16 => write_sample_bytes(writer, samples, |sample, buffer| {
            buffer.extend_from_slice(&(sample as i16).to_le_bytes());
        })?,
        24 => write_sample_bytes(writer, samples, |sample, buffer| {
            let value = sample as u32;
            buffer.extend_from_slice(&[
                (value & 0xff) as u8,
                ((value >> 8) & 0xff) as u8,
                ((value >> 16) & 0xff) as u8,
            ]);
        })?,
        _ => unreachable!(),
    }

    Ok(())
}

fn wav_metadata_bytes(metadata: &WavMetadata) -> Vec<u8> {
    if metadata.is_empty() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if let Some(payload) = metadata.list_info_chunk_payload() {
        append_wav_chunk(&mut bytes, b"LIST", &payload);
    }
    if let Some(payload) = metadata.cue_chunk_payload() {
        append_wav_chunk(&mut bytes, b"cue ", &payload);
    }
    bytes
}

fn append_wav_chunk(buffer: &mut Vec<u8>, id: &[u8; 4], payload: &[u8]) {
    buffer.extend_from_slice(id);
    buffer.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buffer.extend_from_slice(payload);
    if payload.len() % 2 != 0 {
        buffer.push(0);
    }
}

fn write_sample_bytes<W, F>(writer: &mut W, samples: &[i32], mut encode: F) -> Result<()>
where
    W: Write,
    F: FnMut(i32, &mut Vec<u8>),
{
    const CHUNK_CAPACITY: usize = 64 * 1024;
    let mut buffer = Vec::with_capacity(CHUNK_CAPACITY);

    for &sample in samples {
        encode(sample, &mut buffer);
        if buffer.len() >= CHUNK_CAPACITY {
            writer.write_all(&buffer)?;
            buffer.clear();
        }
    }

    if !buffer.is_empty() {
        writer.write_all(&buffer)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{input::WavSpec, metadata::WavMetadata};

    use super::{write_wav, write_wav_with_metadata};

    fn parse_chunk_layout(wav: &[u8]) -> Vec<([u8; 4], u32)> {
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        let mut offset = 12usize;
        let mut chunks = Vec::new();
        while offset + 8 <= wav.len() {
            let id = wav[offset..offset + 4]
                .try_into()
                .expect("fixed wav chunk id slice");
            let size = u32::from_le_bytes(
                wav[offset + 4..offset + 8]
                    .try_into()
                    .expect("fixed wav chunk size slice"),
            );
            chunks.push((id, size));
            offset += 8 + size as usize;
            if size % 2 != 0 {
                offset += 1;
            }
        }
        chunks
    }

    fn synthetic_cuesheet_payload(track_offsets: &[u64], lead_out_offset: u64) -> Vec<u8> {
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

    #[test]
    fn writes_canonical_16bit_wav() {
        let spec = WavSpec {
            sample_rate: 44_100,
            channels: 2,
            bits_per_sample: 16,
            total_samples: 2,
            bytes_per_sample: 2,
        };
        let samples = [1, -2, 3, -4];
        let mut wav = Vec::new();

        write_wav(&mut wav, spec, &samples).unwrap();

        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(
            parse_chunk_layout(&wav),
            vec![(*b"fmt ", 16), (*b"data", 8)]
        );
    }

    #[test]
    fn metadata_wav_layout_is_fixed_and_padded() {
        let spec = WavSpec {
            sample_rate: 44_100,
            channels: 1,
            bits_per_sample: 16,
            total_samples: 2,
            bytes_per_sample: 2,
        };
        let samples = [1, -2];
        let mut metadata = WavMetadata::default();
        metadata.ingest_flac_metadata_block(
            4,
            &[
                0, 0, 0, 0, // vendor len
                1, 0, 0, 0, // comments
                9, 0, 0, 0, // len
                b'T', b'I', b'T', b'L', b'E', b'=', b'O', b'd', b'd',
            ],
            2,
        );
        metadata.ingest_flac_metadata_block(5, &synthetic_cuesheet_payload(&[0], 2), 2);

        let mut wav = Vec::new();
        write_wav_with_metadata(&mut wav, spec, &samples, &metadata).unwrap();

        let chunks = parse_chunk_layout(&wav);
        assert_eq!(
            chunks.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![*b"fmt ", *b"LIST", *b"cue ", *b"data"]
        );

        let list_index = 12 + 8 + 16;
        let list_size = u32::from_le_bytes(wav[list_index + 4..list_index + 8].try_into().unwrap());
        assert_eq!(list_size, 16);
        let padded_byte = wav[list_index + 8 + list_size as usize - 1];
        assert_eq!(padded_byte, 0);
    }
}
