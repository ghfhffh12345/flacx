#![allow(dead_code)]
use std::{
    env, fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const PCM_GUID: [u8; 16] = [
    0x01, 0x00, 0x00, 0x00, // PCM subformat
    0x00, 0x00, 0x10, 0x00, // GUID data2/data3
    0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71, // GUID data4
];
const PCM_FMT_CHUNK_SIZE: u32 = 16;
const EXTENSIBLE_FMT_CHUNK_SIZE: u32 = 40;

pub fn pcm_wav_bytes(
    bits_per_sample: u16,
    channels: u16,
    sample_rate: u32,
    samples: &[i32],
) -> Vec<u8> {
    let bytes_per_sample = bytes_per_sample(bits_per_sample);
    let block_align = usize::from(channels) * bytes_per_sample;
    let data_bytes = samples.len() * bytes_per_sample;
    let riff_size = 4 + (8 + PCM_FMT_CHUNK_SIZE as usize) + (8 + data_bytes);

    let mut bytes = Vec::with_capacity(12 + 8 + PCM_FMT_CHUNK_SIZE as usize + 8 + data_bytes);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(riff_size as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");

    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&PCM_FMT_CHUNK_SIZE.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * block_align as u32).to_le_bytes());
    bytes.extend_from_slice(&(block_align as u16).to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());

    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    write_pcm_samples(&mut bytes, bits_per_sample, samples);

    bytes
}

