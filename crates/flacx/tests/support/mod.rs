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
