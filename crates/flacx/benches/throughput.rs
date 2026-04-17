use std::{
    env,
    fs::{self, File},
    io::{self, Cursor},
    path::{Path, PathBuf},
    time::Duration,
};

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use flacx::{
    builtin, level::Level, read_flac_reader_with_options, read_pcm_reader_with_options,
    DecodeConfig, EncoderConfig, FlacReaderOptions, FlacRecompressSource, PcmReaderOptions,
    RecompressConfig, RecompressMode,
};

#[path = "../tests/support/mod.rs"]
mod support;

use support::{cue_chunk, info_list_chunk, pcm_wav_bytes, sample_fixture, wav_with_chunks};

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

fn recompress_corpus_throughput(c: &mut Criterion) {
    let corpus = BenchmarkCorpus::generate().expect("benchmark corpus");
    let total_bytes = corpus.total_input_bytes();
    let threads = shared_thread_count();

    let mut group = c.benchmark_group("flacx throughput");
    group.throughput(Throughput::Bytes(total_bytes));
    group.measurement_time(Duration::from_secs(5));
    group.bench_function("recompress_corpus_throughput", |b| {
        b.iter(|| run_recompress_corpus(&corpus, threads).expect("recompress corpus"))
    });
    group.finish();
}

fn builtin_bytes_encode(c: &mut Criterion) {
    let corpus = BenchmarkCorpus::generate().expect("benchmark corpus");
    let mut group = c.benchmark_group("flacx throughput");
    group.throughput(Throughput::Bytes(
        corpus.representative_wav_bytes.len() as u64
    ));
    group.measurement_time(Duration::from_secs(5));
    group.bench_function("builtin_bytes_encode", |b| {
        b.iter(|| builtin::encode_bytes(&corpus.representative_wav_bytes).expect("builtin encode"))
    });
    group.finish();
}

fn builtin_bytes_decode(c: &mut Criterion) {
    let corpus = BenchmarkCorpus::generate().expect("benchmark corpus");
    let mut group = c.benchmark_group("flacx throughput");
    group.throughput(Throughput::Bytes(
        corpus.representative_flac_bytes.len() as u64
    ));
    group.measurement_time(Duration::from_secs(5));
    group.bench_function("builtin_bytes_decode", |b| {
        b.iter(|| builtin::decode_bytes(&corpus.representative_flac_bytes).expect("builtin decode"))
    });
    group.finish();
}

fn builtin_bytes_recompress(c: &mut Criterion) {
    let corpus = BenchmarkCorpus::generate().expect("benchmark corpus");
    let mut group = c.benchmark_group("flacx throughput");
    group.throughput(Throughput::Bytes(
        corpus.representative_flac_bytes.len() as u64
    ));
    group.measurement_time(Duration::from_secs(5));
    group.bench_function("builtin_bytes_recompress", |b| {
        b.iter(|| {
            builtin::recompress_bytes(&corpus.representative_flac_bytes)
                .expect("builtin recompress")
        })
    });
    group.finish();
}

fn metadata_write_path(c: &mut Criterion) {
    let corpus = BenchmarkCorpus::generate().expect("benchmark corpus");
    let mut group = c.benchmark_group("flacx throughput");
    group.throughput(Throughput::Bytes(corpus.metadata_flac_bytes.len() as u64));
    group.measurement_time(Duration::from_secs(5));
    group.bench_function("metadata_write_path", |b| {
        b.iter(|| builtin::decode_bytes(&corpus.metadata_flac_bytes).expect("metadata write path"))
    });
    group.finish();
}

fn decode_frame_materialization(c: &mut Criterion) {
    let corpus = BenchmarkCorpus::generate().expect("benchmark corpus");
    let mut group = c.benchmark_group("flacx throughput");
    group.throughput(Throughput::Bytes(
        corpus.decode_materialization_flac_bytes.len() as u64,
    ));
    group.measurement_time(Duration::from_secs(5));
    group.bench_function("decode_frame_materialization", |b| {
        b.iter(|| {
            builtin::decode_bytes(&corpus.decode_materialization_flac_bytes)
                .expect("decode frame materialization")
        })
    });
    group.finish();
}

