use std::{
    fs::{self, File},
    io::{self, Cursor},
    path::{Path, PathBuf},
    time::Duration,
};

use criterion::{
    criterion_group, criterion_main, measurement::WallTime, BenchmarkGroup, Criterion, Throughput,
};
use flacx::{
    builtin, read_flac_reader_with_options, DecodeConfig, EncoderConfig, FlacReaderOptions,
    RecompressConfig, RecompressMode, WavReader, WavReaderOptions,
};

#[path = "../tests/support/mod.rs"]
mod support;

use support::{cue_chunk, info_list_chunk, pcm_wav_bytes, sample_fixture, wav_with_chunks};
use support::{
    large_streaming_decode_flac_bytes, large_streaming_decode_wav_bytes,
    TestDecoder as DecodeHarness,
};

const DEFAULT_MEASUREMENT_TIME: Duration = Duration::from_secs(5);
const LARGE_STREAMING_SAMPLE_SIZE: usize = 10;
const DECODE_THREAD_VARIANTS: [usize; 4] = [1, 2, 4, 8];

fn corpus_root(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .map(|path| path.join(name))
        .find(|path| path.is_dir())
        .unwrap_or_else(|| panic!("corpus root '{name}' should exist from the workspace root"))
}

fn configure_throughput_group(group: &mut BenchmarkGroup<'_, WallTime>, bytes: u64) {
    group.throughput(Throughput::Bytes(bytes));
    group.measurement_time(DEFAULT_MEASUREMENT_TIME);
}

fn configure_large_streaming_group(group: &mut BenchmarkGroup<'_, WallTime>) {
    group.measurement_time(DEFAULT_MEASUREMENT_TIME);
    group.sample_size(LARGE_STREAMING_SAMPLE_SIZE);
}

fn encode_corpus_throughput(c: &mut Criterion) {
    let corpus = Corpus::load().expect("benchmark corpus");
    let total_bytes = corpus.wav_total_input_bytes();
    let threads = shared_thread_count();

    let mut group = c.benchmark_group("flacx throughput");
    configure_throughput_group(&mut group, total_bytes);
    group.bench_function("corpus_encode", |b| {
        b.iter(|| run_encode_corpus(&corpus, threads).expect("encode corpus"))
    });
    group.finish();
}

fn decode_corpus_throughput(c: &mut Criterion) {
    let corpus = Corpus::load().expect("benchmark corpus");
    let total_bytes = corpus.flac_total_input_bytes();
    let threads = shared_thread_count();

    let mut group = c.benchmark_group("flacx throughput");
    configure_throughput_group(&mut group, total_bytes);
    group.bench_function("corpus_decode", |b| {
        b.iter(|| run_decode_corpus(&corpus, threads).expect("decode corpus"))
    });
    group.finish();
}

fn recompress_corpus_throughput(c: &mut Criterion) {
    let corpus = Corpus::load().expect("benchmark corpus");
    let total_bytes = corpus.flac_total_input_bytes();
    let threads = shared_thread_count();

    let mut group = c.benchmark_group("flacx throughput");
    configure_throughput_group(&mut group, total_bytes);
    group.bench_function("corpus_recompress", |b| {
        b.iter(|| run_recompress_corpus(&corpus, threads).expect("recompress corpus"))
    });
    group.finish();
}

fn builtin_bytes_encode(c: &mut Criterion) {
    let corpus = Corpus::load().expect("benchmark corpus");
    let mut group = c.benchmark_group("flacx throughput");
    configure_throughput_group(&mut group, corpus.representative_wav_bytes.len() as u64);
    group.bench_function("bytes_encode", |b| {
        b.iter(|| builtin::encode_bytes(&corpus.representative_wav_bytes).expect("builtin encode"))
    });
    group.finish();
}

fn builtin_bytes_decode(c: &mut Criterion) {
    let corpus = Corpus::load().expect("benchmark corpus");
    let mut group = c.benchmark_group("flacx throughput");
    configure_throughput_group(&mut group, corpus.representative_flac_bytes.len() as u64);
    group.bench_function("bytes_decode", |b| {
        b.iter(|| builtin::decode_bytes(&corpus.representative_flac_bytes).expect("builtin decode"))
    });
    group.finish();
}

