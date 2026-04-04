use std::{fs, io::Cursor};

use flacx::{Decoder, Encoder, EncoderConfig, level::Level};

mod support;

use support::{
    corrupt_last_frame_crc, corrupt_magic, pcm_wav_bytes, sample_fixture, truncate_bytes,
    unique_temp_path,
};

#[test]
fn round_trips_16bit_mono_wav_bytes_exactly() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::new(EncoderConfig::default().with_level(Level::Level0))
        .encode_bytes(&wav)
        .unwrap();

    let decoded = Decoder::default().decode_bytes(&flac).unwrap();
    assert_eq!(decoded, wav);
}

#[test]
fn round_trips_16bit_stereo_wav_bytes_exactly() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 6_144));
    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&wav)
        .unwrap();

    let decoded = Decoder::default().decode_bytes(&flac).unwrap();
    assert_eq!(decoded, wav);
}

#[test]
fn round_trips_24bit_mono_wav_bytes_exactly() {
    let samples: Vec<i32> = (0..5_000)
        .map(|index| ((index as i32 * 9_731) % 16_000_000) - 8_000_000)
        .collect();
    let wav = pcm_wav_bytes(24, 1, 96_000, &samples);
    let flac = Encoder::new(EncoderConfig::default().with_threads(3))
        .encode_bytes(&wav)
        .unwrap();

    let decoded = Decoder::default().decode_bytes(&flac).unwrap();
    assert_eq!(decoded, wav);
}

#[test]
fn round_trips_partial_tail_block_exactly() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 5_111));
    let flac = Encoder::new(
        EncoderConfig::default()
            .with_level(Level::Level0)
            .with_block_size(576),
    )
    .encode_bytes(&wav)
    .unwrap();

    let decoded = Decoder::default().decode_bytes(&flac).unwrap();
    assert_eq!(decoded, wav);
}

#[test]
fn round_trips_constant_signal_exactly() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &vec![12_345; 6_144]);
    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&wav)
        .unwrap();

    let decoded = Decoder::default().decode_bytes(&flac).unwrap();
    assert_eq!(decoded, wav);
}

#[test]
fn decode_file_writes_identical_wav_bytes() {
    let wav = pcm_wav_bytes(16, 1, 32_000, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("wav");
    fs::write(&input_path, flac).unwrap();

    let summary = Decoder::default()
        .decode_file(&input_path, &output_path)
        .unwrap();

    assert_eq!(summary.total_samples, 2_048);
    assert_eq!(fs::read(&output_path).unwrap(), wav);

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn rejects_invalid_flac_magic() {
    let error = Decoder::default()
        .decode_bytes(&corrupt_magic(b"fLaC"))
        .unwrap_err();
    assert!(error.to_string().contains("invalid flac"));
}

#[test]
fn rejects_truncated_streaminfo() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 128));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let error = Decoder::default()
        .decode_bytes(&truncate_bytes(&flac, 12))
        .unwrap_err();
    assert!(error.to_string().contains("invalid flac"));
}

#[test]
fn rejects_bad_frame_crc() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let error = Decoder::default()
        .decode_bytes(&corrupt_last_frame_crc(&flac))
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("invalid flac") || message.contains("decode error"));
}

#[test]
fn decode_uses_seekable_io() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 512));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let mut output = Cursor::new(Vec::new());

    let summary = Decoder::default()
        .decode(Cursor::new(flac), &mut output)
        .unwrap();

    assert_eq!(summary.total_samples, 512);
    assert_eq!(output.into_inner(), wav);
}
