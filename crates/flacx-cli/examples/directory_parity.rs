use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Instant, SystemTime},
};

use flacx::{DecodeConfig, Encoder, EncoderConfig};
use walkdir::WalkDir;

const DEFAULT_REPEATS: usize = 3;
const DEFAULT_DEPTH: usize = 1;
const ACCEPTANCE_RATIO: f64 = 1.03;
const PINNED_CORES: usize = 8;
const MIB: f64 = 1024.0 * 1024.0;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    pin_current_process_to_first_n_cores(PINNED_CORES)?;
    ensure_parent_terminal_is_interactive()?;

    let flacx_bin = flacx_binary_path()?;
    if !flacx_bin.exists() {
        return Err(format!(
            "missing CLI binary at {}; run `cargo build --release --workspace` first",
            flacx_bin.display()
        )
        .into());
    }

    let threads = shared_thread_count();
    let wav_root = repo_root().join("test-wavs");
    let wav_inputs = discover_inputs(&wav_root, "wav")?;
    if wav_inputs.is_empty() {
        return Err(format!("no WAV files found under {}", wav_root.display()).into());
    }

    let decode_root = prepare_decode_corpus(&wav_root, &wav_inputs, threads)?;
    let decode_inputs = discover_inputs(&decode_root, "flac")?;

    println!(
        "interactive directory parity benchmark: repeats={} acceptance_ratio<={:.2} pinned_cores={} threads={} corpus={} decode_corpus={} cli={}",
        DEFAULT_REPEATS,
        ACCEPTANCE_RATIO,
        PINNED_CORES,
        threads,
        wav_root.display(),
        decode_root.display(),
        flacx_bin.display()
    );

    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        warm_up_encode(&flacx_bin, &wav_root, &wav_inputs, threads)?;
        warm_up_decode(&flacx_bin, &decode_root, &decode_inputs, threads)?;

        let encode = benchmark_encode(&flacx_bin, &wav_root, &wav_inputs, threads)?;
        let decode = benchmark_decode(&flacx_bin, &decode_root, &decode_inputs, threads)?;

        print_measurement("encode", &encode);
        print_measurement("decode", &decode);

        if encode.median_ratio > ACCEPTANCE_RATIO {
            return Err(format!(
                "encode interactive folder-path regression: median ratio {:.4} exceeds {:.2}",
                encode.median_ratio, ACCEPTANCE_RATIO
            )
            .into());
        }
        if decode.median_ratio > ACCEPTANCE_RATIO {
            return Err(format!(
                "decode interactive folder-path regression: median ratio {:.4} exceeds {:.2}",
                decode.median_ratio, ACCEPTANCE_RATIO
            )
            .into());
        }

        Ok(())
    })();

    let _ = fs::remove_dir_all(&decode_root);
    result
}

#[derive(Debug)]
struct ThroughputMeasurement {
    elapsed_seconds: f64,
    average_per_file_throughput_mib_s: f64,
    file_count: usize,
}

#[derive(Debug)]
struct ParityMeasurement {
    repeats: Vec<RepeatMeasurement>,
    median_ratio: f64,
}

#[derive(Debug)]
struct RepeatMeasurement {
    repeat: usize,
    order: &'static str,
    folder: ThroughputMeasurement,
    single: ThroughputMeasurement,
    ratio: f64,
}

#[derive(Debug)]
struct TraceSummary {
    interactive_proven: bool,
    file_runs: Vec<FileRun>,
}

#[derive(Debug)]
struct FileRun {
    input_bytes: u64,
    elapsed_seconds: f64,
}

fn warm_up_encode(
    flacx_bin: &Path,
    input_root: &Path,
    inputs: &[PathBuf],
    threads: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let folder_output = fresh_temp_dir("warmup-encode-folder")?;
    run_folder_encode(flacx_bin, input_root, &folder_output, threads)?;
    let _ = fs::remove_dir_all(folder_output);

    let single_output = fresh_temp_dir("warmup-encode-single")?;
    run_single_encode_average(flacx_bin, input_root, inputs, &single_output, threads)?;
    let _ = fs::remove_dir_all(single_output);
    Ok(())
}