fn builtin_bytes_recompress(c: &mut Criterion) {
    let corpus = Corpus::load().expect("benchmark corpus");
    let mut group = c.benchmark_group("flacx throughput");
    configure_throughput_group(&mut group, corpus.representative_flac_bytes.len() as u64);
    group.bench_function("bytes_recompress", |b| {
        b.iter(|| {
            builtin::recompress_bytes(&corpus.representative_flac_bytes)
                .expect("builtin recompress")
        })
    });
    group.finish();
}

fn encode_multiframe_streaming_path(c: &mut Criterion) {
    let input = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 10_432));
    let config = EncoderConfig::default()
        .with_threads(shared_thread_count())
        .with_block_schedule(vec![576, 1_152, 576, 2_304, 4_096, 576, 1_152]);
    let mut group = c.benchmark_group("flacx throughput");
    configure_throughput_group(&mut group, input.len() as u64);
    group.sample_size(20);
    group.bench_function("encode_multiframe_streaming_path", |b| {
        b.iter(|| encode_fixture_bytes(&config, &input).expect("encode multiframe streaming path"))
    });
    group.finish();
}

fn decode_large_streaming_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("flacx throughput");
    configure_large_streaming_group(&mut group);
    bench_large_streaming_real_decode_matrix(
        &mut group,
        "decode_large_streaming_real_decode_path",
        |_threads, input| Throughput::Bytes(input.len() as u64),
        large_streaming_decode_flac_bytes,
    );
    group.finish();
}

fn matched_large_streaming_encode_decode(c: &mut Criterion) {
    let threads = shared_thread_count();
    let wav_input = large_streaming_decode_wav_bytes();
    let encoder_config = EncoderConfig::default().with_threads(threads);
    let mut group = c.benchmark_group("flacx matched throughput");
    configure_throughput_group(&mut group, wav_input.len() as u64);
    group.sample_size(LARGE_STREAMING_SAMPLE_SIZE);
    group.bench_function("matched_large_streaming_encode", |b| {
        b.iter(|| encode_fixture_bytes(&encoder_config, &wav_input).expect("matched encode"))
    });
    bench_large_streaming_real_decode_matrix(
        &mut group,
        "matched_large_streaming_real_decode",
        |_, _| Throughput::Bytes(wav_input.len() as u64),
        large_streaming_decode_flac_bytes,
    );
    group.finish();
}

fn recompress_streaming_verify_handoff(c: &mut Criterion) {
    let corpus = Corpus::load().expect("benchmark corpus");
    let config = RecompressConfig::default().with_threads(shared_thread_count());
    let mut group = c.benchmark_group("flacx throughput");
    configure_throughput_group(
        &mut group,
        corpus.recompress_streaming_flac_bytes.len() as u64,
    );
    group.bench_function("recompress_streaming_verify_handoff", |b| {
        b.iter(|| {
            let reader = read_flac_reader_with_options(
                Cursor::new(&corpus.recompress_streaming_flac_bytes),
                recompress_reader_options(config),
            )
            .expect("streaming recompress reader");
            let source = reader.into_recompress_source();
            let mut recompressor = config.into_recompressor(Cursor::new(Vec::new()));
            recompressor
                .recompress(source)
                .expect("streaming recompress verify handoff");
            recompressor.into_inner().into_inner()
        })
    });
    group.finish();
}

fn metadata_write_path(c: &mut Criterion) {
    let corpus = Corpus::load().expect("benchmark corpus");
    let mut group = c.benchmark_group("flacx throughput");
    configure_throughput_group(&mut group, corpus.metadata_flac_bytes.len() as u64);
    group.bench_function("metadata_write_path", |b| {
        // Synthetic fixture retained because it exercises the metadata-bearing write path,
        // which the repository corpora do not cover directly.
        b.iter(|| builtin::decode_bytes(&corpus.metadata_flac_bytes).expect("metadata write path"))
    });
    group.finish();
}

