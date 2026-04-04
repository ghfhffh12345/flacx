use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Instant,
};

use flacx::{DecodeConfig, Decoder, Encoder, EncoderConfig, level::Level};

const DEFAULT_REPEATS: usize = 3;
const PINNED_CORES: usize = 8;
const MIB: f64 = 1024.0 * 1024.0;
const DEFAULT_CORPUS: [&str; 3] = [
    "test-wavs/test1.wav",
    "test-wavs/test2.wav",
    "test-wavs/test3.wav",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    pin_current_process_to_first_n_cores(PINNED_CORES)?;
    let threads = shared_thread_count();

    if let Some(path) = env::args().nth(1).map(PathBuf::from) {
        run_single_input(&path, threads)?;
    } else {
        run_test_wavs_corpus(threads)?;
    }

    Ok(())
}

fn run_single_input(wav_path: &Path, threads: usize) -> Result<(), Box<dyn std::error::Error>> {
    let measurement = benchmark_fixture(wav_path, threads)?;
    print_fixture_result("single", wav_path, &measurement, threads);
    ensure_decode_not_slower("single", &measurement)?;
    Ok(())
}

fn run_test_wavs_corpus(threads: usize) -> Result<(), Box<dyn std::error::Error>> {
    let corpus = default_corpus();
    println!(
        "test-wavs corpus: {} files, {} repeats, pinned cores={} (best effort), threads={} shared across encode+decode, self-generated FLAC inputs",
        corpus.len(),
        DEFAULT_REPEATS,
        PINNED_CORES,
        threads
    );

    let mut encode_throughputs = Vec::with_capacity(corpus.len());
    let mut decode_throughputs = Vec::with_capacity(corpus.len());
    let mut flac_bytes = Vec::with_capacity(corpus.len());
    let mut flac_ratios = Vec::with_capacity(corpus.len());

    for wav_path in corpus {
        let measurement = benchmark_fixture(&wav_path, threads)?;
        print_fixture_result("case", &wav_path, &measurement, threads);

        encode_throughputs.push(measurement.encode_throughput_mib_s());
        decode_throughputs.push(measurement.decode_throughput_mib_s());
        flac_bytes.push(measurement.flac_bytes as f64);
        flac_ratios.push(measurement.flac_ratio());
    }

    let corpus_encode_throughput = median(&mut encode_throughputs);
    let corpus_decode_throughput = median(&mut decode_throughputs);
    let corpus_flac_bytes = median(&mut flac_bytes);
    let corpus_flac_ratio = median(&mut flac_ratios);

    println!(
        "corpus-median: encode={:.3} MiB/s decode={:.3} MiB/s delta={:+.3} MiB/s | flac={:.1} B ratio={:.6}",
        corpus_encode_throughput,
        corpus_decode_throughput,
        corpus_decode_throughput - corpus_encode_throughput,
        corpus_flac_bytes,
        corpus_flac_ratio
    );

    if corpus_decode_throughput < corpus_encode_throughput {
        return Err(format!(
            "corpus-median decode throughput regressed: encode={corpus_encode_throughput:.3} MiB/s decode={corpus_decode_throughput:.3} MiB/s"
        )
        .into());
    }

    Ok(())
}

fn benchmark_fixture(
    wav_path: &Path,
    threads: usize,
) -> Result<FixtureMeasurement, Box<dyn std::error::Error>> {
    let source_bytes = fs::metadata(wav_path)?.len();
    let fixture_name = wav_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or("invalid fixture name")?;

    let mut flac_sizes = Vec::with_capacity(DEFAULT_REPEATS);
    let mut decoded_sizes = Vec::with_capacity(DEFAULT_REPEATS);
    let mut encode_seconds = Vec::with_capacity(DEFAULT_REPEATS);
    let mut decode_seconds = Vec::with_capacity(DEFAULT_REPEATS);

    for repeat in 0..DEFAULT_REPEATS {
        let flac_output = temp_path(&format!("{fixture_name}-flacx-{repeat}"), "flac");
        let encode_run = encode_flacx(wav_path, &flac_output, threads)?;
        flac_sizes.push(encode_run.bytes);
        encode_seconds.push(encode_run.elapsed_seconds);

        let wav_output = temp_path(&format!("{fixture_name}-decoded-{repeat}"), "wav");
        let decode_run = decode_flacx(&flac_output, &wav_output, threads)?;
        decode_seconds.push(decode_run.elapsed_seconds);
        decoded_sizes.push(decode_run.bytes);

        if decode_run.bytes == 0 {
            return Err(
                format!("case `{fixture_name}` produced an empty WAV on repeat {repeat}").into(),
            );
        }

        let _ = fs::remove_file(flac_output);
        let _ = fs::remove_file(wav_output);
    }

    let flac_bytes = stable_size(&flac_sizes, fixture_name, "flac")?;
    stable_size(&decoded_sizes, fixture_name, "decoded wav")?;

    Ok(FixtureMeasurement {
        source_bytes,
        flac_bytes,
        encode_seconds: median(&mut encode_seconds),
        decode_seconds: median(&mut decode_seconds),
    })
}

fn default_corpus() -> Vec<PathBuf> {
    DEFAULT_CORPUS
        .iter()
        .map(|relative| repo_root().join(relative))
        .collect()
}

struct TimedStep {
    bytes: u64,
    elapsed_seconds: f64,
}

struct FixtureMeasurement {
    source_bytes: u64,
    flac_bytes: u64,
    encode_seconds: f64,
    decode_seconds: f64,
}

