//! Command-line WAV/FLAC conversion built on the `flacx` workspace library.
//!
//! This crate stays separate from the publishable library package while
//! reusing the same encode pipeline and workspace version.
//!
use std::{
    io::{self, IsTerminal, Write},
    process::ExitCode,
};

use clap::{Args, Parser, Subcommand};
use flacx::{DecodeConfig, EncoderConfig, level::Level};
use flacx_cli::{DecodeCommand, EncodeCommand, decode_command, encode_command};

const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Parser)]
#[command(
    name = "flacx",
    about = "WAV/FLAC conversion using the flacx library",
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

    let interactive = io::stderr().is_terminal();
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
    let mut config = DecodeConfig::default();
    if let Some(threads) = args.threads {
        config = config.with_threads(threads);
    }

    let interactive = io::stderr().is_terminal();
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

#[cfg(test)]
mod tests {
    use super::{Cli, Commands};
    use clap::Parser;

    #[test]
    fn encode_command_defaults_threads_and_depth() {
        let cli = Cli::parse_from(["flacx", "encode", "input.wav"]);

        match cli.command {
            Commands::Encode(args) => {
                assert_eq!(args.input, std::path::PathBuf::from("input.wav"));
                assert_eq!(args.output, None);
                assert_eq!(args.threads, 8);
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
            "--depth",
            "0",
        ]);

        match cli.command {
            Commands::Encode(args) => {
                assert_eq!(args.input, std::path::PathBuf::from("input-dir"));
                assert_eq!(args.output, Some(std::path::PathBuf::from("out-dir")));
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
        ]);

        match cli.command {
            Commands::Decode(args) => {
                assert_eq!(args.threads, Some(4));
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
                assert_eq!(args.depth, 1);
            }
            _ => panic!("expected decode command"),
        }
    }
}