fn decode_frame_materialization(c: &mut Criterion) {
    let corpus = Corpus::load().expect("benchmark corpus");
    let mut group = c.benchmark_group("flacx throughput");
    configure_throughput_group(
        &mut group,
        corpus.decode_materialization_flac_bytes.len() as u64,
    );
    group.bench_function("decode_frame_materialization", |b| {
        // Synthetic fixture retained because it forces multi-frame decode materialization
        // behavior that is not reliably represented by the fixed corpus subset.
        b.iter(|| {
            builtin::decode_bytes(&corpus.decode_materialization_flac_bytes)
                .expect("decode frame materialization")
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
    encode_multiframe_streaming_path,
    decode_large_streaming_path,
    matched_large_streaming_encode_decode,
    recompress_streaming_verify_handoff,
    metadata_write_path,
    decode_frame_materialization
);
criterion_main!(benches);

struct CorpusInput {
    path: PathBuf,
    bytes: Vec<u8>,
}

struct Corpus {
    wav_inputs: Vec<CorpusInput>,
    flac_inputs: Vec<CorpusInput>,
    representative_wav_bytes: Vec<u8>,
    representative_flac_bytes: Vec<u8>,
    recompress_streaming_flac_bytes: Vec<u8>,
    metadata_flac_bytes: Vec<u8>,
    decode_materialization_flac_bytes: Vec<u8>,
}

impl Corpus {
    fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let wav_root = corpus_root("test-wavs");
        let flac_root = corpus_root("test-flacs");
        let wav_files = load_sorted_corpus_files(&wav_root, "wav")?;
        let flac_files = load_sorted_corpus_files(&flac_root, "flac")?;
        let flac_files = flac_files
            .into_iter()
            .filter(|path| benchmark_safe_flac(path))
            .collect::<Vec<_>>();
        let wav_inputs = select_wav_subset(&wav_files)?;

        let wav_inputs = wav_inputs
            .into_iter()
            .map(CorpusInput::load)
            .collect::<Result<Vec<_>, _>>()?;
        let threads = shared_thread_count();
        let config = EncoderConfig::default().with_threads(threads);
        let flac_inputs = if flac_files.is_empty() {
            wav_inputs
                .iter()
                .enumerate()
                .map(|(index, fixture)| {
                    Ok(CorpusInput {
                        path: PathBuf::from(format!("synthetic-{index}.flac")),
                        bytes: encode_fixture_bytes(&config, &fixture.bytes)?,
                    })
                })
                .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?
        } else {
            select_flac_subset(&flac_files)?
                .into_iter()
                .map(CorpusInput::load)
                .collect::<Result<Vec<_>, _>>()?
        };
        let representative_wav_bytes = wav_inputs
            .first()
            .expect("wav corpus selection")
            .bytes
            .clone();
        let representative_flac_bytes = flac_inputs
            .first()
            .expect("flac corpus selection")
            .bytes
            .clone();
        let encode_streaming_wav_bytes = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 10_432));
        let recompress_streaming_flac_bytes = encode_fixture_bytes(
            &EncoderConfig::default()
                .with_threads(threads)
                .with_block_schedule(vec![576, 1_152, 576, 2_304, 4_096, 576, 1_152]),
            &encode_streaming_wav_bytes,
        )?;
        let metadata_flac_bytes = encode_fixture_bytes(&config, &metadata_wav_fixture())?;
        let decode_materialization_flac_bytes = encode_fixture_bytes(
            &EncoderConfig::default()
                .with_threads(threads)
                .with_block_schedule(vec![576, 1_152, 576, 2_304, 4_096, 576, 1_152]),
            &pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 10_432)),
        )?;

        Ok(Self {
            wav_inputs,
            flac_inputs,
            representative_wav_bytes,
            representative_flac_bytes,
            recompress_streaming_flac_bytes,
            metadata_flac_bytes,
            decode_materialization_flac_bytes,
        })
    }

    fn wav_total_input_bytes(&self) -> u64 {
        self.wav_inputs
            .iter()
            .map(|fixture| fixture.bytes.len() as u64)
            .sum()
    }

    fn flac_total_input_bytes(&self) -> u64 {
        self.flac_inputs
            .iter()
            .map(|fixture| fixture.bytes.len() as u64)
            .sum()
    }
}

impl CorpusInput {
    fn load(path: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            bytes: fs::read(&path)?,
            path,
        })
    }
}

fn run_encode_corpus(corpus: &Corpus, threads: usize) -> Result<(), Box<dyn std::error::Error>> {
    let config = EncoderConfig::default().with_threads(threads);
    with_temp_dir("flacx-benchmark-encode", |output_root| {
        for input in &corpus.wav_inputs {
            let output = output_root
                .join(input.path.file_stem().expect("fixture file stem"))
                .with_extension("flac");
            encode_fixture_file(&config, &input.path, &output)?;
        }
        Ok(())
    })
}

