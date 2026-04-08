//! Command-line WAV/FLAC conversion and FLAC recompression built on the `flacx`
//! workspace library.
//!
//! This crate stays separate from the publishable library package while
//! reusing the same encode pipeline and workspace version.
//!
use std::{
    env,
    io::{self, IsTerminal, Write},
    process::ExitCode,
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use flacx::{DecodeConfig, EncoderConfig, RecompressConfig, level::Level};
use flacx_cli::{
    DecodeCommand, EncodeCommand, RecompressCommand, decode_command, encode_command,
    recompress_command,
};

const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Parser)]
#[command(
    name = "flacx",
    about = "WAV/FLAC conversion and FLAC recompression using the flacx library",
    version = CLI_VERSION,
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Encode a supported WAV file to FLAC.
    Encode(EncodeArgs),
    /// Decode a supported FLAC file to WAV.
    Decode(DecodeArgs),
    /// Recompress a supported FLAC file to a new FLAC.
    Recompress(RecompressArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ModePreset {
    Loose,
    Default,
    Strict,
}

#[derive(Debug, Args)]
struct EncodeArgs {
    /// Input WAV path.
    input: std::path::PathBuf,
    /// Output FLAC path for a single file, or destination directory for a folder input.
    #[arg(short, long)]
    output: Option<std::path::PathBuf>,
    /// Compression level (0-8).
    #[arg(long, default_value_t = 8u8, value_parser = clap::value_parser!(u8).range(0..=8))]
    level: u8,
    /// Number of encoding threads.
    #[arg(long, default_value_t = 8usize)]
    threads: usize,
    /// Override the FLAC block size.
    #[arg(long)]
    block_size: Option<u16>,
    /// Policy preset for fxmd handling and relaxable validation.
    #[arg(long, value_enum, default_value_t = ModePreset::Default)]
    mode: ModePreset,
    /// Maximum folder traversal depth; only applies when the input is a directory. Use 0 for unlimited depth.
    #[arg(long, default_value_t = 1usize)]
    depth: usize,
}

#[derive(Debug, Args)]
struct DecodeArgs {
    /// Input FLAC path.
    input: std::path::PathBuf,
    /// Output WAV path for a single file, or destination directory for a folder input.
    #[arg(short, long)]
    output: Option<std::path::PathBuf>,
    /// Number of decoding threads.
    #[arg(long)]
    threads: Option<usize>,
    /// Policy preset for fxmd handling and relaxable validation.
    #[arg(long, value_enum, default_value_t = ModePreset::Default)]
    mode: ModePreset,
    /// Maximum folder traversal depth; only applies when the input is a directory. Use 0 for unlimited depth.
    #[arg(long, default_value_t = 1usize)]
    depth: usize,
}

#[derive(Debug, Args)]
struct RecompressArgs {
    /// Input FLAC path.
    input: std::path::PathBuf,
    /// Output FLAC path for a single file, or destination directory for a folder input.
    #[arg(short, long)]
    output: Option<std::path::PathBuf>,
    /// Compression level (0-8).
    #[arg(long, default_value_t = 8u8, value_parser = clap::value_parser!(u8).range(0..=8))]
    level: u8,
    /// Number of recompression worker threads.
    #[arg(long, default_value_t = 8usize)]
    threads: usize,
    /// Override the FLAC block size.
    #[arg(long)]
    block_size: Option<u16>,
    /// Policy preset for metadata handling and relaxable validation.
    #[arg(long, value_enum, default_value_t = ModePreset::Default)]
    mode: ModePreset,
    /// Maximum folder traversal depth; only applies when the input is a directory. Use 0 for unlimited depth.
    #[arg(long, default_value_t = 1usize)]
    depth: usize,
}

fn main() -> ExitCode {
    if let Err(error) = run() {
        let _ = writeln!(io::stderr(), "{error}");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    match Cli::parse().command {
        Commands::Encode(args) => encode(args)?,
        Commands::Decode(args) => decode(args)?,
        Commands::Recompress(args) => recompress(args)?,
    }
    Ok(())
}

fn encode(args: EncodeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let level = Level::try_from(args.level).map_err(|_| "invalid level")?;
    let mut config = EncoderConfig::default()
        .with_level(level)
        .with_threads(args.threads);
    if let Some(block_size) = args.block_size {
        config = config.with_block_size(block_size);
    }
    config = apply_encode_mode(config, args.mode);

    let interactive = io::stderr().is_terminal();
    enforce_interactive_mode(interactive, interactive_required())?;
    let mut stderr = io::stderr().lock();
    let command = EncodeCommand {
        input: args.input,
        output: args.output,
        depth: args.depth,
        config,
    };
    encode_command(&command, interactive, &mut stderr)?;
    Ok(())
}

fn decode(args: DecodeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = apply_decode_mode(DecodeConfig::default(), args.mode);
    if let Some(threads) = args.threads {
        config = config.with_threads(threads);
    }

    let interactive = io::stderr().is_terminal();
    enforce_interactive_mode(interactive, interactive_required())?;
    let mut stderr = io::stderr().lock();
    let command = DecodeCommand {
        input: args.input,
        output: args.output,
        depth: args.depth,
        config,
    };
    decode_command(&command, interactive, &mut stderr)?;
    Ok(())
}

fn recompress(args: RecompressArgs) -> Result<(), Box<dyn std::error::Error>> {
    let level = Level::try_from(args.level).map_err(|_| "invalid level")?;
    let mut encode = EncoderConfig::default()
        .with_level(level)
        .with_threads(args.threads);
    if let Some(block_size) = args.block_size {
        encode = encode.with_block_size(block_size);
    }
    encode = apply_encode_mode(encode, args.mode);

    let decode = apply_decode_mode(
        DecodeConfig::default().with_threads(args.threads),
        args.mode,
    );
    let config = RecompressConfig::default()
        .with_decode_config(decode)
        .with_encode_config(encode);

    let interactive = io::stderr().is_terminal();
    enforce_interactive_mode(interactive, interactive_required())?;
    let mut stderr = io::stderr().lock();
    let command = RecompressCommand {
        input: args.input,
        output: args.output,
        depth: args.depth,
        config,
    };
    recompress_command(&command, interactive, &mut stderr)?;
    Ok(())
}

fn apply_encode_mode(config: EncoderConfig, mode: ModePreset) -> EncoderConfig {
    match mode {
        ModePreset::Loose => config
            .with_capture_fxmd(false)
            .with_strict_fxmd_validation(false),
        ModePreset::Default => config
            .with_capture_fxmd(true)
            .with_strict_fxmd_validation(true),
        ModePreset::Strict => config
            .with_capture_fxmd(true)
            .with_strict_fxmd_validation(true),
    }
}

fn apply_decode_mode(config: DecodeConfig, mode: ModePreset) -> DecodeConfig {
    match mode {
        ModePreset::Loose => config
            .with_emit_fxmd(false)
            .with_strict_channel_mask_provenance(false)
            .with_strict_seektable_validation(false),
        ModePreset::Default => config
            .with_emit_fxmd(true)
            .with_strict_channel_mask_provenance(false)
            .with_strict_seektable_validation(false),
        ModePreset::Strict => config
            .with_emit_fxmd(true)
            .with_strict_channel_mask_provenance(true)
            .with_strict_seektable_validation(true),
    }
}

fn interactive_required() -> bool {
    env::var_os("FLACX_REQUIRE_INTERACTIVE").is_some()
}

fn enforce_interactive_mode(
    interactive: bool,
    require_interactive: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if require_interactive && !interactive {
        return Err("interactive terminal required for this proof run".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Cli, Commands, ModePreset, apply_decode_mode, apply_encode_mode, enforce_interactive_mode,
    };
    use clap::Parser;

    #[test]
    fn encode_command_defaults_threads_and_depth() {
        let cli = Cli::parse_from(["flacx", "encode", "input.wav"]);

        match cli.command {
            Commands::Encode(args) => {
                assert_eq!(args.input, std::path::PathBuf::from("input.wav"));
                assert_eq!(args.output, None);
                assert_eq!(args.threads, 8);
                assert_eq!(args.mode, ModePreset::Default);
                assert_eq!(args.depth, 1);
            }
            _ => panic!("expected encode command"),
        }
    }

    #[test]
    fn encode_command_parses_output_and_depth_flags() {
        let cli = Cli::parse_from([
            "flacx",
            "encode",
            "input-dir",
            "-o",
            "out-dir",
            "--mode",
            "strict",
            "--depth",
            "0",
        ]);

        match cli.command {
            Commands::Encode(args) => {
                assert_eq!(args.input, std::path::PathBuf::from("input-dir"));
                assert_eq!(args.output, Some(std::path::PathBuf::from("out-dir")));
                assert_eq!(args.mode, ModePreset::Strict);
                assert_eq!(args.depth, 0);
            }
            _ => panic!("expected encode command"),
        }
    }

    #[test]
    fn encode_command_accepts_depth_for_single_file_input() {
        let cli = Cli::parse_from(["flacx", "encode", "input.wav", "--depth", "3"]);

        match cli.command {
            Commands::Encode(args) => {
                assert_eq!(args.input, std::path::PathBuf::from("input.wav"));
                assert_eq!(args.depth, 3);
            }
            _ => panic!("expected encode command"),
        }
    }

    #[test]
    fn decode_command_parses_output_depth_and_threads_flags() {
        let cli = Cli::parse_from([
            "flacx",
            "decode",
            "input.flac",
            "-o",
            "out-dir",
            "--depth",
            "0",
            "--threads",
            "4",
            "--mode",
            "loose",
        ]);

        match cli.command {
            Commands::Decode(args) => {
                assert_eq!(args.threads, Some(4));
                assert_eq!(args.mode, ModePreset::Loose);
                assert_eq!(args.input, std::path::PathBuf::from("input.flac"));
                assert_eq!(args.output, Some(std::path::PathBuf::from("out-dir")));
                assert_eq!(args.depth, 0);
            }
            _ => panic!("expected decode command"),
        }
    }

    #[test]
    fn decode_command_defaults_output_and_depth() {
        let cli = Cli::parse_from(["flacx", "decode", "input.flac"]);

        match cli.command {
            Commands::Decode(args) => {
                assert_eq!(args.input, std::path::PathBuf::from("input.flac"));
                assert_eq!(args.output, None);
                assert_eq!(args.mode, ModePreset::Default);
                assert_eq!(args.depth, 1);
            }
            _ => panic!("expected decode command"),
        }
    }

    #[test]
    fn recompress_command_defaults_threads_and_depth() {
        let cli = Cli::parse_from(["flacx", "recompress", "input.flac"]);

        match cli.command {
            Commands::Recompress(args) => {
                assert_eq!(args.input, std::path::PathBuf::from("input.flac"));
                assert_eq!(args.output, None);
                assert_eq!(args.threads, 8);
                assert_eq!(args.mode, ModePreset::Default);
                assert_eq!(args.depth, 1);
            }
            _ => panic!("expected recompress command"),
        }
    }

    #[test]
    fn recompress_command_parses_output_depth_and_level_flags() {
        let cli = Cli::parse_from([
            "flacx",
            "recompress",
            "album.flac",
            "-o",
            "album.recompressed.flac",
            "--level",
            "0",
            "--threads",
            "4",
            "--depth",
            "0",
            "--mode",
            "strict",
        ]);

        match cli.command {
            Commands::Recompress(args) => {
                assert_eq!(args.input, std::path::PathBuf::from("album.flac"));
                assert_eq!(
                    args.output,
                    Some(std::path::PathBuf::from("album.recompressed.flac"))
                );
                assert_eq!(args.level, 0);
                assert_eq!(args.threads, 4);
                assert_eq!(args.depth, 0);
                assert_eq!(args.mode, ModePreset::Strict);
            }
            _ => panic!("expected recompress command"),
        }
    }

    #[test]
    fn preset_mapping_matches_cli_contract() {
        let encode_default =
            apply_encode_mode(flacx::EncoderConfig::default(), ModePreset::Default);
        assert!(encode_default.capture_fxmd);
        assert!(encode_default.strict_fxmd_validation);

        let encode_loose = apply_encode_mode(flacx::EncoderConfig::default(), ModePreset::Loose);
        assert!(!encode_loose.capture_fxmd);
        assert!(!encode_loose.strict_fxmd_validation);

        let decode_strict = apply_decode_mode(flacx::DecodeConfig::default(), ModePreset::Strict);
        assert!(decode_strict.emit_fxmd);
        assert!(decode_strict.strict_channel_mask_provenance);
        assert!(decode_strict.strict_seektable_validation);

        let recompress_loose_encode =
            apply_encode_mode(flacx::EncoderConfig::default(), ModePreset::Loose);
        let recompress_loose_decode =
            apply_decode_mode(flacx::DecodeConfig::default(), ModePreset::Loose);
        assert!(!recompress_loose_encode.capture_fxmd);
        assert!(!recompress_loose_encode.strict_fxmd_validation);
        assert!(!recompress_loose_decode.emit_fxmd);
    }

    #[test]
    fn require_interactive_helper_only_rejects_non_interactive_runs_when_required() {
        assert!(enforce_interactive_mode(true, true).is_ok());
        assert!(enforce_interactive_mode(true, false).is_ok());
        assert!(enforce_interactive_mode(false, false).is_ok());
        assert!(enforce_interactive_mode(false, true).is_err());
    }
}
