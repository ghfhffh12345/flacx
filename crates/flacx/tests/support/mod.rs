#![allow(dead_code)]
use std::{
    env, fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn pcm_wav_bytes(
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

pub fn wav_with_chunks(mut wav: Vec<u8>, chunks: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let data_chunk_offset = wav
        .windows(4)
        .position(|window| window == b"data")
        .expect("data chunk present");
    let mut suffix = wav.split_off(data_chunk_offset);
    for (id, payload) in chunks {
        append_chunk(&mut wav, id, payload);
    }
    wav.append(&mut suffix);
    let riff_size = (wav.len() - 8) as u32;
    wav[4..8].copy_from_slice(&riff_size.to_le_bytes());
    wav
}

pub fn info_list_chunk(entries: &[([u8; 4], &[u8])]) -> Vec<u8> {
    let mut payload = b"INFO".to_vec();
    for (id, value) in entries {
        append_chunk(&mut payload, id, value);
    }
    payload
}

pub fn cue_chunk(offsets: &[u32]) -> Vec<u8> {
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

pub fn flac_metadata_blocks(flac_bytes: &[u8]) -> Vec<ParsedMetadataBlock> {
    split_flac_stream(flac_bytes).0
}

pub fn replace_flac_optional_metadata(
    flac_bytes: &[u8],
    optional_blocks: &[ParsedMetadataBlock],
) -> Vec<u8> {
    let (blocks, frames) = split_flac_stream(flac_bytes);
    let streaminfo = blocks
        .first()
        .expect("streaminfo metadata block present")
        .payload
        .clone();
    let mut rebuilt = Vec::new();
    rebuilt.extend_from_slice(b"fLaC");
    let total_blocks = 1 + optional_blocks.len();
    for (index, block) in std::iter::once(ParsedMetadataBlock {
        is_last: total_blocks == 1,
        block_type: 0,
        payload: streaminfo,
    })
    .chain(optional_blocks.iter().cloned())
    .enumerate()
    {
        rebuilt.extend_from_slice(&flac_metadata_header(
            block.block_type,
            index + 1 == total_blocks,
            block.payload.len(),
        ));
        rebuilt.extend_from_slice(&block.payload);
    }
    rebuilt.extend_from_slice(&frames);
    rebuilt
}

pub fn vorbis_comment_block(entries: &[(&str, &str)]) -> ParsedMetadataBlock {
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_le_bytes());
    payload.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (key, value) in entries {
        let entry = format!("{key}={value}");
        payload.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        payload.extend_from_slice(entry.as_bytes());
    }
    ParsedMetadataBlock {
        is_last: false,
        block_type: 4,
        payload,
    }
}

pub fn cuesheet_block(track_offsets: &[u64], lead_out_offset: u64) -> ParsedMetadataBlock {
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
    ParsedMetadataBlock {
        is_last: false,
        block_type: 5,
        payload,
    }
}

pub fn application_block(payload: &[u8]) -> ParsedMetadataBlock {
    ParsedMetadataBlock {
        is_last: false,
        block_type: 2,
        payload: payload.to_vec(),
    }
}

pub fn wav_info_entries(wav_bytes: &[u8]) -> Vec<([u8; 4], String)> {
    let mut entries = Vec::new();
    for (chunk_id, payload) in wav_chunks(wav_bytes) {
        if chunk_id == *b"LIST" && payload.starts_with(b"INFO") {
            let mut offset = 4usize;
            while offset + 8 <= payload.len() {
                let info_id = payload[offset..offset + 4]
                    .try_into()
                    .expect("fixed wav info id slice");
                let size = u32::from_le_bytes(
                    payload[offset + 4..offset + 8]
                        .try_into()
                        .expect("fixed wav info size slice"),
                ) as usize;
                offset += 8;
                if offset + size > payload.len() {
                    return entries;
                }
                let raw = &payload[offset..offset + size];
                let end = raw.iter().position(|&byte| byte == 0).unwrap_or(raw.len());
                entries.push((info_id, String::from_utf8_lossy(&raw[..end]).into_owned()));
                offset += size;
                if size % 2 != 0 {
                    offset += 1;
                }
            }
        }
    }
    entries
}

pub fn wav_cue_points(wav_bytes: &[u8]) -> Vec<u32> {
    for (chunk_id, payload) in wav_chunks(wav_bytes) {
        if chunk_id == *b"cue " {
            if payload.len() < 4 {
                return Vec::new();
            }
            let cue_count = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
            let mut points = Vec::with_capacity(cue_count);
            let mut offset = 4usize;
            for _ in 0..cue_count {
                if offset + 24 > payload.len() {
                    return points;
                }
                points.push(u32::from_le_bytes(
                    payload[offset + 20..offset + 24].try_into().unwrap(),
                ));
                offset += 24;
            }
            return points;
        }
    }
    Vec::new()
}

fn split_flac_stream(flac_bytes: &[u8]) -> (Vec<ParsedMetadataBlock>, Vec<u8>) {
    assert_eq!(&flac_bytes[..4], b"fLaC");
    let mut offset = 4usize;
    let mut blocks = Vec::new();
    loop {
        let header = flac_bytes[offset];
        let is_last = header & 0x80 != 0;
        let block_type = header & 0x7f;
        let length = u32::from_be_bytes([
            0,
            flac_bytes[offset + 1],
            flac_bytes[offset + 2],
            flac_bytes[offset + 3],
        ]) as usize;
        offset += 4;
        blocks.push(ParsedMetadataBlock {
            is_last,
            block_type,
            payload: flac_bytes[offset..offset + length].to_vec(),
        });
        offset += length;
        if is_last {
            return (blocks, flac_bytes[offset..].to_vec());
        }
    }
}

pub fn vorbis_comments(payload: &[u8]) -> Vec<String> {
    let vendor_len = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
    let mut offset = 4 + vendor_len;
    let comment_count =
        u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;
    let mut comments = Vec::with_capacity(comment_count);
    for _ in 0..comment_count {
        let length = u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        comments.push(String::from_utf8(payload[offset..offset + length].to_vec()).unwrap());
        offset += length;
    }
    comments
}

fn append_chunk(bytes: &mut Vec<u8>, id: &[u8; 4], payload: &[u8]) {
    bytes.extend_from_slice(id);
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(payload);
    if payload.len() % 2 != 0 {
        bytes.push(0);
    }
}

fn flac_metadata_header(block_type: u8, is_last: bool, payload_len: usize) -> [u8; 4] {
    let [_, b1, b2, b3] = (payload_len as u32).to_be_bytes();
    [
        if is_last {
            0x80 | block_type
        } else {
            block_type
        },
        b1,
        b2,
        b3,
    ]
}

fn wav_chunks(wav_bytes: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
    assert_eq!(&wav_bytes[..4], b"RIFF");
    assert_eq!(&wav_bytes[8..12], b"WAVE");
    let mut chunks = Vec::new();
    let mut offset = 12usize;
    while offset + 8 <= wav_bytes.len() {
        let id = wav_bytes[offset..offset + 4]
            .try_into()
            .expect("fixed wav chunk id slice");
        let size = u32::from_le_bytes(
            wav_bytes[offset + 4..offset + 8]
                .try_into()
                .expect("fixed wav chunk size slice"),
        ) as usize;
        offset += 8;
        chunks.push((id, wav_bytes[offset..offset + size].to_vec()));
        offset += size;
        if size % 2 != 0 {
            offset += 1;
        }
    }
    chunks
}

pub fn sample_fixture(channels: u16, frames: usize) -> Vec<i32> {
    let mut samples = Vec::with_capacity(frames * usize::from(channels));
    for frame in 0..frames {
        let left = ((frame as i32 * 97) % 30_000) - 15_000;
        samples.push(left);
        if channels == 2 {
            let right = ((frame as i32 * 131) % 28_000) - 14_000;
            samples.push(right);
        }
    }
    samples
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMetadataBlock {
    pub is_last: bool,
    pub block_type: u8,
    pub payload: Vec<u8>,
}

pub fn unique_temp_path(extension: &str) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    env::temp_dir().join(format!(
        "flacx-test-{}-{}.{}",
        std::process::id(),
        timestamp,
        extension
    ))
}

pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub fn decode_with_ffmpeg(flac_bytes: &[u8], bits_per_sample: u16) -> Vec<i32> {
    assert!(
        ffmpeg_available(),
        "ffmpeg is required for decoder-oracle integration tests"
    );

    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("raw");
    fs::write(&input_path, flac_bytes).unwrap();

    let codec = match bits_per_sample {
        16 => "pcm_s16le",
        24 => "pcm_s24le",
        _ => unreachable!(),
    };
    let format = match bits_per_sample {
        16 => "s16le",
        24 => "s24le",
        _ => unreachable!(),
    };

    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-i",
            input_path.to_str().unwrap(),
            "-f",
            format,
            "-acodec",
            codec,
            output_path.to_str().unwrap(),
        ])
        .status()
        .unwrap();

    assert!(status.success(), "ffmpeg failed to decode test FLAC");

    let raw = fs::read(&output_path).unwrap();
    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);

    match bits_per_sample {
        16 => raw
            .chunks_exact(2)
            .map(|chunk| i16::from_le_bytes(chunk.try_into().unwrap()) as i32)
            .collect(),
        24 => raw
            .chunks_exact(3)
            .map(|chunk| {
                let mut value =
                    i32::from(chunk[0]) | (i32::from(chunk[1]) << 8) | (i32::from(chunk[2]) << 16);
                if value & 0x0080_0000 != 0 {
                    value |= !0x00ff_ffff;
                }
                value
            })
            .collect(),
        _ => unreachable!(),
    }
}

pub fn corrupt_magic(flac_bytes: &[u8]) -> Vec<u8> {
    let mut bytes = flac_bytes.to_vec();
    if bytes.len() >= 4 {
        bytes[..4].copy_from_slice(b"bad!");
    }
    bytes
}

pub fn truncate_bytes(bytes: &[u8], len: usize) -> Vec<u8> {
    bytes[..bytes.len().min(len)].to_vec()
}

pub fn corrupt_last_frame_crc(flac_bytes: &[u8]) -> Vec<u8> {
    let mut bytes = flac_bytes.to_vec();
    if let Some(last) = bytes.last_mut() {
        *last ^= 0x01;
    }
    bytes
}
