use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use flacx::{
    DecodeConfig, EncoderConfig, RecompressConfig, Recompressor, builtin::decode_bytes,
    level::Level,
};
use flacx_cli::{
    DecodeCommand, EncodeCommand, RecompressCommand, decode_command, encode_command,
    recompress_command,
};

#[path = "../../flacx/tests/support/mod.rs"]
mod support;

use support::TestEncoder as Encoder;
use support::{
    aifc_pcm_bytes, aiff_pcm_bytes, caf_lpcm_bytes, cuesheet_block, extensible_pcm_wav_bytes,
    pcm_wav_bytes, raw_pcm_bytes, raw_seektable_block, replace_flac_optional_metadata,
    rf64_pcm_wav_bytes, sample_fixture, unique_temp_path, vorbis_comment_block, w64_pcm_wav_bytes,
    wav_chunk_payloads, wav_data_bytes,
};

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

fn write_bytes_file(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
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

fn assert_wav_audio_eq(actual: &[u8], expected: &[u8]) {
    assert_eq!(wav_data_bytes(actual), wav_data_bytes(expected));
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

fn decode_cli_output(input_path: &Path, output_path: &Path, args: &[&str]) -> Output {
    let mut command_args = vec!["decode"];
    command_args.extend_from_slice(args);
    command_args.push(input_path.to_str().unwrap());
    command_args.push("-o");
    command_args.push(output_path.to_str().unwrap());
    Command::new(flacx_bin())
        .args(command_args)
        .output()
        .unwrap()
}

fn encode_cli_output(input_path: &Path, output_path: &Path, args: &[&str]) -> Output {
    let mut command_args = vec!["encode"];
    command_args.extend_from_slice(args);
    command_args.push(input_path.to_str().unwrap());
    command_args.push("-o");
    command_args.push(output_path.to_str().unwrap());
    Command::new(flacx_bin())
        .args(command_args)
        .output()
        .unwrap()
}

fn recompress_cli_output(input_path: &Path, output_path: &Path, args: &[&str]) -> Output {
    let mut command_args = vec!["recompress"];
    command_args.extend_from_slice(args);
    command_args.push(input_path.to_str().unwrap());
    command_args.push("-o");
    command_args.push(output_path.to_str().unwrap());
    Command::new(flacx_bin())
        .args(command_args)
        .output()
        .unwrap()
}

#[test]
fn help_lists_encode_and_decode_commands() {
    let output = Command::new(flacx_bin()).arg("--help").output().unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("encode"));
    assert!(stdout.contains("decode"));
    assert!(stdout.contains("recompress"));
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
    assert!(stdout.contains("--output-family <OUTPUT_FAMILY>"));
    assert!(stdout.contains("--depth <DEPTH>"));
    assert!(stdout.contains("--mode <MODE>"));
    assert!(stdout.contains("--threads <THREADS>"));
    assert!(stdout.contains("loose"));
    assert!(stdout.contains("default"));
    assert!(stdout.contains("strict"));
    assert!(!stdout.contains("--strict-channel-mask-provenance"));
    assert!(!stdout.contains("--strict-seektable-validation"));
}

#[test]
fn recompress_help_lists_output_depth_mode_and_block_size() {
    let output = Command::new(flacx_bin())
        .args(["recompress", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("-o, --output <OUTPUT>"));
    assert!(stdout.contains("--in-place"));
    assert!(stdout.contains("--depth <DEPTH>"));
    assert!(stdout.contains("--mode <MODE>"));
    assert!(stdout.contains("--level <LEVEL>"));
    assert!(stdout.contains("--block-size <BLOCK_SIZE>"));
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
fn recompress_command_matches_library_output() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("flac");
    fs::write(&input_path, &flac).unwrap();

    let output = recompress_cli_output(
        &input_path,
        &output_path,
        &["--level", "0", "--threads", "1", "--block-size", "576"],
    );

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cli_bytes = fs::read(&output_path).unwrap();
    let library_bytes = Recompressor::new(
        RecompressConfig::default()
            .with_level(Level::Level0)
            .with_threads(1)
            .with_block_size(576),
    )
    .recompress_bytes(&flac)
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
fn recompress_single_file_without_output_writes_sibling_recompressed_file() {
    let input_dir = unique_temp_dir();
    let input_path = input_dir.join("album.flac");
    write_flac_file(&input_path, 1, 2_048);
    let output_path = input_dir.join("album.recompressed.flac");

    let output = Command::new(flacx_bin())
        .args(["recompress", input_path.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output_path.exists());

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn recompress_rejects_same_path_output() {
    let input_dir = unique_temp_dir();
    let input_path = input_dir.join("album.flac");
    write_flac_file(&input_path, 1, 2_048);

    let output = Command::new(flacx_bin())
        .args([
            "recompress",
            input_path.to_str().unwrap(),
            "-o",
            input_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("single-file recompress output must differ from the input path")
    );

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn recompress_directory_without_output_writes_sibling_recompressed_files() {
    let input_dir = unique_temp_dir();
    let first = input_dir.join("disc1").join("first.flac");
    let second = input_dir.join("disc1").join("second.flac");
    write_flac_file(&first, 1, 1_024);
    write_flac_file(&second, 1, 2_048);

    let command = RecompressCommand {
        input: input_dir.clone(),
        output: None,
        in_place: false,
        depth: 0,
        config: RecompressConfig::default().with_threads(1),
    };
    let mut stderr = Vec::new();

    recompress_command(&command, true, &mut stderr).unwrap();

    assert!(
        input_dir
            .join("disc1")
            .join("first.recompressed.flac")
            .exists()
    );
    assert!(
        input_dir
            .join("disc1")
            .join("second.recompressed.flac")
            .exists()
    );

    let stderr = String::from_utf8(stderr).unwrap();
    let final_lines = final_progress_frame_lines(&stderr);
    assert_eq!(final_lines.len(), 2);
    assert!(final_lines[0].starts_with("Batch | "));
    assert!(
        final_lines[1].contains("disc1/first.flac | Encode | ")
            || final_lines[1].contains("disc1/second.flac | Encode | ")
    );

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn recompress_interactive_progress_reports_decode_and_encode_phases() {
    let input_dir = unique_temp_dir();
    let input_path = input_dir.join("album.flac");
    write_flac_file(&input_path, 1, 2_048);
    let output_path = input_dir.join("album.recompressed.flac");
    let command = RecompressCommand {
        input: input_path.clone(),
        output: Some(output_path),
        in_place: false,
        depth: 1,
        config: RecompressConfig::default().with_threads(1),
    };
    let mut stderr = Vec::new();

    recompress_command(&command, true, &mut stderr).unwrap();

    let stderr = String::from_utf8(stderr).unwrap();
    assert!(stderr.contains(" | Decode | "));
    assert!(stderr.contains(" | Encode | "));

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn recompress_single_file_in_place_requires_explicit_opt_in() {
    let input_dir = unique_temp_dir();
    let input_path = input_dir.join("album.flac");
    let (_, original) = write_flac_file(&input_path, 1, 2_048);

    let output = Command::new(flacx_bin())
        .args([
            "recompress",
            input_path.to_str().unwrap(),
            "--in-place",
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
    let rewritten = fs::read(&input_path).unwrap();
    assert_ne!(rewritten, original);

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn recompress_rejects_in_place_when_output_is_also_supplied() {
    let input_dir = unique_temp_dir();
    let input_path = input_dir.join("album.flac");
    write_flac_file(&input_path, 1, 2_048);
    let output_path = input_dir.join("album.recompressed.flac");

    let output = Command::new(flacx_bin())
        .args([
            "recompress",
            input_path.to_str().unwrap(),
            "--in-place",
            "-o",
            output_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn recompress_directory_in_place_rewrites_existing_sources() {
    let input_dir = unique_temp_dir();
    let first = input_dir.join("disc1").join("first.flac");
    let second = input_dir.join("disc1").join("second.flac");
    let (_, first_original) = write_flac_file(&first, 1, 1_024);
    let (_, second_original) = write_flac_file(&second, 1, 2_048);

    let command = RecompressCommand {
        input: input_dir.clone(),
        output: None,
        in_place: true,
        depth: 0,
        config: RecompressConfig::default()
            .with_level(Level::Level0)
            .with_threads(1)
            .with_block_size(576),
    };

    recompress_command(&command, false, &mut Vec::new()).unwrap();

    assert_ne!(fs::read(&first).unwrap(), first_original);
    assert_ne!(fs::read(&second).unwrap(), second_original);

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
        raw_descriptor: None,
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
        raw_descriptor: None,
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
        raw_descriptor: None,
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
        raw_descriptor: None,
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
    let raw_path = input_dir.join("ignore.raw");
    write_wav_file(&wav_path, 1, 2_048);
    fs::write(&txt_path, b"not audio").unwrap();
    fs::write(&raw_path, [0u8; 8]).unwrap();

    let output = Command::new(flacx_bin())
        .args(["encode", input_dir.to_str().unwrap(), "--threads", "1"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(input_dir.join("keep.flac").exists());
    assert!(!input_dir.join("ignore.flac").exists());
    assert!(!input_dir.join("ignore.raw.flac").exists());

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn encode_directory_accepts_rf64_and_w64_inputs() {
    let input_dir = unique_temp_dir();
    let rf64_path = input_dir.join("keep-rf64.rf64");
    let w64_path = input_dir.join("keep-w64.w64");
    write_bytes_file(
        &rf64_path,
        &rf64_pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048)),
    );
    write_bytes_file(
        &w64_path,
        &w64_pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048)),
    );

    let output = Command::new(flacx_bin())
        .args(["encode", input_dir.to_str().unwrap(), "--threads", "1"])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(input_dir.join("keep-rf64.flac").exists());
    assert!(input_dir.join("keep-w64.flac").exists());
    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn encode_directory_accepts_aif_aiff_and_aifc_inputs() {
    let input_dir = unique_temp_dir();
    write_bytes_file(
        &input_dir.join("keep-aif.aif"),
        &aiff_pcm_bytes(16, 1, 44_100, &sample_fixture(1, 1_024)),
    );
    write_bytes_file(
        &input_dir.join("keep-aiff.aiff"),
        &aiff_pcm_bytes(24, 2, 48_000, &sample_fixture(2, 512)),
    );
    write_bytes_file(
        &input_dir.join("keep-aifc.aifc"),
        &aifc_pcm_bytes(*b"NONE", 20, 4, 96_000, &sample_fixture(4, 256)),
    );

    let output = Command::new(flacx_bin())
        .args(["encode", input_dir.to_str().unwrap(), "--threads", "1"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(input_dir.join("keep-aif.flac").exists());
    assert!(input_dir.join("keep-aiff.flac").exists());
    assert!(input_dir.join("keep-aifc.flac").exists());
    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn encode_directory_accepts_caf_inputs() {
    let input_dir = unique_temp_dir();
    write_bytes_file(
        &input_dir.join("keep-caf.caf"),
        &caf_lpcm_bytes(16, 16, 2, 44_100, true, &sample_fixture(2, 1_024)),
    );

    let output = Command::new(flacx_bin())
        .args(["encode", input_dir.to_str().unwrap(), "--threads", "1"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(input_dir.join("keep-caf.flac").exists());
    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn encode_command_rejects_unsupported_aifc_variants() {
    let cases = [
        (
            "ace2",
            aifc_pcm_bytes(*b"ACE2", 16, 1, 44_100, &sample_fixture(1, 8)),
        ),
        (
            "ace8",
            aifc_pcm_bytes(*b"ACE8", 16, 1, 44_100, &sample_fixture(1, 8)),
        ),
        (
            "mac3",
            aifc_pcm_bytes(*b"MAC3", 16, 1, 44_100, &sample_fixture(1, 8)),
        ),
        (
            "mac6",
            aifc_pcm_bytes(*b"MAC6", 16, 1, 44_100, &sample_fixture(1, 8)),
        ),
        (
            "float",
            aifc_pcm_bytes(*b"fl32", 32, 1, 44_100, &sample_fixture(1, 8)),
        ),
        (
            "bad-sowt",
            aifc_pcm_bytes(*b"sowt", 24, 1, 44_100, &sample_fixture(1, 8)),
        ),
        (
            "unknown",
            aifc_pcm_bytes(*b"????", 16, 1, 44_100, &sample_fixture(1, 8)),
        ),
    ];

    for (label, bytes) in cases {
        let input_path = unique_temp_path("aifc");
        let output_path = unique_temp_path("flac");
        fs::write(&input_path, &bytes).unwrap();

        let output = Command::new(flacx_bin())
            .args([
                "encode",
                input_path.to_str().unwrap(),
                "-o",
                output_path.to_str().unwrap(),
            ])
            .output()
            .unwrap();

        assert!(
            !output.status.success(),
            "{label} should fail but succeeded"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("AIFC") || stderr.contains("float") || stderr.contains("16-bit"),
            "unexpected stderr for {label}: {stderr}"
        );

        let _ = fs::remove_file(input_path);
        let _ = fs::remove_file(output_path);
    }
}

#[test]
fn encode_command_accepts_raw_pcm_with_explicit_flags() {
    let input_path = unique_temp_path("pcm");
    let output_path = unique_temp_path("flac");
    let samples = sample_fixture(2, 1_024);
    fs::write(
        &input_path,
        raw_pcm_bytes(16, 16, flacx::RawPcmByteOrder::LittleEndian, &samples),
    )
    .unwrap();

    let output = encode_cli_output(
        &input_path,
        &output_path,
        &[
            "--raw",
            "--sample-rate",
            "44100",
            "--channels",
            "2",
            "--bits-per-sample",
            "16",
            "--container-bits",
            "16",
            "--byte-order",
            "le",
        ],
    );

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output_path.exists());

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn encode_command_rejects_raw_mode_without_required_descriptor_flags() {
    let input_path = unique_temp_path("pcm");
    let output_path = unique_temp_path("flac");
    fs::write(&input_path, [0u8; 8]).unwrap();

    let output = encode_cli_output(
        &input_path,
        &output_path,
        &["--raw", "--sample-rate", "44100"],
    );

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--raw requires --channels"));

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn encode_command_rejects_raw_directory_input() {
    let input_dir = unique_temp_dir();
    fs::write(input_dir.join("input.raw"), [0u8; 8]).unwrap();
    let output = Command::new(flacx_bin())
        .args([
            "encode",
            input_dir.to_str().unwrap(),
            "--raw",
            "--sample-rate",
            "44100",
            "--channels",
            "2",
            "--bits-per-sample",
            "16",
            "--container-bits",
            "16",
            "--byte-order",
            "le",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("raw PCM encode does not support directory input")
    );

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
        let output = decode_cli_output(&input_path, &output_path, &["--threads", &threads_arg]);

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains('\r'),
            "decode should not emit progress output"
        );
        assert_wav_audio_eq(&fs::read(&output_path).unwrap(), &wav);

        let _ = fs::remove_file(input_path);
        let _ = fs::remove_file(output_path);
    }
}

#[test]
fn decode_command_can_emit_wave64_via_output_extension() {
    let samples = sample_fixture(1, 2_048);
    let wav = pcm_wav_bytes(16, 1, 44_100, &samples);
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("w64");
    fs::write(&input_path, &flac).unwrap();

    let output = decode_cli_output(&input_path, &output_path, &[]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let decoded = fs::read(&output_path).unwrap();
    let reencoded = Encoder::default().encode_bytes(&decoded).unwrap();
    let round_tripped = decode_bytes(&reencoded).unwrap();
    assert_eq!(wav_data_bytes(&round_tripped), wav_data_bytes(&wav));

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn decode_command_keeps_ordinary_files_green_with_strict_mode() {
    let samples = sample_fixture(2, 4_096);
    let wav = pcm_wav_bytes(16, 2, 44_100, &samples);
    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&wav)
        .unwrap();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("wav");
    fs::write(&input_path, &flac).unwrap();

    let output = decode_cli_output(&input_path, &output_path, &["--mode", "strict"]);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_wav_audio_eq(&fs::read(&output_path).unwrap(), &wav);

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn decode_command_loose_omits_fxmd_output_while_default_preserves_it() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(&flac, &[cuesheet_block(&[0], 2_048)]);
    let input_path = unique_temp_path("flac");
    let default_output_path = unique_temp_path("wav");
    let loose_output_path = unique_temp_path("wav");
    fs::write(&input_path, &flac).unwrap();

    let default_output = decode_cli_output(&input_path, &default_output_path, &[]);
    assert!(
        default_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&default_output.stderr)
    );
    let default_wav = fs::read(&default_output_path).unwrap();
    assert_eq!(wav_chunk_payloads(&default_wav, *b"fxmd").len(), 1);

    let loose_output = decode_cli_output(&input_path, &loose_output_path, &["--mode", "loose"]);
    assert!(
        loose_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&loose_output.stderr)
    );
    let loose_wav = fs::read(&loose_output_path).unwrap();
    assert_eq!(wav_chunk_payloads(&loose_wav, *b"fxmd").len(), 0);
    assert_wav_audio_eq(&default_wav, &wav);
    assert_wav_audio_eq(&loose_wav, &wav);

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(default_output_path);
    let _ = fs::remove_file(loose_output_path);
}

#[test]
fn decode_command_function_passes_strict_channel_mask_provenance_into_config() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("wav");
    fs::write(&input_path, &flac).unwrap();

    let command = DecodeCommand {
        input: input_path.clone(),
        output: Some(output_path.clone()),
        depth: 1,
        config: DecodeConfig::default().with_strict_channel_mask_provenance(true),
    };
    let mut stderr = Vec::new();

    decode_command(&command, false, &mut stderr).unwrap();

    assert!(stderr.is_empty());
    assert_wav_audio_eq(&fs::read(&output_path).unwrap(), &wav);

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn decode_command_function_passes_strict_seektable_validation_into_config() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(&flac, &[raw_seektable_block(&[0u8; 17])]);
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("wav");
    fs::write(&input_path, &flac).unwrap();

    let command = DecodeCommand {
        input: input_path.clone(),
        output: Some(output_path.clone()),
        depth: 1,
        config: DecodeConfig::default().with_strict_seektable_validation(true),
    };
    let mut stderr = Vec::new();

    let error = decode_command(&command, false, &mut stderr).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("seektable payload length must be a multiple of 18 bytes")
    );
    assert!(stderr.is_empty());
    assert!(!output_path.exists());

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn decode_command_fails_on_missing_provenance_marker_for_non_ordinary_layout() {
    let wav = extensible_pcm_wav_bytes(16, 16, 4, 48_000, 0x0001_2104, &sample_fixture(4, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(
        &flac,
        &[vorbis_comment_block(&[(
            "WAVEFORMATEXTENSIBLE_CHANNEL_MASK",
            "0x00012104",
        )])],
    );
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("wav");
    fs::write(&input_path, &flac).unwrap();

    let output = decode_cli_output(&input_path, &output_path, &["--mode", "strict"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("FLACX_CHANNEL_LAYOUT_PROVENANCE"));
    assert!(!output_path.exists());

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn decode_command_fails_on_invalid_seektable_when_strict_validation_is_enabled() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(&flac, &[raw_seektable_block(&[0u8; 17])]);
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("wav");
    fs::write(&input_path, &flac).unwrap();

    let output = decode_cli_output(&input_path, &output_path, &["--mode", "strict"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("seektable payload length must be a multiple of 18 bytes"));
    assert!(!output_path.exists());

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
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
    assert_wav_audio_eq(&fs::read(&output_path).unwrap(), &wav);

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
    assert_wav_audio_eq(&fs::read(&output_path).unwrap(), &wav);

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
    assert_wav_audio_eq(&fs::read(&output_path).unwrap(), &wav);

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
fn decode_directory_output_family_selector_uses_requested_extension() {
    let input_dir = unique_temp_dir();
    let output_dir = unique_temp_path("outdir");
    let flac_path = input_dir.join("disc1").join("song.flac");
    let (wav, _) = write_flac_file(&flac_path, 1, 2_048);

    let output = Command::new(flacx_bin())
        .args([
            "decode",
            input_dir.to_str().unwrap(),
            "-o",
            output_dir.to_str().unwrap(),
            "--output-family",
            "aiff",
            "--depth",
            "0",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output_path = output_dir.join("disc1").join("song.aiff");
    assert!(output_path.exists());
    let decoded = fs::read(&output_path).unwrap();
    let reencoded = Encoder::default().encode_bytes(&decoded).unwrap();
    let round_tripped = decode_bytes(&reencoded).unwrap();
    assert_wav_audio_eq(&round_tripped, &wav);

    let _ = fs::remove_dir_all(input_dir);
    let _ = fs::remove_dir_all(output_dir);
}

#[test]
fn decode_directory_output_family_selector_without_output_root_uses_sibling_requested_extension() {
    let input_dir = unique_temp_dir();
    let flac_path = input_dir.join("disc1").join("song.flac");
    let (wav, _) = write_flac_file(&flac_path, 1, 2_048);

    let output = Command::new(flacx_bin())
        .args([
            "decode",
            input_dir.to_str().unwrap(),
            "--output-family",
            "caf",
            "--depth",
            "0",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output_path = input_dir.join("disc1").join("song.caf");
    assert!(output_path.exists());
    let decoded = fs::read(&output_path).unwrap();
    let reencoded = Encoder::default().encode_bytes(&decoded).unwrap();
    let round_tripped = decode_bytes(&reencoded).unwrap();
    assert_wav_audio_eq(&round_tripped, &wav);

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn decode_command_rejects_output_family_for_single_file_input() {
    let input_dir = unique_temp_dir();
    let input_path = input_dir.join("song.flac");
    write_flac_file(&input_path, 1, 1_024);

    let output = Command::new(flacx_bin())
        .args([
            "decode",
            input_path.to_str().unwrap(),
            "--output-family",
            "caf",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("--output-family is only supported for directory decode")
    );

    let _ = fs::remove_dir_all(input_dir);
}

#[test]
fn decode_command_rejects_unsupported_output_extension() {
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("raw");
    write_flac_file(&input_path, 1, 1_024);

    let output = decode_cli_output(&input_path, &output_path, &[]);

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unsupported decode output extension")
    );

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
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
    assert_wav_audio_eq(
        &fs::read(output_dir.join("disc1").join("set").join("song.wav")).unwrap(),
        &wav,
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
