use std::{
    env,
    fs::{self, File},
    path::{Path, PathBuf},
    process::Command,
};

use flacx::Encoder;

const DEFAULT_REPEATS: usize = 3;
const PINNED_CORES: usize = 8;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    pin_current_process_to_first_n_cores(PINNED_CORES)?;

    if let Some(path) = env::args().nth(1).map(PathBuf::from) {
        run_single_input(&path)?;
    } else {
        run_locked_corpus()?;
    }

    Ok(())
}

fn run_single_input(wav_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let output_path = wav_path.with_extension("flacx.flac");
    let baseline_output_path = wav_path.with_extension("baseline.flac");
    let input_bytes = fs::metadata(wav_path)?.len();

    let flacx_bytes = encode_flacx(wav_path, &output_path)?;
    let flacx_ratio = size_ratio(flacx_bytes, input_bytes);

    println!(
        "flacx(single): encoded={} B ratio={:.6}",
        flacx_bytes, flacx_ratio
    );

    if flac_available() {
        let baseline_bytes = encode_baseline_flac(wav_path, &baseline_output_path)?;
        let baseline_ratio = size_ratio(baseline_bytes, input_bytes);
        let delta_bytes = signed_delta(flacx_bytes, baseline_bytes);
        let delta_ratio = flacx_ratio - baseline_ratio;
        println!(
            "baseline(single, flac -f -8): encoded={} B ratio={:.6} delta={:+} B ({:+.6} ratio)",
            baseline_bytes, baseline_ratio, delta_bytes, delta_ratio
        );
    } else {
        println!("baseline(single, flac -f -8): unavailable (`flac` not found)");
    }

    let _ = fs::remove_file(output_path);
    let _ = fs::remove_file(baseline_output_path);
    Ok(())
}

fn run_locked_corpus() -> Result<(), Box<dyn std::error::Error>> {
    let corpus = load_locked_corpus(Path::new("benchmarks/locked-corpus.txt"))?;
    if !flac_available() {
        return Err(
            "locked-corpus ratio benchmark requires `flac -f -8`, but `flac` was not found".into(),
        );
    }
    let mut flacx_case_medians = Vec::new();
    let mut baseline_case_medians = Vec::new();
    let mut flacx_case_ratios = Vec::new();
    let mut baseline_case_ratios = Vec::new();

    println!(
        "locked corpus: {} cases, {} repeats, pinned cores={} (best effort), baseline=flac -f -8",
        corpus.len(),
        DEFAULT_REPEATS,
        PINNED_CORES
    );

    for case in &corpus {
        let wav_bytes = case.to_wav_bytes();
        let wav_path = temp_path(&format!("{}-input", case.name), "wav");
        fs::write(&wav_path, &wav_bytes)?;
        let input_bytes = wav_bytes.len() as u64;

        let mut case_flacx = Vec::new();
        let mut case_baseline = Vec::new();
        for repeat in 0..DEFAULT_REPEATS {
            let flacx_output = temp_path(&format!("{}-flacx-{repeat}", case.name), "flac");
            let flacx_bytes = encode_flacx(&wav_path, &flacx_output)?;
            case_flacx.push(flacx_bytes);
            let _ = fs::remove_file(flacx_output);

            let baseline_output = temp_path(&format!("{}-baseline-{repeat}", case.name), "flac");
            let baseline_bytes = encode_baseline_flac(&wav_path, &baseline_output)?;
            case_baseline.push(baseline_bytes);
            let _ = fs::remove_file(baseline_output);
        }

        let flacx_bytes = stable_size(&case_flacx, &case.name, "flacx")?;
        let baseline_bytes = stable_size(&case_baseline, &case.name, "baseline")?;
        let flacx_ratio = size_ratio(flacx_bytes, input_bytes);
        let baseline_ratio = size_ratio(baseline_bytes, input_bytes);
        let delta_bytes = signed_delta(flacx_bytes, baseline_bytes);
        let delta_ratio = flacx_ratio - baseline_ratio;

        flacx_case_medians.push(flacx_bytes as f64);
        baseline_case_medians.push(baseline_bytes as f64);
        flacx_case_ratios.push(flacx_ratio);
        baseline_case_ratios.push(baseline_ratio);

        println!(
            "case={:<16} flacx={:>8} B ratio={:.6} baseline={:>8} B ratio={:.6} delta={:+} B ({:+.6} ratio)",
            case.name,
            flacx_bytes,
            flacx_ratio,
            baseline_bytes,
            baseline_ratio,
            delta_bytes,
            delta_ratio
        );

        let _ = fs::remove_file(&wav_path);
    }

    let flacx_median_bytes = median(&mut flacx_case_medians);
    let baseline_median_bytes = median(&mut baseline_case_medians);
    let flacx_median_ratio = median(&mut flacx_case_ratios);
    let baseline_median_ratio = median(&mut baseline_case_ratios);
    let median_delta_bytes = flacx_median_bytes - baseline_median_bytes;
    let median_delta_ratio = flacx_median_ratio - baseline_median_ratio;
    println!(
        "corpus-median: flacx={:.1} B ratio={:.6} baseline={:.1} B ratio={:.6} delta={:+.1} B ({:+.6} ratio)",
        flacx_median_bytes,
        flacx_median_ratio,
        baseline_median_bytes,
        baseline_median_ratio,
        median_delta_bytes,
        median_delta_ratio
    );

    if flacx_median_bytes > baseline_median_bytes * 1.05
        || flacx_median_ratio > baseline_median_ratio * 1.05
    {
        return Err(format!(
            "locked-corpus median encoded size/ratio regressed: flacx={flacx_median_bytes:.1} B ({flacx_median_ratio:.6}) baseline={baseline_median_bytes:.1} B ({baseline_median_ratio:.6})"
        )
        .into());
    }

    Ok(())
}

