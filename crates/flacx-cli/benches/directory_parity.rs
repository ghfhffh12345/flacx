use std::{
    error::Error,
    fs,
    hint::black_box,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use flacx::{DecodeConfig, Encoder, EncoderConfig, level::Level};
use flacx_cli::{DecodeCommand, EncodeCommand, decode_command, encode_command};

#[path = "../../flacx/tests/support/mod.rs"]
mod support;

use support::{pcm_wav_bytes, sample_fixture};

const DEFAULT_FIXTURE_FRAMES: usize = 2_048;

fn benchmark_thread_count() -> usize {
    let encode_threads = EncoderConfig::default().threads.max(1);
    let decode_threads = DecodeConfig::default().threads.max(1);
    assert_eq!(
        encode_threads, decode_threads,
        "default encode/decode thread counts diverged"
    );
    encode_threads
}

fn criterion_benches(c: &mut Criterion) {
    let corpus = DirectoryParityCorpus::prepare().expect("prepare CLI benchmark corpus");

    let mut group = c.benchmark_group("directory_parity");
    group.bench_function("encode_directory_parity", |b| {
        b.iter_batched(
            || TempDir::new("flacx-cli-bench-encode-output").expect("create encode output dir"),
            |output_dir| {
                corpus
                    .run_encode_directory_parity(output_dir.path())
                    .expect("encode benchmark run");
                black_box(())
            },
            BatchSize::PerIteration,
        );
    });
    group.bench_function("decode_directory_parity", |b| {
        b.iter_batched(
            || TempDir::new("flacx-cli-bench-decode-output").expect("create decode output dir"),
            |output_dir| {
                corpus
                    .run_decode_directory_parity(output_dir.path())
                    .expect("decode benchmark run");
                black_box(())
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

criterion_group!(benches, criterion_benches);
criterion_main!(benches);

struct DirectoryParityCorpus {
    wav_root: TempDir,
    flac_root: TempDir,
    threads: usize,
}

impl DirectoryParityCorpus {
    fn prepare() -> Result<Self, Box<dyn Error>> {
        let wav_root = TempDir::new("flacx-cli-bench-wav-corpus")?;
        let flac_root = TempDir::new("flacx-cli-bench-flac-corpus")?;
        let threads = benchmark_thread_count();
        let encoder = Encoder::new(
            EncoderConfig::default()
                .with_level(Level::Level8)
                .with_threads(threads),
        );

        for (relative, channels, frames) in benchmark_fixtures() {
            let wav_path = wav_root.path().join(relative).with_extension("wav");
            if let Some(parent) = wav_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let wav_bytes = pcm_wav_bytes(16, channels, 44_100, &sample_fixture(channels, frames));
            fs::write(&wav_path, &wav_bytes)?;

            let flac_path = flac_root.path().join(relative).with_extension("flac");
            if let Some(parent) = flac_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let flac_bytes = encoder.encode_bytes(&wav_bytes)?;
            fs::write(&flac_path, &flac_bytes)?;
        }

        Ok(Self {
            wav_root,
            flac_root,
            threads,
        })
    }

    fn run_encode_directory_parity(&self, output_root: &Path) -> Result<(), Box<dyn Error>> {
        let command = EncodeCommand {
            input: self.wav_root.path().to_path_buf(),
            output: Some(output_root.to_path_buf()),
            depth: 0,
            config: EncoderConfig::default()
                .with_level(Level::Level8)
                .with_threads(self.threads),
            raw_descriptor: None,
        };
        let mut stderr = Vec::new();
        encode_command(&command, false, &mut stderr)?;
        black_box(stderr);
        Ok(())
    }

    fn run_decode_directory_parity(&self, output_root: &Path) -> Result<(), Box<dyn Error>> {
        let command = DecodeCommand {
            input: self.flac_root.path().to_path_buf(),
            output: Some(output_root.to_path_buf()),
            depth: 0,
            config: DecodeConfig::default().with_threads(self.threads),
        };
        let mut stderr = Vec::new();
        decode_command(&command, false, &mut stderr)?;
        black_box(stderr);
        Ok(())
    }
}

fn benchmark_fixtures() -> [(&'static str, u16, usize); 4] {
    [
        ("disc1/track01", 1, DEFAULT_FIXTURE_FRAMES),
        ("disc1/track02", 2, DEFAULT_FIXTURE_FRAMES * 2),
        ("disc2/live/track01", 3, DEFAULT_FIXTURE_FRAMES),
        ("disc2/live/track02", 6, DEFAULT_FIXTURE_FRAMES / 2),
    ]
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = std::env::temp_dir().join(format!(
            "flacx-cli-bench-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
