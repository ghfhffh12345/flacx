use std::{fs, io::Cursor};

use flacx::{Encoder, EncoderConfig, encode_bytes, encode_file, level::Level};

mod support;

use support::{pcm_wav_bytes, sample_fixture, unique_temp_path};

#[test]
fn top_level_encode_bytes_matches_default_encoder() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let via_function = encode_bytes(&wav).unwrap();
    let via_encoder = Encoder::default().encode_bytes(&wav).unwrap();
    assert_eq!(via_function, via_encoder);
}

#[test]
fn encode_file_uses_configured_options() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 4_096));
    let input_path = unique_temp_path("wav");
    let output_path = unique_temp_path("flac");
    fs::write(&input_path, &wav).unwrap();

    let summary = Encoder::new(
        EncoderConfig::default()
            .with_level(Level::Level0)
            .with_threads(1)
            .with_block_size(576),
    )
    .encode_file(&input_path, &output_path)
    .unwrap();

    assert_eq!(summary.block_size, 576);
    assert_eq!(summary.channels, 2);
    assert!(output_path.exists());

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn convenience_encode_file_matches_default_encoder_output() {
    let wav = pcm_wav_bytes(16, 1, 32_000, &sample_fixture(1, 2_048));
    let input_path = unique_temp_path("wav");
    let output_path = unique_temp_path("flac");
    fs::write(&input_path, &wav).unwrap();

    let summary = encode_file(&input_path, &output_path).unwrap();
    let bytes_from_file = fs::read(&output_path).unwrap();
    let bytes_from_memory = Encoder::default().encode_bytes(&wav).unwrap();

    assert_eq!(summary.total_samples, 2_048);
    assert_eq!(bytes_from_file, bytes_from_memory);

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn api_accepts_seekable_readers_and_writers() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 1_024));
    let mut output = Cursor::new(Vec::new());
    let summary = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode(Cursor::new(wav), &mut output)
        .unwrap();

    assert!(summary.frame_count >= 1);
    assert!(output.get_ref().starts_with(b"fLaC"));
}