fn load_locked_corpus(path: &Path) -> Result<Vec<CorpusCase>, Box<dyn std::error::Error>> {
    let raw = fs::read_to_string(path)?;
    let mut cases = Vec::new();
    for line in raw
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
    {
        let parts: Vec<_> = line.split(',').map(str::trim).collect();
        if parts.len() != 6 {
            return Err(format!("invalid corpus line: {line}").into());
        }
        cases.push(CorpusCase {
            name: parts[0].to_string(),
            channels: parts[1].parse()?,
            bits_per_sample: parts[2].parse()?,
            sample_rate: parts[3].parse()?,
            duration_seconds: parts[4].parse()?,
            pattern: Pattern::parse(parts[5])?,
        });
    }
    Ok(cases)
}

#[derive(Clone)]
struct CorpusCase {
    name: String,
    channels: u16,
    bits_per_sample: u16,
    sample_rate: u32,
    duration_seconds: f32,
    pattern: Pattern,
}

impl CorpusCase {
    fn to_wav_bytes(&self) -> Vec<u8> {
        let frames = (self.sample_rate as f32 * self.duration_seconds) as usize;
        let samples = self
            .pattern
            .samples(frames, self.channels, self.bits_per_sample);
        pcm_wav_bytes(
            self.bits_per_sample,
            self.channels,
            self.sample_rate,
            &samples,
        )
    }
}

#[derive(Clone, Copy)]
enum Pattern {
    Music,
    Speech,
    Impulse,
    Constant,
    Ramp,
    Noise,
}

