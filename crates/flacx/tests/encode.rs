use std::io::Cursor;

use flacx::{EncodeOptions, FlacEncoder};

mod support;

use support::{decode_with_ffmpeg, pcm_wav_bytes, sample_fixture};

#[test]
fn patches_streaminfo_after_encoding() {
    let samples = sample_fixture(2, 5_000);
    let wav = pcm_wav_bytes(16, 2, 44_100, &samples);
    let encoder = FlacEncoder::new(EncodeOptions::default().with_threads(2));
    let flac = encoder.encode_bytes(&wav).unwrap();

    assert_eq!(&flac[..4], b"fLaC");
    assert_eq!(&flac[4..8], &[0x80, 0x00, 0x00, 0x22]);
    let min_block = u16::from_be_bytes([flac[8], flac[9]]);
    let max_block = u16::from_be_bytes([flac[10], flac[11]]);
    let min_frame = u32::from_be_bytes([0, flac[12], flac[13], flac[14]]);
    let max_frame = u32::from_be_bytes([0, flac[15], flac[16], flac[17]]);
    let expected_block_size = encoder.options().block_size;

    assert_eq!(min_block, expected_block_size);
    assert_eq!(max_block, expected_block_size);
    assert!(min_frame > 0);
    assert!(max_frame >= min_frame);
}

#[test]
fn produces_identical_output_across_thread_counts() {
    let samples = sample_fixture(2, 8_192);
    let wav = pcm_wav_bytes(16, 2, 44_100, &samples);

    let single_threaded = FlacEncoder::new(EncodeOptions::default().with_threads(1))
        .encode_bytes(&wav)
        .unwrap();
    let multi_threaded = FlacEncoder::new(EncodeOptions::default().with_threads(4))
        .encode_bytes(&wav)
        .unwrap();

    assert_eq!(single_threaded, multi_threaded);
}

#[test]
fn round_trips_16bit_stereo_with_ffmpeg_oracle() {
    let samples = sample_fixture(2, 6_144);
    let wav = pcm_wav_bytes(16, 2, 44_100, &samples);
    let flac = FlacEncoder::new(EncodeOptions::default().with_threads(4))
        .encode_bytes(&wav)
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
    let flac = FlacEncoder::new(EncodeOptions::default().with_threads(3))
        .encode_bytes(&wav)
        .unwrap();

    let decoded = decode_with_ffmpeg(&flac, 24);
    assert_eq!(decoded, samples);
}

#[test]
fn round_trips_constant_16bit_mono_with_ffmpeg_oracle() {
    let samples = vec![12_345; 6_144];
    let wav = pcm_wav_bytes(16, 1, 44_100, &samples);
    let flac = FlacEncoder::new(EncodeOptions::default().with_threads(2))
        .encode_bytes(&wav)
        .unwrap();

    let decoded = decode_with_ffmpeg(&flac, 16);
    assert_eq!(decoded, samples);
}

#[test]
fn public_api_requires_seekable_io_but_accepts_cursor_inputs() {
    let samples = sample_fixture(1, 2_048);
    let wav = pcm_wav_bytes(16, 1, 32_000, &samples);
    let mut output = Cursor::new(Vec::new());

    let summary = FlacEncoder::new(EncodeOptions::default().with_threads(2))
        .encode(Cursor::new(wav), &mut output)
        .unwrap();

    assert_eq!(summary.total_samples, 2_048);
    assert!(summary.frame_count >= 1);
}
