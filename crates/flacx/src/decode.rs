use std::{
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{
    config::{DecodeBuilder, DecodeConfig},
    error::{Error, Result},
    progress::{NoProgress, ProgressSink},
    read::read_flac_with_config,
    stream_info::StreamInfo,
    wav_output::write_wav,
};

#[cfg(feature = "progress")]
use crate::progress::CallbackProgress;

static TEMP_OUTPUT_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeSummary {
    pub frame_count: usize,
    pub total_samples: u64,
    pub block_size: u16,
    pub min_frame_size: u32,
    pub max_frame_size: u32,
    pub min_block_size: u16,
    pub max_block_size: u16,
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
}

/// Primary library façade for FLAC-to-WAV conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decoder {
    config: DecodeConfig,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new(DecodeConfig::default())
    }
}

impl Decoder {
    #[must_use]
    pub fn builder() -> DecodeBuilder {
        DecodeConfig::builder()
    }

    #[must_use]
    pub fn new(config: DecodeConfig) -> Self {
        Self { config }
    }

    #[must_use]
    pub fn config(&self) -> DecodeConfig {
        self.config
    }

    #[must_use]
    pub fn with_threads(self, threads: usize) -> Self {
        Self::new(self.config.with_threads(threads))
    }

    pub fn decode<R, W>(&self, input: R, output: W) -> Result<DecodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
    {
        let mut progress = NoProgress;
        self.decode_into(input, output, &mut progress)
    }

    #[cfg(feature = "progress")]
    pub fn decode_with_progress<R, W, F>(
        &self,
        input: R,
        output: W,
        mut on_progress: F,
    ) -> Result<DecodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
        F: FnMut(crate::progress::ProgressSnapshot) -> Result<()>,
    {
        let mut progress = CallbackProgress::new(&mut on_progress);
        self.decode_into(input, output, &mut progress)
    }

    pub fn decode_file<P, Q>(&self, input_path: P, output_path: Q) -> Result<DecodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
    {
        let mut progress = NoProgress;
        self.decode_file_with_sink(input_path, output_path, &mut progress)
    }

    #[cfg(feature = "progress")]
    pub fn decode_file_with_progress<P, Q, F>(
        &self,
        input_path: P,
        output_path: Q,
        mut on_progress: F,
    ) -> Result<DecodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
        F: FnMut(crate::progress::ProgressSnapshot) -> Result<()>,
    {
        let mut progress = CallbackProgress::new(&mut on_progress);
        self.decode_file_with_sink(input_path, output_path, &mut progress)
    }

    pub fn decode_bytes(&self, input: &[u8]) -> Result<Vec<u8>> {
        let mut output = Cursor::new(Vec::new());
        self.decode(Cursor::new(input), &mut output)?;
        Ok(output.into_inner())
    }

    fn decode_into<R, W, P>(
        &self,
        input: R,
        mut output: W,
        progress: &mut P,
    ) -> Result<DecodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
        P: ProgressSink,
    {
        let (wav, stream_info, frame_count) = read_flac_with_config(input, self.config, progress)?;
        write_wav(&mut output, wav.spec, &wav.samples)?;
        output.flush()?;
        Ok(summary_from_stream_info(stream_info, frame_count))
    }

    fn decode_file_with_sink<P, Q, R>(
        &self,
        input_path: P,
        output_path: Q,
        progress: &mut R,
    ) -> Result<DecodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
        R: ProgressSink,
    {
        let input_path = input_path.as_ref();
        let output_path = output_path.as_ref();
        let (temp_path, temp_file) = open_temp_output(output_path)?;

        let result = (|| {
            let input = File::open(input_path)?;
            self.decode_into(input, temp_file, progress)
        })();
        match result {
            Ok(summary) => {
                if let Err(error) = commit_temp_output(&temp_path, output_path) {
                    let _ = fs::remove_file(&temp_path);
                    return Err(error);
                }
                Ok(summary)
            }
            Err(error) => {
                let _ = fs::remove_file(&temp_path);
                Err(error)
            }
        }
    }
}

pub fn decode_file<P, Q>(input_path: P, output_path: Q) -> Result<DecodeSummary>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    Decoder::default().decode_file(input_path, output_path)
}

pub fn decode_bytes(input: &[u8]) -> Result<Vec<u8>> {
    Decoder::default().decode_bytes(input)
}

fn open_temp_output(output_path: &Path) -> Result<(PathBuf, File)> {
    let parent = output_path.parent().unwrap_or_else(|| Path::new("."));
    let pid = std::process::id();

    for _ in 0..1_024 {
        let suffix = TEMP_OUTPUT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_name = format!(".flacx-{pid}-{suffix}.tmp");
        let temp_path = parent.join(temp_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }

    Err(Error::Thread(
        "failed to allocate a temporary output path".into(),
    ))
}

fn commit_temp_output(temp_path: &Path, output_path: &Path) -> Result<()> {
    match fs::rename(temp_path, output_path) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            let _ = fs::remove_file(output_path);
            fs::rename(temp_path, output_path)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn summary_from_stream_info(stream_info: StreamInfo, frame_count: usize) -> DecodeSummary {
    DecodeSummary {
        frame_count,
        total_samples: stream_info.total_samples,
        block_size: stream_info.max_block_size,
        min_frame_size: stream_info.min_frame_size,
        max_frame_size: stream_info.max_frame_size,
        min_block_size: stream_info.min_block_size,
        max_block_size: stream_info.max_block_size,
        sample_rate: stream_info.sample_rate,
        channels: stream_info.channels,
        bits_per_sample: stream_info.bits_per_sample,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::Decoder;

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn unique_path(extension: &str) -> std::path::PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "flacx-decode-test-{}-{id}.{extension}",
            std::process::id()
        ))
    }

    #[test]
    fn decode_file_cleans_up_temp_output_on_failure() {
        let input_path = unique_path("flac");
        let output_path = unique_path("wav");
        fs::write(&input_path, b"not a flac file").unwrap();

        let result = Decoder::default().decode_file(&input_path, &output_path);
        assert!(result.is_err());
        assert!(!output_path.exists());

        let _ = fs::remove_file(input_path);
        let _ = fs::remove_file(output_path);
    }
}
