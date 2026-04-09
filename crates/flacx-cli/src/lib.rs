//! Command-line support utilities for the `flacx-cli` crate.
//!
//! `flacx-cli` provides the command-line interface for WAV/FLAC conversion in
//! this workspace. It stays separate from the publishable `flacx` library
//! crate while reusing the same encode/decode pipeline and workspace version.
//!
//! Progress rendering stays a CLI concern. The library reports real encode,
//! decode, and recompress progress, while this crate decides when and how to
//! render live single-file and batch progress.
//!
//! # Command shape
//!
//! - `flacx encode <input> [-o <output-or-dir>] [--depth <depth>]`
//! - `flacx decode <input> [-o <output-or-dir>] [--depth <depth>]`
//! - `flacx recompress <input> [-o <output-or-dir>] [--in-place] [--depth <depth>]`
//! - encode-only flags:
//!   - `--output`
//!   - `--level`
//!   - `--threads`
//!   - `--block-size`
//!   - `--mode`
//!   - `--depth` (directory input only)
//! - decode-only flags:
//!   - `--output`
//!   - `--threads`
//!   - `--mode`
//!   - `--depth` (directory input only)
//! - recompress-only flags:
//!   - `--output`
//!   - `--in-place`
//!   - `--level`
//!   - `--threads`
//!   - `--block-size`
//!   - `--mode`
//!   - `--depth` (directory input only)

