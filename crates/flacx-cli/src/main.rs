//! Command-line WAV-to-FLAC encoding built on the `flacx` workspace library.
//!
//! This crate stays separate from the publishable library package while
//! reusing the same encode pipeline and workspace version.
//!
use std::{
    io::{self, IsTerminal, Write},
    process::ExitCode,
};

use clap::{Args, Parser, Subcommand};
use flacx::{EncoderConfig, level::Level};
use flacx_cli::{EncodeCommand, encode_command};

#[derive(Debug, Parser)]
#[command(
    name = "flacx",
    about = "WAV-to-FLAC encoding using the flacx library",
    version,
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
}

#[derive(Debug, Args)]
struct EncodeArgs {
    /// Input WAV path.
    input: std::path::PathBuf,
    /// Output FLAC path.
    output: std::path::PathBuf,
    /// Compression level (0-8).
    #[arg(long, default_value_t = 8u8, value_parser = clap::value_parser!(u8).range(0..=8))]
    level: u8,
    /// Number of encoding threads.
    #[arg(long)]
    threads: Option<usize>,
    /// Override the FLAC block size.
    #[arg(long)]
    block_size: Option<u16>,
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
    }
    Ok(())
}

fn encode(args: EncodeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let level = Level::try_from(args.level).map_err(|_| "invalid level")?;
    let mut config = EncoderConfig::default().with_level(level);
    if let Some(threads) = args.threads {
        config = config.with_threads(threads);
    }
    if let Some(block_size) = args.block_size {
        config = config.with_block_size(block_size);
    }

    let interactive = io::stderr().is_terminal();
    let mut stderr = io::stderr().lock();
    let command = EncodeCommand {
        input: args.input,
        output: args.output,
        config,
    };
    encode_command(&command, interactive, &mut stderr)?;
    Ok(())
}
