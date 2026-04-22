//! Command-line conversion between supported PCM containers and FLAC, plus
//! FLAC recompression, built on the `flacx` workspace library.
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
use flacx::{
    DecodeConfig, EncoderConfig, RawPcmByteOrder, RawPcmDescriptor, RecompressConfig,
    RecompressMode, level::Level,
};
use flacx_cli::{
    DecodeCommand, EncodeCommand, RecompressCommand, decode_command, encode_command,
    recompress_command,
};

const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Convert between FLAC and PCM containers, or recompress existing FLAC files.
///
/// `flacx` is the authoritative command-line reference for this workspace.
/// Use `encode` to create FLAC from supported PCM containers or raw PCM,
/// `decode` to write FLAC back to a PCM container, and `recompress` to
/// rewrite existing FLAC files with a different encoding policy. Batch
/// directory work stays sequential across files for every command; `--threads`
/// only adjusts codec threading within the current file.
///
/// Run `flacx <command> --help` for command-specific workflow details,
/// defaults, and batch-processing rules.
#[derive(Debug, Parser)]
#[command(
    name = "flacx",
    version = CLI_VERSION,
    propagate_version = true,
    subcommand_help_heading = "Commands"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Encode supported PCM-container input or explicit raw PCM to FLAC, one file at a time.
    Encode(EncodeArgs),
    /// Decode FLAC into a supported PCM container, one file at a time.
    Decode(DecodeArgs),
    /// Recompress existing FLAC files into new FLAC output, one file at a time.
    Recompress(RecompressArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ModePreset {
    /// Favor tolerance by disabling fxmd capture or emission and relaxing validation.
    Loose,
    /// Keep the standard metadata capture or emission policy and normal validation.
    Default,
    /// Preserve metadata handling while enabling the strictest validation checks.
    Strict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum RawByteOrderArg {
    /// Interpret raw PCM bytes as little-endian samples.
    Le,
    /// Interpret raw PCM bytes as big-endian samples.
    Be,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum DecodeOutputFamilyArg {
    /// Write RIFF/WAVE output (`.wav`).
    Wave,
    /// Write RF64 output (`.rf64`).
    Rf64,
    /// Write Sony Wave64 output (`.w64`).
    W64,
    /// Write AIFF output (`.aiff`).
    Aiff,
    /// Write AIFF-C output (`.aifc`).
    Aifc,
    /// Write Core Audio Format output (`.caf`).
    Caf,
}

/// Encode one file or walk a directory of supported PCM inputs into FLAC.
///
/// Single-file runs write `<input>.flac` by default. Directory runs process
/// matched inputs sequentially, one file at a time, while preserving the
/// relative layout beneath the input root and honoring `--depth` when
/// searching for supported source files.
///
/// Add `--raw` to treat the input as headerless signed-integer PCM data. In
/// raw mode, `--sample-rate`, `--channels`, `--bits-per-sample`,
/// `--container-bits`, and `--byte-order` are required. Raw 3-8 channel input
/// also requires `--channel-mask`.
#[derive(Debug, Args)]
struct EncodeArgs {
    /// File or directory to encode.
    ///
    /// Supported container input includes `.wav`, `.rf64`, `.w64`, `.aif`,
    /// `.aiff`, `.aifc`, and `.caf`. With `--raw`, any file path is accepted
    /// and interpreted as signed-integer PCM bytes using the accompanying raw
    /// format flags.
    #[arg(value_name = "INPUT")]
    input: std::path::PathBuf,
    /// Output path for the encoded FLAC.
    ///
    /// For a single file, pass a destination `.flac` path. For a directory
    /// input, pass a destination directory root. If omitted, single-file
    /// encode writes the output next to the input with a `.flac` extension.
    #[arg(short, long, help_heading = "Output")]
    output: Option<std::path::PathBuf>,
    /// FLAC compression level.
    ///
    /// Level `0` favors speed and level `8` favors compression ratio.
    #[arg(
        long,
        default_value_t = 8u8,
        value_parser = clap::value_parser!(u8).range(0..=8),
        help_heading = "Encoding"
    )]
    level: u8,
    /// Number of per-file codec worker threads.
    ///
    /// This only affects work inside the file currently being encoded.
    /// Directory batches still encode matching files sequentially, one at a
    /// time.
    #[arg(long, default_value_t = 8usize, help_heading = "Encoding")]
    threads: usize,
    /// Override the FLAC block size.
    ///
    /// Leave unset to use the encoder's default block-size choice.
    #[arg(long, help_heading = "Encoding")]
    block_size: Option<u16>,
    /// Metadata and validation preset.
    ///
    /// Use `loose` for maximum tolerance, `default` for standard behavior, and
    /// `strict` for the strongest validation checks.
    #[arg(
        long,
        value_enum,
        default_value_t = ModePreset::Default,
        help_heading = "Validation"
    )]
    mode: ModePreset,
    /// Maximum directory traversal depth.
    ///
    /// This only applies when the input is a directory. Use `0` to recurse
    /// without a depth limit.
    #[arg(
        long,
        default_value_t = 1usize,
        value_name = "DEPTH",
        help_heading = "Validation"
    )]
    depth: usize,
    /// Treat the input as raw signed-integer PCM instead of a self-describing container.
    ///
    /// When this flag is present, `INPUT` is read as headerless PCM and the raw
    /// format flags become the authoritative description of the sample layout.
    #[arg(long, help_heading = "Raw PCM input")]
    raw: bool,
    /// Raw PCM sample rate in Hz.
    ///
    /// Required with `--raw`.
    #[arg(long, help_heading = "Raw PCM input")]
    sample_rate: Option<u32>,
    /// Raw PCM channel count.
    ///
    /// Required with `--raw`.
    #[arg(long, help_heading = "Raw PCM input")]
    channels: Option<u8>,
    /// Raw PCM valid bits per sample.
    ///
    /// Required with `--raw`.
    #[arg(long, help_heading = "Raw PCM input")]
    bits_per_sample: Option<u8>,
    /// Raw PCM container bits per sample.
    ///
    /// Required with `--raw`.
    #[arg(long, help_heading = "Raw PCM input")]
    container_bits: Option<u8>,
    /// Raw PCM byte order.
    ///
    /// Required with `--raw`.
    #[arg(long, value_enum, help_heading = "Raw PCM input")]
    byte_order: Option<RawByteOrderArg>,
    /// Raw PCM channel mask in hex.
    ///
    /// Required for 3-8 channel `--raw` input. Prefix values with `0x` to make
    /// the speaker mask obvious, for example `0x33`.
    #[arg(long, help_heading = "Raw PCM input")]
    channel_mask: Option<String>,
}