use std::{
    env,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use flacx::{
    DecodeConfig, Decoder, Encoder, EncoderConfig, Error, ProgressSnapshot, RecompressConfig,
    RecompressPhase, RecompressProgress, Recompressor, Result, inspect_flac_total_samples,
    inspect_wav_total_samples,
};
use walkdir::WalkDir;

const SINGLE_PROGRESS_BAR_WIDTH: usize = 24;
const BATCH_PROGRESS_BAR_WIDTH: usize = 10;
const ESTIMATE_WARMUP: Duration = Duration::from_millis(250);
const SINGLE_RENDER_INTERVAL: Duration = Duration::from_millis(125);
const BATCH_RENDER_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeCommand {
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    pub depth: usize,
    pub config: EncoderConfig,
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
            Self::Encode => &["wav", "rf64", "w64"],
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
}

struct ProgressTrace {
    file: File,
    kind: CommandKind,
    interactive: bool,
    batch_mode: bool,
}

pub fn encode_command(
    command: &EncodeCommand,
    interactive: bool,
    stderr: &mut impl Write,
) -> Result<()> {
    let planned = plan_encode_worklist(command)?;
    let trace = ProgressTrace::from_env(CommandKind::Encode, interactive, planned.is_directory)?;
    let mut progress = BatchProgressCoordinator::new(
        stderr,
        interactive,
        trace,
        planned.total_samples,
        planned.is_directory,
    );

    for item in planned.items {
        if item.ensure_parent_dirs {
            ensure_output_parent_dirs(&item.output)?;
        }
        progress.begin_file(
            &item.display_name,
            item.input_bytes,
            item.phase_total_samples,
            item.overall_total_samples,
        );
        let result = Encoder::new(command.config.clone()).encode_file_with_progress(
            &item.input,
            &item.output,
            |update| {
                progress.observe(update)?;
                Ok(())
            },
        );

        match result {
            Ok(_) => progress.finish_current_file()?,
            Err(error) => {
                let _ = progress.end();
                return Err(error);
            }
        }
    }

    progress.finish()?;
    Ok(())
}

pub fn decode_command(
    command: &DecodeCommand,
    interactive: bool,
    stderr: &mut impl Write,
) -> Result<()> {
    let planned = plan_decode_worklist(command)?;
    let trace = ProgressTrace::from_env(CommandKind::Decode, interactive, planned.is_directory)?;
    let mut progress = BatchProgressCoordinator::new(
        stderr,
        interactive,
        trace,
        planned.total_samples,
        planned.is_directory,
    );

    for item in planned.items {
        if item.ensure_parent_dirs {
            ensure_output_parent_dirs(&item.output)?;
        }
        progress.begin_file(
            &item.display_name,
            item.input_bytes,
            item.phase_total_samples,
            item.overall_total_samples,
        );
        let result = Decoder::new(command.config).decode_file_with_progress(
            &item.input,
            &item.output,
            |update| {
                progress.observe(update)?;
                Ok(())
            },
        );

        match result {
            Ok(_) => progress.finish_current_file()?,
            Err(error) => {
                let _ = progress.end();
                return Err(error);
            }
        }
    }

    progress.finish()?;
    Ok(())
}

pub fn recompress_command(
    command: &RecompressCommand,
    interactive: bool,
    stderr: &mut impl Write,
) -> Result<()> {
    let planned = plan_recompress_worklist(command)?;
    let trace =
        ProgressTrace::from_env(CommandKind::Recompress, interactive, planned.is_directory)?;
    let mut progress = BatchProgressCoordinator::new(
        stderr,
        interactive,
        trace,
        planned.total_samples,
        planned.is_directory,
    );

    for item in planned.items {
        if item.ensure_parent_dirs {
            ensure_output_parent_dirs(&item.output)?;
        }
        progress.begin_file(
            &item.display_name,
            item.input_bytes,
            item.phase_total_samples,
            item.overall_total_samples,
        );
        let result = Recompressor::new(command.config).recompress_file_with_progress(
            &item.input,
            &item.output,
            |update: RecompressProgress| {
                progress.observe_recompress(update)?;
                Ok(())
            },
        );

        match result {
            Ok(_) => progress.finish_current_file()?,
            Err(error) => {
                let _ = progress.end();
                return Err(error);
            }
        }
    }

    progress.finish()?;
    Ok(())
}

fn plan_encode_worklist(command: &EncodeCommand) -> Result<PlannedWorklist> {
    plan_worklist(
        CommandKind::Encode,
        &command.input,
        command.output.as_deref(),
        command.depth,
        inspect_wav_file_total_samples,
    )
}

fn plan_decode_worklist(command: &DecodeCommand) -> Result<PlannedWorklist> {
    plan_worklist(
        CommandKind::Decode,
        &command.input,
        command.output.as_deref(),
        command.depth,
        inspect_flac_file_total_samples,
    )
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
        Some(output_root) => Some(validate_or_create_output_root(kind, output_root)?),
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

    Ok(PlannedWorklist {
        items: worklist,
        total_samples,
        is_directory: true,
    })
}

fn validate_or_create_output_root(kind: CommandKind, output_root: &Path) -> Result<PathBuf> {
    if output_root.exists() {
        if !output_root.is_dir() {
            return Err(kind.planning_error(kind.directory_output_error(output_root)));
        }
    } else {
        fs::create_dir_all(output_root)?;
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
        if !entry.file_type().is_file() || !has_any_extension(entry.path(), kind.source_extensions()) {
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
                Some(validate_or_create_output_root(
                    CommandKind::Recompress,
                    output_root,
                )?)
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
    Ok(PlannedWorklist {
        items,
        total_samples,
        is_directory: true,
    })
}

fn inspect_wav_file_total_samples(path: &Path) -> Result<u64> {
    inspect_wav_total_samples(File::open(path)?)
}

fn inspect_flac_file_total_samples(path: &Path) -> Result<u64> {
    inspect_flac_total_samples(File::open(path)?)
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
        .is_some_and(|ext| extensions.iter().any(|candidate| ext.eq_ignore_ascii_case(candidate)))
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
    fn from_env(kind: CommandKind, interactive: bool, batch_mode: bool) -> Result<Option<Self>> {
        let Some(path) = env::var_os("FLACX_PROGRESS_TRACE").map(PathBuf::from) else {
            return Ok(None);
        };
        let mut trace = Self {
            file: File::create(path)?,
            kind,
            interactive,
            batch_mode,
        };
        trace.write_command_header()?;
        Ok(Some(trace))
    }

    fn write_command_header(&mut self) -> std::io::Result<()> {
        writeln!(
            self.file,
            "event=command\tkind={}\tinteractive={}\tbatch_mode={}",
            self.kind.as_str(),
            u8::from(self.interactive),
            u8::from(self.batch_mode)
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
        writeln!(
            self.file,
            "event=file_finish\tkind={}\tinteractive={}\tbatch_mode={}\tfilename={}\tinput_bytes={}\telapsed_seconds={:.9}",
            self.kind.as_str(),
            u8::from(self.interactive),
            u8::from(self.batch_mode),
            sanitize_trace_field(filename),
            input_bytes,
            elapsed.as_secs_f64()
        )?;
        self.file.flush()?;
        Ok(())
    }
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
    current_file: Option<CurrentFileProgress>,
    batch_started_at: Option<Instant>,
    overall_state: ProgressState,
    file_state: ProgressState,
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
            current_file: None,
            batch_started_at: None,
            overall_state: ProgressState::default(),
            file_state: ProgressState::default(),
        }
    }

    fn begin_file(
        &mut self,
        filename: &str,
        input_bytes: u64,
        phase_total_samples: u64,
        overall_total_samples: u64,
    ) {
        self.current_file = Some(CurrentFileProgress {
            filename: filename.to_string(),
            input_bytes,
            phase_total_samples,
            overall_total_samples,
            phase: None,
            saw_update: false,
            started_at: Some(Instant::now()),
            phase_started_at: None,
        });
        self.file_state = ProgressState::default();
    }

    fn observe(&mut self, progress: ProgressSnapshot) -> std::io::Result<()> {
        let batch_elapsed = self.batch_elapsed();
        let file_elapsed = self.current_file_elapsed();
        self.observe_with_elapsed(progress, batch_elapsed, file_elapsed)
    }

    fn observe_recompress(&mut self, progress: RecompressProgress) -> std::io::Result<()> {
        let batch_elapsed = self.batch_elapsed();
        self.observe_recompress_with_elapsed(progress, batch_elapsed)
    }

    fn observe_with_elapsed(
        &mut self,
        progress: ProgressSnapshot,
        batch_elapsed: Duration,
        file_elapsed: Duration,
    ) -> std::io::Result<()> {
        let (filename, total_samples) = {
            let current = self
                .current_file
                .as_mut()
                .expect("current file must be set");
            current.saw_update = true;
            (current.filename.clone(), current.phase_total_samples)
        };
        let force_render = progress.processed_samples >= total_samples
            || self
                .completed_samples
                .saturating_add(progress.processed_samples)
                >= self.total_samples;
        let identities = progress_frame_identities(&filename, self.batch_mode, None);
        if !self
            .renderer
            .should_render(force_render, &identities, self.batch_mode)
        {
            return Ok(());
        }

        let display = self.display_for_snapshot(&filename, total_samples, progress);
        let overall_estimate = self.overall_state.observe(
            display
                .overall
                .processed_samples
                .min(display.overall.total_samples),
            display.overall.total_samples,
            batch_elapsed,
        );
        let file_estimate = display.file.map(|file| {
            self.file_state.observe(
                file.processed_samples.min(file.total_samples),
                file.total_samples,
                file_elapsed,
            )
        });
        let frame = format_progress_frame(
            &display,
            &overall_estimate,
            file_estimate.as_ref(),
            batch_elapsed,
            file_elapsed,
        );
        self.renderer.render(frame)
    }

    fn observe_recompress_with_elapsed(
        &mut self,
        progress: RecompressProgress,
        batch_elapsed: Duration,
    ) -> std::io::Result<()> {
        let (filename, phase_label) = {
            let current = self
                .current_file
                .as_mut()
                .expect("current file must be set");
            current.saw_update = true;
            if current.phase != Some(progress.phase) {
                current.phase = Some(progress.phase);
                current.phase_started_at = Some(Instant::now());
                self.file_state = ProgressState::default();
            }
            let phase_label = current.phase.expect("phase must be set").as_str();
            (current.filename.clone(), phase_label)
        };
        let phase_elapsed = self.current_phase_elapsed();
        let force_render = progress.overall_processed_samples >= progress.overall_total_samples
            || self
                .completed_samples
                .saturating_add(progress.overall_processed_samples)
                >= self.total_samples;
        let identities = progress_frame_identities(&filename, self.batch_mode, Some(phase_label));
        if !self
            .renderer
            .should_render(force_render, &identities, self.batch_mode)
        {
            return Ok(());
        }

        let display = self.display_for_recompress_progress(&filename, progress);
        let overall_estimate = self.overall_state.observe(
            display
                .overall
                .processed_samples
                .min(display.overall.total_samples),
            display.overall.total_samples,
            batch_elapsed,
        );
        let file_estimate = display.file.map(|file| {
            self.file_state.observe(
                file.processed_samples.min(file.total_samples),
                file.total_samples,
                phase_elapsed,
            )
        });
        let frame = format_progress_frame(
            &display,
            &overall_estimate,
            file_estimate.as_ref(),
            batch_elapsed,
            phase_elapsed,
        );
        self.renderer.render(frame)
    }

    fn finish_current_file(&mut self) -> std::io::Result<()> {
        let Some((needs_final_update, total_samples)) = self
            .current_file
            .as_ref()
            .map(|current| (!current.saw_update, current.phase_total_samples))
        else {
            return Ok(());
        };
        if needs_final_update {
            let batch_elapsed = self.batch_elapsed();
            let file_elapsed = self.current_file_elapsed();
            let completed = ProgressSnapshot {
                processed_samples: total_samples,
                total_samples,
                completed_frames: 0,
                total_frames: 0,
            };
            self.observe_with_elapsed(completed, batch_elapsed, file_elapsed)?;
        }
        let current = self
            .current_file
            .take()
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
        Ok(())
    }

    fn finish(&mut self) -> std::io::Result<()> {
        self.renderer.end()
    }

    fn end(&mut self) -> std::io::Result<()> {
        self.renderer.end()
    }

    fn batch_elapsed(&mut self) -> Duration {
        let started_at = self.batch_started_at.get_or_insert_with(Instant::now);
        started_at.elapsed()
    }

    fn current_file_elapsed(&mut self) -> Duration {
        let current = self
            .current_file
            .as_mut()
            .expect("current file must be set before observing progress");
        let started_at = current.started_at.get_or_insert_with(Instant::now);
        started_at.elapsed()
    }

    fn current_phase_elapsed(&mut self) -> Duration {
        let current = self
            .current_file
            .as_mut()
            .expect("current file must be set before observing phase progress");
        let started_at = current.phase_started_at.get_or_insert_with(Instant::now);
        started_at.elapsed()
    }

    fn display_for_snapshot(
        &self,
        filename: &str,
        file_total_samples: u64,
        progress: ProgressSnapshot,
    ) -> ProgressDisplay {
        let file_processed = progress.processed_samples.min(file_total_samples);
        let overall_processed = if self.batch_mode {
            self.completed_samples.saturating_add(file_processed)
        } else {
            file_processed
        };

        ProgressDisplay {
            filename: filename.to_string(),
            overall: SampleProgress {
                processed_samples: overall_processed.min(self.total_samples),
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
    ) -> ProgressDisplay {
        let overall_processed = if self.batch_mode {
            self.completed_samples
                .saturating_add(progress.overall_processed_samples)
        } else {
            progress.overall_processed_samples
        };

        ProgressDisplay {
            filename: filename.to_string(),
            overall: SampleProgress {
                processed_samples: overall_processed.min(self.total_samples),
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
    last_line_identities: Vec<String>,
    last_render_at: Option<Instant>,
}

impl<W: Write> ProgressRenderer<W> {
    fn new(writer: W, interactive: bool) -> Self {
        Self {
            writer,
            interactive,
            has_drawn: false,
            last_line_widths: Vec::new(),
            last_line_identities: Vec::new(),
            last_render_at: None,
        }
    }

    #[cfg(test)]
    fn observe_frame(&mut self, frame: ProgressFrame) -> std::io::Result<()> {
        self.render(frame)
    }

    fn should_render(&self, force: bool, identities: &[String], batch_mode: bool) -> bool {
        if !self.interactive {
            return false;
        }
        if force || !self.has_drawn || self.last_line_identities != identities {
            return true;
        }

        self.last_render_at
            .is_none_or(|last_rendered| last_rendered.elapsed() >= render_interval(batch_mode))
    }

    fn render(&mut self, frame: ProgressFrame) -> std::io::Result<()> {
        if !self.interactive {
            return Ok(());
        }
        self.draw_frame(&frame)?;
        self.last_render_at = Some(Instant::now());
        Ok(())
    }

    fn draw_frame(&mut self, frame: &ProgressFrame) -> std::io::Result<()> {
        let previous_height = self.last_line_widths.len();
        if self.has_drawn && previous_height > 1 {
            write!(self.writer, "\x1b[{}A", previous_height - 1)?;
        }

        let total_lines = frame.lines.len().max(previous_height);
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
            let padded_width = line.len().max(previous_width);
            write!(self.writer, "{line:<padded_width$}")?;
        }

        self.has_drawn = true;
        self.last_line_widths = frame.lines.iter().map(String::len).collect();
        self.last_line_identities = frame
            .lines
            .iter()
            .map(|line| frame_line_identity(line))
            .collect();
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
    samples_per_second: Option<f64>,
}

impl ProgressEstimate {
    fn warming_up() -> Self {
        Self {
            eta: None,
            samples_per_second: None,
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

        ProgressEstimate {
            eta: Some(Duration::from_secs_f64(eta_seconds.max(0.0))),
            samples_per_second: Some(samples_per_second),
        }
    }
}

fn format_progress_frame(
    display: &ProgressDisplay,
    overall_estimate: &ProgressEstimate,
    file_estimate: Option<&ProgressEstimate>,
    batch_elapsed: Duration,
    file_elapsed: Duration,
) -> ProgressFrame {
    if let Some(file) = display.file {
        let warming_up = ProgressEstimate::warming_up();
        let file_estimate = file_estimate.unwrap_or(&warming_up);
        return ProgressFrame {
            lines: vec![
                format_batch_overall_line(display.overall, overall_estimate, batch_elapsed),
                format_batch_file_line(
                    &display.filename,
                    display.phase_label.unwrap_or("File"),
                    file,
                    file_estimate,
                    file_elapsed,
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
        )],
    }
}

fn format_progress_line(
    label: &str,
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
    let rate = estimate
        .samples_per_second
        .map(format_speed)
        .unwrap_or_else(|| "warmup".to_string());

    format!(
        "{label} | {} {:>5.1}% | Elapsed {} | ETA {} | Rate {}",
        format_progress_bar(progress_ratio(progress), bar_width),
        progress_ratio(progress) * 100.0,
        elapsed,
        eta,
        rate
    )
}

fn format_single_file_line(
    filename: &str,
    phase_label: Option<&str>,
    progress: SampleProgress,
    estimate: &ProgressEstimate,
    elapsed: Duration,
) -> String {
    let label = phase_label
        .map(|phase| format!("{filename} | {phase}"))
        .unwrap_or_else(|| filename.to_string());
    format_progress_line(
        &label,
        progress,
        estimate,
        elapsed,
        SINGLE_PROGRESS_BAR_WIDTH,
    )
}

fn format_batch_overall_line(
    progress: SampleProgress,
    estimate: &ProgressEstimate,
    elapsed: Duration,
) -> String {
    format_progress_line(
        "Batch",
        progress,
        estimate,
        elapsed,
        BATCH_PROGRESS_BAR_WIDTH,
    )
}

fn format_batch_file_line(
    filename: &str,
    phase_label: &str,
    progress: SampleProgress,
    estimate: &ProgressEstimate,
    elapsed: Duration,
) -> String {
    format!(
        "{filename} | {}",
        format_progress_line(
            phase_label,
            progress,
            estimate,
            elapsed,
            BATCH_PROGRESS_BAR_WIDTH,
        )
    )
}

fn progress_frame_identities(
    filename: &str,
    batch_mode: bool,
    phase_label: Option<&str>,
) -> Vec<String> {
    if batch_mode {
        vec![
            "Batch".to_string(),
            format!("{filename} | {}", phase_label.unwrap_or("File")),
        ]
    } else {
        vec![
            phase_label
                .map(|phase| format!("{filename} | {phase}"))
                .unwrap_or_else(|| filename.to_string()),
        ]
    }
}

fn render_interval(batch_mode: bool) -> Duration {
    if batch_mode {
        BATCH_RENDER_INTERVAL
    } else {
        SINGLE_RENDER_INTERVAL
    }
}

fn frame_line_identity(line: &str) -> String {
    if line.starts_with("Batch | ") {
        return "Batch".to_string();
    }
    let mut parts = line.split(" | ");
    let first = parts.next().unwrap_or_default();
    let second = parts.next().unwrap_or_default();
    if second.starts_with('[') || second.is_empty() {
        first.to_string()
    } else {
        format!("{first} | {second}")
    }
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

fn format_speed(samples_per_second: f64) -> String {
    if samples_per_second >= 1_000_000.0 {
        format!("{:.1}M/s", samples_per_second / 1_000_000.0)
    } else if samples_per_second >= 1_000.0 {
        format!("{:.1}k/s", samples_per_second / 1_000.0)
    } else {
        format!("{samples_per_second:.0}/s")
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
        ProgressRenderer, ProgressState, SampleProgress, format_progress_frame,
        progress_frame_identities, relative_display_name,
    };
    use flacx::{ProgressSnapshot, RecompressPhase, RecompressProgress};

    fn final_frame_lines(output: &str) -> Vec<&str> {
        output
            .trim_end_matches('\n')
            .rsplit("\x1b[1A")
            .next()
            .unwrap_or(output)
            .split('\n')
            .map(|line| line.trim_start_matches('\r'))
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
                    samples_per_second: Some(333.0),
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
        assert!(output.contains("Rate 333/s"));
        assert!(output.ends_with('\n'));
        assert!(!output.ends_with("\n\n"));
    }

    #[test]
    fn progress_renderer_waits_for_two_advancing_updates_and_elapsed_time() {
        let mut state = ProgressState::default();

        let first = state.observe(50, 200, Duration::from_millis(0));
        assert_eq!(first, ProgressEstimate::warming_up());

        let no_advance = state.observe(50, 200, Duration::from_millis(400));
        assert_eq!(no_advance, ProgressEstimate::warming_up());

        let second_advance = state.observe(100, 200, Duration::from_millis(200));
        assert_eq!(second_advance, ProgressEstimate::warming_up());

        let stabilized = state.observe(150, 200, Duration::from_millis(300));
        assert!(stabilized.eta.is_some());
        assert!(stabilized.samples_per_second.is_some());
    }

    #[test]
    fn progress_renderer_ignores_zero_progress_before_warmup_starts() {
        let mut state = ProgressState::default();

        let initial = state.observe(0, 200, Duration::from_millis(500));
        assert_eq!(initial, ProgressEstimate::warming_up());

        let first_advance = state.observe(50, 200, Duration::from_millis(600));
        assert_eq!(first_advance, ProgressEstimate::warming_up());

        let second_advance = state.observe(100, 200, Duration::from_millis(700));
        assert_eq!(second_advance, ProgressEstimate::warming_up());

        let stabilized = state.observe(150, 200, Duration::from_millis(900));
        assert!(stabilized.eta.is_some());
    }

    #[test]
    fn progress_renderer_overwrites_stale_characters_when_line_shrinks() {
        let mut renderer = ProgressRenderer::new(Vec::new(), true);
        renderer
            .observe_frame(ProgressFrame {
                lines: vec!["wide-name.wav | [==>---------------------]  10.0% | Elapsed 00:00 | ETA --:-- | Rate warmup".into()],
            })
            .unwrap();
        renderer
            .observe_frame(ProgressFrame {
                lines: vec!["x.wav | [========================] 100.0% | Elapsed 00:00 | ETA 00:00 | Rate 333/s".into()],
            })
            .unwrap();
        renderer.end().unwrap();

        let output = String::from_utf8(renderer.writer).unwrap();
        let frames: Vec<&str> = output.trim_end_matches('\n').split('\r').collect();
        let warmup = frames
            .iter()
            .find(|frame| frame.contains("Rate warmup"))
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
                samples_per_second: Some(12_345.0),
            },
            Some(&ProgressEstimate {
                eta: Some(Duration::from_secs(1)),
                samples_per_second: Some(4_096.0),
            }),
            Duration::from_secs(4),
            Duration::from_secs(2),
        );
        assert_eq!(frame.lines.len(), 2);
        assert!(frame.lines[0].starts_with("Batch | "));
        assert!(frame.lines[0].contains("50.0%"));
        assert!(frame.lines[0].contains("Elapsed 00:04"));
        assert!(frame.lines[0].contains("ETA 00:03"));
        assert!(frame.lines[0].contains("Rate 12.3k/s"));
        assert!(frame.lines[1].starts_with("album/disc1/song.wav | File | "));
        assert!(frame.lines[1].contains("50.0%"));
        assert!(frame.lines[1].contains("Elapsed 00:02"));
        assert!(frame.lines[1].contains("ETA 00:01"));
        assert!(frame.lines[1].contains("Rate 4.1k/s"));
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
        assert!(frame.lines[1].contains("Rate warmup"));
    }

    #[test]
    fn batch_renderer_clears_stale_characters_on_both_lines_when_frame_shrinks() {
        let mut renderer = ProgressRenderer::new(Vec::new(), true);
        renderer
            .observe_frame(ProgressFrame {
                lines: vec![
                    "Batch overall progress | [=====>----]  55.0% | Elapsed 00:15 | ETA 00:12 | Rate 12.3k/s"
                        .into(),
                    "disc-one-with-a-very-long-name.wav | File | [=====>----]  55.0% | Elapsed 00:09 | ETA 00:07 | Rate 6.1k/s"
                        .into(),
                ],
            })
            .unwrap();
        renderer
            .observe_frame(ProgressFrame {
                lines: vec![
                    "Batch | [==========] 100.0% | Elapsed 00:16 | ETA 00:00 | Rate 12.3k/s".into(),
                    "x.wav | File | [==========] 100.0% | Elapsed 00:01 | ETA 00:00 | Rate 333/s"
                        .into(),
                ],
            })
            .unwrap();
        renderer.end().unwrap();

        let output = String::from_utf8(renderer.writer).unwrap();
        assert!(output.contains("\x1b[1A"));
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
        progress.begin_file("disc1/first.wav", 1_024, 100, 100);
        progress
            .observe_with_elapsed(
                ProgressSnapshot {
                    processed_samples: 50,
                    total_samples: 100,
                    completed_frames: 1,
                    total_frames: 2,
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
    fn batch_renderer_uses_a_longer_refresh_interval_than_single_file_mode() {
        let identities = progress_frame_identities("disc1/first.wav", true, None);
        let mut renderer = ProgressRenderer::new(Vec::new(), true);
        renderer.has_drawn = true;
        renderer.last_line_identities = identities.clone();
        renderer.last_render_at = Some(Instant::now() - Duration::from_millis(150));

        assert!(!renderer.should_render(false, &identities, true));
        assert!(renderer.should_render(false, &identities, false));
    }

    #[test]
    fn recompress_batch_phase_switch_resets_file_estimate_state() {
        let mut progress = BatchProgressCoordinator::new(Vec::new(), true, None, 200, true);
        progress.begin_file("disc1/first.flac", 1_024, 100, 200);
        progress
            .observe_recompress(RecompressProgress {
                phase: RecompressPhase::Decode,
                phase_processed_samples: 50,
                phase_total_samples: 100,
                overall_processed_samples: 50,
                overall_total_samples: 200,
                completed_frames: 1,
                total_frames: 2,
            })
            .unwrap();
        progress.renderer.last_render_at = Some(Instant::now() - Duration::from_secs(1));
        progress
            .observe_recompress(RecompressProgress {
                phase: RecompressPhase::Decode,
                phase_processed_samples: 100,
                phase_total_samples: 100,
                overall_processed_samples: 100,
                overall_total_samples: 200,
                completed_frames: 2,
                total_frames: 2,
            })
            .unwrap();
        assert_eq!(progress.file_state.advancing_updates, 2);

        progress
            .observe_recompress(RecompressProgress {
                phase: RecompressPhase::Encode,
                phase_processed_samples: 10,
                phase_total_samples: 100,
                overall_processed_samples: 110,
                overall_total_samples: 200,
                completed_frames: 1,
                total_frames: 2,
            })
            .unwrap();

        assert_eq!(progress.file_state.advancing_updates, 1);
        assert_eq!(
            progress
                .current_file
                .as_ref()
                .and_then(|current| current.phase),
            Some(RecompressPhase::Encode)
        );
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
}