fn warm_up_decode(
    flacx_bin: &Path,
    input_root: &Path,
    inputs: &[PathBuf],
    threads: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let folder_output = fresh_temp_dir("warmup-decode-folder")?;
    run_folder_decode(flacx_bin, input_root, &folder_output, threads)?;
    let _ = fs::remove_dir_all(folder_output);

    let single_output = fresh_temp_dir("warmup-decode-single")?;
    run_single_decode_average(flacx_bin, input_root, inputs, &single_output, threads)?;
    let _ = fs::remove_dir_all(single_output);
    Ok(())
}

fn benchmark_encode(
    flacx_bin: &Path,
    input_root: &Path,
    inputs: &[PathBuf],
    threads: usize,
) -> Result<ParityMeasurement, Box<dyn std::error::Error>> {
    benchmark_case(|repeat, folder_first| {
        let folder_output = fresh_temp_dir(&format!("encode-folder-{repeat}"))?;
        let single_output = fresh_temp_dir(&format!("encode-single-{repeat}"))?;

        let (folder, single) = if folder_first {
            (
                run_folder_encode(flacx_bin, input_root, &folder_output, threads)?,
                run_single_encode_average(flacx_bin, input_root, inputs, &single_output, threads)?,
            )
        } else {
            let single =
                run_single_encode_average(flacx_bin, input_root, inputs, &single_output, threads)?;
            let folder = run_folder_encode(flacx_bin, input_root, &folder_output, threads)?;
            (folder, single)
        };

        let _ = fs::remove_dir_all(folder_output);
        let _ = fs::remove_dir_all(single_output);
        Ok((folder, single))
    })
}

fn benchmark_decode(
    flacx_bin: &Path,
    input_root: &Path,
    inputs: &[PathBuf],
    threads: usize,
) -> Result<ParityMeasurement, Box<dyn std::error::Error>> {
    benchmark_case(|repeat, folder_first| {
        let folder_output = fresh_temp_dir(&format!("decode-folder-{repeat}"))?;
        let single_output = fresh_temp_dir(&format!("decode-single-{repeat}"))?;

        let (folder, single) = if folder_first {
            (
                run_folder_decode(flacx_bin, input_root, &folder_output, threads)?,
                run_single_decode_average(flacx_bin, input_root, inputs, &single_output, threads)?,
            )
        } else {
            let single =
                run_single_decode_average(flacx_bin, input_root, inputs, &single_output, threads)?;
            let folder = run_folder_decode(flacx_bin, input_root, &folder_output, threads)?;
            (folder, single)
        };

        let _ = fs::remove_dir_all(folder_output);
        let _ = fs::remove_dir_all(single_output);
        Ok((folder, single))
    })
}

fn benchmark_case(
    mut run_repeat: impl FnMut(
        usize,
        bool,
    ) -> Result<
        (ThroughputMeasurement, ThroughputMeasurement),
        Box<dyn std::error::Error>,
    >,
) -> Result<ParityMeasurement, Box<dyn std::error::Error>> {
    let mut repeats = Vec::with_capacity(DEFAULT_REPEATS);
    let mut ratios = Vec::with_capacity(DEFAULT_REPEATS);

    for repeat in 0..DEFAULT_REPEATS {
        let folder_first = repeat % 2 == 0;
        let (folder, single) = run_repeat(repeat, folder_first)?;
        let ratio = single.average_per_file_throughput_mib_s
            / folder.average_per_file_throughput_mib_s.max(f64::EPSILON);
        repeats.push(RepeatMeasurement {
            repeat: repeat + 1,
            order: if folder_first {
                "folder->single"
            } else {
                "single->folder"
            },
            folder,
            single,
            ratio,
        });
        ratios.push(ratio);
    }

    Ok(ParityMeasurement {
        repeats,
        median_ratio: median(&mut ratios),
    })
}

fn run_folder_encode(
    flacx_bin: &Path,
    input_root: &Path,
    output_root: &Path,
    threads: usize,
) -> Result<ThroughputMeasurement, Box<dyn std::error::Error>> {
    run_cli_measurement(
        flacx_bin,
        [
            "encode".to_string(),
            input_root.display().to_string(),
            "-o".to_string(),
            output_root.display().to_string(),
            "--threads".to_string(),
            threads.to_string(),
        ],
        "folder-encode-trace",
    )
}