impl FixtureMeasurement {
    fn flac_ratio(&self) -> f64 {
        size_ratio(self.flac_bytes, self.source_bytes)
    }

    fn encode_throughput_mib_s(&self) -> f64 {
        throughput_mib_s(self.source_bytes, self.encode_seconds)
    }

    fn decode_throughput_mib_s(&self) -> f64 {
        throughput_mib_s(self.source_bytes, self.decode_seconds)
    }
}

fn print_fixture_result(
    label: &str,
    wav_path: &Path,
    measurement: &FixtureMeasurement,
    threads: usize,
) {
    println!(
        "{label}={:<12} threads={:<3} source={:>10} B | flacx-encode={:>10} B ratio={:.6} time={:.3}s thr={:.3} MiB/s | flacx-decode={:>10} B time={:.3}s thr={:.3} MiB/s | delta={:+.3} MiB/s",
        wav_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("unknown"),
        threads,
        measurement.source_bytes,
        measurement.flac_bytes,
        measurement.flac_ratio(),
        measurement.encode_seconds,
        measurement.encode_throughput_mib_s(),
        measurement.source_bytes,
        measurement.decode_seconds,
        measurement.decode_throughput_mib_s(),
        measurement.decode_throughput_mib_s() - measurement.encode_throughput_mib_s(),
    );
}

fn ensure_decode_not_slower(
    label: &str,
    measurement: &FixtureMeasurement,
) -> Result<(), Box<dyn std::error::Error>> {
    if measurement.decode_throughput_mib_s() < measurement.encode_throughput_mib_s() {
        return Err(format!(
            "{label} decode throughput regressed: encode={:.3} MiB/s decode={:.3} MiB/s",
            measurement.encode_throughput_mib_s(),
            measurement.decode_throughput_mib_s(),
        )
        .into());
    }

    Ok(())
}

fn encode_flacx(
    wav_path: &Path,
    output_path: &Path,
    threads: usize,
) -> Result<TimedStep, Box<dyn std::error::Error>> {
    let start = Instant::now();
    benchmark_encoder(threads)?.encode_file(wav_path, output_path)?;
    Ok(TimedStep {
        bytes: encoded_size(output_path)?,
        elapsed_seconds: start.elapsed().as_secs_f64(),
    })
}

fn decode_flacx(
    flac_path: &Path,
    output_path: &Path,
    threads: usize,
) -> Result<TimedStep, Box<dyn std::error::Error>> {
    let start = Instant::now();
    benchmark_decoder(threads)?.decode_file(flac_path, output_path)?;
    Ok(TimedStep {
        bytes: encoded_size(output_path)?,
        elapsed_seconds: start.elapsed().as_secs_f64(),
    })
}

fn benchmark_encoder(threads: usize) -> Result<Encoder, Box<dyn std::error::Error>> {
    let mut config = EncoderConfig::default();
    if let Some(level) = env::var("FLACX_LEVEL").ok() {
        let level = level.parse::<u8>()?;
        config = config.with_level(Level::try_from(level).map_err(|_| "invalid FLACX_LEVEL")?);
    }
    Ok(Encoder::new(config.with_threads(threads)))
}

fn benchmark_decoder(threads: usize) -> Result<Decoder, Box<dyn std::error::Error>> {
    Ok(Decoder::new(DecodeConfig::default().with_threads(threads)))
}

fn encoded_size(path: &Path) -> Result<u64, Box<dyn std::error::Error>> {
    Ok(fs::metadata(path)?.len())
}

fn throughput_mib_s(input_bytes: u64, seconds: f64) -> f64 {
    if seconds <= f64::EPSILON {
        0.0
    } else {
        input_bytes as f64 / seconds / MIB
    }
}

fn size_ratio(encoded_bytes: u64, source_bytes: u64) -> f64 {
    encoded_bytes as f64 / source_bytes as f64
}

fn stable_size(
    values: &[u64],
    case_name: &str,
    label: &str,
) -> Result<u64, Box<dyn std::error::Error>> {
    let first = values.first().copied().ok_or("missing benchmark samples")?;
    if values.iter().any(|&value| value != first) {
        return Err(format!(
            "case `{case_name}` produced unstable {label} sizes across repeats: {values:?}"
        )
        .into());
    }
    Ok(first)
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|left, right| left.partial_cmp(right).unwrap());
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

fn temp_path(stem: &str, extension: &str) -> PathBuf {
    env::temp_dir().join(format!("flacx-{stem}.{extension}"))
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn shared_thread_count() -> usize {
    let encode_threads = EncoderConfig::default().threads;
    let decode_threads = DecodeConfig::default().threads;
    assert_eq!(
        encode_threads, decode_threads,
        "default encode/decode thread counts diverged"
    );
    encode_threads.max(1)
}

#[cfg(windows)]
fn pin_current_process_to_first_n_cores(
    core_count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::ffi::c_void;

    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn SetProcessAffinityMask(handle: *mut c_void, process_affinity_mask: usize) -> i32;
    }

    if core_count == 0 {
        return Ok(());
    }

    let width = usize::BITS as usize;
    let bounded = core_count.min(width);
    let mask = if bounded >= width {
        usize::MAX
    } else {
        (1usize << bounded) - 1
    };

    let ok = unsafe { SetProcessAffinityMask(GetCurrentProcess(), mask) };
    if ok == 0 {
        return Err("SetProcessAffinityMask failed".into());
    }

    Ok(())
}

#[cfg(not(windows))]
fn pin_current_process_to_first_n_cores(
    _core_count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}
