use std::{
    env,
    fs::{self, File},
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

use flacx::{Encoder, EncoderConfig, level::Level};

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

    if let Some(path) = env::args().nth(1).map(PathBuf::from) {
        run_single_input(&path)?;
    } else {
        run_test_wavs_corpus()?;
    }

    Ok(())
}

fn run_single_input(wav_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    ensure_flac_available()?;
    let measurement = benchmark_fixture(wav_path)?;
    print_fixture_result("single", wav_path, &measurement);
    Ok(())
}

fn run_test_wavs_corpus() -> Result<(), Box<dyn std::error::Error>> {
    ensure_flac_available()?;
    let corpus = default_corpus();
    println!(
        "test-wavs corpus: {} files, {} repeats, pinned cores={} (best effort), baseline=flac -f -8 --totally-silent",
        corpus.len(),
        DEFAULT_REPEATS,
        PINNED_CORES
    );

    let mut flacx_throughputs = Vec::with_capacity(corpus.len());
    let mut baseline_throughputs = Vec::with_capacity(corpus.len());
    let mut flacx_bytes = Vec::with_capacity(corpus.len());
    let mut baseline_bytes = Vec::with_capacity(corpus.len());
    let mut flacx_ratios = Vec::with_capacity(corpus.len());
    let mut baseline_ratios = Vec::with_capacity(corpus.len());

    for wav_path in corpus {
        let measurement = benchmark_fixture(&wav_path)?;
        print_fixture_result("case", &wav_path, &measurement);

        flacx_throughputs.push(measurement.flacx_throughput_mib_s());
        baseline_throughputs.push(measurement.baseline_throughput_mib_s());
        flacx_bytes.push(measurement.flacx_bytes as f64);
        baseline_bytes.push(measurement.baseline_bytes as f64);
        flacx_ratios.push(measurement.flacx_ratio());
        baseline_ratios.push(measurement.baseline_ratio());
    }

    let corpus_flacx_throughput = median(&mut flacx_throughputs);
    let corpus_baseline_throughput = median(&mut baseline_throughputs);
    let corpus_flacx_bytes = median(&mut flacx_bytes);
    let corpus_baseline_bytes = median(&mut baseline_bytes);
    let corpus_flacx_ratio = median(&mut flacx_ratios);
    let corpus_baseline_ratio = median(&mut baseline_ratios);

    println!(
        "corpus-median: flacx={:.3} MiB/s baseline={:.3} MiB/s delta={:+.3} MiB/s | flacx={:.1} B ratio={:.6} baseline={:.1} B ratio={:.6}",
        corpus_flacx_throughput,
        corpus_baseline_throughput,
        corpus_flacx_throughput - corpus_baseline_throughput,
        corpus_flacx_bytes,
        corpus_flacx_ratio,
        corpus_baseline_bytes,
        corpus_baseline_ratio
    );

    if corpus_flacx_throughput < corpus_baseline_throughput {
        return Err(format!(
            "corpus-median throughput regressed: flacx={corpus_flacx_throughput:.3} MiB/s baseline={corpus_baseline_throughput:.3} MiB/s"
        )
        .into());
    }

    if corpus_flacx_bytes > corpus_baseline_bytes * 1.05
        || corpus_flacx_ratio > corpus_baseline_ratio * 1.05
    {
        return Err(format!(
            "corpus-median encoded size/ratio regressed: flacx={corpus_flacx_bytes:.1} B ({corpus_flacx_ratio:.6}) baseline={corpus_baseline_bytes:.1} B ({corpus_baseline_ratio:.6})"
        )
        .into());
    }

    Ok(())
}

fn benchmark_fixture(wav_path: &Path) -> Result<FixtureMeasurement, Box<dyn std::error::Error>> {
    let input_bytes = fs::metadata(wav_path)?.len();
    let fixture_name = wav_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or("invalid fixture name")?;

    let mut flacx_sizes = Vec::with_capacity(DEFAULT_REPEATS);
    let mut baseline_sizes = Vec::with_capacity(DEFAULT_REPEATS);
    let mut flacx_seconds = Vec::with_capacity(DEFAULT_REPEATS);
    let mut baseline_seconds = Vec::with_capacity(DEFAULT_REPEATS);

    for repeat in 0..DEFAULT_REPEATS {
        let flacx_output = temp_path(&format!("{fixture_name}-flacx-{repeat}"), "flac");
        let flacx_run = encode_flacx(wav_path, &flacx_output)?;
        flacx_sizes.push(flacx_run.encoded_bytes);
        flacx_seconds.push(flacx_run.elapsed_seconds);
        let _ = fs::remove_file(flacx_output);

        let baseline_output = temp_path(&format!("{fixture_name}-baseline-{repeat}"), "flac");
        let baseline_run = encode_baseline_flac(wav_path, &baseline_output)?;
        baseline_sizes.push(baseline_run.encoded_bytes);
        baseline_seconds.push(baseline_run.elapsed_seconds);
        let _ = fs::remove_file(baseline_output);
    }

    let flacx_bytes = stable_size(&flacx_sizes, fixture_name, "flacx")?;
    let baseline_bytes = stable_size(&baseline_sizes, fixture_name, "baseline")?;

    Ok(FixtureMeasurement {
        source_bytes: input_bytes,
        flacx_bytes,
        baseline_bytes,
        flacx_seconds: median(&mut flacx_seconds),
        baseline_seconds: median(&mut baseline_seconds),
    })
}

