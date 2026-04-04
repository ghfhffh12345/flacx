use std::{fs, io::Cursor, thread::available_parallelism};

use flacx::{
    DecodeConfig, Decoder, Encoder, EncoderConfig, decode_bytes, decode_file, level::Level,
};

mod support;

use support::{
    corrupt_last_frame_crc, corrupt_magic, pcm_wav_bytes, sample_fixture, truncate_bytes,
    unique_temp_path,
};

fn decode_thread_variants() -> [usize; 2] {
    [1, DecodeConfig::default().threads.max(2)]
}

fn decoder_for_threads(threads: usize) -> Decoder {
    Decoder::new(DecodeConfig::default().with_threads(threads))
}

fn decode_bytes_with_threads(flac: &[u8], threads: usize) -> Vec<u8> {
    decoder_for_threads(threads).decode_bytes(flac).unwrap()
}

fn assert_round_trips_bytes_exactly(wav: &[u8], flac: &[u8]) {
    for threads in decode_thread_variants() {
        let decoded = decode_bytes_with_threads(flac, threads);
        assert_eq!(
            decoded, wav,
            "decode_bytes changed output for threads={threads}"
        );
    }
}

fn assert_decode_error_stable(flac: &[u8]) {
    let mut messages = Vec::new();
    for threads in decode_thread_variants() {
        let error = decoder_for_threads(threads).decode_bytes(flac).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("invalid flac") || message.contains("decode error"),
            "unexpected decode error for threads={threads}: {message}"
        );
        messages.push((threads, message));
    }

    let first = &messages[0].1;
    for (threads, message) in messages.iter().skip(1) {
        assert_eq!(
            message, first,
            "decode error changed across thread counts for threads={threads}"
        );
    }
}

#[test]
fn decode_config_default_threads_matches_available_parallelism() {
    let expected = available_parallelism().map(usize::from).unwrap_or(1);
    assert_eq!(DecodeConfig::default().threads, expected);
}

#[test]
fn decode_config_with_threads_clamps_to_one() {
    assert_eq!(DecodeConfig::default().with_threads(0).threads, 1);
}

#[test]
fn round_trips_16bit_mono_wav_bytes_exactly() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::new(EncoderConfig::default().with_level(Level::Level0))
        .encode_bytes(&wav)
        .unwrap();

    assert_round_trips_bytes_exactly(&wav, &flac);
}

#[test]
fn round_trips_16bit_stereo_wav_bytes_exactly() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 6_144));
    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&wav)
        .unwrap();

    assert_round_trips_bytes_exactly(&wav, &flac);
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

    assert_round_trips_bytes_exactly(&wav, &flac);
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

    assert_round_trips_bytes_exactly(&wav, &flac);
}

#[test]
fn round_trips_constant_signal_exactly() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &vec![12_345; 6_144]);
    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&wav)
        .unwrap();

    assert_round_trips_bytes_exactly(&wav, &flac);
}

#[test]
fn decode_file_writes_identical_wav_bytes() {
    let wav = pcm_wav_bytes(16, 1, 32_000, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    fs::write(&input_path, flac).unwrap();

    for threads in decode_thread_variants() {
        let output_path = unique_temp_path("wav");
        let summary = decoder_for_threads(threads)
            .decode_file(&input_path, &output_path)
            .unwrap();

        assert_eq!(summary.total_samples, 2_048);
        assert_eq!(
            fs::read(&output_path).unwrap(),
            wav,
            "decode_file changed output for threads={threads}"
        );

        let _ = fs::remove_file(output_path);
    }

    let _ = fs::remove_file(input_path);
}

#[test]
fn failed_decode_file_does_not_leave_accepted_wav_output() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 4_096));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("wav");
    let sentinel = b"keep-existing-output";
    fs::write(&input_path, corrupt_last_frame_crc(&flac)).unwrap();
    fs::write(&output_path, sentinel).unwrap();

    let error = decoder_for_threads(4)
        .decode_file(&input_path, &output_path)
        .unwrap_err();

    let message = error.to_string();
    assert!(
        message.contains("invalid flac") || message.contains("decode error"),
        "unexpected decode cleanup error: {message}"
    );
    assert_eq!(
        fs::read(&output_path).unwrap(),
        sentinel,
        "decode failure should not overwrite existing WAV output"
    );

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn free_decode_helpers_remain_functional() {
    let wav = pcm_wav_bytes(16, 1, 32_000, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    assert_eq!(decode_bytes(&flac).unwrap(), wav);

    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("wav");
    fs::write(&input_path, &flac).unwrap();

    let summary = decode_file(&input_path, &output_path).unwrap();
    assert_eq!(summary.total_samples, 2_048);
    assert_eq!(fs::read(&output_path).unwrap(), wav);

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn rejects_invalid_flac_magic() {
    assert_decode_error_stable(&corrupt_magic(b"fLaC"));
}

#[test]
fn rejects_truncated_streaminfo() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 128));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    assert_decode_error_stable(&truncate_bytes(&flac, 12));
}

#[test]
fn rejects_bad_frame_crc() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    assert_decode_error_stable(&corrupt_last_frame_crc(&flac));
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
