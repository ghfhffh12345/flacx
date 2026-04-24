use std::{
    error::Error,
    fs,
    hint::black_box,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use flacx::{DecodeConfig, EncoderConfig, level::Level};
use flacx_cli::{DecodeCommand, EncodeCommand, decode_command, encode_command};

#[path = "../../flacx/tests/support/mod.rs"]
mod support;

use support::{
    encode_flac_bytes_with_config, extensible_pcm_wav_bytes, large_streaming_decode_flac_bytes,
    pcm_wav_bytes, sample_fixture,
};

const DEFAULT_FIXTURE_FRAMES: usize = 2_048;

fn benchmark_thread_count() -> usize {
    let encode_threads = EncoderConfig::default().threads().max(1);
    let decode_threads = DecodeConfig::default().threads().max(1);
    assert_eq!(
        encode_threads, decode_threads,
        "default encode/decode thread counts diverged"
    );
    encode_threads
}

fn bench_with_temp_output_dir(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    name: &str,
    temp_prefix: &'static str,
    run: impl Fn(&Path) -> Result<(), Box<dyn Error>>,
) {
    group.bench_function(name, |b| {
        b.iter_batched(
            || TempDir::new(temp_prefix).expect("create output dir"),
            |output_dir| {
                run(output_dir.path()).expect("benchmark run");
                black_box(())
            },
            BatchSize::PerIteration,
        );
    });
}

fn criterion_benches(c: &mut Criterion) {
    let corpus = DirectoryParityCorpus::prepare().expect("prepare CLI benchmark corpus");
    let large_decode = LargeDecodeFixture::prepare().expect("prepare large CLI decode fixture");

    let mut group = c.benchmark_group("directory_parity");
    bench_with_temp_output_dir(
        &mut group,
        "encode_directory_parity",
        "flacx-cli-bench-encode-output",
        |output_root| corpus.run_encode_directory_parity(output_root),
    );
    bench_with_temp_output_dir(
        &mut group,
        "decode_directory_parity",
        "flacx-cli-bench-decode-output",
        |output_root| corpus.run_decode_directory_parity(output_root),
    );
    group.finish();

    let mut large_group = c.benchmark_group("streaming_throughput");
    large_group.throughput(Throughput::Bytes(large_decode.input_bytes));
    large_group.sample_size(20);
    bench_with_temp_output_dir(
        &mut large_group,
        "decode_large_streaming_single_file",
        "flacx-cli-bench-large-decode-output",
        |output_root| large_decode.run(output_root),
    );
    large_group.finish();
}

criterion_group!(benches, criterion_benches);
criterion_main!(benches);

struct DirectoryParityCorpus {
    wav_root: TempDir,
    flac_root: TempDir,
    threads: usize,
}

struct LargeDecodeFixture {
    _input_dir: TempDir,
    input_path: PathBuf,
    input_bytes: u64,
    threads: usize,
}

impl LargeDecodeFixture {
    fn prepare() -> Result<Self, Box<dyn Error>> {
        let input_dir = TempDir::new("flacx-cli-bench-large-decode-input")?;
        let input_path = input_dir.path().join("large-input.flac");
        let threads = benchmark_thread_count();
        let flac_bytes = large_streaming_decode_flac_bytes(threads);
        let input_bytes = flac_bytes.len() as u64;
        fs::write(&input_path, flac_bytes)?;
        Ok(Self {
            _input_dir: input_dir,
            input_path,
            input_bytes,
            threads,
        })
    }

    fn run(&self, output_root: &Path) -> Result<(), Box<dyn Error>> {
        let command = DecodeCommand {
            input: self.input_path.clone(),
            output: Some(output_root.join("large-output.wav")),
            depth: 0,
            config: DecodeConfig::default().with_threads(self.threads),
        };
        let mut stderr = Vec::new();
        decode_command(&command, false, &mut stderr)?;
        black_box(stderr);
        Ok(())
    }
}

impl DirectoryParityCorpus {
    fn prepare() -> Result<Self, Box<dyn Error>> {
        let wav_root = TempDir::new("flacx-cli-bench-wav-corpus")?;
        let flac_root = TempDir::new("flacx-cli-bench-flac-corpus")?;
        let threads = benchmark_thread_count();
        let config = EncoderConfig::default()
            .with_level(Level::Level8)
            .with_threads(threads);

        for fixture in benchmark_fixtures() {
            let wav_path = wav_root.path().join(fixture.relative).with_extension("wav");
            if let Some(parent) = wav_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let wav_bytes = fixture.wav_bytes();
            fs::write(&wav_path, &wav_bytes)?;

            let flac_path = flac_root
                .path()
                .join(fixture.relative)
                .with_extension("flac");
            if let Some(parent) = flac_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let flac_bytes = encode_flac_bytes_with_config(config.clone(), &wav_bytes);
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

fn benchmark_fixtures() -> [BenchmarkFixture; 4] {
    [
        BenchmarkFixture::pcm("disc1/track01", 1, DEFAULT_FIXTURE_FRAMES),
        BenchmarkFixture::pcm("disc1/track02", 2, DEFAULT_FIXTURE_FRAMES * 2),
        BenchmarkFixture::extensible("disc2/live/track01", 4, DEFAULT_FIXTURE_FRAMES, 0x0001_2104),
        BenchmarkFixture::extensible(
            "disc2/live/track02",
            6,
            DEFAULT_FIXTURE_FRAMES / 2,
            0x0000_003f,
        ),
    ]
}

#[derive(Clone, Copy)]
struct BenchmarkFixture {
    relative: &'static str,
    channels: u16,
    frames: usize,
    channel_mask: Option<u32>,
}

impl BenchmarkFixture {
    const fn pcm(relative: &'static str, channels: u16, frames: usize) -> Self {
        Self {
            relative,
            channels,
            frames,
            channel_mask: None,
        }
    }

    const fn extensible(
        relative: &'static str,
        channels: u16,
        frames: usize,
        channel_mask: u32,
    ) -> Self {
        Self {
            relative,
            channels,
            frames,
            channel_mask: Some(channel_mask),
        }
    }

    fn wav_bytes(&self) -> Vec<u8> {
        let samples = sample_fixture(self.channels, self.frames);
        match self.channel_mask {
            Some(channel_mask) => {
                extensible_pcm_wav_bytes(16, 16, self.channels, 44_100, channel_mask, &samples)
            }
            None => pcm_wav_bytes(16, self.channels, 44_100, &samples),
        }
    }
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