impl Pattern {
    fn parse(value: &str) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(match value {
            "music" => Self::Music,
            "speech" => Self::Speech,
            "impulse" => Self::Impulse,
            "constant" => Self::Constant,
            "ramp" => Self::Ramp,
            "noise" => Self::Noise,
            _ => return Err(format!("unknown pattern `{value}`").into()),
        })
    }

    fn samples(self, frames: usize, channels: u16, bits_per_sample: u16) -> Vec<i32> {
        let amplitude = ((1i64 << (bits_per_sample - 1)) - 1) as i32;
        let soft_limit = ((amplitude as f64) * 0.80) as i32;
        let mut seed = 0x1234_5678u64;
        let mut out = Vec::with_capacity(frames * usize::from(channels));

        for frame in 0..frames {
            let t = frame as f64 / frames.max(1) as f64;
            for channel in 0..channels {
                let phase = frame as f64 / 48.0 + f64::from(channel) * 0.33;
                let sample = match self {
                    Self::Music => {
                        let mix = (phase.sin() * 0.55)
                            + ((phase / 2.7).sin() * 0.30)
                            + ((phase / 7.1).cos() * 0.15);
                        (mix * f64::from(soft_limit)) as i32
                    }
                    Self::Speech => {
                        let envelope = ((frame as f64 / 200.0).sin().abs() * 0.65) + 0.15;
                        let formant = ((frame as f64 / 23.0).sin() * 0.7)
                            + ((frame as f64 / 79.0).sin() * 0.3);
                        (formant * envelope * f64::from(soft_limit)) as i32
                    }
                    Self::Impulse => {
                        if frame % 400 == 0 {
                            if channel == 0 {
                                soft_limit
                            } else {
                                -soft_limit
                            }
                        } else {
                            ((frame as i32 * (channel as i32 + 1) * 17) % 128) - 64
                        }
                    }
                    Self::Constant => soft_limit / 3,
                    Self::Ramp => {
                        let period = 1024i32;
                        let centered = i64::from((frame as i32 % period) - (period / 2));
                        let scaled = (centered * i64::from(soft_limit)) / i64::from(period / 2);
                        scaled as i32
                    }
                    Self::Noise => {
                        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                        let value = ((seed >> 33) as i32) & 0x7fff;
                        value - 0x3fff
                    }
                };

                let clipped = sample.clamp(-soft_limit, soft_limit);
                let slight_pan = if channels == 2 && channel == 1 {
                    ((f64::from(clipped) * (0.75 + t * 0.2)) as i32).clamp(-soft_limit, soft_limit)
                } else {
                    clipped
                };
                out.push(slight_pan);
            }
        }

        out
    }
}

fn pcm_wav_bytes(
    bits_per_sample: u16,
    channels: u16,
    sample_rate: u32,
    samples: &[i32],
) -> Vec<u8> {
    let bytes_per_sample = usize::from(bits_per_sample / 8);
    let block_align = usize::from(channels) * bytes_per_sample;
    let data_bytes = samples.len() * bytes_per_sample;
    let riff_size = 4 + (8 + 16) + (8 + data_bytes);

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

    match bits_per_sample {
        16 => {
            for &sample in samples {
                bytes.extend_from_slice(&(sample as i16).to_le_bytes());
            }
        }
        24 => {
            for &sample in samples {
                let value = sample as u32;
                bytes.extend_from_slice(&[
                    (value & 0xff) as u8,
                    ((value >> 8) & 0xff) as u8,
                    ((value >> 16) & 0xff) as u8,
                ]);
            }
        }
        _ => unreachable!(),
    }

    bytes
}

fn flac_available() -> bool {
    Command::new("flac")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn encode_flacx(wav_path: &Path, output_path: &Path) -> Result<u64, Box<dyn std::error::Error>> {
    Encoder::default().encode_wav_to_flac(File::open(wav_path)?, File::create(output_path)?)?;
    encoded_size(output_path)
}

fn encode_baseline_flac(
    wav_path: &Path,
    output_path: &Path,
) -> Result<u64, Box<dyn std::error::Error>> {
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

    encoded_size(output_path)
}

fn encoded_size(path: &Path) -> Result<u64, Box<dyn std::error::Error>> {
    Ok(fs::metadata(path)?.len())
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
    env::temp_dir().join(format!("flacx-{stem}.{}", extension))
}

#[cfg(test)]
mod tests {
    use super::Pattern;

    #[test]
    fn ramp_pattern_stays_within_24bit_soft_limit() {
        let samples = Pattern::Ramp.samples(96_000, 1, 24);
        let soft_limit = (((1i64 << 23) - 1) as f64 * 0.80) as i32;
        assert!(
            samples
                .into_iter()
                .all(|sample| (-soft_limit..=soft_limit).contains(&sample))
        );
    }
}

#[cfg(windows)]
fn pin_current_process_to_first_n_cores(
    core_count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::ffi::c_void;

    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn SetProcessAffinityMask(handle: *mut c_void, mask: usize) -> i32;
    }

    let limited = core_count.clamp(1, usize::BITS as usize);
    let mask = if limited >= usize::BITS as usize {
        usize::MAX
    } else {
        (1usize << limited) - 1
    };

    let ok = unsafe { SetProcessAffinityMask(GetCurrentProcess(), mask) };
    if ok == 0 {
        return Err("failed to set process affinity".into());
    }

    Ok(())
}

#[cfg(not(windows))]
fn pin_current_process_to_first_n_cores(
    _core_count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}
