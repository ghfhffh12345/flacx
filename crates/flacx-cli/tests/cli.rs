use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use flacx::{DecodeConfig, Encoder, EncoderConfig, level::Level};
use flacx_cli::{DecodeCommand, EncodeCommand, decode_command, encode_command};

#[path = "../../flacx/tests/support/mod.rs"]
mod support;

use support::{pcm_wav_bytes, sample_fixture, unique_temp_path};

fn flacx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_flacx")
}

fn unique_temp_dir() -> PathBuf {
    let path = unique_temp_path("dir");
    fs::create_dir_all(&path).unwrap();
    path
}

fn write_wav_file(path: &Path, channels: u16, frames: usize) -> Vec<u8> {
    let wav = pcm_wav_bytes(16, channels, 44_100, &sample_fixture(channels, frames));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, &wav).unwrap();
    wav
}

fn write_flac_file(path: &Path, channels: u16, frames: usize) -> (Vec<u8>, Vec<u8>) {
    let wav = pcm_wav_bytes(16, channels, 44_100, &sample_fixture(channels, frames));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, &flac).unwrap();
    (wav, flac)
}

fn final_progress_frame_lines(stderr: &str) -> Vec<&str> {
    stderr
        .trim_end_matches('\n')
        .rsplit("\x1b[1A")
        .next()
        .unwrap_or(stderr)
        .split('\n')
        .map(|line| line.trim_start_matches('\r'))
        .collect()
}

#[test]
fn help_lists_encode_and_decode_commands() {
    let output = Command::new(flacx_bin()).arg("--help").output().unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("encode"));
    assert!(stdout.contains("decode"));
    assert!(stdout.contains("--help"));
}

