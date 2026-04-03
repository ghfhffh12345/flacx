use std::{fs, process::Command};

use flacx::{EncodeOptions, FlacEncoder, level::Level};

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

    let cli_bytes = fs::read(&output_path).unwrap();
    let library_bytes = FlacEncoder::new(
        EncodeOptions::default()
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
