use std::{
    env, fs,
    io::Cursor,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use flacx::Encoder;

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

fn sample_fixture(channels: u16, frames: usize) -> Vec<i32> {
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

fn unique_temp_path(extension: &str) -> PathBuf {
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

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn decode_with_ffmpeg(flac_bytes: &[u8], bits_per_sample: u16) -> Vec<i32> {
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

#[test]
fn patches_streaminfo_after_encoding() {
    let samples = sample_fixture(2, 5_000);
    let wav = pcm_wav_bytes(16, 2, 44_100, &samples);
    let encoder = Encoder::default().with_threads(2);
    let flac = encoder.encode_wav_bytes(&wav).unwrap();

    assert_eq!(&flac[..4], b"fLaC");
    assert_eq!(&flac[4..8], &[0x80, 0x00, 0x00, 0x22]);
    let min_block = u16::from_be_bytes([flac[8], flac[9]]);
    let max_block = u16::from_be_bytes([flac[10], flac[11]]);
    let min_frame = u32::from_be_bytes([0, flac[12], flac[13], flac[14]]);
    let max_frame = u32::from_be_bytes([0, flac[15], flac[16], flac[17]]);
    let expected_block_size = encoder.config().block_size;

    assert_eq!(min_block, expected_block_size);
    assert_eq!(max_block, expected_block_size);
    assert!(min_frame > 0);
    assert!(max_frame >= min_frame);
}

#[test]
fn produces_identical_output_across_thread_counts() {
    let samples = sample_fixture(2, 8_192);
    let wav = pcm_wav_bytes(16, 2, 44_100, &samples);

    let single_threaded = Encoder::default()
        .with_threads(1)
        .encode_wav_bytes(&wav)
        .unwrap();
    let multi_threaded = Encoder::default()
        .with_threads(4)
        .encode_wav_bytes(&wav)
        .unwrap();

    assert_eq!(single_threaded, multi_threaded);
}

#[test]
fn round_trips_16bit_stereo_with_ffmpeg_oracle() {
    let samples = sample_fixture(2, 6_144);
    let wav = pcm_wav_bytes(16, 2, 44_100, &samples);
    let flac = Encoder::default()
        .with_threads(4)
        .encode_wav_bytes(&wav)
        .unwrap();

    let decoded = decode_with_ffmpeg(&flac, 16);
    assert_eq!(decoded, samples);
}

#[test]
fn round_trips_24bit_mono_with_ffmpeg_oracle() {
    let samples: Vec<i32> = (0..5_000)
        .map(|index| ((index as i32 * 9_731) % 16_000_000) - 8_000_000)
        .collect();
    let wav = pcm_wav_bytes(24, 1, 96_000, &samples);
    let flac = Encoder::default()
        .with_threads(3)
        .encode_wav_bytes(&wav)
        .unwrap();

    let decoded = decode_with_ffmpeg(&flac, 24);
    assert_eq!(decoded, samples);
}

#[test]
fn round_trips_constant_16bit_mono_with_ffmpeg_oracle() {
    let samples = vec![12_345; 6_144];
    let wav = pcm_wav_bytes(16, 1, 44_100, &samples);
    let flac = Encoder::default()
        .with_threads(2)
        .encode_wav_bytes(&wav)
        .unwrap();

    let decoded = decode_with_ffmpeg(&flac, 16);
    assert_eq!(decoded, samples);
}

#[test]
fn public_api_requires_seekable_io_but_accepts_cursor_inputs() {
    let samples = sample_fixture(1, 2_048);
    let wav = pcm_wav_bytes(16, 1, 32_000, &samples);
    let mut output = Cursor::new(Vec::new());

    let summary = Encoder::default()
        .with_threads(2)
        .encode_wav_to_flac(Cursor::new(wav), &mut output)
        .unwrap();

    assert_eq!(summary.total_samples, 2_048);
    assert!(summary.frame_count >= 1);
}