#[test]
fn encode_help_lists_output_depth_and_default_threads() {
    let output = Command::new(flacx_bin())
        .args(["encode", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("-o, --output <OUTPUT>"));
    assert!(stdout.contains("--depth <DEPTH>"));
    assert!(stdout.contains("[default: 8]"));
    assert!(stdout.contains("only applies when the input is a directory"));
}

#[test]
fn decode_help_lists_output_depth_and_threads() {
    let output = Command::new(flacx_bin())
        .args(["decode", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("-o, --output <OUTPUT>"));
    assert!(stdout.contains("--depth <DEPTH>"));
    assert!(stdout.contains("--threads <THREADS>"));
}

#[test]
fn version_reports_workspace_version() {
    let output = Command::new(flacx_bin()).arg("--version").output().unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        format!("flacx {}", env!("CARGO_PKG_VERSION"))
    );
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
            "-o",
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
fn encode_command_emits_progress_trace_when_requested() {
    let input_dir = unique_temp_dir();
    let input_path = input_dir.join("input.wav");
    write_wav_file(&input_path, 1, 2_048);
    let output_path = input_dir.join("input.flac");
    let trace_path = input_dir.join("progress.trace");

    let output = Command::new(flacx_bin())
        .env("FLACX_PROGRESS_TRACE", &trace_path)
        .args([
            "encode",
            input_path.to_str().unwrap(),
            "-o",
            output_path.to_str().unwrap(),
            "--threads",
            "1",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let trace = fs::read_to_string(&trace_path).unwrap();
    assert!(trace.contains("event=command"));
    assert!(trace.contains("kind=encode"));
    assert!(trace.contains("interactive=0"));
    assert!(trace.contains("batch_mode=0"));
    assert!(trace.contains("event=file_finish"));
    assert!(trace.contains("filename=input.wav"));

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn encode_command_renders_filename_elapsed_and_progress_when_interactive() {
    let samples = sample_fixture(1, 2_048);
    let wav = pcm_wav_bytes(16, 1, 44_100, &samples);
    let input_path = unique_temp_path("wav");
    let output_path = unique_temp_path("flac");
    fs::write(&input_path, &wav).unwrap();

    let command = EncodeCommand {
        input: input_path.clone(),
        output: Some(output_path.clone()),
        depth: 1,
        config: EncoderConfig::default()
            .with_level(Level::Level0)
            .with_threads(1)
            .with_block_size(576),
    };
    let mut stderr = Vec::new();

    encode_command(&command, true, &mut stderr).unwrap();

    let stderr = String::from_utf8(stderr).unwrap();
    assert!(stderr.contains('\r'));
    assert!(
        stderr.contains("input.wav")
            || stderr.contains(input_path.file_name().unwrap().to_str().unwrap())
    );
    assert!(stderr.contains("100.0%"));
    assert!(stderr.contains("Elapsed"));
    assert!(stderr.contains("ETA"));
    assert!(stderr.contains("Rate"));
    assert!(stderr.ends_with('\n'));

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn encode_directory_progress_shows_overall_and_file_progress() {
    let input_dir = unique_temp_dir();
    let first = input_dir.join("disc1").join("first.wav");
    let second = input_dir.join("disc1").join("second.wav");
    write_wav_file(&first, 1, 1_024);
    write_wav_file(&second, 1, 2_048);

    let command = EncodeCommand {
        input: input_dir.clone(),
        output: None,
        depth: 0,
        config: EncoderConfig::default().with_threads(1),
    };
    let mut stderr = Vec::new();

    encode_command(&command, true, &mut stderr).unwrap();

    let stderr = String::from_utf8(stderr).unwrap();
    let final_lines = final_progress_frame_lines(&stderr);
    assert_eq!(final_lines.len(), 2);
    assert!(final_lines[0].starts_with("Batch | "));
    assert!(final_lines[0].contains("Elapsed"));
    assert!(final_lines[0].contains("ETA"));
    assert!(final_lines[0].contains("Rate"));
    assert!(
        final_lines[1].contains("disc1/first.wav | File | ")
            || final_lines[1].contains("disc1/second.wav | File | ")
    );
    assert!(final_lines[1].contains("Elapsed"));
    assert!(final_lines[1].contains("ETA"));
    assert!(final_lines[1].contains("Rate"));

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn encode_single_file_directory_still_uses_batch_progress_layout() {
    let input_dir = unique_temp_dir();
    let wav_path = input_dir.join("only.wav");
    write_wav_file(&wav_path, 1, 2_048);

    let command = EncodeCommand {
        input: input_dir.clone(),
        output: None,
        depth: 0,
        config: EncoderConfig::default().with_threads(1),
    };
    let mut stderr = Vec::new();

    encode_command(&command, true, &mut stderr).unwrap();

    let stderr = String::from_utf8(stderr).unwrap();
    let final_lines = final_progress_frame_lines(&stderr);
    assert_eq!(final_lines.len(), 2);
    assert!(final_lines[0].starts_with("Batch | "));
    assert!(final_lines[1].contains("only.wav | File | "));

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn encode_single_match_folder_still_uses_batch_progress_layout() {
    let input_dir = unique_temp_dir();
    let only = input_dir.join("disc1").join("only.wav");
    write_wav_file(&only, 1, 1_024);

    let command = EncodeCommand {
        input: input_dir.clone(),
        output: None,
        depth: 0,
        config: EncoderConfig::default().with_threads(1),
    };
    let mut stderr = Vec::new();

    encode_command(&command, true, &mut stderr).unwrap();

    let stderr = String::from_utf8(stderr).unwrap();
    let final_lines = final_progress_frame_lines(&stderr);
    assert_eq!(final_lines.len(), 2);
    assert!(final_lines[0].starts_with("Batch | "));
    assert!(final_lines[1].contains("disc1/only.wav | File | "));

    let _ = fs::remove_dir_all(input_dir);
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
            "-o",
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
fn encode_command_without_output_writes_sibling_flac() {
    let input_dir = unique_temp_dir();
    let input_path = input_dir.join("input.wav");
    let wav = write_wav_file(&input_path, 1, 2_048);
    let output_path = input_dir.join("input.flac");

    let output = Command::new(flacx_bin())
        .args(["encode", input_path.to_str().unwrap(), "--threads", "1"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(&output_path).unwrap(),
        Encoder::new(EncoderConfig::default().with_threads(1))
            .encode_bytes(&wav)
            .unwrap()
    );

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn encode_directory_without_output_writes_sibling_flacs_at_default_depth() {
    let input_dir = unique_temp_dir();
    let top_wav = input_dir.join("top.wav");
    let nested_wav = input_dir.join("nested").join("deep.wav");
    write_wav_file(&top_wav, 1, 2_048);
    write_wav_file(&nested_wav, 1, 2_048);

    let output = Command::new(flacx_bin())
        .args(["encode", input_dir.to_str().unwrap(), "--threads", "1"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(input_dir.join("top.flac").exists());
    assert!(!input_dir.join("nested").join("deep.flac").exists());

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn encode_directory_with_output_root_preserves_relative_subpaths_and_creates_parents() {
    let input_dir = unique_temp_dir();
    let output_dir = unique_temp_path("outdir");
    let nested_wav = input_dir.join("disc1").join("set").join("song.wav");
    write_wav_file(&nested_wav, 1, 2_048);

    let output = Command::new(flacx_bin())
        .args([
            "encode",
            input_dir.to_str().unwrap(),
            "-o",
            output_dir.to_str().unwrap(),
            "--threads",
            "1",
            "--depth",
            "0",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(
        output_dir
            .join("disc1")
            .join("set")
            .join("song.flac")
            .exists()
    );

    let _ = fs::remove_dir_all(input_dir);
    let _ = fs::remove_dir_all(output_dir);
}

#[test]
fn encode_directory_depth_zero_includes_nested_descendants() {
    let input_dir = unique_temp_dir();
    let top_wav = input_dir.join("top.wav");
    let nested_wav = input_dir.join("nested").join("deep.wav");
    write_wav_file(&top_wav, 1, 2_048);
    write_wav_file(&nested_wav, 1, 2_048);

    let output = Command::new(flacx_bin())
        .args([
            "encode",
            input_dir.to_str().unwrap(),
            "--threads",
            "1",
            "--depth",
            "0",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(input_dir.join("top.flac").exists());
    assert!(input_dir.join("nested").join("deep.flac").exists());

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn encode_directory_depth_two_includes_one_nested_level_only() {
    let input_dir = unique_temp_dir();
    let top_wav = input_dir.join("top.wav");
    let nested_wav = input_dir.join("nested").join("deep.wav");
    let deeper_wav = input_dir.join("nested").join("deeper").join("skip.wav");
    write_wav_file(&top_wav, 1, 2_048);
    write_wav_file(&nested_wav, 1, 2_048);
    write_wav_file(&deeper_wav, 1, 2_048);

    let output = Command::new(flacx_bin())
        .args([
            "encode",
            input_dir.to_str().unwrap(),
            "--threads",
            "1",
            "--depth",
            "2",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(input_dir.join("top.flac").exists());
    assert!(input_dir.join("nested").join("deep.flac").exists());
    assert!(
        !input_dir
            .join("nested")
            .join("deeper")
            .join("skip.flac")
            .exists()
    );

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn encode_directory_skips_non_wav_files() {
    let input_dir = unique_temp_dir();
    let wav_path = input_dir.join("keep.wav");
    let txt_path = input_dir.join("ignore.txt");
    write_wav_file(&wav_path, 1, 2_048);
    fs::write(&txt_path, b"not audio").unwrap();

    let output = Command::new(flacx_bin())
        .args(["encode", input_dir.to_str().unwrap(), "--threads", "1"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(input_dir.join("keep.flac").exists());
    assert!(!input_dir.join("ignore.flac").exists());

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn encode_directory_rejects_output_file_path() {
    let input_dir = unique_temp_dir();
    let output_path = unique_temp_path("flac");
    write_wav_file(&input_dir.join("song.wav"), 1, 2_048);
    fs::write(&output_path, b"existing file").unwrap();

    let output = Command::new(flacx_bin())
        .args([
            "encode",
            input_dir.to_str().unwrap(),
            "-o",
            output_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not a directory"));
    assert!(!input_dir.join("song.flac").exists());

    let _ = fs::remove_dir_all(input_dir);
    let _ = fs::remove_file(output_path);
}

#[test]
fn encode_directory_validates_exact_batch_totals_before_dispatch() {
    let input_dir = unique_temp_dir();
    let output_dir = unique_temp_path("outdir");
    write_wav_file(&input_dir.join("a-good.wav"), 1, 2_048);
    fs::write(input_dir.join("b-bad.wav"), b"not a wav").unwrap();
    write_wav_file(&input_dir.join("c-good.wav"), 1, 2_048);

    let output = Command::new(flacx_bin())
        .args([
            "encode",
            input_dir.to_str().unwrap(),
            "-o",
            output_dir.to_str().unwrap(),
            "--threads",
            "1",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(!output_dir.join("a-good.flac").exists());
    assert!(!output_dir.join("c-good.flac").exists());

    let _ = fs::remove_dir_all(input_dir);
    let _ = fs::remove_dir_all(output_dir);
}

#[test]
fn decode_command_accepts_threads_and_round_trips_exact_wav_bytes() {
    let samples = sample_fixture(2, 4_096);
    let wav = pcm_wav_bytes(16, 2, 44_100, &samples);
    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&wav)
        .unwrap();
    for threads in [1, 4] {
        let input_path = unique_temp_path("flac");
        let output_path = unique_temp_path("wav");
        fs::write(&input_path, &flac).unwrap();

        let threads_arg = threads.to_string();
        let input_arg = input_path.to_str().unwrap().to_owned();
        let output_arg = output_path.to_str().unwrap().to_owned();
        let output = Command::new(flacx_bin())
            .args([
                "decode",
                "--threads",
                threads_arg.as_str(),
                input_arg.as_str(),
                "-o",
                output_arg.as_str(),
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
}

#[test]
fn decode_command_function_renders_filename_elapsed_and_progress_when_interactive() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("wav");
    fs::write(&input_path, &flac).unwrap();

    let command = DecodeCommand {
        input: input_path.clone(),
        output: Some(output_path.clone()),
        depth: 1,
        config: DecodeConfig::default().with_threads(4),
    };
    let mut stderr = Vec::new();

    decode_command(&command, true, &mut stderr).unwrap();

    let stderr = String::from_utf8(stderr).unwrap();
    assert!(stderr.contains('\r'));
    assert!(stderr.contains("Elapsed"));
    assert!(stderr.contains("ETA"));
    assert!(stderr.contains("Rate"));
    assert!(stderr.contains("100.0%"));
    assert_eq!(fs::read(&output_path).unwrap(), wav);

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn decode_directory_progress_shows_overall_and_file_progress() {
    let input_dir = unique_temp_dir();
    let first = input_dir.join("disc1").join("first.flac");
    let second = input_dir.join("disc1").join("second.flac");
    write_flac_file(&first, 1, 1_024);
    write_flac_file(&second, 1, 2_048);

    let command = DecodeCommand {
        input: input_dir.clone(),
        output: None,
        depth: 0,
        config: DecodeConfig::default().with_threads(1),
    };
    let mut stderr = Vec::new();

    decode_command(&command, true, &mut stderr).unwrap();

    let stderr = String::from_utf8(stderr).unwrap();
    let final_lines = final_progress_frame_lines(&stderr);
    assert_eq!(final_lines.len(), 2);
    assert!(final_lines[0].starts_with("Batch | "));
    assert!(final_lines[0].contains("Elapsed"));
    assert!(final_lines[0].contains("ETA"));
    assert!(final_lines[0].contains("Rate"));
    assert!(
        final_lines[1].contains("disc1/first.flac | File | ")
            || final_lines[1].contains("disc1/second.flac | File | ")
    );
    assert!(final_lines[1].contains("Elapsed"));
    assert!(final_lines[1].contains("ETA"));
    assert!(final_lines[1].contains("Rate"));

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn decode_single_file_directory_still_uses_batch_progress_layout() {
    let input_dir = unique_temp_dir();
    let flac_path = input_dir.join("only.flac");
    write_flac_file(&flac_path, 1, 2_048);

    let command = DecodeCommand {
        input: input_dir.clone(),
        output: None,
        depth: 0,
        config: DecodeConfig::default().with_threads(1),
    };
    let mut stderr = Vec::new();

    decode_command(&command, true, &mut stderr).unwrap();

    let stderr = String::from_utf8(stderr).unwrap();
    let final_lines = final_progress_frame_lines(&stderr);
    assert_eq!(final_lines.len(), 2);
    assert!(final_lines[0].starts_with("Batch | "));
    assert!(final_lines[1].contains("only.flac | File | "));

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn decode_single_match_folder_still_uses_batch_progress_layout() {
    let input_dir = unique_temp_dir();
    let only = input_dir.join("disc1").join("only.flac");
    write_flac_file(&only, 1, 1_024);

    let command = DecodeCommand {
        input: input_dir.clone(),
        output: None,
        depth: 0,
        config: DecodeConfig::default().with_threads(1),
    };
    let mut stderr = Vec::new();

    decode_command(&command, true, &mut stderr).unwrap();

    let stderr = String::from_utf8(stderr).unwrap();
    let final_lines = final_progress_frame_lines(&stderr);
    assert_eq!(final_lines.len(), 2);
    assert!(final_lines[0].starts_with("Batch | "));
    assert!(final_lines[1].contains("disc1/only.flac | File | "));

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn decode_command_function_is_silent_when_non_interactive() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("wav");
    fs::write(&input_path, &flac).unwrap();

    let command = DecodeCommand {
        input: input_path.clone(),
        output: Some(output_path.clone()),
        depth: 1,
        config: DecodeConfig::default().with_threads(2),
    };
    let mut stderr = Vec::new();

    decode_command(&command, false, &mut stderr).unwrap();
    assert!(stderr.is_empty());
    assert_eq!(fs::read(&output_path).unwrap(), wav);

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn decode_command_rejects_invalid_flac_input() {
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("wav");
    let sentinel = b"keep-existing-output";
    fs::write(&input_path, b"not a flac").unwrap();
    fs::write(&output_path, sentinel).unwrap();

    let output = Command::new(flacx_bin())
        .args([
            "decode",
            "--threads",
            "4",
            input_path.to_str().unwrap(),
            "-o",
            output_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid flac") || stderr.contains("unsupported flac"));
    assert_eq!(fs::read(&output_path).unwrap(), sentinel);

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn decode_command_without_output_writes_sibling_wav() {
    let input_dir = unique_temp_dir();
    let input_path = input_dir.join("input.flac");
    let (wav, _) = write_flac_file(&input_path, 1, 2_048);
    let output_path = input_dir.join("input.wav");

    let output = Command::new(flacx_bin())
        .args(["decode", input_path.to_str().unwrap(), "--threads", "1"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(fs::read(&output_path).unwrap(), wav);

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn decode_directory_without_output_writes_sibling_wavs_at_default_depth() {
    let input_dir = unique_temp_dir();
    let top_flac = input_dir.join("top.flac");
    let nested_flac = input_dir.join("nested").join("deep.flac");
    write_flac_file(&top_flac, 1, 2_048);
    write_flac_file(&nested_flac, 1, 2_048);

    let output = Command::new(flacx_bin())
        .args(["decode", input_dir.to_str().unwrap(), "--threads", "1"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(input_dir.join("top.wav").exists());
    assert!(!input_dir.join("nested").join("deep.wav").exists());

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn decode_directory_with_output_root_preserves_relative_subpaths_and_creates_parents() {
    let input_dir = unique_temp_dir();
    let output_dir = unique_temp_path("outdir");
    let nested_flac = input_dir.join("disc1").join("set").join("song.flac");
    let (wav, _) = write_flac_file(&nested_flac, 1, 2_048);

    let output = Command::new(flacx_bin())
        .args([
            "decode",
            input_dir.to_str().unwrap(),
            "-o",
            output_dir.to_str().unwrap(),
            "--threads",
            "1",
            "--depth",
            "0",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        fs::read(output_dir.join("disc1").join("set").join("song.wav")).unwrap(),
        wav
    );

    let _ = fs::remove_dir_all(input_dir);
    let _ = fs::remove_dir_all(output_dir);
}

#[test]
fn decode_directory_depth_zero_includes_nested_descendants() {
    let input_dir = unique_temp_dir();
    let top_flac = input_dir.join("top.flac");
    let nested_flac = input_dir.join("nested").join("deep.flac");
    write_flac_file(&top_flac, 1, 2_048);
    write_flac_file(&nested_flac, 1, 2_048);

    let output = Command::new(flacx_bin())
        .args([
            "decode",
            input_dir.to_str().unwrap(),
            "--threads",
            "1",
            "--depth",
            "0",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(input_dir.join("top.wav").exists());
    assert!(input_dir.join("nested").join("deep.wav").exists());

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn decode_directory_depth_two_includes_one_nested_level_only() {
    let input_dir = unique_temp_dir();
    let top_flac = input_dir.join("top.flac");
    let nested_flac = input_dir.join("nested").join("deep.flac");
    let deeper_flac = input_dir.join("nested").join("deeper").join("skip.flac");
    write_flac_file(&top_flac, 1, 2_048);
    write_flac_file(&nested_flac, 1, 2_048);
    write_flac_file(&deeper_flac, 1, 2_048);

    let output = Command::new(flacx_bin())
        .args([
            "decode",
            input_dir.to_str().unwrap(),
            "--threads",
            "1",
            "--depth",
            "2",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(input_dir.join("top.wav").exists());
    assert!(input_dir.join("nested").join("deep.wav").exists());
    assert!(
        !input_dir
            .join("nested")
            .join("deeper")
            .join("skip.wav")
            .exists()
    );

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn decode_directory_skips_non_flac_files() {
    let input_dir = unique_temp_dir();
    let flac_path = input_dir.join("keep.flac");
    let txt_path = input_dir.join("ignore.txt");
    write_flac_file(&flac_path, 1, 2_048);
    fs::write(&txt_path, b"not audio").unwrap();

    let output = Command::new(flacx_bin())
        .args(["decode", input_dir.to_str().unwrap(), "--threads", "1"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(input_dir.join("keep.wav").exists());
    assert!(!input_dir.join("ignore.wav").exists());

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn decode_directory_rejects_output_file_path() {
    let input_dir = unique_temp_dir();
    let output_path = unique_temp_path("wav");
    write_flac_file(&input_dir.join("song.flac"), 1, 2_048);
    fs::write(&output_path, b"existing file").unwrap();

    let output = Command::new(flacx_bin())
        .args([
            "decode",
            input_dir.to_str().unwrap(),
            "-o",
            output_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not a directory"));
    assert!(!input_dir.join("song.wav").exists());

    let _ = fs::remove_dir_all(input_dir);
    let _ = fs::remove_file(output_path);
}

#[test]
fn decode_directory_validates_exact_batch_totals_before_dispatch() {
    let input_dir = unique_temp_dir();
    let output_dir = unique_temp_path("outdir");
    write_flac_file(&input_dir.join("a-good.flac"), 1, 2_048);
    fs::write(input_dir.join("b-bad.flac"), b"not a flac").unwrap();
    write_flac_file(&input_dir.join("c-good.flac"), 1, 2_048);

    let output = Command::new(flacx_bin())
        .args([
            "decode",
            input_dir.to_str().unwrap(),
            "-o",
            output_dir.to_str().unwrap(),
            "--threads",
            "1",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(!output_dir.join("a-good.wav").exists());
    assert!(!output_dir.join("c-good.wav").exists());

    let _ = fs::remove_dir_all(input_dir);
    let _ = fs::remove_dir_all(output_dir);
}
