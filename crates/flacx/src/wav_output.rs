use std::{io::Write, sync::mpsc, thread};

use crate::{
    error::{Error, Result},
    input::{
        PcmEnvelope, WavSpec, append_encoded_sample, container_bits_from_valid_bits,
        ordinary_channel_mask,
    },
    md5::Md5,
    metadata::WavMetadata,
};

const PCM_FMT_CHUNK_SIZE: u32 = 16;
const EXTENSIBLE_FMT_CHUNK_SIZE: u32 = 40;
const PCM_SUBFORMAT_GUID: [u8; 16] = [
    0x01, 0x00, 0x00, 0x00, // PCM subformat
    0x00, 0x00, 0x10, 0x00, // GUID data2/data3
    0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71, // GUID data4
];

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
    write_wav_with_metadata_and_md5(writer, spec, samples, metadata).map(|_| ())
}

pub(crate) fn write_wav_with_metadata_and_md5<W: Write>(
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
    if !matches!(spec.bytes_per_sample, 1 | 2 | 3 | 4) {
        return Err(Error::UnsupportedWav(format!(
            "only byte-aligned PCM containers are supported, found {} bytes/sample",
            spec.bytes_per_sample
        )));
    }
    if !(4..=32).contains(&spec.bits_per_sample) {
        return Err(Error::UnsupportedWav(format!(
            "only FLAC-native 4..32 valid bits/sample are supported, found {}",
            spec.bits_per_sample
        )));
    }
    if samples.len() % usize::from(spec.channels) != 0 {
        return Err(Error::Decode(
            "decoded samples are not aligned to the channel count".into(),
        ));
    }

    let container_bits_per_sample = container_bits_from_valid_bits(u16::from(spec.bits_per_sample));
    if u16::from(spec.bytes_per_sample) * 8 != container_bits_per_sample {
        return Err(Error::UnsupportedWav(format!(
            "bytes/sample does not match the chosen container width for {} valid bits/sample",
            spec.bits_per_sample
        )));
    }

    let ordinary_mask = ordinary_channel_mask(u16::from(spec.channels)).ok_or_else(|| {
        Error::UnsupportedWav(format!(
            "no ordinary channel mask exists for {} channels",
            spec.channels
        ))
    })?;
    let channel_mask = match spec.channel_mask {
        0 => ordinary_mask,
        mask if mask == ordinary_mask => mask,
        mask => {
            return Err(Error::UnsupportedWav(format!(
                "non-ordinary channel mask {mask:#010x} is not supported on output"
            )));
        }
    };
    let envelope = PcmEnvelope {
        channels: u16::from(spec.channels),
        valid_bits_per_sample: u16::from(spec.bits_per_sample),
        container_bits_per_sample,
        channel_mask,
    };
    let use_canonical_pcm =
        spec.channels <= 2 && envelope.valid_bits_per_sample == envelope.container_bits_per_sample;
    let fmt_chunk_size = if use_canonical_pcm {
        PCM_FMT_CHUNK_SIZE
    } else {
        EXTENSIBLE_FMT_CHUNK_SIZE
    };

    let block_align = usize::from(spec.channels) * usize::from(container_bits_per_sample / 8);
    let data_bytes = samples.len() * usize::from(container_bits_per_sample / 8);
    let metadata_bytes = wav_metadata_bytes(metadata);
    let riff_size = 4 + (8 + fmt_chunk_size as usize) + metadata_bytes.len() + (8 + data_bytes);

    writer.write_all(b"RIFF")?;
    writer.write_all(&(riff_size as u32).to_le_bytes())?;
    writer.write_all(b"WAVE")?;

    writer.write_all(b"fmt ")?;
    writer.write_all(&fmt_chunk_size.to_le_bytes())?;
    writer.write_all(&(if use_canonical_pcm { 1u16 } else { 0xFFFEu16 }).to_le_bytes())?;
    writer.write_all(&(u16::from(spec.channels)).to_le_bytes())?;
    writer.write_all(&spec.sample_rate.to_le_bytes())?;
    writer.write_all(&(spec.sample_rate * block_align as u32).to_le_bytes())?;
    writer.write_all(&(block_align as u16).to_le_bytes())?;
    writer.write_all(&container_bits_per_sample.to_le_bytes())?;

    if !use_canonical_pcm {
        writer.write_all(&22u16.to_le_bytes())?;
        writer.write_all(&(u16::from(spec.bits_per_sample)).to_le_bytes())?;
        writer.write_all(&channel_mask.to_le_bytes())?;
        writer.write_all(&PCM_SUBFORMAT_GUID)?;
    }

    writer.write_all(&metadata_bytes)?;

    writer.write_all(b"data")?;
    writer.write_all(&(data_bytes as u32).to_le_bytes())?;

    let streaminfo_md5 = write_sample_bytes(writer, samples, envelope)?;

    Ok(streaminfo_md5)
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

fn write_sample_bytes<W: Write>(
    writer: &mut W,
    samples: &[i32],
    envelope: PcmEnvelope,
) -> Result<[u8; 16]> {
    const CHUNK_CAPACITY: usize = 64 * 1024;
    let (hash_sender, hash_receiver) = mpsc::sync_channel::<Vec<u8>>(2);
    let hash_worker = thread::spawn(move || {
        let mut md5 = Md5::new();
        for chunk in hash_receiver {
            md5.update(&chunk);
        }
        md5.finalize()
    });
    let mut buffer = Vec::with_capacity(CHUNK_CAPACITY);

    for &sample in samples {
        append_encoded_sample(&mut buffer, sample, envelope)?;
        if buffer.len() >= CHUNK_CAPACITY {
            writer.write_all(&buffer)?;
            hash_sender
                .send(std::mem::replace(
                    &mut buffer,
                    Vec::with_capacity(CHUNK_CAPACITY),
                ))
                .map_err(|_| Error::Thread("streaminfo md5 worker stopped".into()))?;
        }
    }

    if !buffer.is_empty() {
        writer.write_all(&buffer)?;
        hash_sender
            .send(buffer)
            .map_err(|_| Error::Thread("streaminfo md5 worker stopped".into()))?;
    }
    drop(hash_sender);

    hash_worker
        .join()
        .map_err(|_| Error::Thread("streaminfo md5 worker panicked".into()))
}

#[cfg(test)]
mod tests {
    use crate::{
        input::{WavSpec, ordinary_channel_mask},
        metadata::WavMetadata,
    };

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
            channel_mask: ordinary_channel_mask(2u16).unwrap(),
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
    fn writes_extensible_wav_for_padded_container() {
        let spec = WavSpec {
            sample_rate: 48_000,
            channels: 2,
            bits_per_sample: 12,
            total_samples: 2,
            bytes_per_sample: 2,
            channel_mask: ordinary_channel_mask(2u16).unwrap(),
        };
        let samples = [0x123, -0x123];
        let mut wav = Vec::new();

        write_wav(&mut wav, spec, &samples).unwrap();

        assert_eq!(
            parse_chunk_layout(&wav),
            vec![(*b"fmt ", 40), (*b"data", 4)]
        );
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 0xFFFE);
    }

    #[test]
    fn metadata_wav_layout_is_fixed_and_padded() {
        let spec = WavSpec {
            sample_rate: 44_100,
            channels: 1,
            bits_per_sample: 16,
            total_samples: 2,
            bytes_per_sample: 2,
            channel_mask: ordinary_channel_mask(1u16).unwrap(),
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