fn run_single_encode_average(
    flacx_bin: &Path,
    input_root: &Path,
    inputs: &[PathBuf],
    output_root: &Path,
    threads: usize,
) -> Result<ThroughputMeasurement, Box<dyn std::error::Error>> {
    let mut elapsed_seconds = 0.0;
    let mut throughputs = Vec::with_capacity(inputs.len());

    for (index, input) in inputs.iter().enumerate() {
        let output = output_root
            .join(input.strip_prefix(input_root)?)
            .with_extension("flac");
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }

        let measurement = run_cli_measurement(
            flacx_bin,
            [
                "encode".to_string(),
                input.display().to_string(),
                "-o".to_string(),
                output.display().to_string(),
                "--threads".to_string(),
                threads.to_string(),
            ],
            &format!("single-encode-trace-{index}"),
        )?;
        elapsed_seconds += measurement.elapsed_seconds;
        throughputs.push(measurement.average_per_file_throughput_mib_s);
    }

    Ok(ThroughputMeasurement {
        elapsed_seconds,
        average_per_file_throughput_mib_s: mean(&throughputs),
        file_count: inputs.len(),
    })
}

fn run_folder_decode(
    flacx_bin: &Path,
    input_root: &Path,
    output_root: &Path,
    threads: usize,
) -> Result<ThroughputMeasurement, Box<dyn std::error::Error>> {
    run_cli_measurement(
        flacx_bin,
        [
            "decode".to_string(),
            input_root.display().to_string(),
            "-o".to_string(),
            output_root.display().to_string(),
            "--threads".to_string(),
            threads.to_string(),
        ],
        "folder-decode-trace",
    )
}

fn run_single_decode_average(
    flacx_bin: &Path,
    input_root: &Path,
    inputs: &[PathBuf],
    output_root: &Path,
    threads: usize,
) -> Result<ThroughputMeasurement, Box<dyn std::error::Error>> {
    let mut elapsed_seconds = 0.0;
    let mut throughputs = Vec::with_capacity(inputs.len());

    for (index, input) in inputs.iter().enumerate() {
        let output = output_root
            .join(input.strip_prefix(input_root)?)
            .with_extension("wav");
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }

        let measurement = run_cli_measurement(
            flacx_bin,
            [
                "decode".to_string(),
                input.display().to_string(),
                "-o".to_string(),
                output.display().to_string(),
                "--threads".to_string(),
                threads.to_string(),
            ],
            &format!("single-decode-trace-{index}"),
        )?;
        elapsed_seconds += measurement.elapsed_seconds;
        throughputs.push(measurement.average_per_file_throughput_mib_s);
    }

    Ok(ThroughputMeasurement {
        elapsed_seconds,
        average_per_file_throughput_mib_s: mean(&throughputs),
        file_count: inputs.len(),
    })
}

