use std::{
    env,
    fs::{self, File},
    path::{Path, PathBuf},
    time::Duration,
};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use flacx::{
    DecodeConfig, EncoderConfig, FlacReaderOptions, PcmReaderOptions,
    read_flac_reader_with_options, read_pcm_reader_with_options,
};

#[path = "../tests/support/mod.rs"]
mod support;

use support::{pcm_wav_bytes, sample_fixture};

const CORPUS_FIXTURES: [CorpusFixture; 3] = [
    CorpusFixture {
        file_name: "mono-compact.wav",
        channels: 1,
        frames: 4_096,
    },
    CorpusFixture {
        file_name: "stereo-medium.wav",
        channels: 2,
        frames: 8_192,
    },
    CorpusFixture {
        file_name: "stereo-large.wav",
        channels: 2,
        frames: 16_384,
    },
];

fn encode_corpus_throughput(c: &mut Criterion) {
    let corpus = BenchmarkCorpus::generate().expect("benchmark corpus");
    let total_bytes = corpus.total_input_bytes();
    let threads = shared_thread_count();

    let mut group = c.benchmark_group("flacx throughput");
    group.throughput(Throughput::Bytes(total_bytes));
    group.measurement_time(Duration::from_secs(5));
    group.bench_function("encode_corpus_throughput", |b| {
        b.iter(|| run_encode_corpus(&corpus, threads).expect("encode corpus"))
    });
    group.finish();
}

fn decode_corpus_throughput(c: &mut Criterion) {
    let corpus = BenchmarkCorpus::generate().expect("benchmark corpus");
    let total_bytes = corpus.total_input_bytes();
    let threads = shared_thread_count();

    let mut group = c.benchmark_group("flacx throughput");
    group.throughput(Throughput::Bytes(total_bytes));
    group.measurement_time(Duration::from_secs(5));
    group.bench_function("decode_corpus_throughput", |b| {
        b.iter(|| run_decode_corpus(&corpus, threads).expect("decode corpus"))
    });
    group.finish();
}

criterion_group!(benches, encode_corpus_throughput, decode_corpus_throughput);
criterion_main!(benches);

struct CorpusFixture {
    file_name: &'static str,
    channels: u16,
    frames: usize,
}

struct BenchmarkCorpus {
    wav_root: PathBuf,
    wav_inputs: Vec<PathBuf>,
    flac_root: PathBuf,
    flac_inputs: Vec<PathBuf>,
    total_input_bytes: u64,
}

impl BenchmarkCorpus {
    fn generate() -> Result<Self, Box<dyn std::error::Error>> {
        let wav_root = fresh_temp_dir("flacx-library-bench-wav")?;
        let flac_root = fresh_temp_dir("flacx-library-bench-flac")?;
        let threads = shared_thread_count();
        let config = EncoderConfig::default().with_threads(threads);

        let mut wav_inputs = Vec::with_capacity(CORPUS_FIXTURES.len());
        let mut flac_inputs = Vec::with_capacity(CORPUS_FIXTURES.len());
        let mut total_input_bytes = 0u64;

        for fixture in CORPUS_FIXTURES {
            let wav_path = wav_root.join(fixture.file_name);
            if let Some(parent) = wav_path.parent() {
                fs::create_dir_all(parent)?;
            }

            let samples = sample_fixture(fixture.channels, fixture.frames);
            let wav_bytes = pcm_wav_bytes(16, fixture.channels, 44_100, &samples);
            total_input_bytes += wav_bytes.len() as u64;
            fs::write(&wav_path, wav_bytes)?;
            wav_inputs.push(wav_path.clone());

            let flac_path = flac_root.join(fixture.file_name).with_extension("flac");
            if let Some(parent) = flac_path.parent() {
                fs::create_dir_all(parent)?;
            }
            encode_fixture_file(&config, &wav_path, &flac_path)?;
            flac_inputs.push(flac_path);
        }

        Ok(Self {
            wav_root,
            wav_inputs,
            flac_root,
            flac_inputs,
            total_input_bytes,
        })
    }

    fn total_input_bytes(&self) -> u64 {
        self.total_input_bytes
    }
}

impl Drop for BenchmarkCorpus {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.wav_root);
        let _ = fs::remove_dir_all(&self.flac_root);
    }
}

fn run_encode_corpus(
    corpus: &BenchmarkCorpus,
    threads: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = EncoderConfig::default().with_threads(threads);
    with_temp_dir("flacx-library-bench-encode", |output_root| {
        for input in &corpus.wav_inputs {
            let output = output_root
                .join(input.file_stem().expect("fixture file stem"))
                .with_extension("flac");
            encode_fixture_file(&config, input, &output)?;
        }
        Ok(())
    })
}

fn run_decode_corpus(
    corpus: &BenchmarkCorpus,
    threads: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = DecodeConfig::default().with_threads(threads);
    with_temp_dir("flacx-library-bench-decode", |output_root| {
        for input in &corpus.flac_inputs {
            let output = output_root
                .join(input.file_stem().expect("fixture file stem"))
                .with_extension("wav");
            let reader = read_flac_reader_with_options(
                File::open(input)?,
                FlacReaderOptions {
                    strict_seektable_validation: config.strict_seektable_validation,
                    strict_channel_mask_provenance: config.strict_channel_mask_provenance,
                },
            )?;
            let metadata = reader.metadata().clone();
            let stream = reader.into_pcm_stream();
            let mut decoder = config.clone().into_decoder(File::create(&output)?);
            decoder.set_metadata(metadata);
            decoder.decode(stream)?;
        }
        Ok(())
    })
}

fn with_temp_dir<T>(
    label: &str,
    f: impl FnOnce(&Path) -> Result<T, Box<dyn std::error::Error>>,
) -> Result<T, Box<dyn std::error::Error>> {
    let path = fresh_temp_dir(label)?;
    let result = f(&path);
    let _ = fs::remove_dir_all(&path);
    result
}

fn fresh_temp_dir(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let path = env::temp_dir().join(format!("flacx-{label}-{}-{unique}", std::process::id()));
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn encode_fixture_file(
    config: &EncoderConfig,
    input: &Path,
    output: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let reader = read_pcm_reader_with_options(
        File::open(input)?,
        PcmReaderOptions {
            capture_fxmd: config.capture_fxmd,
            strict_fxmd_validation: config.strict_fxmd_validation,
        },
    )?;
    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let mut encoder = config.clone().into_encoder(File::create(output)?);
    encoder.set_metadata(metadata);
    encoder.encode(stream)?;
    Ok(())
}

fn shared_thread_count() -> usize {
    let encode_threads = EncoderConfig::default().threads.max(1);
    let decode_threads = DecodeConfig::default().threads.max(1);
    assert_eq!(
        encode_threads, decode_threads,
        "default encode/decode thread counts diverged"
    );
    encode_threads
}