/// Decode FLAC into a supported PCM container.
///
/// Single-file runs use the explicit `--output` extension when provided and
/// otherwise default to `.wav`. Directory runs decode matched FLAC inputs
/// sequentially, one file at a time, while preserving the relative layout
/// beneath the input root; use `--output-family` to choose which container
/// extension gets generated for batch output paths.
#[derive(Debug, Args)]
struct DecodeArgs {
    /// FLAC file or directory to decode.
    ///
    /// Directory input searches for `.flac` files at or below the configured
    /// traversal depth.
    #[arg(value_name = "INPUT")]
    input: std::path::PathBuf,
    /// Output path for decoded PCM-container data.
    ///
    /// For a single file, pass a destination file path with the container
    /// extension you want. For a directory input, pass a destination directory
    /// root. If omitted, single-file decode writes next to the input using the
    /// container implied by `--output-family` or the default `.wav`.
    #[arg(short, long, help_heading = "Output")]
    output: Option<std::path::PathBuf>,
    /// Output container family for generated decode paths.
    ///
    /// Directory decode uses this for every generated output path. Single-file
    /// decode uses it only when `--output` is omitted; otherwise the explicit
    /// output-path extension wins.
    #[arg(long, value_enum, help_heading = "Output")]
    output_family: Option<DecodeOutputFamilyArg>,
    /// Number of per-file codec worker threads.
    ///
    /// Defaults to `8` to match the CLI's encode-side threading policy.
    /// Directory batches still decode matching files sequentially, one at a
    /// time.
    #[arg(long, default_value = "8", help_heading = "Decoding")]
    threads: Option<usize>,
    /// Metadata and validation preset.
    ///
    /// Use `loose` for maximum tolerance, `default` for standard behavior, and
    /// `strict` for the strongest validation checks.
    #[arg(
        long,
        value_enum,
        default_value_t = ModePreset::Default,
        help_heading = "Validation"
    )]
    mode: ModePreset,
    /// Maximum directory traversal depth.
    ///
    /// This only applies when the input is a directory. Use `0` to recurse
    /// without a depth limit.
    #[arg(
        long,
        default_value_t = 1usize,
        value_name = "DEPTH",
        help_heading = "Validation"
    )]
    depth: usize,
}

