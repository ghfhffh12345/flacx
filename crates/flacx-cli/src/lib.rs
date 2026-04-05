//! Command-line support utilities for the `flacx-cli` crate.
//!
//! `flacx-cli` provides the command-line interface for WAV/FLAC conversion in
//! this workspace. It stays separate from the publishable `flacx` library
//! crate while reusing the same encode/decode pipeline and workspace version.
//!
//! Progress rendering stays a CLI concern. The library reports real encode and
//! decode progress, while this crate decides when and how to render live
//! single-file and batch progress.
//!
//! # Command shape
//!
//! - `flacx encode <input> [-o <output-or-dir>] [--depth <depth>]`
//! - `flacx decode <input> [-o <output-or-dir>] [--depth <depth>]`
//! - encode-only flags:
//!   - `--output`
//!   - `--level`
//!   - `--threads`
//!   - `--block-size`
//!   - `--depth` (directory input only)
//! - decode-only flags:
//!   - `--output`
//!   - `--threads`
//!   - `--depth` (directory input only)

use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use flacx::{
    DecodeConfig, Decoder, Encoder, EncoderConfig, Error, ProgressSnapshot, Result,
    inspect_flac_total_samples, inspect_wav_total_samples,
};
use walkdir::WalkDir;

const SINGLE_PROGRESS_BAR_WIDTH: usize = 24;
const BATCH_PROGRESS_BAR_WIDTH: usize = 10;
const ESTIMATE_WARMUP: Duration = Duration::from_millis(250);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandKind {
    Encode,
    Decode,
}