fn run_decode_corpus(corpus: &Corpus, threads: usize) -> Result<(), Box<dyn std::error::Error>> {
    let config = DecodeConfig::default().with_threads(threads);
    with_temp_dir("flacx-benchmark-decode", |output_root| {
        for input in &corpus.flac_inputs {
            let output = output_root
                .join(input.path.file_stem().expect("fixture file stem"))
                .with_extension("wav");
            let reader = read_flac_reader_with_options(
                Cursor::new(&input.bytes),
                FlacReaderOptions {
                    strict_seektable_validation: config.strict_seektable_validation(),
                    strict_channel_mask_provenance: config.strict_channel_mask_provenance(),
                },
            )?;
            let mut decoder = config.into_decoder(File::create(&output)?);
            decoder.decode_source(reader.into_decode_source())?;
        }
        Ok(())
    })
}

fn run_recompress_corpus(
    corpus: &Corpus,
    threads: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = RecompressConfig::default().with_threads(threads);
    with_temp_dir("flacx-benchmark-recompress", |output_root| {
        for input in &corpus.flac_inputs {
            let output = output_root
                .join(input.path.file_stem().expect("fixture file stem"))
                .with_extension("flac");
            let reader = read_flac_reader_with_options(
                Cursor::new(&input.bytes),
                recompress_reader_options(config),
            )?;
            let source = reader.into_recompress_source();
            let mut recompressor = config.into_recompressor(File::create(&output)?);
            recompressor.recompress(source)?;
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
    let path = std::env::temp_dir().join(format!("flacx-{label}-{}-{unique}", std::process::id()));
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn encode_fixture_file(
    config: &EncoderConfig,
    input: &Path,
    output: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let reader = WavReader::with_reader_options(
        File::open(input)?,
        WavReaderOptions {
            capture_fxmd: config.capture_fxmd(),
            strict_fxmd_validation: config.strict_fxmd_validation(),
        },
    )?;
    let mut encoder = config.clone().into_encoder(File::create(output)?);
    encoder.encode_source(reader.into_source())?;
    Ok(())
}

fn encode_fixture_bytes(
    config: &EncoderConfig,
    input: &[u8],
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let reader = WavReader::with_reader_options(
        Cursor::new(input),
        WavReaderOptions {
            capture_fxmd: config.capture_fxmd(),
            strict_fxmd_validation: config.strict_fxmd_validation(),
        },
    )?;
    let mut encoder = config.clone().into_encoder(Cursor::new(Vec::new()));
    encoder.encode_source(reader.into_source())?;
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

fn load_sorted_corpus_files(
    root: &Path,
    extension: &str,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
            {
                files.push(path);
            }
        }
    }
    files.sort();

    if files.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} corpus is empty", root.display()),
        )
        .into());
    }

    Ok(files)
}

fn select_wav_subset(files: &[PathBuf]) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    if files.len() < 3 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "need at least 3 wav corpus files",
        )
        .into());
    }

    Ok(vec![
        files[0].clone(),
        files[files.len() / 2].clone(),
        files[files.len() - 1].clone(),
    ])
}

fn select_flac_subset(files: &[PathBuf]) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    if files.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "need at least 1 decodable flac corpus file",
        )
        .into());
    }
    if files.len() <= 4 {
        return Ok(files.to_vec());
    }

    let lower_middle = files.len() / 2 - 1;
    let upper_middle = files.len() / 2;
    Ok(vec![
        files[0].clone(),
        files[lower_middle].clone(),
        files[upper_middle].clone(),
        files[files.len() - 1].clone(),
    ])
}

fn benchmark_safe_flac(path: &Path) -> bool {
    fs::read(path)
        .ok()
        .and_then(|bytes| builtin::decode_bytes(&bytes).ok())
        .is_some()
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
    let encode_threads = EncoderConfig::default().threads().max(1);
    let decode_threads = DecodeConfig::default().threads().max(1);
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

fn bench_large_streaming_real_decode_matrix<F, T>(
    group: &mut BenchmarkGroup<'_, WallTime>,
    name_prefix: &str,
    throughput: T,
    input_for_threads: F,
) where
    F: Fn(usize) -> Vec<u8>,
    T: Fn(usize, &[u8]) -> Throughput,
{
    for threads in DECODE_THREAD_VARIANTS {
        let input = input_for_threads(threads);
        let decoder = DecodeHarness::new(DecodeConfig::default().with_threads(threads));
        group.throughput(throughput(threads, &input));
        group.bench_function(format!("{name_prefix}_threads_{threads}"), |b| {
            b.iter(|| {
                decoder
                    .decode_bytes(&input)
                    .expect("dispatcher-backed large streaming decode")
            })
        });
    }
}
