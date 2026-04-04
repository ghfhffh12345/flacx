use std::{fs, process::Command};

use flacx::{Decoder, Encoder, EncoderConfig, level::Level};
use flacx_cli::{DecodeCommand, EncodeCommand, decode_command, encode_command};

#[path = "../../flacx/tests/support/mod.rs"]
mod support;

use support::{pcm_wav_bytes, sample_fixture, unique_temp_path};

fn flacx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_flacx")
}

#[test]
fn help_lists_encode_command() {
    let output = Command::new(flacx_bin()).arg("--help").output().unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("encode"));
    assert!(stdout.contains("decode"));
    assert!(stdout.contains("--help"));
}

#[test]
fn encode_command_matches_library_output() {
    let samples = sample_fixture(1, 2_048);
    let wav = pcm_wav_bytes(16, 1, 44_100, &samples);
    let input_path = unique_temp_path("wav");
    let output_path = unique_temp_path("flac");
    fs::write(&input_path, &wav).unwrap();

    let output = Command::new(flacx_bin())
        .args([
            "encode",
            input_path.to_str().unwrap(),
            output_path.to_str().unwrap(),
            "--level",
            "0",
            "--threads",
            "1",
            "--block-size",
            "576",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains('\r'),
        "non-interactive stderr should not contain live progress output"
    );

    let cli_bytes = fs::read(&output_path).unwrap();
    let library_bytes = Encoder::new(
        EncoderConfig::default()
            .with_level(Level::Level0)
            .with_threads(1)
            .with_block_size(576),
    )
    .encode_bytes(&wav)
    .unwrap();
    assert_eq!(cli_bytes, library_bytes);

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn encode_command_renders_progress_bar_when_interactive() {
    let samples = sample_fixture(1, 2_048);
    let wav = pcm_wav_bytes(16, 1, 44_100, &samples);
    let input_path = unique_temp_path("wav");
    let output_path = unique_temp_path("flac");
    fs::write(&input_path, &wav).unwrap();

    let command = EncodeCommand {
        input: input_path.clone(),
        output: output_path.clone(),
        config: EncoderConfig::default()
            .with_level(Level::Level0)
            .with_threads(1)
            .with_block_size(576),
    };
    let mut stderr = Vec::new();

    encode_command(&command, true, &mut stderr).unwrap();

    let stderr = String::from_utf8(stderr).unwrap();
    assert!(stderr.contains('\r'));
    assert!(stderr.contains("100.0%"));
    assert!(stderr.contains("ETA"));
    assert!(stderr.contains("Rate"));
    assert!(stderr.ends_with('\n'));

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn encode_command_rejects_invalid_wav_input() {
    let input_path = unique_temp_path("wav");
    let output_path = unique_temp_path("flac");
    fs::write(&input_path, b"not a wav").unwrap();

    let output = Command::new(flacx_bin())
        .args([
            "encode",
            input_path.to_str().unwrap(),
            output_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid wav") || stderr.contains("unsupported wav"));

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn decode_command_matches_original_wav_bytes() {
    let samples = sample_fixture(2, 4_096);
    let wav = pcm_wav_bytes(16, 2, 44_100, &samples);
    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&wav)
        .unwrap();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("wav");
    fs::write(&input_path, &flac).unwrap();

    let output = Command::new(flacx_bin())
        .args([
            "decode",
            input_path.to_str().unwrap(),
            output_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains('\r'),
        "decode should not emit progress output"
    );
    assert_eq!(fs::read(&output_path).unwrap(), wav);

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn decode_command_function_matches_library_output() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("wav");
    fs::write(&input_path, &flac).unwrap();

    let command = DecodeCommand {
        input: input_path.clone(),
        output: output_path.clone(),
    };

    decode_command(&command).unwrap();

    let cli_bytes = fs::read(&output_path).unwrap();
    let library_bytes = Decoder::default().decode_bytes(&flac).unwrap();
    assert_eq!(cli_bytes, library_bytes);
    assert_eq!(cli_bytes, wav);

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn decode_command_rejects_invalid_flac_input() {
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("wav");
    fs::write(&input_path, b"not a flac").unwrap();

    let output = Command::new(flacx_bin())
        .args([
            "decode",
            input_path.to_str().unwrap(),
            output_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid flac") || stderr.contains("unsupported flac"));

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}