/// Recompress existing FLAC files into new FLAC output.
///
/// Single-file runs write `<input-stem>.recompressed.flac` by default.
/// Directory runs recompress matched FLAC inputs sequentially, one file at a
/// time, while preserving the relative layout beneath the input root. Use
/// `--in-place` to replace each source FLAC only after a successful
/// recompression.
#[derive(Debug, Args)]
struct RecompressArgs {
    /// FLAC file or directory to recompress.
    ///
    /// Directory input searches for `.flac` files at or below the configured
    /// traversal depth.
    #[arg(value_name = "INPUT")]
    input: std::path::PathBuf,
    /// Output path for recompressed FLAC data.
    ///
    /// For a single file, pass a destination `.flac` path that differs from the
    /// input. For a directory input, pass a destination directory root. If
    /// omitted and `--in-place` is not set, single-file recompress writes
    /// `<input-stem>.recompressed.flac`.
    #[arg(short, long, help_heading = "Output")]
    output: Option<std::path::PathBuf>,
    /// Replace the source FLAC file or files after successful recompression.
    ///
    /// This rewrites output in place and cannot be combined with `--output`.
    #[arg(long, conflicts_with = "output", help_heading = "Output")]
    in_place: bool,
    /// FLAC compression level.
    ///
    /// Level `0` favors speed and level `8` favors compression ratio.
    #[arg(
        long,
        default_value_t = 8u8,
        value_parser = clap::value_parser!(u8).range(0..=8),
        help_heading = "Recompression"
    )]
    level: u8,
    /// Number of per-file codec worker threads.
    ///
    /// This only affects work inside the file currently being recompressed.
    /// Directory batches still recompress matching files sequentially, one at a
    /// time.
    #[arg(long, default_value_t = 8usize, help_heading = "Recompression")]
    threads: usize,
    /// Override the FLAC block size.
    ///
    /// Leave unset to use the recompressor's default block-size choice.
    #[arg(long, help_heading = "Recompression")]
    block_size: Option<u16>,
    /// Metadata and validation preset.
    ///
    /// Use `loose` for maximum tolerance, `default` for standard behavior, and
    /// `strict` for the strongest validation checks.
    #[arg(
        long,
        value_enum,
        default_value_t = ModePreset::Default,
        help_heading = "Validation"
    )]
    mode: ModePreset,
    /// Maximum directory traversal depth.
    ///
    /// This only applies when the input is a directory. Use `0` to recurse
    /// without a depth limit.
    #[arg(
        long,
        default_value_t = 1usize,
        value_name = "DEPTH",
        help_heading = "Validation"
    )]
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
    let raw_descriptor = build_raw_descriptor(&args)?;
    let command = EncodeCommand {
        input: args.input,
        output: args.output,
        depth: args.depth,
        config,
        raw_descriptor,
    };
    encode_command(&command, interactive, &mut stderr)?;
    Ok(())
}

