use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Instant,
};

use flacx::{DecodeConfig, Decoder, Encoder, EncoderConfig, level::Level};

const DEFAULT_REPEATS: usize = 3;
const PINNED_CORES: usize = 8;
const MIB: f64 = 1024.0 * 1024.0;
const SYNTHETIC_MULTICHANNEL_FRAMES: usize = 48_000 * 5;
const DEFAULT_CORPUS: [&str; 3] = [
    "test-wavs/test1.wav",
    "test-wavs/test2.wav",
    "test-wavs/test3.wav",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    pin_current_process_to_first_n_cores(PINNED_CORES)?;
    let threads = shared_thread_count();

    match env::args().nth(1).as_deref() {
        Some("--multichannel") => run_synthetic_multichannel_benchmark(threads)?,
        Some(path) => run_single_input(&PathBuf::from(path), threads)?,
        None => run_test_wavs_corpus(threads)?,
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

fn run_synthetic_multichannel_benchmark(threads: usize) -> Result<(), Box<dyn std::error::Error>> {
    let fixtures = synthetic_multichannel_fixtures();
    println!(
        "synthetic multichannel corpus: {} layouts, {} repeats, pinned cores={} (best effort), threads={} shared across encode+decode",
        fixtures.len(),
        DEFAULT_REPEATS,
        PINNED_CORES,
        threads
    );

    let mut encode_throughputs = Vec::with_capacity(fixtures.len());
    let mut decode_throughputs = Vec::with_capacity(fixtures.len());
    let mut flac_bytes = Vec::with_capacity(fixtures.len());
    let mut flac_ratios = Vec::with_capacity(fixtures.len());

    for fixture in fixtures {
        let measurement = benchmark_synthetic_fixture(&fixture, threads)?;
        print_fixture_result("layout", Path::new(&fixture.label), &measurement, threads);
        encode_throughputs.push(measurement.encode_throughput_mib_s());
        decode_throughputs.push(measurement.decode_throughput_mib_s());
        flac_bytes.push(measurement.flac_bytes as f64);
        flac_ratios.push(measurement.flac_ratio());
    }

    let encode_median = median(&mut encode_throughputs);
    let decode_median = median(&mut decode_throughputs);
    let flac_bytes_median = median(&mut flac_bytes);
    let flac_ratio_median = median(&mut flac_ratios);

    println!(
        "multichannel-median: encode={:.3} MiB/s decode={:.3} MiB/s delta={:+.3} MiB/s | flac={:.1} B ratio={:.6}",
        encode_median,
        decode_median,
        decode_median - encode_median,
        flac_bytes_median,
        flac_ratio_median
    );

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

fn benchmark_synthetic_fixture(
    fixture: &SyntheticFixture,
    threads: usize,
) -> Result<FixtureMeasurement, Box<dyn std::error::Error>> {
    let wav_bytes = synthetic_multichannel_wav_bytes(
        fixture.channels,
        fixture.valid_bits_per_sample,
        fixture.container_bits_per_sample,
        fixture.sample_rate,
        SYNTHETIC_MULTICHANNEL_FRAMES,
    );
    let source_path = temp_path(&fixture.label, "wav");
    fs::write(&source_path, &wav_bytes)?;
    let measurement = benchmark_fixture(&source_path, threads);
    let _ = fs::remove_file(source_path);
    measurement
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

#[derive(Debug, Clone, Copy)]
struct SyntheticFixture {
    label: &'static str,
    channels: u16,
    valid_bits_per_sample: u16,
    container_bits_per_sample: u16,
    sample_rate: u32,
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

fn synthetic_multichannel_fixtures() -> [SyntheticFixture; 6] {
    [
        SyntheticFixture {
            label: "ch3-16bit",
            channels: 3,
            valid_bits_per_sample: 16,
            container_bits_per_sample: 16,
            sample_rate: 48_000,
        },
        SyntheticFixture {
            label: "ch4-24bit",
            channels: 4,
            valid_bits_per_sample: 24,
            container_bits_per_sample: 24,
            sample_rate: 48_000,
        },
        SyntheticFixture {
            label: "ch5-20in24",
            channels: 5,
            valid_bits_per_sample: 20,
            container_bits_per_sample: 24,
            sample_rate: 44_100,
        },
        SyntheticFixture {
            label: "ch6-16bit",
            channels: 6,
            valid_bits_per_sample: 16,
            container_bits_per_sample: 16,
            sample_rate: 48_000,
        },
        SyntheticFixture {
            label: "ch7-24bit",
            channels: 7,
            valid_bits_per_sample: 24,
            container_bits_per_sample: 24,
            sample_rate: 48_000,
        },
        SyntheticFixture {
            label: "ch8-24bit",
            channels: 8,
            valid_bits_per_sample: 24,
            container_bits_per_sample: 24,
            sample_rate: 96_000,
        },
    ]
}

fn synthetic_multichannel_wav_bytes(
    channels: u16,
    valid_bits_per_sample: u16,
    container_bits_per_sample: u16,
    sample_rate: u32,
    frames: usize,
) -> Vec<u8> {
    let samples = synthetic_samples(channels, frames, valid_bits_per_sample);
    if channels <= 2 && valid_bits_per_sample == container_bits_per_sample {
        pcm_wav_bytes(container_bits_per_sample, channels, sample_rate, &samples)
    } else {
        extensible_pcm_wav_bytes(
            valid_bits_per_sample,
            container_bits_per_sample,
            channels,
            sample_rate,
            ordinary_channel_mask(channels).expect("ordinary mask"),
            &samples,
        )
    }
}

fn synthetic_samples(channels: u16, frames: usize, valid_bits_per_sample: u16) -> Vec<i32> {
    let amplitude = ((1i64 << valid_bits_per_sample.saturating_sub(1)) - 1).min(0x7fff_ffff) as i32;
    let cycle = (usize::from(channels) * 97).max(257);
    let mut samples = Vec::with_capacity(frames * usize::from(channels));
    for frame in 0..frames {
        let base = (((frame % cycle) as i32 * 65_521) % amplitude.max(1)) - (amplitude / 2);
        for channel in 0..channels {
            let channel_bias = (i32::from(channel) * 1_013) & (amplitude >> 3);
            let sample = (base + channel_bias).clamp(-(amplitude / 2), amplitude / 2);
            samples.push(sample);
        }
    }
    samples
}

fn ordinary_channel_mask(channels: u16) -> Option<u32> {
    match channels {
        1 => Some(0x0004),
        2 => Some(0x0003),
        3 => Some(0x0007),
        4 => Some(0x0033),
        5 => Some(0x0037),
        6 => Some(0x003F),
        7 => Some(0x070F),
        8 => Some(0x063F),
        _ => None,
    }
}

fn pcm_wav_bytes(
    bits_per_sample: u16,
    channels: u16,
    sample_rate: u32,
    samples: &[i32],
) -> Vec<u8> {
    let bytes_per_sample = bytes_per_sample(bits_per_sample);
    let block_align = usize::from(channels) * bytes_per_sample;
    let data_bytes = samples.len() * bytes_per_sample;
    let riff_size = 4 + (8 + 16usize) + (8 + data_bytes);

    let mut bytes = Vec::with_capacity(12 + 8 + 16 + 8 + data_bytes);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(riff_size as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");

    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * block_align as u32).to_le_bytes());
    bytes.extend_from_slice(&(block_align as u16).to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());

    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    write_pcm_samples(&mut bytes, bits_per_sample, samples);
    bytes
}

fn extensible_pcm_wav_bytes(
    valid_bits_per_sample: u16,
    container_bits_per_sample: u16,
    channels: u16,
    sample_rate: u32,
    channel_mask: u32,
    samples: &[i32],
) -> Vec<u8> {
    const PCM_GUID: [u8; 16] = [
        0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B,
        0x71,
    ];

    let bytes_per_sample = bytes_per_sample(container_bits_per_sample);
    let block_align = usize::from(channels) * bytes_per_sample;
    let data_bytes = samples.len() * bytes_per_sample;
    let riff_size = 4 + (8 + 40usize) + (8 + data_bytes);

    let mut bytes = Vec::with_capacity(12 + 8 + 40 + 8 + data_bytes);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(riff_size as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");

    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&40u32.to_le_bytes());
    bytes.extend_from_slice(&0xFFFEu16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * block_align as u32).to_le_bytes());
    bytes.extend_from_slice(&(block_align as u16).to_le_bytes());
    bytes.extend_from_slice(&container_bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(&22u16.to_le_bytes());
    bytes.extend_from_slice(&valid_bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(&channel_mask.to_le_bytes());
    bytes.extend_from_slice(&PCM_GUID);

    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    write_left_aligned_samples(
        &mut bytes,
        container_bits_per_sample,
        valid_bits_per_sample,
        samples,
    );
    bytes
}

fn write_pcm_samples(bytes: &mut Vec<u8>, bits_per_sample: u16, samples: &[i32]) {
    for &sample in samples {
        match bits_per_sample {
            8 => bytes.push(sample as u8),
            16 => bytes.extend_from_slice(&(sample as i16).to_le_bytes()),
            24 => {
                let value = sample as u32;
                bytes.extend_from_slice(&[
                    (value & 0xff) as u8,
                    ((value >> 8) & 0xff) as u8,
                    ((value >> 16) & 0xff) as u8,
                ]);
            }
            32 => bytes.extend_from_slice(&sample.to_le_bytes()),
            _ => unreachable!("unsupported PCM container width"),
        }
    }
}

fn write_left_aligned_samples(
    bytes: &mut Vec<u8>,
    container_bits_per_sample: u16,
    valid_bits_per_sample: u16,
    samples: &[i32],
) {
    let shift = container_bits_per_sample - valid_bits_per_sample;
    for &sample in samples {
        let shifted = if shift == 0 { sample } else { sample << shift };
        match container_bits_per_sample {
            8 => bytes.push(shifted as u8),
            16 => bytes.extend_from_slice(&(shifted as i16).to_le_bytes()),
            24 => {
                let value = shifted as u32;
                bytes.extend_from_slice(&[
                    (value & 0xff) as u8,
                    ((value >> 8) & 0xff) as u8,
                    ((value >> 16) & 0xff) as u8,
                ]);
            }
            32 => bytes.extend_from_slice(&shifted.to_le_bytes()),
            _ => unreachable!("unsupported extensible PCM container width"),
        }
    }
}

fn bytes_per_sample(bits_per_sample: u16) -> usize {
    match bits_per_sample {
        8 => 1,
        16 => 2,
        24 => 3,
        32 => 4,
        _ => unreachable!("unsupported bits per sample"),
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