fn default_corpus() -> Vec<PathBuf> {
    DEFAULT_CORPUS
        .iter()
        .map(|relative| repo_root().join(relative))
        .collect()
}

struct TimedEncode {
    encoded_bytes: u64,
    elapsed_seconds: f64,
}

struct FixtureMeasurement {
    source_bytes: u64,
    flacx_bytes: u64,
    baseline_bytes: u64,
    flacx_seconds: f64,
    baseline_seconds: f64,
}

impl FixtureMeasurement {
    fn flacx_ratio(&self) -> f64 {
        size_ratio(self.flacx_bytes, self.source_bytes)
    }

    fn baseline_ratio(&self) -> f64 {
        size_ratio(self.baseline_bytes, self.source_bytes)
    }

    fn flacx_throughput_mib_s(&self) -> f64 {
        throughput_mib_s(self.source_bytes, self.flacx_seconds)
    }

    fn baseline_throughput_mib_s(&self) -> f64 {
        throughput_mib_s(self.source_bytes, self.baseline_seconds)
    }
}

fn print_fixture_result(label: &str, wav_path: &Path, measurement: &FixtureMeasurement) {
    println!(
        "{label}={:<12} source={:>10} B | flacx={:>10} B ratio={:.6} time={:.3}s thr={:.3} MiB/s | baseline={:>10} B ratio={:.6} time={:.3}s thr={:.3} MiB/s | delta={:+} B {:+.3}s {:+.3} MiB/s",
        wav_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("unknown"),
        measurement.source_bytes,
        measurement.flacx_bytes,
        measurement.flacx_ratio(),
        measurement.flacx_seconds,
        measurement.flacx_throughput_mib_s(),
        measurement.baseline_bytes,
        measurement.baseline_ratio(),
        measurement.baseline_seconds,
        measurement.baseline_throughput_mib_s(),
        signed_delta(measurement.flacx_bytes, measurement.baseline_bytes),
        measurement.flacx_seconds - measurement.baseline_seconds,
        measurement.flacx_throughput_mib_s() - measurement.baseline_throughput_mib_s(),
    );
}

fn ensure_flac_available() -> Result<(), Box<dyn std::error::Error>> {
    if flac_available() {
        Ok(())
    } else {
        Err(
            "benchmark requires baseline `flac -f -8 --totally-silent`, but `flac` was not found"
                .into(),
        )
    }
}

fn flac_available() -> bool {
    Command::new("flac")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn encode_flacx(
    wav_path: &Path,
    output_path: &Path,
) -> Result<TimedEncode, Box<dyn std::error::Error>> {
    let start = Instant::now();
    benchmark_encoder()?.encode(File::open(wav_path)?, File::create(output_path)?)?;
    Ok(TimedEncode {
        encoded_bytes: encoded_size(output_path)?,
        elapsed_seconds: start.elapsed().as_secs_f64(),
    })
}

fn benchmark_encoder() -> Result<Encoder, Box<dyn std::error::Error>> {
    let mut config = EncoderConfig::default();
    if let Some(level) = env::var("FLACX_LEVEL").ok() {
        let level = level.parse::<u8>()?;
        config = config.with_level(Level::try_from(level).map_err(|_| "invalid FLACX_LEVEL")?);
    }
    Ok(Encoder::new(config))
}

fn encode_baseline_flac(
    wav_path: &Path,
    output_path: &Path,
) -> Result<TimedEncode, Box<dyn std::error::Error>> {
    let start = Instant::now();
    let output = Command::new("flac")
        .args([
            "-f",
            "-8",
            "--totally-silent",
            "-o",
            output_path.to_str().ok_or("invalid output path")?,
            wav_path.to_str().ok_or("invalid wav path")?,
        ])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let status = output.status.code().map_or_else(
            || "terminated by signal".to_string(),
            |code| code.to_string(),
        );
        return Err(
            format!("baseline encoder `flac -f -8` failed (status {status}): {stderr}").into(),
        );
    }

    Ok(TimedEncode {
        encoded_bytes: encoded_size(output_path)?,
        elapsed_seconds: start.elapsed().as_secs_f64(),
    })
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

fn signed_delta(left: u64, right: u64) -> i64 {
    left as i64 - right as i64
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

#[cfg(test)]
mod tests {
    use super::median;

    #[test]
    fn median_handles_even_and_odd_lengths() {
        let mut odd = vec![3.0, 1.0, 2.0];
        let mut even = vec![4.0, 1.0, 3.0, 2.0];
        assert_eq!(median(&mut odd), 2.0);
        assert_eq!(median(&mut even), 2.5);
    }
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