fn build_raw_descriptor(
    args: &EncodeArgs,
) -> Result<Option<RawPcmDescriptor>, Box<dyn std::error::Error>> {
    if !args.raw {
        if args.sample_rate.is_some()
            || args.channels.is_some()
            || args.bits_per_sample.is_some()
            || args.container_bits.is_some()
            || args.byte_order.is_some()
            || args.channel_mask.is_some()
        {
            return Err("raw PCM flags require --raw".into());
        }
        return Ok(None);
    }

    let sample_rate = args.sample_rate.ok_or("--raw requires --sample-rate")?;
    let channels = args.channels.ok_or("--raw requires --channels")?;
    let bits_per_sample = args
        .bits_per_sample
        .ok_or("--raw requires --bits-per-sample")?;
    let container_bits = args
        .container_bits
        .ok_or("--raw requires --container-bits")?;
    let byte_order = match args.byte_order.ok_or("--raw requires --byte-order")? {
        RawByteOrderArg::Le => RawPcmByteOrder::LittleEndian,
        RawByteOrderArg::Be => RawPcmByteOrder::BigEndian,
    };
    let channel_mask = match args.channel_mask.as_deref() {
        Some(mask) => Some(parse_channel_mask(mask)?),
        None => None,
    };
    if (3..=8).contains(&channels) && channel_mask.is_none() {
        return Err("--raw 3..=8 channel input requires --channel-mask".into());
    }

    Ok(Some(RawPcmDescriptor {
        sample_rate,
        channels,
        valid_bits_per_sample: bits_per_sample,
        container_bits_per_sample: container_bits,
        byte_order,
        channel_mask,
    }))
}

fn parse_channel_mask(mask: &str) -> Result<u32, Box<dyn std::error::Error>> {
    let trimmed = mask.trim();
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    Ok(u32::from_str_radix(hex, 16)?)
}

