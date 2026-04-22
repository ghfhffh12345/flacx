//! Shared command implementations for the `flacx` CLI.
//!
//! `flacx-cli` owns command planning, directory traversal, and progress
//! rendering for the `flacx` binary while delegating encode, decode, and
//! recompress work to the workspace `flacx` library.
//!
//! The authoritative user-facing reference lives in clap help:
//! `flacx --help`, `flacx encode --help`, `flacx decode --help`, and
//! `flacx recompress --help`.

use std::{
    collections::BTreeMap,
    env,
    fs::{self, File},
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use flacx::{
    DecodeConfig, EncoderConfig, Error, FlacReaderOptions, PcmReader, ProgressSnapshot,
    RawPcmDescriptor, RawPcmReader, RecompressConfig, RecompressMode, RecompressPhase,
    RecompressProgress, Result, WavReader, WavReaderOptions, inspect_flac_total_samples,
    inspect_raw_pcm_total_samples, inspect_wav_total_samples, read_flac_reader_with_options,
};
use terminal_size::{Width, terminal_size};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use walkdir::WalkDir;

const SINGLE_PROGRESS_BAR_WIDTH: usize = 24;
const BATCH_PROGRESS_BAR_WIDTH: usize = 10;
const ESTIMATE_WARMUP: Duration = Duration::from_millis(250);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const CLI_READ_BUFFER_CAPACITY: usize = 4 * 1024 * 1024;
const CLI_WRITE_BUFFER_CAPACITY: usize = 10 * 1024 * 1024;
const DEFAULT_PROGRESS_LINE_BUDGET: usize = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EncodeInputFamily {
    WavLike,
    AiffLike,
    Caf,
    Dynamic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeCommand {
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    pub depth: usize,
    pub config: EncoderConfig,
    pub raw_descriptor: Option<RawPcmDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeCommand {
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    pub depth: usize,
    pub config: DecodeConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecompressCommand {
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    pub in_place: bool,
    pub depth: usize,
    pub config: RecompressConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandKind {
    Encode,
    Decode,
    Recompress,
}

impl CommandKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Encode => "encode",
            Self::Decode => "decode",
            Self::Recompress => "recompress",
        }
    }

    fn source_extensions(self) -> &'static [&'static str] {
        match self {
            Self::Encode => &["wav", "rf64", "w64", "aif", "aiff", "aifc", "caf"],
            Self::Decode | Self::Recompress => &["flac"],
        }
    }

    fn target_extension(self) -> &'static str {
        match self {
            Self::Encode => "flac",
            Self::Decode => "wav",
            Self::Recompress => "flac",
        }
    }

    fn single_file_output_error(self, output: &Path) -> String {
        match self {
            Self::Encode => format!(
                "output path '{}' is a directory; use a file path for single-file encode",
                output.display()
            ),
            Self::Decode => format!(
                "output path '{}' is a directory; use a file path for single-file decode",
                output.display()
            ),
            Self::Recompress => format!(
                "output path '{}' is a directory; use a file path for single-file recompress",
                output.display()
            ),
        }
    }

    fn directory_output_error(self, output_root: &Path) -> String {
        match self {
            Self::Encode => format!(
                "output path '{}' is not a directory for folder encode",
                output_root.display()
            ),
            Self::Decode => format!(
                "output path '{}' is not a directory for folder decode",
                output_root.display()
            ),
            Self::Recompress => format!(
                "output path '{}' is not a directory for folder recompress",
                output_root.display()
            ),
        }
    }

    fn traversal_error(self, input_root: &Path, error: &walkdir::Error) -> String {
        match self {
            Self::Encode => format!(
                "failed to traverse encode input directory '{}': {error}",
                input_root.display()
            ),
            Self::Decode => format!(
                "failed to traverse decode input directory '{}': {error}",
                input_root.display()
            ),
            Self::Recompress => format!(
                "failed to traverse recompress input directory '{}': {error}",
                input_root.display()
            ),
        }
    }

    fn empty_directory_worklist_error(self, input_root: &Path, depth: usize) -> String {
        let noun = match self {
            Self::Encode => "supported PCM input files",
            Self::Decode | Self::Recompress => "FLAC input files",
        };
        let depth_hint = if depth == 0 {
            "at any depth".to_string()
        } else {
            format!("within depth {depth}")
        };
        format!(
            "no {noun} found under '{}' {depth_hint}; check the directory contents or adjust --depth",
            input_root.display()
        )
    }

    fn planning_error(self, message: impl Into<String>) -> Error {
        match self {
            Self::Encode => Error::Encode(message.into()),
            Self::Decode => Error::Decode(message.into()),
            Self::Recompress => Error::Encode(message.into()),
        }
    }

    fn default_output_path(self, input: &Path) -> PathBuf {
        match self {
            Self::Encode | Self::Decode => input.with_extension(self.target_extension()),
            Self::Recompress => {
                let stem = input
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("output");
                input.with_file_name(format!("{stem}.recompressed.flac"))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedWorklist {
    items: Vec<ConversionWorkItem>,
    total_samples: u64,
    is_directory: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConversionWorkItem {
    input: PathBuf,
    output: PathBuf,
    display_name: String,
    ensure_parent_dirs: bool,
    input_bytes: u64,
    phase_total_samples: u64,
    overall_total_samples: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProgressDisplay {
    filename: String,
    overall: SampleProgress,
    file: Option<SampleProgress>,
    phase_label: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProgressFrame {
    lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SampleProgress {
    processed_samples: u64,
    total_samples: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ByteProgress {
    input_bytes_read: u64,
    output_bytes_written: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CurrentFileProgress {
    filename: String,
    input_bytes: u64,
    phase_total_samples: u64,
    overall_total_samples: u64,
    phase: Option<RecompressPhase>,
    saw_update: bool,
    started_at: Option<Instant>,
    phase_started_at: Option<Instant>,
    current_progress: Option<ProgressSnapshot>,
    current_recompress_progress: Option<RecompressProgress>,
    file_state: ProgressState,
}

impl CurrentFileProgress {
    fn overall_processed_samples(&self) -> u64 {
        if let Some(progress) = self.current_recompress_progress {
            progress
                .overall_processed_samples
                .min(self.overall_total_samples)
        } else {
            self.current_progress
                .map_or(0, |progress| progress.processed_samples)
                .min(self.phase_total_samples)
        }
    }

    fn overall_processed_bytes(&self) -> ByteProgress {
        if let Some(progress) = self.current_recompress_progress {
            ByteProgress {
                input_bytes_read: progress.overall_input_bytes_read,
                output_bytes_written: progress.overall_output_bytes_written,
            }
        } else {
            self.current_progress.map_or(ByteProgress::default(), |progress| ByteProgress {
                input_bytes_read: progress.input_bytes_read,
                output_bytes_written: progress.output_bytes_written,
            })
        }
    }
}

#[derive(Debug)]
enum ProgressEvent {
    BeginFile {
        file_id: usize,
        filename: String,
        input_bytes: u64,
        phase_total_samples: u64,
        overall_total_samples: u64,
    },
    Progress {
        file_id: usize,
        progress: ProgressSnapshot,
    },
    RecompressProgress {
        file_id: usize,
        progress: RecompressProgress,
    },
    FinishFile {
        file_id: usize,
    },
}

struct ProgressTrace {
    file: File,
    kind: CommandKind,
    interactive: bool,
    batch_mode: bool,
    started_at: Instant,
    planning_started_at: Option<Instant>,
    command_header_written: bool,
}

pub fn encode_command(
    command: &EncodeCommand,
    interactive: bool,
    stderr: &mut impl Write,
) -> Result<()> {
    let (planned, trace) = plan_with_trace(CommandKind::Encode, interactive, || {
        plan_encode_worklist(command)
    })?;
    let config = command.config.clone();
    let raw_descriptor = command.raw_descriptor;
    let progress_mode = progress_reporting_mode(interactive, trace.is_some());

    if progress_mode == ProgressReportingMode::Disabled {
        for (file_id, item) in planned.items.into_iter().enumerate() {
            run_encode_work_item(item, file_id, config.clone(), raw_descriptor, None)?;
        }
        return Ok(());
    }

    run_with_progress_events(
        stderr,
        interactive,
        trace,
        planned.total_samples,
        planned.is_directory,
        move |sender| {
            for (file_id, item) in planned.items.into_iter().enumerate() {
                run_encode_work_item(item, file_id, config.clone(), raw_descriptor, Some(&sender))?;
            }

            Ok(())
        },
    )
}

fn run_encode_work_item(
    item: ConversionWorkItem,
    file_id: usize,
    config: EncoderConfig,
    raw_descriptor: Option<RawPcmDescriptor>,
    sender: Option<&mpsc::Sender<ProgressEvent>>,
) -> Result<()> {
    if item.ensure_parent_dirs {
        ensure_output_parent_dirs(&item.output)?;
    }
    if let Some(sender) = sender {
        send_progress_event(
            sender,
            ProgressEvent::BeginFile {
                file_id,
                filename: item.display_name.clone(),
                input_bytes: item.input_bytes,
                phase_total_samples: item.phase_total_samples,
                overall_total_samples: item.overall_total_samples,
            },
        )?;
    }

    let result = if let Some(raw_descriptor) = raw_descriptor {
        let reader = RawPcmReader::new(open_buffered_reader(&item.input)?, raw_descriptor)?;
        let stream = reader.into_pcm_stream()?;
        let mut encoder = config.into_encoder(create_buffered_writer(&item.output)?);
        if let Some(sender) = sender {
            encoder
                .encode_with_progress(stream, |update| {
                    send_progress_event(
                        sender,
                        ProgressEvent::Progress {
                            file_id,
                            progress: update,
                        },
                    )?;
                    Ok(())
                })
                .map(|_| ())
        } else {
            encoder.encode(stream).map(|_| ())
        }
    } else {
        let wav_reader_options = WavReaderOptions {
            capture_fxmd: config.capture_fxmd(),
            strict_fxmd_validation: config.strict_fxmd_validation(),
        };
        let mut encoder = config.into_encoder(create_buffered_writer(&item.output)?);
        match sender {
            Some(sender) => {
                let on_progress = |update| {
                    send_progress_event(
                        sender,
                        ProgressEvent::Progress {
                            file_id,
                            progress: update,
                        },
                    )?;
                    Ok(())
                };
                match encode_input_family(&item.input) {
                    EncodeInputFamily::WavLike => encoder
                        .encode_source_with_progress(
                            WavReader::with_reader_options(
                                open_buffered_reader(&item.input)?,
                                wav_reader_options,
                            )?
                            .into_source(),
                            on_progress,
                        )
                        .map(|_| ()),
                    EncodeInputFamily::AiffLike => encoder
                        .encode_source_with_progress(
                            flacx::AiffReader::new(open_buffered_reader(&item.input)?)?
                                .into_source(),
                            on_progress,
                        )
                        .map(|_| ()),
                    EncodeInputFamily::Caf => encoder
                        .encode_source_with_progress(
                            flacx::CafReader::new(open_buffered_reader(&item.input)?)?
                                .into_source(),
                            on_progress,
                        )
                        .map(|_| ()),
                    EncodeInputFamily::Dynamic => encoder
                        .encode_source_with_progress(
                            PcmReader::with_reader_options(
                                open_buffered_reader(&item.input)?,
                                wav_reader_options,
                            )?
                            .into_source(),
                            on_progress,
                        )
                        .map(|_| ()),
                }
            }
            None => match encode_input_family(&item.input) {
                EncodeInputFamily::WavLike => encoder
                    .encode_source(
                        WavReader::with_reader_options(
                            open_buffered_reader(&item.input)?,
                            wav_reader_options,
                        )?
                        .into_source(),
                    )
                    .map(|_| ()),
                EncodeInputFamily::AiffLike => encoder
                    .encode_source(
                        flacx::AiffReader::new(open_buffered_reader(&item.input)?)?.into_source(),
                    )
                    .map(|_| ()),
                EncodeInputFamily::Caf => encoder
                    .encode_source(
                        flacx::CafReader::new(open_buffered_reader(&item.input)?)?.into_source(),
                    )
                    .map(|_| ()),
                EncodeInputFamily::Dynamic => encoder
                    .encode_source(
                        PcmReader::with_reader_options(
                            open_buffered_reader(&item.input)?,
                            wav_reader_options,
                        )?
                        .into_source(),
                    )
                    .map(|_| ()),
            },
        }
    };

    result?;
    if let Some(sender) = sender {
        send_progress_event(sender, ProgressEvent::FinishFile { file_id })?;
    }
    Ok(())
}

pub fn decode_command(
    command: &DecodeCommand,
    interactive: bool,
    stderr: &mut impl Write,
) -> Result<()> {
    let (planned, trace) = plan_with_trace(CommandKind::Decode, interactive, || {
        plan_decode_worklist(command)
    })?;
    let config = command.config;
    let progress_mode = progress_reporting_mode(interactive, trace.is_some());

    if progress_mode == ProgressReportingMode::Disabled {
        for (file_id, item) in planned.items.into_iter().enumerate() {
            run_decode_work_item(item, file_id, config, None)?;
        }
        return Ok(());
    }

    run_with_progress_events(
        stderr,
        interactive,
        trace,
        planned.total_samples,
        planned.is_directory,
        move |sender| {
            for (file_id, item) in planned.items.into_iter().enumerate() {
                run_decode_work_item(item, file_id, config, Some(&sender))?;
            }

            Ok(())
        },
    )
}

fn run_decode_work_item(
    item: ConversionWorkItem,
    file_id: usize,
    config: DecodeConfig,
    sender: Option<&mpsc::Sender<ProgressEvent>>,
) -> Result<()> {
    if item.ensure_parent_dirs {
        ensure_output_parent_dirs(&item.output)?;
    }
    if let Some(sender) = sender {
        send_progress_event(
            sender,
            ProgressEvent::BeginFile {
                file_id,
                filename: item.display_name.clone(),
                input_bytes: item.input_bytes,
                phase_total_samples: item.phase_total_samples,
                overall_total_samples: item.overall_total_samples,
            },
        )?;
    }
    let output_container =
        decode_output_container_from_path(&item.output)?.unwrap_or(config.output_container());
    let reader = read_flac_reader_with_options(
        open_buffered_reader(&item.input)?,
        FlacReaderOptions {
            strict_seektable_validation: config.strict_seektable_validation(),
            strict_channel_mask_provenance: config.strict_channel_mask_provenance(),
        },
    )?;
    let mut decoder = config
        .with_output_container(output_container)
        .into_decoder(create_buffered_writer(&item.output)?);
    match sender {
        Some(sender) => {
            decoder.decode_source_with_progress(reader.into_decode_source(), |update| {
                send_progress_event(
                    sender,
                    ProgressEvent::Progress {
                        file_id,
                        progress: update,
                    },
                )?;
                Ok(())
            })?;
            send_progress_event(sender, ProgressEvent::FinishFile { file_id })?;
        }
        None => {
            decoder.decode_source(reader.into_decode_source())?;
        }
    }
    Ok(())
}

fn recompress_temp_output_path(path: &Path) -> PathBuf {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("out");
    path.with_extension(format!("{extension}.tmp"))
}

fn recompress_reader_options(config: RecompressConfig) -> FlacReaderOptions {
    match config.mode() {
        RecompressMode::Loose | RecompressMode::Default => FlacReaderOptions {
            strict_seektable_validation: false,
            strict_channel_mask_provenance: false,
        },
        RecompressMode::Strict => FlacReaderOptions {
            strict_seektable_validation: true,
            strict_channel_mask_provenance: true,
        },
    }
}

fn decode_output_container_from_path(path: &Path) -> Result<Option<flacx::PcmContainer>> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("rf64") => Ok(Some(flacx::PcmContainer::Rf64)),
        Some(ext) if ext.eq_ignore_ascii_case("w64") => Ok(Some(flacx::PcmContainer::Wave64)),
        Some(ext) if ext.eq_ignore_ascii_case("aif") || ext.eq_ignore_ascii_case("aiff") => {
            Ok(Some(flacx::PcmContainer::Aiff))
        }
        Some(ext) if ext.eq_ignore_ascii_case("aifc") => Ok(Some(flacx::PcmContainer::Aifc)),
        Some(ext) if ext.eq_ignore_ascii_case("caf") => Ok(Some(flacx::PcmContainer::Caf)),
        Some(ext) if ext.eq_ignore_ascii_case("wav") => Ok(Some(flacx::PcmContainer::Wave)),
        Some(ext) => Err(Error::Decode(format!(
            "unsupported decode output extension '.{ext}'"
        ))),
        None => Ok(None),
    }
}

fn plan_with_trace(
    kind: CommandKind,
    interactive: bool,
    plan: impl FnOnce() -> Result<PlannedWorklist>,
) -> Result<(PlannedWorklist, Option<ProgressTrace>)> {
    let mut trace = ProgressTrace::from_env(kind, interactive)?;
    if let Some(trace) = trace.as_mut() {
        trace.on_planning_start()?;
    }
    let planned = plan()?;
    if let Some(trace) = trace.as_mut() {
        trace.on_planning_finish(
            planned.is_directory,
            planned.items.len(),
            planned.total_samples,
        )?;
    }
    Ok((planned, trace))
}

pub fn recompress_command(
    command: &RecompressCommand,
    interactive: bool,
    stderr: &mut impl Write,
) -> Result<()> {
    let (planned, trace) = plan_with_trace(CommandKind::Recompress, interactive, || {
        plan_recompress_worklist(command)
    })?;
    let config = command.config;
    let progress_mode = progress_reporting_mode(interactive, trace.is_some());

    if progress_mode == ProgressReportingMode::Disabled {
        for (file_id, item) in planned.items.into_iter().enumerate() {
            run_recompress_work_item(item, file_id, config, None)?;
        }
        return Ok(());
    }

    run_with_progress_events(
        stderr,
        interactive,
        trace,
        planned.total_samples,
        planned.is_directory,
        move |sender| {
            for (file_id, item) in planned.items.into_iter().enumerate() {
                run_recompress_work_item(item, file_id, config, Some(&sender))?;
            }

            Ok(())
        },
    )
}

fn run_recompress_work_item(
    item: ConversionWorkItem,
    file_id: usize,
    config: RecompressConfig,
    sender: Option<&mpsc::Sender<ProgressEvent>>,
) -> Result<()> {
    if item.ensure_parent_dirs {
        ensure_output_parent_dirs(&item.output)?;
    }
    if let Some(sender) = sender {
        send_progress_event(
            sender,
            ProgressEvent::BeginFile {
                file_id,
                filename: item.display_name.clone(),
                input_bytes: item.input_bytes,
                phase_total_samples: item.phase_total_samples,
                overall_total_samples: item.overall_total_samples,
            },
        )?;
    }
    let temp_output = recompress_temp_output_path(&item.output);
    let reader = read_flac_reader_with_options(
        open_buffered_reader(&item.input)?,
        recompress_reader_options(config),
    )?;
    let source = reader.into_recompress_source();
    let mut recompressor = config.into_recompressor(create_buffered_writer(&temp_output)?);
    let result = match sender {
        Some(sender) => recompressor.recompress_with_progress(source, |update| {
            send_progress_event(
                sender,
                ProgressEvent::RecompressProgress {
                    file_id,
                    progress: update,
                },
            )?;
            Ok(())
        }),
        None => recompressor.recompress(source),
    };

    match result {
        Ok(_) => {
            fs::rename(&temp_output, &item.output)?;
            if let Some(sender) = sender {
                send_progress_event(sender, ProgressEvent::FinishFile { file_id })?;
            }
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&temp_output);
            Err(error)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgressReportingMode {
    Disabled,
    Events,
}

fn progress_reporting_mode(interactive: bool, trace_enabled: bool) -> ProgressReportingMode {
    if interactive || trace_enabled {
        ProgressReportingMode::Events
    } else {
        ProgressReportingMode::Disabled
    }
}

fn encode_input_family(path: &Path) -> EncodeInputFamily {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext)
            if ext.eq_ignore_ascii_case("wav")
                || ext.eq_ignore_ascii_case("rf64")
                || ext.eq_ignore_ascii_case("w64") =>
        {
            EncodeInputFamily::WavLike
        }
        Some(ext) if ext.eq_ignore_ascii_case("aif") || ext.eq_ignore_ascii_case("aiff") => {
            EncodeInputFamily::AiffLike
        }
        Some(ext) if ext.eq_ignore_ascii_case("aifc") => EncodeInputFamily::AiffLike,
        Some(ext) if ext.eq_ignore_ascii_case("caf") => EncodeInputFamily::Caf,
        _ => EncodeInputFamily::Dynamic,
    }
}

fn plan_encode_worklist(command: &EncodeCommand) -> Result<PlannedWorklist> {
    if let Some(raw_descriptor) = command.raw_descriptor {
        return plan_raw_encode_worklist(command, raw_descriptor);
    }
    plan_worklist(
        CommandKind::Encode,
        &command.input,
        command.output.as_deref(),
        command.depth,
        inspect_pcm_file_total_samples,
    )
}

fn plan_raw_encode_worklist(
    command: &EncodeCommand,
    raw_descriptor: RawPcmDescriptor,
) -> Result<PlannedWorklist> {
    if command.input.is_dir() {
        return Err(CommandKind::Encode
            .planning_error("raw PCM encode does not support directory input in Stage 3"));
    }

    let total_samples =
        inspect_raw_pcm_total_samples(open_buffered_reader(&command.input)?, raw_descriptor)?;
    let output = match command.output.as_deref() {
        Some(output) => {
            if output.is_dir() {
                return Err(CommandKind::Encode
                    .planning_error(CommandKind::Encode.single_file_output_error(output)));
            }
            output.to_path_buf()
        }
        None => CommandKind::Encode.default_output_path(&command.input),
    };
    let item = ConversionWorkItem {
        input: command.input.clone(),
        output,
        display_name: file_display_name(&command.input),
        ensure_parent_dirs: false,
        input_bytes: input_file_size(&command.input)?,
        phase_total_samples: total_samples,
        overall_total_samples: total_samples,
    };
    Ok(PlannedWorklist {
        total_samples: item.overall_total_samples,
        items: vec![item],
        is_directory: false,
    })
}

fn plan_decode_worklist(command: &DecodeCommand) -> Result<PlannedWorklist> {
    if command.input.is_dir() {
        plan_decode_directory_worklist(command)
    } else {
        let item = plan_decode_single_file_work_item(command)?;
        Ok(PlannedWorklist {
            total_samples: item.overall_total_samples,
            items: vec![item],
            is_directory: false,
        })
    }
}

fn plan_recompress_worklist(command: &RecompressCommand) -> Result<PlannedWorklist> {
    if command.input.is_dir() {
        plan_recompress_directory_worklist(command)
    } else {
        let item = plan_recompress_single_file_work_item(command)?;
        Ok(PlannedWorklist {
            total_samples: item.overall_total_samples,
            items: vec![item],
            is_directory: false,
        })
    }
}

fn plan_decode_single_file_work_item(command: &DecodeCommand) -> Result<ConversionWorkItem> {
    let input = &command.input;
    let total_samples = inspect_flac_file_total_samples(input)?;
    let output = match command.output.as_deref() {
        Some(output) => {
            if output.is_dir() {
                return Err(CommandKind::Decode
                    .planning_error(CommandKind::Decode.single_file_output_error(output)));
            }
            output.to_path_buf()
        }
        None => input.with_extension(decode_target_extension(command.config.output_container())),
    };

    Ok(ConversionWorkItem {
        input: input.to_path_buf(),
        output,
        display_name: file_display_name(input),
        ensure_parent_dirs: false,
        input_bytes: input_file_size(input)?,
        phase_total_samples: total_samples,
        overall_total_samples: total_samples,
    })
}

fn plan_decode_directory_worklist(command: &DecodeCommand) -> Result<PlannedWorklist> {
    let output_root = match command.output.as_deref() {
        Some(output_root) => Some(validate_output_root(CommandKind::Decode, output_root)?),
        None => None,
    };
    let target_extension = decode_target_extension(command.config.output_container());

    let mut walker = WalkDir::new(&command.input)
        .follow_links(false)
        .min_depth(1);
    if command.depth != 0 {
        walker = walker.max_depth(command.depth);
    }

    let mut items = Vec::new();
    let mut total_samples = 0u64;
    for entry in walker {
        let entry = entry.map_err(|error| {
            CommandKind::Decode
                .planning_error(CommandKind::Decode.traversal_error(&command.input, &error))
        })?;
        if !entry.file_type().is_file() || !has_any_extension(entry.path(), &["flac"]) {
            continue;
        }

        let input = entry.path().to_path_buf();
        let relative = input.strip_prefix(&command.input).map_err(|_| {
            CommandKind::Decode.planning_error("failed to derive relative input path")
        })?;
        let total_for_file = inspect_flac_file_total_samples(&input)?;
        total_samples = total_samples.checked_add(total_for_file).ok_or_else(|| {
            CommandKind::Decode
                .planning_error("total sample count overflowed batch progress accounting")
        })?;
        let display_name = relative_display_name(relative);
        let (output, ensure_parent_dirs) = match output_root.as_deref() {
            Some(output_root) => (
                output_root.join(relative).with_extension(target_extension),
                true,
            ),
            None => (input.with_extension(target_extension), false),
        };

        items.push(ConversionWorkItem {
            input,
            output,
            display_name,
            ensure_parent_dirs,
            input_bytes: input_file_size(entry.path())?,
            phase_total_samples: total_for_file,
            overall_total_samples: total_for_file,
        });
    }

    items.sort_by(|left, right| left.display_name.cmp(&right.display_name));
    if items.is_empty() {
        return Err(CommandKind::Decode.planning_error(
            CommandKind::Decode.empty_directory_worklist_error(&command.input, command.depth),
        ));
    }
    Ok(PlannedWorklist {
        items,
        total_samples,
        is_directory: true,
    })
}

fn decode_target_extension(container: flacx::PcmContainer) -> &'static str {
    match container {
        flacx::PcmContainer::Rf64 => "rf64",
        flacx::PcmContainer::Wave64 => "w64",
        flacx::PcmContainer::Aiff => "aiff",
        flacx::PcmContainer::Aifc => "aifc",
        flacx::PcmContainer::Caf => "caf",
        flacx::PcmContainer::Auto | flacx::PcmContainer::Wave => "wav",
    }
}

fn plan_worklist(
    kind: CommandKind,
    input: &Path,
    output: Option<&Path>,
    depth: usize,
    inspect_total_samples: fn(&Path) -> Result<u64>,
) -> Result<PlannedWorklist> {
    if input.is_dir() {
        plan_directory_worklist(kind, input, output, depth, inspect_total_samples)
    } else {
        let item = plan_single_file_work_item(kind, input, output, inspect_total_samples)?;
        Ok(PlannedWorklist {
            total_samples: item.overall_total_samples,
            items: vec![item],
            is_directory: false,
        })
    }
}

fn plan_single_file_work_item(
    kind: CommandKind,
    input: &Path,
    output: Option<&Path>,
    inspect_total_samples: fn(&Path) -> Result<u64>,
) -> Result<ConversionWorkItem> {
    let total_samples = inspect_total_samples(input)?;
    let output = match output {
        Some(output) => {
            if output.is_dir() {
                return Err(kind.planning_error(kind.single_file_output_error(output)));
            }
            if kind == CommandKind::Recompress && output == input {
                return Err(kind.planning_error(
                    "single-file recompress output must differ from the input path",
                ));
            }
            output.to_path_buf()
        }
        None => kind.default_output_path(input),
    };

    Ok(ConversionWorkItem {
        input: input.to_path_buf(),
        output,
        display_name: file_display_name(input),
        ensure_parent_dirs: false,
        input_bytes: input_file_size(input)?,
        phase_total_samples: total_samples,
        overall_total_samples: total_samples,
    })
}

fn plan_directory_worklist(
    kind: CommandKind,
    input_root: &Path,
    output_root: Option<&Path>,
    depth: usize,
    inspect_total_samples: fn(&Path) -> Result<u64>,
) -> Result<PlannedWorklist> {
    if kind == CommandKind::Recompress
        && output_root.is_some_and(|output_root| output_root == input_root)
    {
        return Err(kind
            .planning_error("folder recompress output root must differ from the input directory"));
    }
    let output_root = match output_root {
        Some(output_root) => Some(validate_output_root(kind, output_root)?),
        None => None,
    };

    let (mut worklist, total_samples) = collect_directory_work_items(
        kind,
        input_root,
        output_root.as_deref(),
        depth,
        inspect_total_samples,
    )?;
    worklist.sort_by(|left, right| left.display_name.cmp(&right.display_name));
    if worklist.is_empty() {
        return Err(kind.planning_error(kind.empty_directory_worklist_error(input_root, depth)));
    }

    Ok(PlannedWorklist {
        items: worklist,
        total_samples,
        is_directory: true,
    })
}

fn validate_output_root(kind: CommandKind, output_root: &Path) -> Result<PathBuf> {
    if output_root.exists() && !output_root.is_dir() {
        return Err(kind.planning_error(kind.directory_output_error(output_root)));
    }

    Ok(output_root.to_path_buf())
}

fn collect_directory_work_items(
    kind: CommandKind,
    input_root: &Path,
    output_root: Option<&Path>,
    depth: usize,
    inspect_total_samples: fn(&Path) -> Result<u64>,
) -> Result<(Vec<ConversionWorkItem>, u64)> {
    let mut walker = WalkDir::new(input_root).follow_links(false).min_depth(1);
    if depth != 0 {
        walker = walker.max_depth(depth);
    }

    let mut worklist = Vec::new();
    let mut total_samples = 0u64;
    for entry in walker {
        let entry =
            entry.map_err(|error| kind.planning_error(kind.traversal_error(input_root, &error)))?;
        if !entry.file_type().is_file()
            || !has_any_extension(entry.path(), kind.source_extensions())
        {
            continue;
        }

        let input = entry.path().to_path_buf();
        let relative = input
            .strip_prefix(input_root)
            .map_err(|_| kind.planning_error("failed to derive relative input path"))?;
        let total_for_file = inspect_total_samples(&input)?;
        total_samples = total_samples.checked_add(total_for_file).ok_or_else(|| {
            kind.planning_error("total sample count overflowed batch progress accounting")
        })?;
        let display_name = relative_display_name(relative);
        let (output, ensure_parent_dirs) = match output_root {
            Some(output_root) => (
                output_root
                    .join(relative)
                    .with_extension(kind.target_extension()),
                true,
            ),
            None => (kind.default_output_path(&input), false),
        };

        worklist.push(ConversionWorkItem {
            input,
            output,
            display_name,
            ensure_parent_dirs,
            input_bytes: input_file_size(entry.path())?,
            phase_total_samples: total_for_file,
            overall_total_samples: total_for_file,
        });
    }

    Ok((worklist, total_samples))
}

fn plan_recompress_single_file_work_item(
    command: &RecompressCommand,
) -> Result<ConversionWorkItem> {
    let input = &command.input;
    let phase_total_samples = inspect_flac_file_total_samples(input)?;
    let overall_total_samples = phase_total_samples.saturating_mul(2);
    let output = if command.in_place {
        input.to_path_buf()
    } else {
        match command.output.as_deref() {
            Some(output) => {
                if output.is_dir() {
                    return Err(CommandKind::Recompress
                        .planning_error(CommandKind::Recompress.single_file_output_error(output)));
                }
                if output == input {
                    return Err(CommandKind::Recompress
                        .planning_error("single-file recompress output must differ from the input path unless --in-place is used"));
                }
                output.to_path_buf()
            }
            None => CommandKind::Recompress.default_output_path(input),
        }
    };

    Ok(ConversionWorkItem {
        input: input.to_path_buf(),
        output,
        display_name: file_display_name(input),
        ensure_parent_dirs: false,
        input_bytes: input_file_size(input)?,
        phase_total_samples,
        overall_total_samples,
    })
}

fn plan_recompress_directory_worklist(command: &RecompressCommand) -> Result<PlannedWorklist> {
    let output_root = if command.in_place {
        None
    } else {
        match command.output.as_deref() {
            Some(output_root) => {
                if output_root == command.input {
                    return Err(CommandKind::Recompress.planning_error(
                        "folder recompress output root must differ from the input directory unless --in-place is used",
                    ));
                }
                Some(validate_output_root(CommandKind::Recompress, output_root)?)
            }
            None => None,
        }
    };

    let mut walker = WalkDir::new(&command.input)
        .follow_links(false)
        .min_depth(1);
    if command.depth != 0 {
        walker = walker.max_depth(command.depth);
    }

    let mut items = Vec::new();
    let mut total_samples = 0u64;
    for entry in walker {
        let entry = entry.map_err(|error| {
            CommandKind::Recompress
                .planning_error(CommandKind::Recompress.traversal_error(&command.input, &error))
        })?;
        if !entry.file_type().is_file() || !has_any_extension(entry.path(), &["flac"]) {
            continue;
        }

        let input = entry.path().to_path_buf();
        let relative = input.strip_prefix(&command.input).map_err(|_| {
            CommandKind::Recompress.planning_error("failed to derive relative input path")
        })?;
        let phase_total_samples = inspect_flac_file_total_samples(&input)?;
        let overall_total_samples = phase_total_samples.saturating_mul(2);
        total_samples = total_samples
            .checked_add(overall_total_samples)
            .ok_or_else(|| {
                CommandKind::Recompress
                    .planning_error("total sample count overflowed batch progress accounting")
            })?;
        let display_name = relative_display_name(relative);
        let (output, ensure_parent_dirs) = if command.in_place {
            (input.clone(), false)
        } else if let Some(output_root) = output_root.as_deref() {
            (output_root.join(relative).with_extension("flac"), true)
        } else {
            (CommandKind::Recompress.default_output_path(&input), false)
        };

        items.push(ConversionWorkItem {
            input,
            output,
            display_name,
            ensure_parent_dirs,
            input_bytes: input_file_size(entry.path())?,
            phase_total_samples,
            overall_total_samples,
        });
    }

    items.sort_by(|left, right| left.display_name.cmp(&right.display_name));
    if items.is_empty() {
        return Err(CommandKind::Recompress.planning_error(
            CommandKind::Recompress.empty_directory_worklist_error(&command.input, command.depth),
        ));
    }
    Ok(PlannedWorklist {
        items,
        total_samples,
        is_directory: true,
    })
}

fn inspect_pcm_file_total_samples(path: &Path) -> Result<u64> {
    inspect_wav_total_samples(open_buffered_reader(path)?)
}

fn inspect_flac_file_total_samples(path: &Path) -> Result<u64> {
    inspect_flac_total_samples(open_buffered_reader(path)?)
}

fn open_buffered_reader(path: &Path) -> Result<BufReader<File>> {
    Ok(BufReader::with_capacity(
        CLI_READ_BUFFER_CAPACITY,
        File::open(path)?,
    ))
}

fn create_buffered_writer(path: &Path) -> Result<BufWriter<File>> {
    Ok(BufWriter::with_capacity(
        CLI_WRITE_BUFFER_CAPACITY,
        File::create(path)?,
    ))
}

fn input_file_size(path: &Path) -> Result<u64> {
    Ok(fs::metadata(path)?.len())
}

fn ensure_output_parent_dirs(output: &Path) -> Result<()> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn has_any_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            extensions
                .iter()
                .any(|candidate| ext.eq_ignore_ascii_case(candidate))
        })
}

fn file_display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn relative_display_name(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

impl ProgressTrace {
    fn from_env(kind: CommandKind, interactive: bool) -> Result<Option<Self>> {
        let Some(path) = env::var_os("FLACX_PROGRESS_TRACE").map(PathBuf::from) else {
            return Ok(None);
        };
        Ok(Some(Self {
            file: File::create(path)?,
            kind,
            interactive,
            batch_mode: false,
            started_at: Instant::now(),
            planning_started_at: None,
            command_header_written: false,
        }))
    }

    fn write_command_header(&mut self) -> std::io::Result<()> {
        if self.command_header_written {
            return Ok(());
        }
        writeln!(
            self.file,
            "event=command\tkind={}\tinteractive={}\tbatch_mode={}",
            self.kind.as_str(),
            u8::from(self.interactive),
            u8::from(self.batch_mode)
        )?;
        self.command_header_written = true;
        self.file.flush()?;
        Ok(())
    }

    fn on_planning_start(&mut self) -> std::io::Result<()> {
        self.planning_started_at = Some(Instant::now());
        writeln!(
            self.file,
            "event=planning_start\tkind={}\tinteractive={}\tcommand_elapsed_seconds={:.9}",
            self.kind.as_str(),
            u8::from(self.interactive),
            self.started_at.elapsed().as_secs_f64()
        )?;
        self.file.flush()?;
        Ok(())
    }

    fn on_planning_finish(
        &mut self,
        batch_mode: bool,
        item_count: usize,
        total_samples: u64,
    ) -> std::io::Result<()> {
        self.batch_mode = batch_mode;
        self.write_command_header()?;
        writeln!(
            self.file,
            "event=planning_finish\tkind={}\tinteractive={}\tbatch_mode={}\titem_count={}\ttotal_samples={}\tcommand_elapsed_seconds={:.9}\tplanning_elapsed_seconds={:.9}",
            self.kind.as_str(),
            u8::from(self.interactive),
            u8::from(self.batch_mode),
            item_count,
            total_samples,
            self.started_at.elapsed().as_secs_f64(),
            self.planning_started_at
                .map_or(0.0, |started_at| started_at.elapsed().as_secs_f64())
        )?;
        self.file.flush()?;
        Ok(())
    }

    fn on_file_begin(
        &mut self,
        filename: &str,
        input_bytes: u64,
        phase_total_samples: u64,
        overall_total_samples: u64,
    ) -> std::io::Result<()> {
        self.write_command_header()?;
        writeln!(
            self.file,
            "event=file_begin\tkind={}\tinteractive={}\tbatch_mode={}\tfilename={}\tinput_bytes={}\tphase_total_samples={}\toverall_total_samples={}\tcommand_elapsed_seconds={:.9}",
            self.kind.as_str(),
            u8::from(self.interactive),
            u8::from(self.batch_mode),
            sanitize_trace_field(filename),
            input_bytes,
            phase_total_samples,
            overall_total_samples,
            self.started_at.elapsed().as_secs_f64()
        )?;
        self.file.flush()?;
        Ok(())
    }

    fn on_first_progress(
        &mut self,
        filename: &str,
        phase: Option<&str>,
        processed_samples: u64,
    ) -> std::io::Result<()> {
        self.write_command_header()?;
        writeln!(
            self.file,
            "event=first_progress\tkind={}\tinteractive={}\tbatch_mode={}\tfilename={}\tphase={}\tprocessed_samples={}\tcommand_elapsed_seconds={:.9}",
            self.kind.as_str(),
            u8::from(self.interactive),
            u8::from(self.batch_mode),
            sanitize_trace_field(filename),
            phase.unwrap_or("-"),
            processed_samples,
            self.started_at.elapsed().as_secs_f64()
        )?;
        self.file.flush()?;
        Ok(())
    }

    fn on_file_finish(
        &mut self,
        filename: &str,
        input_bytes: u64,
        elapsed: Duration,
    ) -> std::io::Result<()> {
        self.write_command_header()?;
        writeln!(
            self.file,
            "event=file_finish\tkind={}\tinteractive={}\tbatch_mode={}\tfilename={}\tinput_bytes={}\telapsed_seconds={:.9}\tcommand_elapsed_seconds={:.9}",
            self.kind.as_str(),
            u8::from(self.interactive),
            u8::from(self.batch_mode),
            sanitize_trace_field(filename),
            input_bytes,
            elapsed.as_secs_f64(),
            self.started_at.elapsed().as_secs_f64()
        )?;
        self.file.flush()?;
        Ok(())
    }
}

fn run_with_progress_events<W, F>(
    writer: &mut W,
    interactive: bool,
    trace: Option<ProgressTrace>,
    total_samples: u64,
    batch_mode: bool,
    run: F,
) -> Result<()>
where
    W: Write,
    F: FnOnce(mpsc::Sender<ProgressEvent>) -> Result<()> + Send,
{
    thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel();
        let worker = scope.spawn(move || run(sender));

        let render_result = drive_progress_events(
            writer,
            interactive,
            trace,
            total_samples,
            batch_mode,
            receiver,
        );
        let worker_result = match worker.join() {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        };

        render_result?;
        worker_result
    })
}

fn drive_progress_events<W: Write>(
    writer: W,
    interactive: bool,
    trace: Option<ProgressTrace>,
    total_samples: u64,
    batch_mode: bool,
    receiver: mpsc::Receiver<ProgressEvent>,
) -> Result<()> {
    let mut progress =
        BatchProgressCoordinator::new(writer, interactive, trace, total_samples, batch_mode);

    loop {
        match progress.next_heartbeat_delay() {
            Some(timeout) => match receiver.recv_timeout(timeout) {
                Ok(event) => progress.handle_event(event)?,
                Err(mpsc::RecvTimeoutError::Timeout) => progress.heartbeat()?,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => match receiver.recv() {
                Ok(event) => progress.handle_event(event)?,
                Err(_) => break,
            },
        }
    }

    progress.finish()?;
    Ok(())
}

fn send_progress_event(
    sender: &mpsc::Sender<ProgressEvent>,
    event: ProgressEvent,
) -> std::io::Result<()> {
    sender
        .send(event)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::BrokenPipe, error.to_string()))
}

fn sanitize_trace_field(field: &str) -> String {
    field
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

struct BatchProgressCoordinator<W: Write> {
    renderer: ProgressRenderer<W>,
    trace: Option<ProgressTrace>,
    total_samples: u64,
    batch_mode: bool,
    completed_samples: u64,
    completed_input_bytes: u64,
    completed_output_bytes: u64,
    active_files: BTreeMap<usize, CurrentFileProgress>,
    display_file_id: Option<usize>,
    batch_started_at: Option<Instant>,
    overall_state: ProgressState,
}

impl<W: Write> BatchProgressCoordinator<W> {
    fn new(
        writer: W,
        interactive: bool,
        trace: Option<ProgressTrace>,
        total_samples: u64,
        batch_mode: bool,
    ) -> Self {
        Self {
            renderer: ProgressRenderer::new(writer, interactive),
            trace,
            total_samples,
            batch_mode,
            completed_samples: 0,
            completed_input_bytes: 0,
            completed_output_bytes: 0,
            active_files: BTreeMap::new(),
            display_file_id: None,
            batch_started_at: None,
            overall_state: ProgressState::default(),
        }
    }

    fn handle_event(&mut self, event: ProgressEvent) -> std::io::Result<()> {
        match event {
            ProgressEvent::BeginFile {
                file_id,
                filename,
                input_bytes,
                phase_total_samples,
                overall_total_samples,
            } => self.begin_file_for(
                file_id,
                &filename,
                input_bytes,
                phase_total_samples,
                overall_total_samples,
            ),
            ProgressEvent::Progress { file_id, progress } => self.observe_for(file_id, progress),
            ProgressEvent::RecompressProgress { file_id, progress } => {
                self.observe_recompress_for(file_id, progress)
            }
            ProgressEvent::FinishFile { file_id } => self.finish_file(file_id),
        }
    }

    #[cfg(test)]
    fn begin_file(
        &mut self,
        filename: &str,
        input_bytes: u64,
        phase_total_samples: u64,
        overall_total_samples: u64,
    ) -> std::io::Result<()> {
        self.begin_file_for(
            0,
            filename,
            input_bytes,
            phase_total_samples,
            overall_total_samples,
        )
    }

    fn begin_file_for(
        &mut self,
        file_id: usize,
        filename: &str,
        input_bytes: u64,
        phase_total_samples: u64,
        overall_total_samples: u64,
    ) -> std::io::Result<()> {
        self.active_files.insert(
            file_id,
            CurrentFileProgress {
                filename: filename.to_string(),
                input_bytes,
                phase_total_samples,
                overall_total_samples,
                phase: None,
                saw_update: false,
                started_at: Some(Instant::now()),
                phase_started_at: None,
                current_progress: None,
                current_recompress_progress: None,
                file_state: ProgressState::default(),
            },
        );
        self.display_file_id = Some(file_id);
        self.batch_started_at.get_or_insert_with(Instant::now);
        if let Some(trace) = self.trace.as_mut() {
            trace.on_file_begin(
                filename,
                input_bytes,
                phase_total_samples,
                overall_total_samples,
            )?;
        }
        if phase_total_samples == overall_total_samples {
            self.render_initial_file_frame_for(file_id, phase_total_samples)?;
        } else {
            self.render_initial_recompress_frame_for(
                file_id,
                phase_total_samples,
                overall_total_samples,
            )?;
        }
        Ok(())
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn observe(&mut self, progress: ProgressSnapshot) -> std::io::Result<()> {
        self.observe_for(0, progress)
    }

    fn observe_for(&mut self, file_id: usize, progress: ProgressSnapshot) -> std::io::Result<()> {
        let batch_elapsed = self.batch_elapsed();
        let file_elapsed = self.current_file_elapsed_for(file_id);
        self.observe_with_elapsed_for(file_id, progress, batch_elapsed, file_elapsed)
    }

    #[cfg(test)]
    fn observe_recompress(&mut self, progress: RecompressProgress) -> std::io::Result<()> {
        self.observe_recompress_for(0, progress)
    }

    fn observe_recompress_for(
        &mut self,
        file_id: usize,
        progress: RecompressProgress,
    ) -> std::io::Result<()> {
        let batch_elapsed = self.batch_elapsed();
        self.observe_recompress_with_elapsed_for(file_id, progress, batch_elapsed)
    }

    #[cfg(test)]
    fn observe_with_elapsed(
        &mut self,
        progress: ProgressSnapshot,
        batch_elapsed: Duration,
        file_elapsed: Duration,
    ) -> std::io::Result<()> {
        self.observe_with_elapsed_for(0, progress, batch_elapsed, file_elapsed)
    }

    fn observe_with_elapsed_for(
        &mut self,
        file_id: usize,
        progress: ProgressSnapshot,
        batch_elapsed: Duration,
        file_elapsed: Duration,
    ) -> std::io::Result<()> {
        let (filename, total_samples, first_progress_samples) = {
            let current = self
                .active_files
                .get_mut(&file_id)
                .expect("current file must be set");
            let first_progress_samples = (!current.saw_update && progress.processed_samples > 0)
                .then_some(progress.processed_samples.min(current.phase_total_samples));
            current.saw_update = true;
            current.current_progress = Some(progress);
            current.current_recompress_progress = None;
            (
                current.filename.clone(),
                current.phase_total_samples,
                first_progress_samples,
            )
        };
        self.display_file_id = Some(file_id);
        if let Some(processed_samples) = first_progress_samples
            && let Some(trace) = self.trace.as_mut()
        {
            trace.on_first_progress(&filename, None, processed_samples)?;
        }
        self.render_snapshot_for(
            file_id,
            total_samples,
            progress,
            batch_elapsed,
            file_elapsed,
        )
    }

    fn render_snapshot_for(
        &mut self,
        file_id: usize,
        total_samples: u64,
        progress: ProgressSnapshot,
        batch_elapsed: Duration,
        file_elapsed: Duration,
    ) -> std::io::Result<()> {
        let (display, overall_bytes, file_bytes) = {
            let overall_processed = self.active_overall_processed_samples();
            let overall_bytes = self.active_overall_processed_bytes();
            let current = self
                .active_files
                .get(&file_id)
                .expect("current file must still exist while rendering progress");
            (
                self.display_for_snapshot(
                    &current.filename,
                    total_samples,
                    progress,
                    overall_processed,
                ),
                overall_bytes,
                ByteProgress {
                    input_bytes_read: progress.input_bytes_read,
                    output_bytes_written: progress.output_bytes_written,
                },
            )
        };
        let overall_estimate = self.overall_state.observe(
            display
                .overall
                .processed_samples
                .min(display.overall.total_samples),
            display.overall.total_samples,
            overall_bytes.input_bytes_read,
            overall_bytes.output_bytes_written,
            batch_elapsed,
        );
        let file_estimate = display.file.map(|file| {
            self.active_files
                .get_mut(&file_id)
                .expect("current file must still exist while updating file estimate")
                .file_state
                .observe(
                    file.processed_samples.min(file.total_samples),
                    file.total_samples,
                    file_bytes.input_bytes_read,
                    file_bytes.output_bytes_written,
                    file_elapsed,
                )
        });
        let frame = format_progress_frame_with_budget(
            &display,
            &overall_estimate,
            file_estimate.as_ref(),
            batch_elapsed,
            file_elapsed,
            self.renderer.line_budget(),
        );
        self.renderer.render(frame)
    }

    #[cfg(test)]
    fn observe_recompress_with_elapsed(
        &mut self,
        progress: RecompressProgress,
        batch_elapsed: Duration,
    ) -> std::io::Result<()> {
        self.observe_recompress_with_elapsed_for(0, progress, batch_elapsed)
    }

    fn observe_recompress_with_elapsed_for(
        &mut self,
        file_id: usize,
        progress: RecompressProgress,
        batch_elapsed: Duration,
    ) -> std::io::Result<()> {
        let (filename, phase_label, first_progress_samples) = {
            let current = self
                .active_files
                .get_mut(&file_id)
                .expect("current file must be set");
            let first_progress_samples =
                (!current.saw_update && progress.overall_processed_samples > 0).then_some(
                    progress
                        .overall_processed_samples
                        .min(current.overall_total_samples),
                );
            current.saw_update = true;
            if current.phase != Some(progress.phase) {
                current.phase = Some(progress.phase);
                current.phase_started_at = Some(Instant::now());
                current.file_state = ProgressState::default();
            }
            current.current_progress = None;
            current.current_recompress_progress = Some(progress);
            let phase_label = current.phase.expect("phase must be set").as_str();
            (
                current.filename.clone(),
                phase_label,
                first_progress_samples,
            )
        };
        self.display_file_id = Some(file_id);
        if let Some(processed_samples) = first_progress_samples
            && let Some(trace) = self.trace.as_mut()
        {
            trace.on_first_progress(&filename, Some(phase_label), processed_samples)?;
        }
        let phase_elapsed = self.current_phase_elapsed_for(file_id);
        let file_elapsed = self.current_file_elapsed_for(file_id);
        self.render_recompress_snapshot_for(
            file_id,
            &filename,
            progress,
            batch_elapsed,
            phase_elapsed,
            file_elapsed,
        )
    }

    fn render_recompress_snapshot_for(
        &mut self,
        file_id: usize,
        filename: &str,
        progress: RecompressProgress,
        batch_elapsed: Duration,
        phase_elapsed: Duration,
        file_elapsed: Duration,
    ) -> std::io::Result<()> {
        let overall_processed = self.active_overall_processed_samples();
        let overall_bytes = if self.batch_mode {
            self.active_overall_processed_bytes()
        } else {
            ByteProgress {
                input_bytes_read: progress.overall_input_bytes_read,
                output_bytes_written: progress.overall_output_bytes_written,
            }
        };
        let file_bytes = ByteProgress {
            input_bytes_read: progress.phase_input_bytes_read,
            output_bytes_written: progress.phase_output_bytes_written,
        };
        let display = self.display_for_recompress_progress(filename, progress, overall_processed);
        let overall_estimate = self.overall_state.observe(
            display
                .overall
                .processed_samples
                .min(display.overall.total_samples),
            display.overall.total_samples,
            overall_bytes.input_bytes_read,
            overall_bytes.output_bytes_written,
            batch_elapsed,
        );
        let file_estimate = display.file.map(|file| {
            self.active_files
                .get_mut(&file_id)
                .expect("current file must still exist while updating phase estimate")
                .file_state
                .observe(
                    file.processed_samples.min(file.total_samples),
                    file.total_samples,
                    file_bytes.input_bytes_read,
                    file_bytes.output_bytes_written,
                    phase_elapsed,
                )
        });
        let frame = format_progress_frame_with_budget(
            &display,
            &overall_estimate,
            file_estimate.as_ref(),
            batch_elapsed,
            file_elapsed,
            self.renderer.line_budget(),
        );
        self.renderer.render(frame)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn finish_current_file(&mut self) -> std::io::Result<()> {
        self.finish_file(0)
    }

    fn finish_file(&mut self, file_id: usize) -> std::io::Result<()> {
        let Some((needs_final_update, total_samples)) = self
            .active_files
            .get(&file_id)
            .map(|current| (!current.saw_update, current.phase_total_samples))
        else {
            return Ok(());
        };
        if needs_final_update {
            let batch_elapsed = self.batch_elapsed();
            let file_elapsed = self.current_file_elapsed_for(file_id);
            let completed = ProgressSnapshot {
                processed_samples: total_samples,
                total_samples,
                completed_frames: 0,
                total_frames: 0,
                input_bytes_read: self
                    .active_files
                    .get(&file_id)
                    .map_or(0, |current| current.input_bytes),
                output_bytes_written: 0,
            };
            self.observe_with_elapsed_for(file_id, completed, batch_elapsed, file_elapsed)?;
        }
        let current = self
            .active_files
            .remove(&file_id)
            .expect("current file must still exist after final progress update");
        if let Some(trace) = self.trace.as_mut() {
            trace.on_file_finish(
                &current.filename,
                current.input_bytes,
                current
                    .started_at
                    .map_or(Duration::ZERO, |started_at| started_at.elapsed()),
            )?;
        }
        self.completed_samples = self
            .completed_samples
            .saturating_add(current.overall_total_samples);
        let completed_bytes = current.overall_processed_bytes();
        self.completed_input_bytes = self
            .completed_input_bytes
            .saturating_add(completed_bytes.input_bytes_read);
        self.completed_output_bytes = self
            .completed_output_bytes
            .saturating_add(completed_bytes.output_bytes_written);
        if self.display_file_id == Some(file_id) {
            self.display_file_id = self.active_files.keys().next().copied();
        }
        Ok(())
    }

    fn finish(&mut self) -> std::io::Result<()> {
        self.renderer.end()
    }

    #[cfg(test)]
    fn end(&mut self) -> std::io::Result<()> {
        self.renderer.end()
    }

    fn heartbeat(&mut self) -> std::io::Result<()> {
        let Some(file_id) = self.display_file_id else {
            return Ok(());
        };
        let Some((filename, phase_total_samples, current_progress, current_recompress_progress)) =
            self.active_files.get(&file_id).map(|current| {
                (
                    current.filename.clone(),
                    current.phase_total_samples,
                    current.current_progress,
                    current.current_recompress_progress,
                )
            })
        else {
            return Ok(());
        };

        let batch_elapsed = self.batch_elapsed();
        let file_elapsed = self.current_file_elapsed_for(file_id);
        if let Some(progress) = current_progress {
            return self.render_snapshot_for(
                file_id,
                phase_total_samples,
                progress,
                batch_elapsed,
                file_elapsed,
            );
        }

        if let Some(progress) = current_recompress_progress {
            let phase_elapsed = self.current_phase_elapsed_for(file_id);
            return self.render_recompress_snapshot_for(
                file_id,
                &filename,
                progress,
                batch_elapsed,
                phase_elapsed,
                file_elapsed,
            );
        }

        Ok(())
    }

    fn next_heartbeat_delay(&mut self) -> Option<Duration> {
        if !self.renderer.interactive {
            return None;
        }
        let file_id = self.display_file_id?;
        let has_cached_progress = self.active_files.get(&file_id).is_some_and(|current| {
            current.current_progress.is_some() || current.current_recompress_progress.is_some()
        });
        if !has_cached_progress {
            return None;
        }
        let mut next_delay = time_until_next_second(self.batch_elapsed());
        if self.batch_mode {
            next_delay = next_delay.min(time_until_next_second(
                self.current_file_elapsed_for(file_id),
            ));
        }
        Some(next_delay)
    }

    fn batch_elapsed(&mut self) -> Duration {
        let started_at = self.batch_started_at.get_or_insert_with(Instant::now);
        started_at.elapsed()
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn current_file_elapsed(&mut self) -> Duration {
        self.current_file_elapsed_for(0)
    }

    fn current_file_elapsed_for(&mut self, file_id: usize) -> Duration {
        let current = self
            .active_files
            .get_mut(&file_id)
            .expect("current file must be set before observing progress");
        let started_at = current.started_at.get_or_insert_with(Instant::now);
        started_at.elapsed()
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn current_phase_elapsed(&mut self) -> Duration {
        self.current_phase_elapsed_for(0)
    }

    fn current_phase_elapsed_for(&mut self, file_id: usize) -> Duration {
        let current = self
            .active_files
            .get_mut(&file_id)
            .expect("current file must be set before observing phase progress");
        let started_at = current.phase_started_at.get_or_insert_with(Instant::now);
        started_at.elapsed()
    }

    #[cfg(test)]
    fn render_initial_file_frame(&mut self, file_total_samples: u64) -> std::io::Result<()> {
        self.render_initial_file_frame_for(0, file_total_samples)
    }

    fn render_initial_file_frame_for(
        &mut self,
        file_id: usize,
        file_total_samples: u64,
    ) -> std::io::Result<()> {
        let batch_elapsed = self.batch_elapsed();
        let file_elapsed = self.current_file_elapsed_for(file_id);
        let progress = ProgressSnapshot {
            processed_samples: 0,
            total_samples: file_total_samples,
            completed_frames: 0,
            total_frames: 0,
            input_bytes_read: 0,
            output_bytes_written: 0,
        };
        {
            let current = self
                .active_files
                .get_mut(&file_id)
                .expect("current file must exist before rendering initial progress");
            current.current_progress = Some(progress);
            current.current_recompress_progress = None;
        }
        self.render_snapshot_for(
            file_id,
            file_total_samples,
            progress,
            batch_elapsed,
            file_elapsed,
        )
    }

    #[cfg(test)]
    #[allow(dead_code)]
    fn render_initial_recompress_frame(
        &mut self,
        phase_total_samples: u64,
        overall_total_samples: u64,
    ) -> std::io::Result<()> {
        self.render_initial_recompress_frame_for(0, phase_total_samples, overall_total_samples)
    }

    fn render_initial_recompress_frame_for(
        &mut self,
        file_id: usize,
        phase_total_samples: u64,
        overall_total_samples: u64,
    ) -> std::io::Result<()> {
        if let Some(current) = self.active_files.get_mut(&file_id) {
            current.phase = Some(RecompressPhase::Decode);
            current.phase_started_at.get_or_insert_with(Instant::now);
        }
        let batch_elapsed = self.batch_elapsed();
        let phase_elapsed = self.current_phase_elapsed_for(file_id);
        let file_elapsed = self.current_file_elapsed_for(file_id);
        let progress = RecompressProgress {
            phase: RecompressPhase::Decode,
            phase_processed_samples: 0,
            phase_total_samples,
            overall_processed_samples: 0,
            overall_total_samples,
            completed_frames: 0,
            total_frames: 0,
            phase_input_bytes_read: 0,
            phase_output_bytes_written: 0,
            overall_input_bytes_read: 0,
            overall_output_bytes_written: 0,
        };
        {
            let current = self
                .active_files
                .get_mut(&file_id)
                .expect("current file must exist before rendering initial phase progress");
            current.current_progress = None;
            current.current_recompress_progress = Some(progress);
        }
        let filename = self
            .active_files
            .get(&file_id)
            .expect("current file must exist before rendering initial phase progress")
            .filename
            .clone();
        self.render_recompress_snapshot_for(
            file_id,
            &filename,
            progress,
            batch_elapsed,
            phase_elapsed,
            file_elapsed,
        )
    }

    fn active_overall_processed_samples(&self) -> u64 {
        self.active_files
            .values()
            .fold(self.completed_samples, |processed, current| {
                processed.saturating_add(current.overall_processed_samples())
            })
    }

    fn active_overall_processed_bytes(&self) -> ByteProgress {
        self.active_files.values().fold(
            ByteProgress {
                input_bytes_read: self.completed_input_bytes,
                output_bytes_written: self.completed_output_bytes,
            },
            |processed, current| {
                let current_bytes = current.overall_processed_bytes();
                ByteProgress {
                    input_bytes_read: processed
                        .input_bytes_read
                        .saturating_add(current_bytes.input_bytes_read),
                    output_bytes_written: processed
                        .output_bytes_written
                        .saturating_add(current_bytes.output_bytes_written),
                }
            },
        )
    }

    fn display_for_snapshot(
        &self,
        filename: &str,
        file_total_samples: u64,
        progress: ProgressSnapshot,
        overall_processed: u64,
    ) -> ProgressDisplay {
        let file_processed = progress.processed_samples.min(file_total_samples);

        ProgressDisplay {
            filename: filename.to_string(),
            overall: SampleProgress {
                processed_samples: if self.batch_mode {
                    overall_processed.min(self.total_samples)
                } else {
                    file_processed
                },
                total_samples: if self.batch_mode {
                    self.total_samples
                } else {
                    file_total_samples
                },
            },
            file: self.batch_mode.then_some(SampleProgress {
                processed_samples: file_processed,
                total_samples: file_total_samples,
            }),
            phase_label: None,
        }
    }

    fn display_for_recompress_progress(
        &self,
        filename: &str,
        progress: RecompressProgress,
        overall_processed: u64,
    ) -> ProgressDisplay {
        ProgressDisplay {
            filename: filename.to_string(),
            overall: SampleProgress {
                processed_samples: if self.batch_mode {
                    overall_processed.min(self.total_samples)
                } else {
                    progress.overall_processed_samples
                },
                total_samples: if self.batch_mode {
                    self.total_samples
                } else {
                    progress.overall_total_samples
                },
            },
            file: self.batch_mode.then_some(SampleProgress {
                processed_samples: progress.phase_processed_samples,
                total_samples: progress.phase_total_samples,
            }),
            phase_label: Some(progress.phase.as_str()),
        }
    }
}

struct ProgressRenderer<W: Write> {
    writer: W,
    interactive: bool,
    has_drawn: bool,
    last_line_widths: Vec<usize>,
    last_frame_rows: usize,
    line_budget: Option<usize>,
}

impl<W: Write> ProgressRenderer<W> {
    fn new(writer: W, interactive: bool) -> Self {
        let line_budget = interactive.then(detect_progress_line_budget).flatten();
        Self::with_line_budget(writer, interactive, line_budget)
    }

    fn with_line_budget(writer: W, interactive: bool, line_budget: Option<usize>) -> Self {
        Self {
            writer,
            interactive,
            has_drawn: false,
            last_line_widths: Vec::new(),
            last_frame_rows: 0,
            line_budget,
        }
    }

    fn line_budget(&self) -> Option<usize> {
        self.line_budget
    }

    #[cfg(test)]
    fn observe_frame(&mut self, frame: ProgressFrame) -> std::io::Result<()> {
        self.render(frame)
    }

    fn render(&mut self, frame: ProgressFrame) -> std::io::Result<()> {
        if !self.interactive {
            return Ok(());
        }
        self.draw_frame(&frame)?;
        Ok(())
    }

    fn draw_frame(&mut self, frame: &ProgressFrame) -> std::io::Result<()> {
        if self.has_drawn && self.last_frame_rows > 1 {
            write!(self.writer, "\x1b[{}A", self.last_frame_rows - 1)?;
        }

        let previous_height = self.last_line_widths.len();
        let total_lines = frame.lines.len().max(previous_height);
        let mut current_line_widths = Vec::with_capacity(frame.lines.len());
        let mut current_frame_rows = 0usize;
        for line_index in 0..total_lines {
            if line_index > 0 {
                self.writer.write_all(b"\n")?;
            }
            self.writer.write_all(b"\r")?;
            let previous_width = self.last_line_widths.get(line_index).copied().unwrap_or(0);
            let line = frame
                .lines
                .get(line_index)
                .map(String::as_str)
                .unwrap_or("");
            let line_width = display_width(line);
            let padded_width = line_width.max(previous_width);
            self.writer.write_all(line.as_bytes())?;
            if padded_width > line_width {
                write!(
                    self.writer,
                    "{:width$}",
                    "",
                    width = padded_width - line_width
                )?;
            }
            if line_index < frame.lines.len() {
                current_line_widths.push(line_width);
            }
            current_frame_rows =
                current_frame_rows.saturating_add(rendered_row_count(line_width, self.line_budget));
        }

        self.has_drawn = true;
        self.last_line_widths = current_line_widths;
        self.last_frame_rows = current_frame_rows;
        self.writer.flush()
    }

    fn end(&mut self) -> std::io::Result<()> {
        if !self.has_drawn {
            return Ok(());
        }

        self.writer.write_all(b"\n")?;
        self.writer.flush()
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ProgressEstimate {
    eta: Option<Duration>,
    input_bytes_per_second: Option<f64>,
    output_bytes_per_second: Option<f64>,
    total_bytes_per_second: Option<f64>,
}

impl ProgressEstimate {
    fn warming_up() -> Self {
        Self {
            eta: None,
            input_bytes_per_second: None,
            output_bytes_per_second: None,
            total_bytes_per_second: None,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ProgressState {
    first_advancing_elapsed: Option<Duration>,
    last_processed_samples: Option<u64>,
    advancing_updates: u8,
}

impl ProgressState {
    fn observe(
        &mut self,
        processed_samples: u64,
        total_samples: u64,
        input_bytes_read: u64,
        output_bytes_written: u64,
        elapsed: Duration,
    ) -> ProgressEstimate {
        let processed = processed_samples.min(total_samples);
        let advanced = match self.last_processed_samples {
            Some(previous) => processed > previous,
            None => processed > 0,
        };

        if advanced {
            self.advancing_updates = self.advancing_updates.saturating_add(1);
            self.first_advancing_elapsed.get_or_insert(elapsed);
        }

        self.last_processed_samples = Some(processed);

        let Some(first_advancing_elapsed) = self.first_advancing_elapsed else {
            return ProgressEstimate::warming_up();
        };

        let elapsed_since_first_advance = elapsed.saturating_sub(first_advancing_elapsed);
        if self.advancing_updates < 2 || elapsed_since_first_advance < ESTIMATE_WARMUP {
            return ProgressEstimate::warming_up();
        }

        let elapsed_seconds = elapsed.as_secs_f64();
        if elapsed_seconds <= 0.0 || processed == 0 {
            return ProgressEstimate::warming_up();
        }

        let samples_per_second = processed as f64 / elapsed_seconds;
        if samples_per_second <= 0.0 {
            return ProgressEstimate::warming_up();
        }

        let remaining_samples = total_samples.saturating_sub(processed);
        let eta_seconds = remaining_samples as f64 / samples_per_second;
        let total_bytes = input_bytes_read.saturating_add(output_bytes_written);

        ProgressEstimate {
            eta: Some(Duration::from_secs_f64(eta_seconds.max(0.0))),
            input_bytes_per_second: Some(input_bytes_read as f64 / elapsed_seconds),
            output_bytes_per_second: Some(output_bytes_written as f64 / elapsed_seconds),
            total_bytes_per_second: Some(total_bytes as f64 / elapsed_seconds),
        }
    }
}

#[cfg(test)]
fn format_progress_frame(
    display: &ProgressDisplay,
    overall_estimate: &ProgressEstimate,
    file_estimate: Option<&ProgressEstimate>,
    batch_elapsed: Duration,
    file_elapsed: Duration,
) -> ProgressFrame {
    format_progress_frame_with_budget(
        display,
        overall_estimate,
        file_estimate,
        batch_elapsed,
        file_elapsed,
        None,
    )
}

fn format_progress_frame_with_budget(
    display: &ProgressDisplay,
    overall_estimate: &ProgressEstimate,
    file_estimate: Option<&ProgressEstimate>,
    batch_elapsed: Duration,
    file_elapsed: Duration,
    line_budget: Option<usize>,
) -> ProgressFrame {
    if let Some(file) = display.file {
        let warming_up = ProgressEstimate::warming_up();
        let file_estimate = file_estimate.unwrap_or(&warming_up);
        return ProgressFrame {
            lines: vec![
                format_batch_overall_line(
                    display.overall,
                    overall_estimate,
                    batch_elapsed,
                    line_budget,
                ),
                format_batch_file_line(
                    &display.filename,
                    display.phase_label.unwrap_or("File"),
                    file,
                    file_estimate,
                    file_elapsed,
                    line_budget,
                ),
            ],
        };
    }

    ProgressFrame {
        lines: vec![format_single_file_line(
            &display.filename,
            display.phase_label,
            display.overall,
            overall_estimate,
            batch_elapsed,
            line_budget,
        )],
    }
}

fn format_progress_line(
    label: &str,
    progress: SampleProgress,
    estimate: &ProgressEstimate,
    elapsed: Duration,
    bar_width: usize,
    line_budget: Option<usize>,
) -> String {
    format_label_with_progress_suffix(
        label,
        &format_progress_suffix(progress, estimate, elapsed, bar_width),
        line_budget,
    )
}

fn format_single_file_line(
    filename: &str,
    phase_label: Option<&str>,
    progress: SampleProgress,
    estimate: &ProgressEstimate,
    elapsed: Duration,
    line_budget: Option<usize>,
) -> String {
    let progress_suffix =
        format_progress_suffix(progress, estimate, elapsed, SINGLE_PROGRESS_BAR_WIDTH);
    let suffix = phase_label
        .map(|phase| format!(" | {phase}{progress_suffix}"))
        .unwrap_or(progress_suffix);
    format_filename_with_progress_suffix(filename, &suffix, line_budget)
}

fn format_batch_overall_line(
    progress: SampleProgress,
    estimate: &ProgressEstimate,
    elapsed: Duration,
    line_budget: Option<usize>,
) -> String {
    format_progress_line(
        "Batch",
        progress,
        estimate,
        elapsed,
        BATCH_PROGRESS_BAR_WIDTH,
        line_budget,
    )
}

fn format_batch_file_line(
    filename: &str,
    phase_label: &str,
    progress: SampleProgress,
    estimate: &ProgressEstimate,
    elapsed: Duration,
    line_budget: Option<usize>,
) -> String {
    let suffix = format!(
        " | {phase_label}{}",
        format_progress_suffix(progress, estimate, elapsed, BATCH_PROGRESS_BAR_WIDTH)
    );
    format_filename_with_progress_suffix(filename, &suffix, line_budget)
}

fn format_progress_suffix(
    progress: SampleProgress,
    estimate: &ProgressEstimate,
    elapsed: Duration,
    bar_width: usize,
) -> String {
    let elapsed = format_clock(elapsed);
    let eta = estimate
        .eta
        .map(format_clock)
        .unwrap_or_else(|| "--:--".to_string());
    let input_rate = format_byte_speed(estimate.input_bytes_per_second);
    let output_rate = format_byte_speed(estimate.output_bytes_per_second);
    let total_rate = format_byte_speed(estimate.total_bytes_per_second);

    format!(
        " | {} {:>5.1}% | Elapsed {} | ETA {} | In {} | Out {} | Total {}",
        format_progress_bar(progress_ratio(progress), bar_width),
        progress_ratio(progress) * 100.0,
        elapsed,
        eta,
        input_rate,
        output_rate,
        total_rate,
    )
}

fn format_label_with_progress_suffix(
    label: &str,
    suffix: &str,
    line_budget: Option<usize>,
) -> String {
    match line_budget {
        Some(max_width) => {
            let suffix_width = display_width(suffix);
            if suffix_width >= max_width {
                return truncate_display_text(&format!("{label}{suffix}"), max_width);
            }
            let label_width = max_width.saturating_sub(suffix_width);
            format!("{}{}", truncate_display_text(label, label_width), suffix)
        }
        None => format!("{label}{suffix}"),
    }
}

fn format_filename_with_progress_suffix(
    filename: &str,
    suffix: &str,
    line_budget: Option<usize>,
) -> String {
    match line_budget {
        Some(max_width) => {
            let suffix_width = display_width(suffix);
            if suffix_width >= max_width {
                return truncate_display_text(&format!("{filename}{suffix}"), max_width);
            }
            let filename_width = max_width.saturating_sub(suffix_width);
            format!(
                "{}{}",
                truncate_display_text(filename, filename_width),
                suffix
            )
        }
        None => format!("{filename}{suffix}"),
    }
}

fn detect_progress_line_budget() -> Option<usize> {
    terminal_size()
        .map(|(Width(width), _)| usize::from(width))
        .and_then(progress_line_budget_from_columns)
        .or_else(|| {
            env::var("COLUMNS")
                .ok()
                .and_then(|columns| columns.parse::<usize>().ok())
                .and_then(progress_line_budget_from_columns)
        })
        .or(Some(DEFAULT_PROGRESS_LINE_BUDGET))
}

fn progress_line_budget_from_columns(columns: usize) -> Option<usize> {
    (columns > 0).then_some(columns.saturating_sub(1).max(1))
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn truncate_display_text(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let target_width = max_width - 1;
    let mut truncated = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width.saturating_add(char_width) > target_width {
            break;
        }
        truncated.push(ch);
        width = width.saturating_add(char_width);
    }
    truncated.push('…');
    truncated
}

fn rendered_row_count(display_width: usize, line_budget: Option<usize>) -> usize {
    match line_budget {
        Some(line_budget) if line_budget > 0 => display_width.max(1).div_ceil(line_budget),
        _ => 1,
    }
}

fn time_until_next_second(elapsed: Duration) -> Duration {
    Duration::from_secs(
        elapsed
            .as_secs()
            .saturating_add(HEARTBEAT_INTERVAL.as_secs()),
    )
    .saturating_sub(elapsed)
}

fn progress_ratio(progress: SampleProgress) -> f64 {
    if progress.total_samples == 0 {
        1.0
    } else {
        progress.processed_samples.min(progress.total_samples) as f64
            / progress.total_samples as f64
    }
}

fn format_progress_bar(ratio: f64, width: usize) -> String {
    let clamped_ratio = ratio.clamp(0.0, 1.0);
    let filled = (clamped_ratio * width as f64).floor() as usize;

    if filled >= width {
        return format!("[{}]", "=".repeat(width));
    }

    let prefix = "=".repeat(filled);
    let suffix = "-".repeat(width.saturating_sub(filled + 1));
    format!("[{}>{}]", prefix, suffix)
}

fn format_clock(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

fn format_byte_speed(bytes_per_second: Option<f64>) -> String {
    let Some(bytes_per_second) = bytes_per_second else {
        return "warmup".to_string();
    };
    let kib = 1024.0;
    let mib = kib * 1024.0;
    let gib = mib * 1024.0;

    if bytes_per_second >= gib {
        format!("{:.1} GiB/s", bytes_per_second / gib)
    } else if bytes_per_second >= mib {
        format!("{:.1} MiB/s", bytes_per_second / mib)
    } else if bytes_per_second >= kib {
        format!("{:.1} KiB/s", bytes_per_second / kib)
    } else {
        format!("{bytes_per_second:.0} B/s")
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    use super::{
        BatchProgressCoordinator, ProgressDisplay, ProgressEstimate, ProgressFrame,
        ProgressRenderer, ProgressReportingMode, ProgressState, SampleProgress, display_width,
        format_progress_frame, format_progress_frame_with_budget, progress_reporting_mode,
        relative_display_name, rendered_row_count, time_until_next_second, truncate_display_text,
    };
    use flacx::{ProgressSnapshot, RecompressPhase, RecompressProgress};

    fn final_frame_lines(output: &str) -> Vec<&str> {
        let bytes = output.as_bytes();
        let mut last_escape_end = None;
        let mut index = 0usize;
        while index + 2 < bytes.len() {
            if bytes[index] == 0x1b && bytes[index + 1] == b'[' {
                let mut cursor = index + 2;
                while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                    cursor += 1;
                }
                if cursor > index + 2 && cursor < bytes.len() && bytes[cursor] == b'A' {
                    last_escape_end = Some(cursor + 1);
                }
            }
            index += 1;
        }
        let final_frame = last_escape_end
            .map(|offset| &output[offset..])
            .unwrap_or(output);

        final_frame
            .trim_end_matches('\n')
            .split('\n')
            .map(|line| line.split('\r').next_back().unwrap_or(line))
            .collect()
    }

    fn legacy_input_path_sort_key(path: &Path) -> String {
        relative_display_name(path)
    }

    #[test]
    fn progress_renderer_is_silent_when_not_interactive() {
        let mut renderer = ProgressRenderer::new(Vec::new(), false);
        renderer
            .observe_frame(ProgressFrame {
                lines: vec!["input.wav | [============] 100.0%".into()],
            })
            .unwrap();
        renderer.end().unwrap();

        assert!(renderer.writer.is_empty());
    }

    #[test]
    fn progress_renderer_writes_elapsed_filename_eta_and_rate() {
        let mut renderer = ProgressRenderer::new(Vec::new(), true);
        renderer
            .observe_frame(format_progress_frame(
                &ProgressDisplay {
                    filename: "mixdown.wav".into(),
                    overall: SampleProgress {
                        processed_samples: 100,
                        total_samples: 100,
                    },
                    file: None,
                    phase_label: None,
                },
                &ProgressEstimate {
                    eta: Some(Duration::from_secs(0)),
                    input_bytes_per_second: Some(333.0),
                    output_bytes_per_second: Some(666.0),
                    total_bytes_per_second: Some(999.0),
                },
                None,
                Duration::from_millis(300),
                Duration::from_millis(300),
            ))
            .unwrap();
        renderer.end().unwrap();

        let output = String::from_utf8(renderer.writer).unwrap();
        assert!(output.contains('\r'));
        assert!(output.contains("mixdown.wav"));
        assert!(output.contains("100.0%"));
        assert!(output.contains("Elapsed 00:00"));
        assert!(output.contains("ETA 00:00"));
        assert!(output.contains("In 333 B/s"));
        assert!(output.contains("Out 666 B/s"));
        assert!(output.contains("Total 999 B/s"));
        assert!(output.ends_with('\n'));
        assert!(!output.ends_with("\n\n"));
    }

    #[test]
    fn progress_renderer_writes_input_output_and_total_byte_rates() {
        let mut renderer = ProgressRenderer::new(Vec::new(), true);
        renderer
            .observe_frame(format_progress_frame(
                &ProgressDisplay {
                    filename: "mixdown.wav".into(),
                    overall: SampleProgress {
                        processed_samples: 100,
                        total_samples: 100,
                    },
                    file: None,
                    phase_label: None,
                },
                &ProgressEstimate {
                    eta: Some(Duration::from_secs(0)),
                    input_bytes_per_second: Some(3.0 * 1024.0 * 1024.0),
                    output_bytes_per_second: Some(5.0 * 1024.0 * 1024.0),
                    total_bytes_per_second: Some(8.0 * 1024.0 * 1024.0),
                },
                None,
                Duration::from_millis(300),
                Duration::from_millis(300),
            ))
            .unwrap();
        renderer.end().unwrap();

        let output = String::from_utf8(renderer.writer).unwrap();
        assert!(output.contains("In 3.0 MiB/s"));
        assert!(output.contains("Out 5.0 MiB/s"));
        assert!(output.contains("Total 8.0 MiB/s"));
    }

    #[test]
    fn progress_renderer_waits_for_two_advancing_updates_and_elapsed_time() {
        let mut state = ProgressState::default();

        let first = state.observe(50, 200, 100, 200, Duration::from_millis(0));
        assert_eq!(first, ProgressEstimate::warming_up());

        let no_advance = state.observe(50, 200, 100, 200, Duration::from_millis(400));
        assert_eq!(no_advance, ProgressEstimate::warming_up());

        let second_advance = state.observe(100, 200, 200, 400, Duration::from_millis(200));
        assert_eq!(second_advance, ProgressEstimate::warming_up());

        let stabilized = state.observe(150, 200, 300, 600, Duration::from_millis(300));
        assert!(stabilized.eta.is_some());
        assert!(stabilized.input_bytes_per_second.is_some());
        assert!(stabilized.output_bytes_per_second.is_some());
        assert!(stabilized.total_bytes_per_second.is_some());
    }

    #[test]
    fn progress_renderer_ignores_zero_progress_before_warmup_starts() {
        let mut state = ProgressState::default();

        let initial = state.observe(0, 200, 0, 0, Duration::from_millis(500));
        assert_eq!(initial, ProgressEstimate::warming_up());

        let first_advance = state.observe(50, 200, 100, 200, Duration::from_millis(600));
        assert_eq!(first_advance, ProgressEstimate::warming_up());

        let second_advance = state.observe(100, 200, 200, 400, Duration::from_millis(700));
        assert_eq!(second_advance, ProgressEstimate::warming_up());

        let stabilized = state.observe(150, 200, 300, 600, Duration::from_millis(900));
        assert!(stabilized.eta.is_some());
    }

    #[test]
    fn progress_renderer_overwrites_stale_characters_when_line_shrinks() {
        let mut renderer = ProgressRenderer::new(Vec::new(), true);
        renderer
            .observe_frame(ProgressFrame {
                lines: vec!["wide-name.wav | [==>---------------------]  10.0% | Elapsed 00:00 | ETA --:-- | In warmup | Out warmup | Total warmup".into()],
            })
            .unwrap();
        renderer
            .observe_frame(ProgressFrame {
                lines: vec!["x.wav | [========================] 100.0% | Elapsed 00:00 | ETA 00:00 | In 333 B/s | Out 666 B/s | Total 999 B/s".into()],
            })
            .unwrap();
        renderer.end().unwrap();

        let output = String::from_utf8(renderer.writer).unwrap();
        let frames: Vec<&str> = output.trim_end_matches('\n').split('\r').collect();
        let warmup = frames
            .iter()
            .find(|frame| frame.contains("Total warmup"))
            .unwrap();
        let final_frame = frames
            .iter()
            .rev()
            .find(|frame| frame.contains("ETA 00:00"))
            .unwrap();
        assert!(warmup.len() >= final_frame.trim_end().len());
        assert!(final_frame.ends_with(' '));
    }

    #[test]
    fn unicode_display_width_treats_full_width_characters_as_wide() {
        assert_eq!(display_width("テ"), 2);
        assert_eq!(display_width("【】"), 4);
        assert!(
            display_width("『世界の果て』　【テスト】.wav")
                > "『世界の果て』　【テスト】.wav".chars().count()
        );
    }

    #[test]
    fn truncate_display_text_preserves_unicode_boundaries_with_ellipsis() {
        let truncated = truncate_display_text("『世界の果て』　【テスト】.wav", 12);
        assert!(display_width(&truncated) <= 12);
        assert!(truncated.ends_with('…'));
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }

    #[test]
    fn progress_frame_truncates_unicode_filename_to_budget() {
        let frame = format_progress_frame_with_budget(
            &ProgressDisplay {
                filename: "『世界の果て』　【テスト】.wav".into(),
                overall: SampleProgress {
                    processed_samples: 100,
                    total_samples: 100,
                },
                file: None,
                phase_label: None,
                },
                &ProgressEstimate {
                    eta: Some(Duration::from_secs(0)),
                    input_bytes_per_second: Some(333.0),
                    output_bytes_per_second: Some(666.0),
                    total_bytes_per_second: Some(999.0),
                },
                None,
                Duration::from_secs(1),
                Duration::from_secs(1),
                Some(120),
            );

        assert_eq!(frame.lines.len(), 1);
        assert!(display_width(&frame.lines[0]) <= 120);
        assert!(frame.lines[0].contains("Elapsed 00:01"));
        assert!(frame.lines[0].contains("100.0%"));
        assert!(frame.lines[0].contains('…') || frame.lines[0].contains("テスト"));
    }

    #[test]
    fn progress_renderer_rewinds_wrapped_rows_when_budget_is_small() {
        let mut renderer = ProgressRenderer::with_line_budget(Vec::new(), true, Some(16));
        let long_line = "『世界の果て』　【テスト】.wav | File | [==========] 100.0% | Elapsed 00:01 | ETA 00:00 | In 333 B/s | Out 666 B/s | Total 999 B/s";
        let expected_rows_up = rendered_row_count(display_width(long_line), Some(16)) - 1;

        renderer
            .observe_frame(ProgressFrame {
                lines: vec![long_line.into()],
            })
            .unwrap();
        renderer
            .observe_frame(ProgressFrame {
                lines: vec![
                    "x.wav | File | [==========] 100.0% | Elapsed 00:01 | ETA 00:00 | In 333 B/s | Out 666 B/s | Total 999 B/s"
                        .into(),
                ],
            })
            .unwrap();

        let output = String::from_utf8(renderer.writer).unwrap();
        assert!(output.contains(&format!("\x1b[{expected_rows_up}A")));
    }

    #[test]
    fn progress_frame_contains_two_batch_lines_with_independent_progress_bundles() {
        let frame = format_progress_frame(
            &ProgressDisplay {
                filename: "album/disc1/song.wav".into(),
                overall: SampleProgress {
                    processed_samples: 150,
                    total_samples: 300,
                },
                file: Some(SampleProgress {
                    processed_samples: 50,
                    total_samples: 100,
                }),
                phase_label: None,
            },
            &ProgressEstimate {
                eta: Some(Duration::from_secs(3)),
                input_bytes_per_second: Some(12_345.0),
                output_bytes_per_second: Some(24_690.0),
                total_bytes_per_second: Some(37_035.0),
            },
            Some(&ProgressEstimate {
                eta: Some(Duration::from_secs(1)),
                input_bytes_per_second: Some(4_096.0),
                output_bytes_per_second: Some(8_192.0),
                total_bytes_per_second: Some(12_288.0),
            }),
            Duration::from_secs(4),
            Duration::from_secs(2),
        );
        assert_eq!(frame.lines.len(), 2);
        assert!(frame.lines[0].starts_with("Batch | "));
        assert!(frame.lines[0].contains("50.0%"));
        assert!(frame.lines[0].contains("Elapsed 00:04"));
        assert!(frame.lines[0].contains("ETA 00:03"));
        assert!(frame.lines[0].contains("In 12.1 KiB/s"));
        assert!(frame.lines[0].contains("Out 24.1 KiB/s"));
        assert!(frame.lines[0].contains("Total 36.2 KiB/s"));
        assert!(frame.lines[1].starts_with("album/disc1/song.wav | File | "));
        assert!(frame.lines[1].contains("50.0%"));
        assert!(frame.lines[1].contains("Elapsed 00:02"));
        assert!(frame.lines[1].contains("ETA 00:01"));
        assert!(frame.lines[1].contains("In 4.0 KiB/s"));
        assert!(frame.lines[1].contains("Out 8.0 KiB/s"));
        assert!(frame.lines[1].contains("Total 12.0 KiB/s"));
    }

    #[test]
    fn batch_progress_frame_stays_two_lines_while_file_estimate_is_warming_up() {
        let frame = format_progress_frame(
            &ProgressDisplay {
                filename: "album/disc1/song.wav".into(),
                overall: SampleProgress {
                    processed_samples: 10,
                    total_samples: 100,
                },
                file: Some(SampleProgress {
                    processed_samples: 5,
                    total_samples: 50,
                }),
                phase_label: None,
            },
            &ProgressEstimate::warming_up(),
            None,
            Duration::from_secs(1),
            Duration::from_millis(100),
        );
        assert_eq!(frame.lines.len(), 2);
        assert!(frame.lines[0].starts_with("Batch | "));
        assert!(frame.lines[1].starts_with("album/disc1/song.wav | File | "));
        assert!(frame.lines[1].contains("Total warmup"));
    }

    #[test]
    fn batch_renderer_clears_stale_characters_on_both_lines_when_frame_shrinks() {
        let mut renderer = ProgressRenderer::new(Vec::new(), true);
        let line_budget = renderer.line_budget();
        let long_batch_line =
            "Batch overall progress | [=====>----]  55.0% | Elapsed 00:15 | ETA 00:12 | In 12.1 KiB/s | Out 24.1 KiB/s | Total 36.2 KiB/s";
        let long_file_line =
            "disc-one-with-a-very-long-name.wav | File | [=====>----]  55.0% | Elapsed 00:09 | ETA 00:07 | In 6.0 KiB/s | Out 12.0 KiB/s | Total 18.0 KiB/s";
        let expected_rows_up = rendered_row_count(display_width(long_batch_line), line_budget)
            + rendered_row_count(display_width(long_file_line), line_budget)
            - 1;
        renderer
            .observe_frame(ProgressFrame {
                lines: vec![
                    long_batch_line.into(),
                    long_file_line.into(),
                ],
            })
            .unwrap();
        renderer
            .observe_frame(ProgressFrame {
                lines: vec![
                    "Batch | [==========] 100.0% | Elapsed 00:16 | ETA 00:00 | In 12.1 KiB/s | Out 24.1 KiB/s | Total 36.2 KiB/s".into(),
                    "x.wav | File | [==========] 100.0% | Elapsed 00:01 | ETA 00:00 | In 333 B/s | Out 666 B/s | Total 999 B/s"
                        .into(),
                ],
            })
            .unwrap();
        renderer.end().unwrap();

        let output = String::from_utf8(renderer.writer).unwrap();
        assert!(output.contains(&format!("\x1b[{expected_rows_up}A")));
        let final_lines = final_frame_lines(&output);
        assert_eq!(final_lines.len(), 2);
        assert!(final_lines[0].contains("Batch | "));
        assert!(final_lines[1].contains("x.wav | File | "));
        assert!(final_lines[0].ends_with(' '));
        assert!(final_lines[1].ends_with(' '));
    }

    #[test]
    fn batch_progress_uses_exact_total_samples_before_completion() {
        let mut progress = BatchProgressCoordinator::new(Vec::new(), true, None, 300, true);
        progress
            .begin_file("disc1/first.wav", 1_024, 100, 100)
            .unwrap();
        progress
            .observe_with_elapsed(
                ProgressSnapshot {
                    processed_samples: 50,
                    total_samples: 100,
                    completed_frames: 1,
                    total_frames: 2,
                    input_bytes_read: 100,
                    output_bytes_written: 200,
                },
                Duration::from_millis(300),
                Duration::from_millis(300),
            )
            .unwrap();
        progress.end().unwrap();

        let output = String::from_utf8(progress.renderer.writer).unwrap();
        let final_lines = final_frame_lines(&output);
        assert_eq!(final_lines.len(), 2);
        assert!(final_lines[0].contains("Batch | "));
        assert!(final_lines[0].contains("16.7%"));
        assert!(final_lines[1].contains("disc1/first.wav | File | "));
        assert!(final_lines[1].contains("50.0%"));
    }

    #[test]
    fn begin_file_renders_an_initial_zero_progress_frame_for_encode_decode() {
        let mut progress = BatchProgressCoordinator::new(Vec::new(), true, None, 300, true);
        progress
            .begin_file("disc1/first.wav", 1_024, 100, 100)
            .unwrap();
        progress.end().unwrap();

        let output = String::from_utf8(progress.renderer.writer).unwrap();
        let final_lines = final_frame_lines(&output);
        assert_eq!(final_lines.len(), 2);
        assert!(final_lines[0].contains("Batch | "));
        assert!(final_lines[0].contains("0.0%"));
        assert!(final_lines[1].contains("disc1/first.wav | File | "));
        assert!(final_lines[1].contains("0.0%"));
        assert!(final_lines[1].contains("Total warmup"));
    }

    #[test]
    fn initial_frame_preserves_batch_elapsed_across_file_boundaries() {
        let mut progress = BatchProgressCoordinator::new(Vec::new(), true, None, 200, true);
        progress.completed_samples = 100;
        progress
            .begin_file("disc1/second.wav", 1_024, 100, 100)
            .unwrap();
        progress.batch_started_at = Some(Instant::now() - Duration::from_secs(5));
        if let Some(current) = progress.active_files.get_mut(&0) {
            current.started_at = Some(Instant::now() - Duration::from_secs(2));
        }
        progress.render_initial_file_frame(100).unwrap();
        progress.end().unwrap();

        let output = String::from_utf8(progress.renderer.writer).unwrap();
        let final_lines = final_frame_lines(&output);
        assert_eq!(final_lines.len(), 2);
        assert!(final_lines[0].contains("Batch | "));
        assert!(final_lines[0].contains("50.0%"));
        assert!(final_lines[0].contains("Elapsed 00:05"));
        assert!(final_lines[1].contains("disc1/second.wav | File | "));
        assert!(final_lines[1].contains("Elapsed 00:02"));
    }

    #[test]
    fn time_until_next_second_aligns_to_the_next_visible_clock_tick() {
        assert_eq!(
            time_until_next_second(Duration::from_millis(1_250)),
            Duration::from_millis(750)
        );
        assert_eq!(
            time_until_next_second(Duration::from_secs(3)),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn progress_updates_render_immediately_without_waiting_for_a_heartbeat() {
        let mut progress = BatchProgressCoordinator::new(Vec::new(), true, None, 100, true);
        progress
            .begin_file("disc1/first.wav", 1_024, 100, 100)
            .unwrap();
        progress
            .observe_with_elapsed(
                ProgressSnapshot {
                    processed_samples: 25,
                    total_samples: 100,
                    completed_frames: 1,
                    total_frames: 4,
                    input_bytes_read: 100,
                    output_bytes_written: 200,
                },
                Duration::from_millis(300),
                Duration::from_millis(300),
            )
            .unwrap();
        progress
            .observe_with_elapsed(
                ProgressSnapshot {
                    processed_samples: 50,
                    total_samples: 100,
                    completed_frames: 2,
                    total_frames: 4,
                    input_bytes_read: 200,
                    output_bytes_written: 400,
                },
                Duration::from_millis(350),
                Duration::from_millis(350),
            )
            .unwrap();
        progress.end().unwrap();

        let output = String::from_utf8(progress.renderer.writer).unwrap();
        let final_lines = final_frame_lines(&output);
        assert!(final_lines[0].contains("50.0%"));
        assert!(final_lines[1].contains("50.0%"));
    }

    #[test]
    fn heartbeat_repaints_cached_progress_when_callbacks_are_sparse() {
        let mut progress = BatchProgressCoordinator::new(Vec::new(), true, None, 100, true);
        progress
            .begin_file("disc1/first.wav", 1_024, 100, 100)
            .unwrap();
        progress.batch_started_at = Some(Instant::now() - Duration::from_millis(400));
        if let Some(current) = progress.active_files.get_mut(&0) {
            current.started_at = Some(Instant::now() - Duration::from_millis(400));
        }
        progress
            .observe_with_elapsed(
                ProgressSnapshot {
                    processed_samples: 25,
                    total_samples: 100,
                    completed_frames: 1,
                    total_frames: 4,
                    input_bytes_read: 100,
                    output_bytes_written: 200,
                },
                Duration::from_millis(400),
                Duration::from_millis(400),
            )
            .unwrap();
        progress.batch_started_at = Some(Instant::now() - Duration::from_millis(1_400));
        if let Some(current) = progress.active_files.get_mut(&0) {
            current.started_at = Some(Instant::now() - Duration::from_millis(1_400));
        }
        progress.heartbeat().unwrap();
        progress.end().unwrap();

        let output = String::from_utf8(progress.renderer.writer).unwrap();
        let final_lines = final_frame_lines(&output);
        assert!(final_lines[0].contains("25.0%"));
        assert!(final_lines[0].contains("Elapsed 00:01"));
        assert!(final_lines[1].contains("25.0%"));
        assert!(final_lines[1].contains("Elapsed 00:01"));
    }

    #[test]
    fn progress_update_after_heartbeat_uses_the_newer_snapshot() {
        let mut progress = BatchProgressCoordinator::new(Vec::new(), true, None, 100, true);
        progress
            .begin_file("disc1/first.wav", 1_024, 100, 100)
            .unwrap();
        progress
            .observe_with_elapsed(
                ProgressSnapshot {
                    processed_samples: 25,
                    total_samples: 100,
                    completed_frames: 1,
                    total_frames: 4,
                    input_bytes_read: 100,
                    output_bytes_written: 200,
                },
                Duration::from_millis(400),
                Duration::from_millis(400),
            )
            .unwrap();
        progress.batch_started_at = Some(Instant::now() - Duration::from_millis(1_400));
        if let Some(current) = progress.active_files.get_mut(&0) {
            current.started_at = Some(Instant::now() - Duration::from_millis(1_400));
        }
        progress.heartbeat().unwrap();
        progress
            .observe_with_elapsed(
                ProgressSnapshot {
                    processed_samples: 50,
                    total_samples: 100,
                    completed_frames: 2,
                    total_frames: 4,
                    input_bytes_read: 200,
                    output_bytes_written: 400,
                },
                Duration::from_millis(1_450),
                Duration::from_millis(1_450),
            )
            .unwrap();
        progress.end().unwrap();

        let output = String::from_utf8(progress.renderer.writer).unwrap();
        let final_lines = final_frame_lines(&output);
        assert!(final_lines[0].contains("50.0%"));
        assert!(final_lines[1].contains("50.0%"));
    }

    #[test]
    fn recompress_begin_file_primes_heartbeat_before_the_first_progress_callback() {
        let mut progress = BatchProgressCoordinator::new(Vec::new(), true, None, 200, true);
        progress
            .begin_file("disc1/first.flac", 1_024, 100, 200)
            .unwrap();
        assert!(progress.next_heartbeat_delay().is_some());
        progress.batch_started_at = Some(Instant::now() - Duration::from_secs(2));
        if let Some(current) = progress.active_files.get_mut(&0) {
            current.started_at = Some(Instant::now() - Duration::from_secs(2));
            current.phase_started_at = Some(Instant::now() - Duration::from_secs(2));
        }
        progress.heartbeat().unwrap();
        progress.end().unwrap();

        let output = String::from_utf8(progress.renderer.writer).unwrap();
        let final_lines = final_frame_lines(&output);
        assert_eq!(final_lines.len(), 2);
        assert!(final_lines[0].contains("Batch | "));
        assert!(final_lines[0].contains("Elapsed 00:02"));
        assert!(final_lines[1].contains("disc1/first.flac | Decode | "));
        assert!(final_lines[1].contains("Elapsed 00:02"));
        assert!(final_lines[1].contains("Total warmup"));
    }

    #[test]
    fn recompress_batch_phase_switch_resets_file_estimate_state() {
        let mut progress = BatchProgressCoordinator::new(Vec::new(), true, None, 200, true);
        progress
            .begin_file("disc1/first.flac", 1_024, 100, 200)
            .unwrap();
        progress
            .observe_recompress(RecompressProgress {
                phase: RecompressPhase::Decode,
                phase_processed_samples: 50,
                phase_total_samples: 100,
                overall_processed_samples: 50,
                overall_total_samples: 200,
                completed_frames: 1,
                total_frames: 2,
                phase_input_bytes_read: 100,
                phase_output_bytes_written: 200,
                overall_input_bytes_read: 100,
                overall_output_bytes_written: 200,
            })
            .unwrap();
        progress
            .observe_recompress(RecompressProgress {
                phase: RecompressPhase::Decode,
                phase_processed_samples: 100,
                phase_total_samples: 100,
                overall_processed_samples: 100,
                overall_total_samples: 200,
                completed_frames: 2,
                total_frames: 2,
                phase_input_bytes_read: 200,
                phase_output_bytes_written: 400,
                overall_input_bytes_read: 200,
                overall_output_bytes_written: 400,
            })
            .unwrap();
        assert_eq!(
            progress
                .active_files
                .get(&0)
                .unwrap()
                .file_state
                .advancing_updates,
            2
        );

        progress
            .observe_recompress(RecompressProgress {
                phase: RecompressPhase::Encode,
                phase_processed_samples: 10,
                phase_total_samples: 100,
                overall_processed_samples: 110,
                overall_total_samples: 200,
                completed_frames: 1,
                total_frames: 2,
                phase_input_bytes_read: 50,
                phase_output_bytes_written: 75,
                overall_input_bytes_read: 250,
                overall_output_bytes_written: 475,
            })
            .unwrap();

        assert_eq!(
            progress
                .active_files
                .get(&0)
                .unwrap()
                .file_state
                .advancing_updates,
            1
        );
        assert_eq!(
            progress
                .active_files
                .get(&0)
                .and_then(|current| current.phase),
            Some(RecompressPhase::Encode)
        );
    }

    #[test]
    fn recompress_phase_switch_keeps_file_elapsed_continuous() {
        let mut progress = BatchProgressCoordinator::new(Vec::new(), true, None, 200, true);
        progress
            .begin_file("disc1/first.flac", 1_024, 100, 200)
            .unwrap();
        if let Some(current) = progress.active_files.get_mut(&0) {
            current.started_at = Some(Instant::now() - Duration::from_secs(9));
        }
        progress
            .observe_recompress_with_elapsed(
                RecompressProgress {
                    phase: RecompressPhase::Decode,
                    phase_processed_samples: 50,
                    phase_total_samples: 100,
                    overall_processed_samples: 50,
                    overall_total_samples: 200,
                    completed_frames: 1,
                    total_frames: 2,
                    phase_input_bytes_read: 100,
                    phase_output_bytes_written: 200,
                    overall_input_bytes_read: 100,
                    overall_output_bytes_written: 200,
                },
                Duration::from_secs(9),
            )
            .unwrap();
        if let Some(current) = progress.active_files.get_mut(&0) {
            current.started_at = Some(Instant::now() - Duration::from_secs(9));
        }
        progress
            .observe_recompress_with_elapsed(
                RecompressProgress {
                    phase: RecompressPhase::Encode,
                    phase_processed_samples: 10,
                    phase_total_samples: 100,
                    overall_processed_samples: 110,
                    overall_total_samples: 200,
                    completed_frames: 1,
                    total_frames: 2,
                    phase_input_bytes_read: 50,
                    phase_output_bytes_written: 75,
                    overall_input_bytes_read: 250,
                    overall_output_bytes_written: 475,
                },
                Duration::from_secs(9),
            )
            .unwrap();
        progress.end().unwrap();

        let output = String::from_utf8(progress.renderer.writer).unwrap();
        let final_lines = final_frame_lines(&output);
        assert_eq!(final_lines.len(), 2);
        assert!(final_lines[1].contains("disc1/first.flac | Encode | "));
        assert!(final_lines[1].contains("Elapsed 00:09"));
    }

    #[test]
    fn batch_progress_accumulates_interleaved_file_ids() {
        let mut progress = BatchProgressCoordinator::new(Vec::new(), true, None, 200, true);
        progress
            .begin_file_for(0, "disc1/first.wav", 1_024, 100, 100)
            .unwrap();
        progress
            .begin_file_for(1, "disc1/second.wav", 1_024, 100, 100)
            .unwrap();
        progress
            .observe_with_elapsed_for(
                0,
                ProgressSnapshot {
                    processed_samples: 25,
                    total_samples: 100,
                    completed_frames: 1,
                    total_frames: 4,
                    input_bytes_read: 100,
                    output_bytes_written: 200,
                },
                Duration::from_millis(300),
                Duration::from_millis(300),
            )
            .unwrap();
        progress
            .observe_with_elapsed_for(
                1,
                ProgressSnapshot {
                    processed_samples: 50,
                    total_samples: 100,
                    completed_frames: 2,
                    total_frames: 4,
                    input_bytes_read: 200,
                    output_bytes_written: 400,
                },
                Duration::from_millis(400),
                Duration::from_millis(250),
            )
            .unwrap();
        progress.end().unwrap();

        let output = String::from_utf8(progress.renderer.writer).unwrap();
        let final_lines = final_frame_lines(&output);
        assert!(final_lines[0].contains("37.5%"));
        assert!(final_lines[1].contains("disc1/second.wav | File | "));
        assert!(final_lines[1].contains("50.0%"));
    }

    #[test]
    fn cached_relative_display_names_sort_like_the_previous_input_path_order() {
        let input_root = PathBuf::from("fixtures");
        let mut relative_sort = vec![
            input_root.join("disc10").join("track02.wav"),
            input_root.join("disc2").join("track01.wav"),
            input_root.join("disc2").join("track10.wav"),
            input_root.join("disc1").join("bonus").join("track01.wav"),
            input_root.join("disc1").join("track02.wav"),
        ];
        let mut legacy_sort = relative_sort.clone();

        relative_sort.sort_by(|left, right| {
            relative_display_name(left.strip_prefix(&input_root).unwrap()).cmp(
                &relative_display_name(right.strip_prefix(&input_root).unwrap()),
            )
        });
        legacy_sort.sort_by_key(|path| legacy_input_path_sort_key(path));

        assert_eq!(relative_sort, legacy_sort);
    }

    #[test]
    fn progress_reporting_mode_skips_events_when_non_interactive_without_trace() {
        assert_eq!(
            progress_reporting_mode(false, false),
            ProgressReportingMode::Disabled
        );
    }

    #[test]
    fn progress_reporting_mode_keeps_events_for_interactive_or_traced_runs() {
        assert_eq!(
            progress_reporting_mode(true, false),
            ProgressReportingMode::Events
        );
        assert_eq!(
            progress_reporting_mode(false, true),
            ProgressReportingMode::Events
        );
    }
}