fn test_wavs_roundtrip_throughput(c: &mut Criterion) {
    let corpus = BenchmarkCorpus::generate().expect("benchmark corpus");
    let mut group = c.benchmark_group("flacx throughput");
    group.throughput(Throughput::Bytes(
        corpus.representative_test_wav.wav_bytes.len() as u64,
    ));
    group.measurement_time(Duration::from_secs(3));
    group.sample_size(10);
    group.bench_function("test_wavs_roundtrip_throughput", |b| {
        b.iter(|| {
            run_test_wav_roundtrip(&corpus.representative_test_wav).expect("test-wavs roundtrip")
        })
    });
    group.finish();
}

fn test_wavs_level8_threads8_encode_throughput(c: &mut Criterion) {
    let corpus = BenchmarkCorpus::generate().expect("benchmark corpus");
    let mut group = c.benchmark_group("flacx throughput");
    group.throughput(Throughput::Bytes(corpus.test_wav_total_input_bytes()));
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(10);
    group.bench_function("test_wavs_level8_threads8_encode_throughput", |b| {
        b.iter(|| {
            run_test_wav_level8_encode(&corpus.test_wav_inputs).expect("test-wavs level-8 encode")
        })
    });
    group.finish();
}

criterion_group!(
    benches,
    encode_corpus_throughput,
    decode_corpus_throughput,
    recompress_corpus_throughput,
    builtin_bytes_encode,
    builtin_bytes_decode,
    builtin_bytes_recompress,
    metadata_write_path,
    decode_frame_materialization,
    test_wavs_roundtrip_throughput,
    test_wavs_level8_threads8_encode_throughput
);
criterion_main!(benches);

struct CorpusFixture {
    file_name: &'static str,
    channels: u16,
    frames: usize,
}

#[derive(Clone)]
struct TestWavFixture {
    _file_name: String,
    wav_bytes: Vec<u8>,
}

struct BenchmarkCorpus {
    wav_root: PathBuf,
    wav_inputs: Vec<PathBuf>,
    flac_root: PathBuf,
    flac_inputs: Vec<PathBuf>,
    total_input_bytes: u64,
    representative_wav_bytes: Vec<u8>,
    representative_flac_bytes: Vec<u8>,
    metadata_flac_bytes: Vec<u8>,
    decode_materialization_flac_bytes: Vec<u8>,
    test_wav_inputs: Vec<TestWavFixture>,
    test_wav_total_input_bytes: u64,
    representative_test_wav: TestWavFixture,
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
            fs::write(&wav_path, &wav_bytes)?;
            wav_inputs.push(wav_path.clone());

