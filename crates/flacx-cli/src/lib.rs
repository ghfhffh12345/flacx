//! Command-line support utilities for the `flacx-cli` crate.
//!
//! `flacx-cli` provides the command-line interface for WAV-to-FLAC encoding in
//! this workspace. It stays separate from the publishable `flacx` library
//! crate while reusing the same encode pipeline and workspace version.
//!
//! Progress rendering stays a CLI concern. The library reports real encode
//! progress, while this crate decides when and how to render a live progress
//! bar.
//!
//! # Command shape
//!
//! - `flacx encode <input> <output>`
//! - `--level`
//! - `--threads`
//! - `--block-size`

use std::{
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

use flacx::{EncodeProgress, Encoder, EncoderConfig, Result};

const PROGRESS_BAR_WIDTH: usize = 24;
const ESTIMATE_WARMUP: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeCommand {
    pub input: PathBuf,
    pub output: PathBuf,
    pub config: EncoderConfig,
}

pub fn encode_command(
    command: &EncodeCommand,
    interactive: bool,
    stderr: &mut impl Write,
) -> Result<()> {
    let mut progress = ProgressRenderer::new(stderr, interactive);
    let result = Encoder::new(command.config).encode_file_with_progress(
        &command.input,
        &command.output,
        |update| {
            progress.observe(update)?;
            Ok(())
        },
    );

    match result {
        Ok(_) => {
            progress.finish()?;
            Ok(())
        }
        Err(error) => {
            let _ = progress.end();
            Err(error)
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

    fn observe(&mut self, progress: EncodeProgress) -> std::io::Result<()> {
        let elapsed = {
            let started_at = self.started_at.get_or_insert_with(Instant::now);
            started_at.elapsed()
        };
        self.observe_with_elapsed(progress, elapsed)
    }

    fn observe_with_elapsed(
        &mut self,
        progress: EncodeProgress,
        elapsed: Duration,
    ) -> std::io::Result<()> {
        if !self.interactive || progress.total_samples == 0 {
            return Ok(());
        }

        let estimate = self.state.observe(progress, elapsed);
        let line = format_progress_line(progress, &estimate);
        let line_width = line.len();
        let padded_width = line_width.max(self.last_line_width);

        self.has_drawn = true;
        self.writer.write_all(b"\r")?;
        self.writer
            .write_all(format!("{line:<padded_width$}").as_bytes())?;
        self.last_line_width = line_width;
        self.writer.flush()
    }

    fn finish(&mut self) -> std::io::Result<()> {
        self.end()
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
    fn observe(&mut self, progress: EncodeProgress, elapsed: Duration) -> ProgressEstimate {
        let processed = progress.processed_samples.min(progress.total_samples);
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

        let remaining_samples = progress.total_samples.saturating_sub(processed);
        let eta_seconds = remaining_samples as f64 / samples_per_second;

        ProgressEstimate {
            eta: Some(Duration::from_secs_f64(eta_seconds.max(0.0))),
            samples_per_second: Some(samples_per_second),
        }
    }
}

fn format_progress_line(progress: EncodeProgress, estimate: &ProgressEstimate) -> String {
    let processed = progress.processed_samples.min(progress.total_samples);
    let ratio = if progress.total_samples == 0 {
        1.0
    } else {
        processed as f64 / progress.total_samples as f64
    };
    let percent = ratio * 100.0;
    let eta = estimate
        .eta
        .map(format_eta)
        .unwrap_or_else(|| "--:--".to_string());
    let rate = estimate
        .samples_per_second
        .map(format_speed)
        .unwrap_or_else(|| "warmup".to_string());

    format!(
        "{} {:>5.1}% | ETA {} | Rate {}",
        format_progress_bar(ratio),
        percent,
        eta,
        rate
    )
}

fn format_progress_bar(ratio: f64) -> String {
    let clamped_ratio = ratio.clamp(0.0, 1.0);
    let filled = (clamped_ratio * PROGRESS_BAR_WIDTH as f64).floor() as usize;

    if filled >= PROGRESS_BAR_WIDTH {
        return format!("[{}]", "=".repeat(PROGRESS_BAR_WIDTH));
    }

    let prefix = "=".repeat(filled);
    let suffix = "-".repeat(PROGRESS_BAR_WIDTH.saturating_sub(filled + 1));
    format!("[{}>{}]", prefix, suffix)
}

fn format_eta(duration: Duration) -> String {
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

    use super::{ProgressEstimate, ProgressRenderer, ProgressState, format_progress_line};
    use flacx::EncodeProgress;

    #[test]
    fn progress_renderer_is_silent_when_not_interactive() {
        let mut renderer = ProgressRenderer::new(Vec::new(), false);
        renderer
            .observe(EncodeProgress {
                processed_samples: 50,
                total_samples: 100,
                completed_frames: 1,
                total_frames: 2,
            })
            .unwrap();
        renderer.finish().unwrap();

        assert!(renderer.writer.is_empty());
    }

    #[test]
    fn progress_renderer_writes_live_bar_when_interactive() {
        let mut renderer = ProgressRenderer::new(Vec::new(), true);
        renderer
            .observe_with_elapsed(
                EncodeProgress {
                    processed_samples: 50,
                    total_samples: 100,
                    completed_frames: 1,
                    total_frames: 2,
                },
                Duration::from_millis(0),
            )
            .unwrap();
        renderer
            .observe_with_elapsed(
                EncodeProgress {
                    processed_samples: 100,
                    total_samples: 100,
                    completed_frames: 2,
                    total_frames: 2,
                },
                Duration::from_millis(300),
            )
            .unwrap();
        renderer.finish().unwrap();

        let output = String::from_utf8(renderer.writer).unwrap();
        assert!(output.contains('\r'));
        assert!(output.contains("100.0%"));
        assert!(output.contains("ETA 00:00"));
        assert!(output.contains("Rate 333/s"));
        assert!(output.ends_with('\n'));
        assert!(!output.ends_with("\n\n"));
    }

    #[test]
    fn progress_renderer_waits_for_two_advancing_updates_and_elapsed_time() {
        let mut state = ProgressState::default();
        let progress = EncodeProgress {
            processed_samples: 50,
            total_samples: 200,
            completed_frames: 1,
            total_frames: 4,
        };

        let first = state.observe(progress, Duration::from_millis(0));
        assert_eq!(first, ProgressEstimate::warming_up());

        let no_advance = state.observe(progress, Duration::from_millis(400));
        assert_eq!(no_advance, ProgressEstimate::warming_up());

        let second_advance = state.observe(
            EncodeProgress {
                processed_samples: 100,
                total_samples: 200,
                completed_frames: 2,
                total_frames: 4,
            },
            Duration::from_millis(200),
        );
        assert_eq!(second_advance, ProgressEstimate::warming_up());

        let stabilized = state.observe(
            EncodeProgress {
                processed_samples: 150,
                total_samples: 200,
                completed_frames: 3,
                total_frames: 4,
            },
            Duration::from_millis(300),
        );
        assert!(stabilized.eta.is_some());
        assert!(stabilized.samples_per_second.is_some());
    }

    #[test]
    fn progress_renderer_ignores_zero_progress_before_warmup_starts() {
        let mut state = ProgressState::default();

        let initial = state.observe(
            EncodeProgress {
                processed_samples: 0,
                total_samples: 200,
                completed_frames: 0,
                total_frames: 4,
            },
            Duration::from_millis(500),
        );
        assert_eq!(initial, ProgressEstimate::warming_up());

        let first_advance = state.observe(
            EncodeProgress {
                processed_samples: 50,
                total_samples: 200,
                completed_frames: 1,
                total_frames: 4,
            },
            Duration::from_millis(600),
        );
        assert_eq!(first_advance, ProgressEstimate::warming_up());

        let second_advance = state.observe(
            EncodeProgress {
                processed_samples: 100,
                total_samples: 200,
                completed_frames: 2,
                total_frames: 4,
            },
            Duration::from_millis(700),
        );
        assert_eq!(second_advance, ProgressEstimate::warming_up());

        let stabilized = state.observe(
            EncodeProgress {
                processed_samples: 150,
                total_samples: 200,
                completed_frames: 3,
                total_frames: 4,
            },
            Duration::from_millis(900),
        );
        assert!(stabilized.eta.is_some());
    }

    #[test]
    fn progress_renderer_overwrites_stale_characters_when_line_shrinks() {
        let mut renderer = ProgressRenderer::new(Vec::new(), true);
        renderer
            .observe_with_elapsed(
                EncodeProgress {
                    processed_samples: 10,
                    total_samples: 100,
                    completed_frames: 1,
                    total_frames: 10,
                },
                Duration::from_millis(0),
            )
            .unwrap();
        renderer
            .observe_with_elapsed(
                EncodeProgress {
                    processed_samples: 100,
                    total_samples: 100,
                    completed_frames: 10,
                    total_frames: 10,
                },
                Duration::from_millis(300),
            )
            .unwrap();
        renderer.finish().unwrap();

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
    fn progress_line_contains_bar_eta_and_rate() {
        let line = format_progress_line(
            EncodeProgress {
                processed_samples: 50,
                total_samples: 100,
                completed_frames: 1,
                total_frames: 2,
            },
            &ProgressEstimate {
                eta: Some(Duration::from_secs(3)),
                samples_per_second: Some(12_345.0),
            },
        );
        assert!(line.contains('['));
        assert!(line.contains("50.0%"));
        assert!(line.contains("ETA 00:03"));
        assert!(line.contains("Rate 12.3k/s"));
    }
}