fn decode(args: DecodeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = apply_decode_mode(DecodeConfig::default(), args.mode);
    if let Some(threads) = args.threads {
        config = config.with_threads(threads);
    }
    if let Some(output_family) = args.output_family {
        if args.input.is_file() {
            return Err(
                "--output-family is only supported for directory decode; use an explicit output path extension for single-file decode"
                    .into(),
            );
        }
        config = config.with_output_container(match output_family {
            DecodeOutputFamilyArg::Wave => flacx::PcmContainer::Wave,
            DecodeOutputFamilyArg::Rf64 => flacx::PcmContainer::Rf64,
            DecodeOutputFamilyArg::W64 => flacx::PcmContainer::Wave64,
            DecodeOutputFamilyArg::Aiff => flacx::PcmContainer::Aiff,
            DecodeOutputFamilyArg::Aifc => flacx::PcmContainer::Aifc,
            DecodeOutputFamilyArg::Caf => flacx::PcmContainer::Caf,
        });
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
    let mut config = RecompressConfig::default()
        .with_mode(recompress_mode(args.mode))
        .with_level(level)
        .with_threads(args.threads);
    if let Some(block_size) = args.block_size {
        config = config.with_block_size(block_size);
    }

    let interactive = io::stderr().is_terminal();
    enforce_interactive_mode(interactive, interactive_required())?;
    let mut stderr = io::stderr().lock();
    let command = RecompressCommand {
        input: args.input,
        output: args.output,
        in_place: args.in_place,
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

fn recompress_mode(mode: ModePreset) -> RecompressMode {
    match mode {
        ModePreset::Loose => RecompressMode::Loose,
        ModePreset::Default => RecompressMode::Default,
        ModePreset::Strict => RecompressMode::Strict,
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
        Cli, Commands, DecodeOutputFamilyArg, ModePreset, RawByteOrderArg, apply_decode_mode,
        apply_encode_mode, enforce_interactive_mode, recompress_mode,
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
    fn encode_command_parses_raw_pcm_flags() {
        let cli = Cli::parse_from([
            "flacx",
            "encode",
            "input.raw",
            "--raw",
            "--sample-rate",
            "48000",
            "--channels",
            "4",
            "--bits-per-sample",
            "20",
            "--container-bits",
            "24",
            "--byte-order",
            "le",
            "--channel-mask",
            "0x33",
        ]);

        match cli.command {
            Commands::Encode(args) => {
                assert!(args.raw);
                assert_eq!(args.sample_rate, Some(48_000));
                assert_eq!(args.channels, Some(4));
                assert_eq!(args.bits_per_sample, Some(20));
                assert_eq!(args.container_bits, Some(24));
                assert_eq!(args.byte_order, Some(RawByteOrderArg::Le));
                assert_eq!(args.channel_mask.as_deref(), Some("0x33"));
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
    fn decode_command_parses_output_family() {
        let cli = Cli::parse_from([
            "flacx",
            "decode",
            "albums",
            "-o",
            "out-dir",
            "--output-family",
            "caf",
        ]);

        match cli.command {
            Commands::Decode(args) => {
                assert_eq!(args.output_family, Some(DecodeOutputFamilyArg::Caf));
            }
            _ => panic!("expected decode command"),
        }
    }

    #[test]
    fn decode_command_defaults_output_depth_and_threads() {
        let cli = Cli::parse_from(["flacx", "decode", "input.flac"]);

        match cli.command {
            Commands::Decode(args) => {
                assert_eq!(args.input, std::path::PathBuf::from("input.flac"));
                assert_eq!(args.output, None);
                assert_eq!(args.threads, Some(8));
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
                assert!(!args.in_place);
                assert_eq!(args.level, 0);
                assert_eq!(args.threads, 4);
                assert_eq!(args.depth, 0);
                assert_eq!(args.mode, ModePreset::Strict);
            }
            _ => panic!("expected recompress command"),
        }
    }

    #[test]
    fn recompress_command_parses_explicit_in_place_flag() {
        let cli = Cli::parse_from(["flacx", "recompress", "album.flac", "--in-place"]);

        match cli.command {
            Commands::Recompress(args) => {
                assert_eq!(args.input, std::path::PathBuf::from("album.flac"));
                assert!(args.in_place);
                assert_eq!(args.output, None);
            }
            _ => panic!("expected recompress command"),
        }
    }

    #[test]
    fn preset_mapping_matches_cli_contract() {
        let encode_default =
            apply_encode_mode(flacx::EncoderConfig::default(), ModePreset::Default);
        assert!(encode_default.capture_fxmd());
        assert!(encode_default.strict_fxmd_validation());

        let encode_loose = apply_encode_mode(flacx::EncoderConfig::default(), ModePreset::Loose);
        assert!(!encode_loose.capture_fxmd());
        assert!(!encode_loose.strict_fxmd_validation());

        let decode_strict = apply_decode_mode(flacx::DecodeConfig::default(), ModePreset::Strict);
        assert!(decode_strict.emit_fxmd());
        assert!(decode_strict.strict_channel_mask_provenance());
        assert!(decode_strict.strict_seektable_validation());

        let recompress_loose_encode =
            apply_encode_mode(flacx::EncoderConfig::default(), ModePreset::Loose);
        let recompress_loose_decode =
            apply_decode_mode(flacx::DecodeConfig::default(), ModePreset::Loose);
        assert!(!recompress_loose_encode.capture_fxmd());
        assert!(!recompress_loose_encode.strict_fxmd_validation());
        assert!(!recompress_loose_decode.emit_fxmd());

        assert_eq!(
            recompress_mode(ModePreset::Loose),
            flacx::RecompressMode::Loose
        );
        assert_eq!(
            recompress_mode(ModePreset::Default),
            flacx::RecompressMode::Default
        );
        assert_eq!(
            recompress_mode(ModePreset::Strict),
            flacx::RecompressMode::Strict
        );
    }

    #[test]
    fn require_interactive_helper_only_rejects_non_interactive_runs_when_required() {
        assert!(enforce_interactive_mode(true, true).is_ok());
        assert!(enforce_interactive_mode(true, false).is_ok());
        assert!(enforce_interactive_mode(false, false).is_ok());
        assert!(enforce_interactive_mode(false, true).is_err());
    }
}