            let flac_path = flac_root.join(fixture.file_name).with_extension("flac");
            if let Some(parent) = flac_path.parent() {
                fs::create_dir_all(parent)?;
            }
            encode_fixture_file(&config, &wav_path, &flac_path)?;
            flac_inputs.push(flac_path);
        }

        let representative_wav_bytes = fs::read(&wav_inputs[1])?;
        let representative_flac_bytes = fs::read(&flac_inputs[1])?;
        let metadata_flac_bytes = encode_fixture_bytes(&config, &metadata_wav_fixture())?;
        let decode_materialization_flac_bytes = encode_fixture_bytes(
            &EncoderConfig::default()
                .with_threads(threads)
                .with_block_schedule(vec![576, 1_152, 576, 2_304, 4_096, 576, 1_152]),
            &pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 10_432)),
        )?;
        let (test_wav_inputs, _) = load_test_wav_fixtures()?;
        let representative_test_wav = test_wav_inputs
            .first()
            .expect("test-wavs corpus fixture")
            .clone();
        let test_wav_total_input_bytes = test_wav_inputs
            .iter()
            .map(|fixture| fixture.wav_bytes.len() as u64)
            .sum();

        Ok(Self {
            wav_root,
            wav_inputs,
            flac_root,
            flac_inputs,
            total_input_bytes,
            representative_wav_bytes,
            representative_flac_bytes,
            metadata_flac_bytes,
            decode_materialization_flac_bytes,
            test_wav_inputs,
            test_wav_total_input_bytes,
            representative_test_wav,
        })
    }

    fn total_input_bytes(&self) -> u64 {
        self.total_input_bytes
    }

    fn test_wav_total_input_bytes(&self) -> u64 {
        self.test_wav_total_input_bytes
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

fn run_recompress_corpus(
    corpus: &BenchmarkCorpus,
    threads: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = RecompressConfig::default().with_threads(threads);
    with_temp_dir("flacx-library-bench-recompress", |output_root| {
        for input in &corpus.flac_inputs {
            let output = output_root
                .join(input.file_stem().expect("fixture file stem"))
                .with_extension("flac");
            let reader = read_flac_reader_with_options(
                File::open(input)?,
                recompress_reader_options(config),
            )?;
            let source = FlacRecompressSource::from_reader(reader);
            let mut recompressor = config.into_recompressor(File::create(&output)?);
            recompressor.recompress(source)?;
        }
        Ok(())
    })
}

fn run_test_wav_roundtrip(fixture: &TestWavFixture) -> Result<(), Box<dyn std::error::Error>> {
    let encoded = builtin::encode_bytes(&fixture.wav_bytes)?;
    let decoded = builtin::decode_bytes(&encoded)?;
    let _ = builtin::encode_bytes(&decoded)?;
    Ok(())
}

fn run_test_wav_level8_encode(
    fixtures: &[TestWavFixture],
) -> Result<(), Box<dyn std::error::Error>> {
    let config = EncoderConfig::default()
        .with_level(Level::Level8)
        .with_threads(8);
    for fixture in fixtures {
        let _ = encode_fixture_bytes(&config, &fixture.wav_bytes)?;
    }
    Ok(())
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

fn encode_fixture_bytes(
    config: &EncoderConfig,
    input: &[u8],
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let reader = read_pcm_reader_with_options(
        Cursor::new(input),
        PcmReaderOptions {
            capture_fxmd: config.capture_fxmd,
            strict_fxmd_validation: config.strict_fxmd_validation,
        },
    )?;
    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let mut encoder = config.clone().into_encoder(Cursor::new(Vec::new()));
    encoder.set_metadata(metadata);
    encoder.encode(stream)?;
    Ok(encoder.into_inner().into_inner())
}

fn metadata_wav_fixture() -> Vec<u8> {
    wav_with_chunks(
        pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 8_192)),
        &[
            (
                *b"LIST",
                info_list_chunk(&[
                    (*b"IART", b"Benchmark Artist"),
                    (*b"INAM", b"Metadata Write Path"),
                ]),
            ),
            (*b"cue ", cue_chunk(&[0, 4_096])),
        ],
    )
}

fn load_test_wav_fixtures() -> Result<(Vec<TestWavFixture>, u64), Box<dyn std::error::Error>> {
    let corpus_root = locate_test_wavs_root()?;
    let mut files: Vec<PathBuf> = fs::read_dir(&corpus_root)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
        })
        .collect();
    files.sort();

    let mut fixtures = Vec::with_capacity(files.len());
    let mut total_input_bytes = 0u64;
    for path in files {
        let wav_bytes = fs::read(&path)?;
        total_input_bytes += wav_bytes.len() as u64;
        fixtures.push(TestWavFixture {
            _file_name: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("fixture.wav")
                .to_owned(),
            wav_bytes,
        });
    }

    if fixtures.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "test-wavs corpus is empty").into());
    }

    Ok((fixtures, total_input_bytes))
}

fn locate_test_wavs_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(explicit) = env::var("FLACX_TEST_WAVS_ROOT") {
        let path = PathBuf::from(explicit);
        if path.is_dir() {
            return Ok(path);
        }
    }

    let mut cursor = env::current_dir()?;
    loop {
        let candidate = cursor.join("test-wavs");
        if candidate.is_dir() {
            return Ok(candidate);
        }
        if !cursor.pop() {
            break;
        }
    }

    Err(io::Error::new(io::ErrorKind::NotFound, "unable to locate test-wavs corpus").into())
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

fn shared_thread_count() -> usize {
    let encode_threads = EncoderConfig::default().threads.max(1);
    let decode_threads = DecodeConfig::default().threads.max(1);
    let recompress_threads = RecompressConfig::default().threads().max(1);
    assert_eq!(
        encode_threads, decode_threads,
        "default encode/decode thread counts diverged"
    );
    assert_eq!(
        encode_threads, recompress_threads,
        "default encode/recompress thread counts diverged"
    );
    encode_threads
}