fn run_cli_measurement(
    flacx_bin: &Path,
    args: [String; 6],
    trace_label: &str,
) -> Result<ThroughputMeasurement, Box<dyn std::error::Error>> {
    let trace_path = fresh_temp_file(trace_label, "trace")?;
    let start = Instant::now();
    let status = Command::new(flacx_bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env("FLACX_REQUIRE_INTERACTIVE", "1")
        .env("FLACX_PROGRESS_TRACE", &trace_path)
        .status()?;
    let elapsed_seconds = start.elapsed().as_secs_f64();
    if !status.success() {
        let _ = fs::remove_file(&trace_path);
        return Err(format!("CLI measurement failed with status {status}").into());
    }

    let trace = parse_trace(&trace_path)?;
    let _ = fs::remove_file(&trace_path);

    if !trace.interactive_proven {
        return Err("interactive proof trace did not confirm terminal mode".into());
    }
    if trace.file_runs.is_empty() {
        return Err("interactive proof trace did not record any finished files".into());
    }

    let throughputs = trace
        .file_runs
        .iter()
        .map(|file| throughput_mib_s(file.input_bytes, file.elapsed_seconds))
        .collect::<Vec<_>>();

    Ok(ThroughputMeasurement {
        elapsed_seconds,
        average_per_file_throughput_mib_s: mean(&throughputs),
        file_count: trace.file_runs.len(),
    })
}

fn parse_trace(path: &Path) -> Result<TraceSummary, Box<dyn std::error::Error>> {
    let mut interactive_proven = false;
    let mut file_runs = Vec::new();

    for line in fs::read_to_string(path)?.lines() {
        let fields = parse_trace_fields(line);
        match fields.get("event").map(String::as_str) {
            Some("command") => {
                interactive_proven = fields.get("interactive").is_some_and(|value| value == "1");
            }
            Some("file_finish") => {
                let input_bytes = fields
                    .get("input_bytes")
                    .ok_or("missing input_bytes in trace")?
                    .parse::<u64>()?;
                let elapsed_seconds = fields
                    .get("elapsed_seconds")
                    .ok_or("missing elapsed_seconds in trace")?
                    .parse::<f64>()?;
                file_runs.push(FileRun {
                    input_bytes,
                    elapsed_seconds,
                });
            }
            _ => {}
        }
    }

    Ok(TraceSummary {
        interactive_proven,
        file_runs,
    })
}

fn parse_trace_fields(line: &str) -> BTreeMap<String, String> {
    line.split('\t')
        .filter_map(|segment| segment.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn prepare_decode_corpus(
    wav_root: &Path,
    wav_inputs: &[PathBuf],
    threads: usize,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let output_root = fresh_temp_dir("decode-corpus")?;
    for input in wav_inputs {
        let output = output_root
            .join(input.strip_prefix(wav_root)?)
            .with_extension("flac");
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        Encoder::new(EncoderConfig::default().with_threads(threads)).encode_file(input, &output)?;
    }
    Ok(output_root)
}

fn ensure_parent_terminal_is_interactive() -> Result<(), Box<dyn std::error::Error>> {
    if !io::stderr().is_terminal() {
        return Err("run the interactive parity benchmark from a real terminal session".into());
    }
    Ok(())
}

fn flacx_binary_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let example_dir = env::current_exe()?;
    let release_dir = example_dir
        .parent()
        .and_then(Path::parent)
        .ok_or("failed to locate target release directory")?;
    Ok(release_dir.join(format!("flacx{}", env::consts::EXE_SUFFIX)))
}

fn discover_inputs(
    input_root: &Path,
    extension: &str,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut inputs = WalkDir::new(input_root)
        .follow_links(false)
        .min_depth(1)
        .max_depth(DEFAULT_DEPTH)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| has_extension(entry.path(), extension))
        .map(|entry| entry.into_path())
        .collect::<Vec<_>>();
    inputs.sort_by(|left, right| {
        relative_display_name(left.strip_prefix(input_root).unwrap()).cmp(&relative_display_name(
            right.strip_prefix(input_root).unwrap(),
        ))
    });
    Ok(inputs)
}

fn print_measurement(label: &str, measurement: &ParityMeasurement) {
    for repeat in &measurement.repeats {
        println!(
            "{label} repeat={} order={} folder={:.3}s {:.3} MiB/s ({} files) | single={:.3}s {:.3} MiB/s ({} files) | ratio={:.4}",
            repeat.repeat,
            repeat.order,
            repeat.folder.elapsed_seconds,
            repeat.folder.average_per_file_throughput_mib_s,
            repeat.folder.file_count,
            repeat.single.elapsed_seconds,
            repeat.single.average_per_file_throughput_mib_s,
            repeat.single.file_count,
            repeat.ratio,
        );
    }
    println!("{label} median_ratio={:.4}", measurement.median_ratio);
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
}

fn relative_display_name(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn throughput_mib_s(input_bytes: u64, seconds: f64) -> f64 {
    if seconds <= f64::EPSILON {
        0.0
    } else {
        input_bytes as f64 / seconds / MIB
    }
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len().max(1) as f64
}

fn fresh_temp_dir(label: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos();
    let path = env::temp_dir().join(format!(
        "flacx-directory-parity-{label}-{}-{}",
        std::process::id(),
        unique
    ));
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn fresh_temp_file(label: &str, extension: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos();
    Ok(env::temp_dir().join(format!(
        "flacx-directory-parity-{label}-{}-{unique}.{extension}",
        std::process::id()
    )))
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

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
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