pub fn extensible_pcm_wav_bytes(
    valid_bits_per_sample: u16,
    container_bits_per_sample: u16,
    channels: u16,
    sample_rate: u32,
    channel_mask: u32,
    samples: &[i32],
) -> Vec<u8> {
    assert!(matches!(container_bits_per_sample, 8 | 16 | 24 | 32));
    assert!(valid_bits_per_sample >= 4);
    assert!(valid_bits_per_sample <= container_bits_per_sample);

    let bytes_per_sample = bytes_per_sample(container_bits_per_sample);
    let block_align = usize::from(channels) * bytes_per_sample;
    let data_bytes = samples.len() * bytes_per_sample;
    let riff_size = 4 + (8 + EXTENSIBLE_FMT_CHUNK_SIZE as usize) + (8 + data_bytes);

    let mut bytes =
        Vec::with_capacity(12 + 8 + EXTENSIBLE_FMT_CHUNK_SIZE as usize + 8 + data_bytes);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(riff_size as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");

    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&EXTENSIBLE_FMT_CHUNK_SIZE.to_le_bytes());
    bytes.extend_from_slice(&0xFFFEu16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * block_align as u32).to_le_bytes());
    bytes.extend_from_slice(&(block_align as u16).to_le_bytes());
    bytes.extend_from_slice(&container_bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(&22u16.to_le_bytes());
    bytes.extend_from_slice(&valid_bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(&channel_mask.to_le_bytes());
    bytes.extend_from_slice(&PCM_GUID);

    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    write_left_aligned_samples(
        &mut bytes,
        container_bits_per_sample,
        valid_bits_per_sample,
        samples,
    );

    bytes
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedWavFormat {
    pub format_tag: u16,
    pub channels: u16,
    pub sample_rate: u32,
    pub byte_rate: u32,
    pub block_align: u16,
    pub bits_per_sample: u16,
    pub valid_bits_per_sample: Option<u16>,
    pub channel_mask: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParsedFlacBlockingStrategy {
    Fixed,
    Variable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParsedFlacCodedNumberKind {
    FrameNumber,
    SampleNumber,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedFlacFrameHeader {
    pub blocking_strategy: ParsedFlacBlockingStrategy,
    pub coded_number_kind: ParsedFlacCodedNumberKind,
    pub coded_number_value: u64,
    pub block_size_bits: u8,
    pub sample_rate_bits: u8,
    pub channel_assignment_bits: u8,
    pub sample_size_bits: u8,
}

pub fn parse_wav_format(wav_bytes: &[u8]) -> ParsedWavFormat {
    for (chunk_id, payload) in wav_chunks(wav_bytes) {
        if chunk_id == *b"fmt " {
            assert!(payload.len() >= 16, "fmt chunk too short");
            let format_tag = u16::from_le_bytes(payload[0..2].try_into().unwrap());
            let channels = u16::from_le_bytes(payload[2..4].try_into().unwrap());
            let sample_rate = u32::from_le_bytes(payload[4..8].try_into().unwrap());
            let byte_rate = u32::from_le_bytes(payload[8..12].try_into().unwrap());
            let block_align = u16::from_le_bytes(payload[12..14].try_into().unwrap());
            let bits_per_sample = u16::from_le_bytes(payload[14..16].try_into().unwrap());
            let mut valid_bits_per_sample = None;
            let mut channel_mask = None;
            if payload.len() >= 40 {
                valid_bits_per_sample =
                    Some(u16::from_le_bytes(payload[18..20].try_into().unwrap()));
                channel_mask = Some(u32::from_le_bytes(payload[20..24].try_into().unwrap()));
            }
            return ParsedWavFormat {
                format_tag,
                channels,
                sample_rate,
                byte_rate,
                block_align,
                bits_per_sample,
                valid_bits_per_sample,
                channel_mask,
            };
        }
    }

    panic!("fmt chunk not found")
}

pub fn wav_data_bytes(wav_bytes: &[u8]) -> Vec<u8> {
    for (chunk_id, payload) in wav_chunks(wav_bytes) {
        if chunk_id == *b"data" {
            return payload;
        }
    }

    panic!("data chunk not found")
}

pub fn ordinary_channel_mask(channels: u16) -> Option<u32> {
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

pub fn parse_first_flac_frame_header(flac_bytes: &[u8]) -> ParsedFlacFrameHeader {
    let frame_offset = first_flac_frame_offset(flac_bytes);
    let mut bit_offset = frame_offset * 8;
    assert_eq!(read_bits(flac_bytes, &mut bit_offset, 14), 0x3FFE);
    let _first_flag = read_bits(flac_bytes, &mut bit_offset, 1);
    let blocking_strategy = if read_bits(flac_bytes, &mut bit_offset, 1) == 0 {
        ParsedFlacBlockingStrategy::Fixed
    } else {
        ParsedFlacBlockingStrategy::Variable
    };
    let block_size_bits = read_bits(flac_bytes, &mut bit_offset, 4) as u8;
    let sample_rate_bits = read_bits(flac_bytes, &mut bit_offset, 4) as u8;
    let channel_assignment_bits = read_bits(flac_bytes, &mut bit_offset, 4) as u8;
    let sample_size_bits = read_bits(flac_bytes, &mut bit_offset, 3) as u8;
    let _reserved = read_bits(flac_bytes, &mut bit_offset, 1);
    bit_offset = (bit_offset + 7) & !7;
    let coded_number_offset = bit_offset / 8;
    let (coded_number_value, _) = decode_utf8_like_number(&flac_bytes[coded_number_offset..]);

    ParsedFlacFrameHeader {
        blocking_strategy,
        coded_number_kind: match blocking_strategy {
            ParsedFlacBlockingStrategy::Fixed => ParsedFlacCodedNumberKind::FrameNumber,
            ParsedFlacBlockingStrategy::Variable => ParsedFlacCodedNumberKind::SampleNumber,
        },
        coded_number_value,
        block_size_bits,
        sample_rate_bits,
        channel_assignment_bits,
        sample_size_bits,
    }
}

pub fn corrupt_first_flac_frame_sample_number(
    flac_bytes: &[u8],
    wrong_sample_number: u64,
) -> Vec<u8> {
    let mut bytes = flac_bytes.to_vec();
    let frame_offset = first_flac_frame_offset(flac_bytes);
    let mut bit_offset = frame_offset * 8;
    assert_eq!(read_bits(flac_bytes, &mut bit_offset, 14), 0x3FFE);
    let _first_flag = read_bits(flac_bytes, &mut bit_offset, 1);
    let _blocking_strategy = read_bits(flac_bytes, &mut bit_offset, 1);
    let _block_size_bits = read_bits(flac_bytes, &mut bit_offset, 4);
    let _sample_rate_bits = read_bits(flac_bytes, &mut bit_offset, 4);
    let _assignment = read_bits(flac_bytes, &mut bit_offset, 4);
    let _sample_size_bits = read_bits(flac_bytes, &mut bit_offset, 3);
    let _reserved = read_bits(flac_bytes, &mut bit_offset, 1);
    bit_offset = (bit_offset + 7) & !7;

    let coded_number_offset = bit_offset / 8;
    let (_, coded_number_len) = decode_utf8_like_number(&flac_bytes[coded_number_offset..]);
    let replacement = encode_utf8_like_number(wrong_sample_number);
    assert_eq!(
        replacement.len(),
        coded_number_len,
        "replacement coded number must preserve byte length"
    );
    bytes[coded_number_offset..coded_number_offset + coded_number_len]
        .copy_from_slice(&replacement);
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFxvcChunk {
    pub version: u32,
    pub vendor: String,
    pub entries: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFxcsChunk {
    pub version: u32,
    pub raw_payload: Vec<u8>,
}

pub fn fxvc_chunk_payload(vendor: &str, entries: &[&str]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    payload.extend_from_slice(vendor.as_bytes());
    payload.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries {
        payload.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        payload.extend_from_slice(entry.as_bytes());
    }
    payload
}

pub fn parse_fxvc_chunk_payload(payload: &[u8]) -> ParsedFxvcChunk {
    assert!(payload.len() >= 12, "fxvc payload too short");
    let mut offset = 0usize;
    let version = u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap());
    offset += 4;
    let vendor_len = u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;
    let vendor = String::from_utf8(payload[offset..offset + vendor_len].to_vec()).unwrap();
    offset += vendor_len;
    let comment_count =
        u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;
    let mut entries = Vec::with_capacity(comment_count);
    for _ in 0..comment_count {
        let entry_len =
            u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        entries.push(String::from_utf8(payload[offset..offset + entry_len].to_vec()).unwrap());
        offset += entry_len;
    }
    assert_eq!(offset, payload.len(), "fxvc payload trailing bytes");
    ParsedFxvcChunk {
        version,
        vendor,
        entries,
    }
}

pub fn fxcs_chunk_payload(raw_payload: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.extend_from_slice(&(raw_payload.len() as u32).to_le_bytes());
    payload.extend_from_slice(raw_payload);
    payload
}

pub fn parse_fxcs_chunk_payload(payload: &[u8]) -> ParsedFxcsChunk {
    assert!(payload.len() >= 8, "fxcs payload too short");
    let mut offset = 0usize;
    let version = u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap());
    offset += 4;
    let raw_len = u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap()) as usize;
    offset += 4;
    let raw_payload = payload[offset..offset + raw_len].to_vec();
    offset += raw_len;
    assert_eq!(offset, payload.len(), "fxcs payload trailing bytes");
    ParsedFxcsChunk {
        version,
        raw_payload,
    }
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

pub fn streaminfo_md5(flac_bytes: &[u8]) -> [u8; 16] {
    let blocks = split_flac_stream(flac_bytes).0;
    blocks[0].payload[18..34]
        .try_into()
        .expect("fixed STREAMINFO md5 slice")
}

pub fn flac_frames(flac_bytes: &[u8]) -> Vec<u8> {
    split_flac_stream(flac_bytes).1
}

pub fn rewrite_streaminfo_md5(flac_bytes: &[u8], md5: [u8; 16]) -> Vec<u8> {
    let (mut blocks, frames) = split_flac_stream(flac_bytes);
    let streaminfo = blocks
        .first_mut()
        .expect("streaminfo metadata block present");
    streaminfo.payload[18..34].copy_from_slice(&md5);

    let mut rebuilt = Vec::new();
    rebuilt.extend_from_slice(b"fLaC");
    let total_blocks = blocks.len();
    for (index, block) in blocks.into_iter().enumerate() {
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

pub fn rich_cuesheet_payload() -> Vec<u8> {
    let mut payload = cuesheet_block(&[0, 2_048], 4_096).payload;
    payload[..13].copy_from_slice(b"1234567890123");
    payload[128..136].copy_from_slice(&1_176u64.to_be_bytes());
    payload[136] = 0x80;
    payload[405..417].copy_from_slice(b"ABCDEFGHIJKL");
    payload
}

pub fn raw_cuesheet_block(payload: &[u8]) -> ParsedMetadataBlock {
    ParsedMetadataBlock {
        is_last: false,
        block_type: 5,
        payload: payload.to_vec(),
    }
}

pub fn seektable_block(entries: &[(u64, u64, u16)]) -> ParsedMetadataBlock {
    let mut payload = Vec::with_capacity(entries.len() * 18);
    for &(sample_number, frame_offset, sample_count) in entries {
        payload.extend_from_slice(&sample_number.to_be_bytes());
        payload.extend_from_slice(&frame_offset.to_be_bytes());
        payload.extend_from_slice(&sample_count.to_be_bytes());
    }
    ParsedMetadataBlock {
        is_last: false,
        block_type: 3,
        payload,
    }
}

pub fn raw_seektable_block(payload: &[u8]) -> ParsedMetadataBlock {
    ParsedMetadataBlock {
        is_last: false,
        block_type: 3,
        payload: payload.to_vec(),
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
                if !size.is_multiple_of(2) {
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

pub fn sample_fixture(channels: u16, frames: usize) -> Vec<i32> {
    let mut samples = Vec::with_capacity(frames * usize::from(channels));
    for frame in 0..frames {
        for channel in 0..channels {
            let sample = (((frame as i32 * 97) + (channel as i32 * 1_013)) % 30_000) - 15_000;
            samples.push(sample);
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

pub fn parse_vorbis_comment_entries(payload: &[u8]) -> Vec<(String, String)> {
    if payload.len() < 8 {
        return Vec::new();
    }
    let vendor_len = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
    if payload.len() < 4 + vendor_len + 4 {
        return Vec::new();
    }
    let mut cursor = 4 + vendor_len;
    let comment_count = u32::from_le_bytes(payload[cursor..cursor + 4].try_into().unwrap());
    cursor += 4;

    let mut entries = Vec::new();
    for _ in 0..comment_count {
        if cursor + 4 > payload.len() {
            return entries;
        }
        let entry_len =
            u32::from_le_bytes(payload[cursor..cursor + 4].try_into().unwrap()) as usize;
        cursor += 4;
        if cursor + entry_len > payload.len() {
            return entries;
        }
        let entry = String::from_utf8_lossy(&payload[cursor..cursor + entry_len]).into_owned();
        cursor += entry_len;
        if let Some((key, value)) = entry.split_once('=') {
            entries.push((key.to_owned(), value.to_owned()));
        }
    }
    entries
}

pub fn parse_vorbis_comment_vendor(payload: &[u8]) -> String {
    if payload.len() < 4 {
        return String::new();
    }
    let vendor_len = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
    if payload.len() < 4 + vendor_len {
        return String::new();
    }
    String::from_utf8_lossy(&payload[4..4 + vendor_len]).into_owned()
}

pub fn wav_chunk_payloads(wav_bytes: &[u8], id: [u8; 4]) -> Vec<Vec<u8>> {
    wav_chunks(wav_bytes)
        .into_iter()
        .filter_map(|(chunk_id, payload)| (chunk_id == id).then_some(payload))
        .collect()
}

pub fn parse_seektable_entries(payload: &[u8]) -> Vec<(u64, u64, u16)> {
    if !payload.len().is_multiple_of(18) {
        return Vec::new();
    }

    payload
        .chunks_exact(18)
        .map(|chunk| {
            (
                u64::from_be_bytes(chunk[..8].try_into().unwrap()),
                u64::from_be_bytes(chunk[8..16].try_into().unwrap()),
                u16::from_be_bytes(chunk[16..18].try_into().unwrap()),
            )
        })
        .collect()
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
    if !payload.len().is_multiple_of(2) {
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
        if !size.is_multiple_of(2) {
            offset += 1;
        }
    }
    chunks
}

fn first_flac_frame_offset(flac_bytes: &[u8]) -> usize {
    assert_eq!(&flac_bytes[..4], b"fLaC");
    let mut offset = 4usize;
    loop {
        let header = flac_bytes[offset];
        let is_last = header & 0x80 != 0;
        let length = u32::from_be_bytes([
            0,
            flac_bytes[offset + 1],
            flac_bytes[offset + 2],
            flac_bytes[offset + 3],
        ]) as usize;
        offset += 4 + length;
        if is_last {
            return offset;
        }
    }
}

fn read_bits(bytes: &[u8], bit_offset: &mut usize, width: usize) -> u64 {
    let mut value = 0u64;
    for _ in 0..width {
        let byte = bytes[*bit_offset / 8];
        let bit = 7 - (*bit_offset % 8);
        value = (value << 1) | u64::from((byte >> bit) & 1);
        *bit_offset += 1;
    }
    value
}

fn decode_utf8_like_number(bytes: &[u8]) -> (u64, usize) {
    let first = bytes[0];
    let (length, mut value) = match first {
        0x00..=0x7f => (1usize, u64::from(first)),
        0xc0..=0xdf => (2, u64::from(first & 0x1f)),
        0xe0..=0xef => (3, u64::from(first & 0x0f)),
        0xf0..=0xf7 => (4, u64::from(first & 0x07)),
        0xf8..=0xfb => (5, u64::from(first & 0x03)),
        0xfc..=0xfd => (6, u64::from(first & 0x01)),
        0xfe => (7, 0),
        _ => panic!("invalid UTF-8-like FLAC coded number prefix"),
    };
    if length == 1 {
        return (value, 1);
    }
    for &byte in &bytes[1..length] {
        assert_eq!(byte & 0xc0, 0x80, "invalid UTF-8-like continuation byte");
        value = (value << 6) | u64::from(byte & 0x3f);
    }
    (value, length)
}

fn encode_utf8_like_number(value: u64) -> Vec<u8> {
    match value {
        0x0000_0000_0000..=0x0000_0000_007f => vec![value as u8],
        0x0000_0000_0080..=0x0000_0000_07ff => vec![
            0b1100_0000 | ((value >> 6) as u8 & 0b0001_1111),
            0b1000_0000 | (value as u8 & 0b0011_1111),
        ],
        0x0000_0000_0800..=0x0000_0000_ffff => vec![
            0b1110_0000 | ((value >> 12) as u8 & 0b0000_1111),
            0b1000_0000 | ((value >> 6) as u8 & 0b0011_1111),
            0b1000_0000 | (value as u8 & 0b0011_1111),
        ],
        0x0000_0001_0000..=0x0000_001f_ffff => vec![
            0b1111_0000 | ((value >> 18) as u8 & 0b0000_0111),
            0b1000_0000 | ((value >> 12) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 6) as u8 & 0b0011_1111),
            0b1000_0000 | (value as u8 & 0b0011_1111),
        ],
        0x0000_0020_0000..=0x0000_03ff_ffff => vec![
            0b1111_1000 | ((value >> 24) as u8 & 0b0000_0011),
            0b1000_0000 | ((value >> 18) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 12) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 6) as u8 & 0b0011_1111),
            0b1000_0000 | (value as u8 & 0b0011_1111),
        ],
        0x0000_0400_0000..=0x0000_7fff_ffff => vec![
            0b1111_1100 | ((value >> 30) as u8 & 0b0000_0001),
            0b1000_0000 | ((value >> 24) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 18) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 12) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 6) as u8 & 0b0011_1111),
            0b1000_0000 | (value as u8 & 0b0011_1111),
        ],
        0x0000_8000_0000..=0x000f_ffff_ffff => vec![
            0b1111_1110,
            0b1000_0000 | ((value >> 30) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 24) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 18) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 12) as u8 & 0b0011_1111),
            0b1000_0000 | ((value >> 6) as u8 & 0b0011_1111),
            0b1000_0000 | (value as u8 & 0b0011_1111),
        ],
        _ => panic!("coded number exceeds FLAC limit"),
    }
}

fn bytes_per_sample(bits_per_sample: u16) -> usize {
    match bits_per_sample {
        8 => 1,
        16 => 2,
        24 => 3,
        32 => 4,
        _ => unreachable!("unsupported bits per sample: {bits_per_sample}"),
    }
}

fn write_pcm_samples(bytes: &mut Vec<u8>, bits_per_sample: u16, samples: &[i32]) {
    for &sample in samples {
        match bits_per_sample {
            8 => bytes.push(sample as u8),
            16 => bytes.extend_from_slice(&(sample as i16).to_le_bytes()),
            24 => {
                let value = sample as u32;
                bytes.extend_from_slice(&[
                    (value & 0xff) as u8,
                    ((value >> 8) & 0xff) as u8,
                    ((value >> 16) & 0xff) as u8,
                ]);
            }
            32 => bytes.extend_from_slice(&sample.to_le_bytes()),
            _ => unreachable!("unsupported bits per sample: {bits_per_sample}"),
        }
    }
}

fn write_left_aligned_samples(
    bytes: &mut Vec<u8>,
    container_bits_per_sample: u16,
    valid_bits_per_sample: u16,
    samples: &[i32],
) {
    let shift = container_bits_per_sample - valid_bits_per_sample;
    for &sample in samples {
        let shifted = if shift == 0 { sample } else { sample << shift };
        match container_bits_per_sample {
            8 => bytes.push(shifted as u8),
            16 => bytes.extend_from_slice(&(shifted as i16).to_le_bytes()),
            24 => {
                let value = shifted as u32;
                bytes.extend_from_slice(&[
                    (value & 0xff) as u8,
                    ((value >> 8) & 0xff) as u8,
                    ((value >> 16) & 0xff) as u8,
                ]);
            }
            32 => bytes.extend_from_slice(&shifted.to_le_bytes()),
            _ => unreachable!("unsupported container bits per sample: {container_bits_per_sample}"),
        }
    }
}