impl CommandKind {
    fn source_extension(self) -> &'static str {
        match self {
            Self::Encode => "wav",
            Self::Decode => "flac",
        }
    }

    fn target_extension(self) -> &'static str {
        match self {
            Self::Encode => "flac",
            Self::Decode => "wav",
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
        }
    }

    fn planning_error(self, message: impl Into<String>) -> Error {
        match self {
            Self::Encode => Error::Encode(message.into()),
            Self::Decode => Error::Decode(message.into()),
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
    total_samples: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProgressDisplay {
    filename: String,
    overall: SampleProgress,
    file: Option<SampleProgress>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SampleProgress {
    processed_samples: u64,
    total_samples: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CurrentFileProgress {
    filename: String,
    total_samples: u64,
    saw_update: bool,
}

pub fn encode_command(
    command: &EncodeCommand,
    interactive: bool,
    stderr: &mut impl Write,
) -> Result<()> {
    let planned = plan_encode_worklist(command)?;
    let mut progress = BatchProgressCoordinator::new(
        stderr,
        interactive,
        planned.total_samples,
        planned.is_directory,
    );

    for item in planned.items {
        if item.ensure_parent_dirs {
            ensure_output_parent_dirs(&item.output)?;
        }
        progress.begin_file(&item.display_name, item.total_samples);
        let result = Encoder::new(command.config).encode_file_with_progress(
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
    let mut progress = BatchProgressCoordinator::new(
        stderr,
        interactive,
        planned.total_samples,
        planned.is_directory,
    );

    for item in planned.items {
        if item.ensure_parent_dirs {
            ensure_output_parent_dirs(&item.output)?;
        }
        progress.begin_file(&item.display_name, item.total_samples);
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
            total_samples: item.total_samples,
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
    let output = match output {
        Some(output) => {
            if output.is_dir() {
                return Err(kind.planning_error(kind.single_file_output_error(output)));
            }
            output.to_path_buf()
        }
        None => derive_output_path(input, kind.target_extension()),
    };

    Ok(ConversionWorkItem {
        input: input.to_path_buf(),
        output,
        display_name: file_display_name(input),
        ensure_parent_dirs: false,
        total_samples: inspect_total_samples(input)?,
    })
}

fn plan_directory_worklist(
    kind: CommandKind,
    input_root: &Path,
    output_root: Option<&Path>,
    depth: usize,
    inspect_total_samples: fn(&Path) -> Result<u64>,
) -> Result<PlannedWorklist> {
    let output_root = match output_root {
        Some(output_root) => Some(validate_or_create_output_root(kind, output_root)?),
        None => None,
    };

    let mut worklist = collect_directory_work_items(
        kind,
        input_root,
        output_root.as_deref(),
        depth,
        inspect_total_samples,
    )?;
    worklist.sort_by(|left, right| path_sort_key(&left.input).cmp(&path_sort_key(&right.input)));
    let total_samples = worklist.iter().try_fold(0u64, |sum, item| {
        sum.checked_add(item.total_samples).ok_or_else(|| {
            kind.planning_error("total sample count overflowed batch progress accounting")
        })
    })?;

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
) -> Result<Vec<ConversionWorkItem>> {
    let mut walker = WalkDir::new(input_root).follow_links(false).min_depth(1);
    if depth != 0 {
        walker = walker.max_depth(depth);
    }

    let mut worklist = Vec::new();
    for entry in walker {
        let entry =
            entry.map_err(|error| kind.planning_error(kind.traversal_error(input_root, &error)))?;
        if !entry.file_type().is_file() || !has_extension(entry.path(), kind.source_extension()) {
            continue;
        }

        let input = entry.path().to_path_buf();
        let relative = input
            .strip_prefix(input_root)
            .map_err(|_| kind.planning_error("failed to derive relative input path"))?
            .to_path_buf();
        let (output, ensure_parent_dirs) = match output_root {
            Some(output_root) => (
                output_root
                    .join(&relative)
                    .with_extension(kind.target_extension()),
                true,
            ),
            None => (derive_output_path(&input, kind.target_extension()), false),
        };

        worklist.push(ConversionWorkItem {
            total_samples: inspect_total_samples(&input)?,
            input,
            output,
            display_name: relative_display_name(&relative),
            ensure_parent_dirs,
        });
    }

    Ok(worklist)
}

fn inspect_wav_file_total_samples(path: &Path) -> Result<u64> {
    inspect_wav_total_samples(File::open(path)?)
}

fn inspect_flac_file_total_samples(path: &Path) -> Result<u64> {
    inspect_flac_total_samples(File::open(path)?)
}

fn derive_output_path(input: &Path, extension: &str) -> PathBuf {
    input.with_extension(extension)
}

fn ensure_output_parent_dirs(output: &Path) -> Result<()> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
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

fn path_sort_key(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

struct BatchProgressCoordinator<W: Write> {
    renderer: ProgressRenderer<W>,
    total_samples: u64,
    batch_mode: bool,
    completed_samples: u64,
    current_file: Option<CurrentFileProgress>,
}

impl<W: Write> BatchProgressCoordinator<W> {
    fn new(writer: W, interactive: bool, total_samples: u64, batch_mode: bool) -> Self {
        Self {
            renderer: ProgressRenderer::new(writer, interactive),
            total_samples,
            batch_mode,
            completed_samples: 0,
            current_file: None,
        }
    }

    fn begin_file(&mut self, filename: &str, total_samples: u64) {
        self.current_file = Some(CurrentFileProgress {
            filename: filename.to_string(),
            total_samples,
            saw_update: false,
        });
    }

    fn observe(&mut self, progress: ProgressSnapshot) -> std::io::Result<()> {
        let elapsed = self.renderer.elapsed();
        self.observe_with_elapsed(progress, elapsed)
    }

    fn observe_with_elapsed(
        &mut self,
        progress: ProgressSnapshot,
        elapsed: Duration,
    ) -> std::io::Result<()> {
        let (filename, total_samples) = {
            let current = self
                .current_file
                .as_mut()
                .expect("current file must be set");
            current.saw_update = true;
            (current.filename.clone(), current.total_samples)
        };
        let display = self.display_for_snapshot(&filename, total_samples, progress);
        self.renderer.observe_with_elapsed(display, elapsed)
    }

    fn finish_current_file(&mut self) -> std::io::Result<()> {
        let Some(current) = self.current_file.take() else {
            return Ok(());
        };
        if !current.saw_update {
            let elapsed = self.renderer.elapsed();
            let completed = ProgressSnapshot {
                processed_samples: current.total_samples,
                total_samples: current.total_samples,
                completed_frames: 0,
                total_frames: 0,
            };
            self.renderer.observe_with_elapsed(
                self.display_for_snapshot(&current.filename, current.total_samples, completed),
                elapsed,
            )?;
        }
        self.completed_samples = self.completed_samples.saturating_add(current.total_samples);
        Ok(())
    }

    fn finish(&mut self) -> std::io::Result<()> {
        self.renderer.end()
    }

    fn end(&mut self) -> std::io::Result<()> {
        self.renderer.end()
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
        }
    }
}

struct ProgressRenderer<W: Write> {
    writer: W,
    interactive: bool,
    has_drawn: bool,
    started_at: Option<Instant>,
    last_line_width: usize,
    state: ProgressState,
}

impl<W: Write> ProgressRenderer<W> {
    fn new(writer: W, interactive: bool) -> Self {
        Self {
            writer,
            interactive,
            has_drawn: false,
            started_at: None,
            last_line_width: 0,
            state: ProgressState::default(),
        }
    }

    fn elapsed(&mut self) -> Duration {
        let started_at = self.started_at.get_or_insert_with(Instant::now);
        started_at.elapsed()
    }

    fn observe_with_elapsed(
        &mut self,
        display: ProgressDisplay,
        elapsed: Duration,
    ) -> std::io::Result<()> {
        if !self.interactive {
            return Ok(());
        }

        let estimate = self.state.observe(
            display
                .overall
                .processed_samples
                .min(display.overall.total_samples),
            display.overall.total_samples,
            elapsed,
        );
        let line = format_progress_line(&display, &estimate, elapsed);
        let line_width = line.len();
        let padded_width = line_width.max(self.last_line_width);

        self.has_drawn = true;
        self.writer.write_all(b"\r")?;
        self.writer
            .write_all(format!("{line:<padded_width$}").as_bytes())?;
        self.last_line_width = line_width;
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

fn format_progress_line(
    display: &ProgressDisplay,
    estimate: &ProgressEstimate,
    elapsed: Duration,
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

    if let Some(file) = display.file {
        format!(
            "{} | Total {} {:>5.1}% | File {} {:>5.1}% | Elapsed {} | ETA {} | Rate {}",
            display.filename,
            format_progress_bar(progress_ratio(display.overall), BATCH_PROGRESS_BAR_WIDTH),
            progress_ratio(display.overall) * 100.0,
            format_progress_bar(progress_ratio(file), BATCH_PROGRESS_BAR_WIDTH),
            progress_ratio(file) * 100.0,
            elapsed,
            eta,
            rate
        )
    } else {
        format!(
            "{} | {} {:>5.1}% | Elapsed {} | ETA {} | Rate {}",
            display.filename,
            format_progress_bar(progress_ratio(display.overall), SINGLE_PROGRESS_BAR_WIDTH),
            progress_ratio(display.overall) * 100.0,
            elapsed,
            eta,
            rate
        )
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
    use std::time::Duration;

    use super::{
        BatchProgressCoordinator, ProgressDisplay, ProgressEstimate, ProgressRenderer,
        ProgressState, SampleProgress, format_progress_line,
    };
    use flacx::ProgressSnapshot;

    #[test]
    fn progress_renderer_is_silent_when_not_interactive() {
        let mut renderer = ProgressRenderer::new(Vec::new(), false);
        renderer
            .observe_with_elapsed(
                ProgressDisplay {
                    filename: "input.wav".into(),
                    overall: SampleProgress {
                        processed_samples: 50,
                        total_samples: 100,
                    },
                    file: None,
                },
                Duration::from_millis(0),
            )
            .unwrap();
        renderer.end().unwrap();

        assert!(renderer.writer.is_empty());
    }

    #[test]
    fn progress_renderer_writes_elapsed_filename_eta_and_rate() {
        let mut renderer = ProgressRenderer::new(Vec::new(), true);
        renderer
            .observe_with_elapsed(
                ProgressDisplay {
                    filename: "mixdown.wav".into(),
                    overall: SampleProgress {
                        processed_samples: 50,
                        total_samples: 100,
                    },
                    file: None,
                },
                Duration::from_millis(0),
            )
            .unwrap();
        renderer
            .observe_with_elapsed(
                ProgressDisplay {
                    filename: "mixdown.wav".into(),
                    overall: SampleProgress {
                        processed_samples: 100,
                        total_samples: 100,
                    },
                    file: None,
                },
                Duration::from_millis(300),
            )
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
            .observe_with_elapsed(
                ProgressDisplay {
                    filename: "wide-name.wav".into(),
                    overall: SampleProgress {
                        processed_samples: 10,
                        total_samples: 100,
                    },
                    file: None,
                },
                Duration::from_millis(0),
            )
            .unwrap();
        renderer
            .observe_with_elapsed(
                ProgressDisplay {
                    filename: "x.wav".into(),
                    overall: SampleProgress {
                        processed_samples: 100,
                        total_samples: 100,
                    },
                    file: None,
                },
                Duration::from_millis(300),
            )
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
    fn progress_line_contains_batch_bars_elapsed_eta_and_rate() {
        let line = format_progress_line(
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
            },
            &ProgressEstimate {
                eta: Some(Duration::from_secs(3)),
                samples_per_second: Some(12_345.0),
            },
            Duration::from_secs(1),
        );
        assert!(line.contains("album/disc1/song.wav"));
        assert!(line.contains("Total"));
        assert!(line.contains("File"));
        assert!(line.contains("Elapsed 00:01"));
        assert!(line.contains("ETA 00:03"));
        assert!(line.contains("Rate 12.3k/s"));
    }

    #[test]
    fn batch_progress_uses_exact_total_samples_before_completion() {
        let mut progress = BatchProgressCoordinator::new(Vec::new(), true, 300, true);
        progress.begin_file("disc1/first.wav", 100);
        progress
            .observe_with_elapsed(
                ProgressSnapshot {
                    processed_samples: 50,
                    total_samples: 100,
                    completed_frames: 1,
                    total_frames: 2,
                },
                Duration::from_millis(300),
            )
            .unwrap();
        progress.end().unwrap();

        let output = String::from_utf8(progress.renderer.writer).unwrap();
        assert!(output.contains("disc1/first.wav"));
        assert!(output.contains("Total"));
        assert!(output.contains("16.7%"));
        assert!(output.contains("File"));
        assert!(output.contains("50.0%"));
    }
}
