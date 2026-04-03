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

use std::{io::Write, path::PathBuf};

use flacx::{EncodeOptions, EncodeProgress, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeCommand {
    pub input: PathBuf,
    pub output: PathBuf,
    pub options: EncodeOptions,
}

pub fn encode_command(
    command: &EncodeCommand,
    interactive: bool,
    stderr: &mut impl Write,
) -> Result<()> {
    let mut progress = ProgressRenderer::new(stderr, interactive);
    let result = flacx::FlacEncoder::new(command.options).encode_file_with_progress(
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
}

impl<W: Write> ProgressRenderer<W> {
    fn new(writer: W, interactive: bool) -> Self {
        Self {
            writer,
            interactive,
            has_drawn: false,
        }
    }

    fn observe(&mut self, progress: EncodeProgress) -> std::io::Result<()> {
        if !self.interactive || progress.total_samples == 0 {
            return Ok(());
        }

        self.has_drawn = true;
        self.writer.write_all(b"\r")?;
        self.writer
            .write_all(format_progress_line(progress).as_bytes())?;
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

fn format_progress_line(progress: EncodeProgress) -> String {
    let width = 24usize;
    let processed = progress.processed_samples.min(progress.total_samples);
    let ratio = if progress.total_samples == 0 {
        1.0
    } else {
        processed as f64 / progress.total_samples as f64
    };
    let filled = ((ratio * width as f64).round() as usize).min(width);
    let empty = width.saturating_sub(filled);
    let percent = ratio * 100.0;

    format!(
        "[{}{}] {:>6.2}% ({}/{})",
        "#".repeat(filled),
        "-".repeat(empty),
        percent,
        processed,
        progress.total_samples
    )
}

#[cfg(test)]
mod tests {
    use super::{ProgressRenderer, format_progress_line};
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
            .observe(EncodeProgress {
                processed_samples: 50,
                total_samples: 100,
                completed_frames: 1,
                total_frames: 2,
            })
            .unwrap();
        renderer
            .observe(EncodeProgress {
                processed_samples: 100,
                total_samples: 100,
                completed_frames: 2,
                total_frames: 2,
            })
            .unwrap();
        renderer.finish().unwrap();

        let output = String::from_utf8(renderer.writer).unwrap();
        assert!(output.contains('\r'));
        assert!(output.contains("50.00%"));
        assert!(output.contains("100.00%"));
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn progress_line_contains_bar_and_counts() {
        let line = format_progress_line(EncodeProgress {
            processed_samples: 12,
            total_samples: 48,
            completed_frames: 1,
            total_frames: 4,
        });
        assert!(line.contains('['));
        assert!(line.contains("25.00%"));
        assert!(line.contains("(12/48)"));
    }
}
