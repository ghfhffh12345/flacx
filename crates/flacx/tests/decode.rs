#[cfg(feature = "progress")]
use std::{collections::BTreeMap, path::PathBuf, sync::OnceLock};
use std::{fs, io::Cursor, sync::mpsc, thread, thread::available_parallelism, time::Duration};

use flacx::builtin::{decode_bytes, decode_file};
use flacx::{DecodeConfig, EncoderConfig, PcmContainer, level::Level};
#[cfg(feature = "progress")]
use flacx::{DecodePcmStream, EncodePcmStream, Metadata, PcmSpec, StreamInfo};
#[cfg(feature = "progress")]
use flacx::{ProgressSnapshot, read_flac_reader};

mod support;
use support::TestDecoder as DecodeHarness;
use support::TestEncoder as Encoder;

#[cfg(feature = "caf")]
use support::is_caf_bytes;
#[cfg(feature = "progress")]
use support::{
    LARGE_STREAMING_DECODE_SAMPLE_COUNT, large_streaming_decode_flac_bytes,
    large_streaming_decode_wav_bytes,
};
use support::{
    ParsedFlacBlockingStrategy, ParsedFlacCodedNumberKind, ParsedMetadataBlock, application_block,
    corrupt_first_flac_frame_sample_number, corrupt_last_frame_crc, corrupt_magic, cue_chunk,
    cuesheet_block, extensible_pcm_wav_bytes, flac_frames, flac_metadata_blocks, info_list_chunk,
    is_w64_bytes, ordinary_channel_mask, parse_first_flac_frame_header, parse_fxmd_chunk_payload,
    parse_wav_format, pcm_wav_bytes, picture_block, raw_cuesheet_block, raw_seektable_block,
    replace_flac_optional_metadata, rewrite_streaminfo_md5, rich_cuesheet_payload, sample_fixture,
    seektable_block, streaminfo_md5, truncate_bytes, unique_temp_path, vorbis_comment_block,
    vorbis_comments, wav_chunk_payloads, wav_cue_points, wav_data_bytes, wav_info_entries,
    wav_with_chunks,
};
#[cfg(feature = "aiff")]
use support::{is_aifc_bytes, is_aiff_bytes};

fn decode_thread_variants() -> [usize; 2] {
    [1, DecodeConfig::default().threads.max(2)]
}

#[cfg(feature = "progress")]
struct DecodeProfileGuard {
    path: PathBuf,
}

#[cfg(feature = "progress")]
impl DecodeProfileGuard {
    fn new() -> Self {
        let path = unique_temp_path("decode-profile");
        flacx::__set_decode_profile_path_for_current_thread(Some(path.clone()));
        Self { path }
    }

    fn try_summary(&self) -> Option<BTreeMap<String, usize>> {
        fs::read_to_string(&self.path)
            .ok()?
            .lines()
            .rev()
            .find(|line| line.starts_with("event=decode_session_summary"))
            .map(|line| {
                line.split('\t')
                    .skip(1)
                    .filter_map(|field| field.split_once('='))
                    .map(|(key, value)| (key.to_string(), value.parse().unwrap()))
                    .collect()
            })
    }

    fn summary(&self) -> BTreeMap<String, usize> {
        self.try_summary().unwrap()
    }
}

#[cfg(feature = "progress")]
impl Drop for DecodeProfileGuard {
    fn drop(&mut self) {
        flacx::__set_decode_profile_path_for_current_thread(None);
        let _ = fs::remove_file(&self.path);
    }
}

fn decoder_for_threads(threads: usize) -> DecodeHarness {
    DecodeHarness::new(DecodeConfig::default().with_threads(threads))
}

fn decode_bytes_with_threads(flac: &[u8], threads: usize) -> Vec<u8> {
    decoder_for_threads(threads).decode_bytes(flac).unwrap()
}

fn run_decode_with_timeout<T: Send + 'static>(
    job: impl FnOnce() -> flacx::Result<T> + Send + 'static,
) -> flacx::Result<T> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(job());
    });
    receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("decode job should complete without deadlock")
}

fn assert_round_trips_bytes_exactly(wav: &[u8], flac: &[u8]) {
    let expected_format = parse_wav_format(wav);
    for threads in decode_thread_variants() {
        let decoded = decode_bytes_with_threads(flac, threads);
        let decoded_format = parse_wav_format(&decoded);
        assert_eq!(
            wav_data_bytes(&decoded),
            wav_data_bytes(wav),
            "decode_bytes changed audio bytes for threads={threads}"
        );
        assert_eq!(
            decoded_format.channels, expected_format.channels,
            "decode_bytes changed channel count for threads={threads}"
        );
        assert_eq!(
            decoded_format.sample_rate, expected_format.sample_rate,
            "decode_bytes changed sample rate for threads={threads}"
        );
        assert_eq!(
            decoded_format.bits_per_sample, expected_format.bits_per_sample,
            "decode_bytes changed bit depth for threads={threads}"
        );
    }
}

fn assert_decode_error_stable(flac: &[u8]) {
    let mut messages = Vec::new();
    for threads in decode_thread_variants() {
        let error = decoder_for_threads(threads).decode_bytes(flac).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("invalid flac") || message.contains("decode error"),
            "unexpected decode error for threads={threads}: {message}"
        );
        messages.push((threads, message));
    }

    let first = &messages[0].1;
    for (threads, message) in messages.iter().skip(1) {
        assert_eq!(
            message, first,
            "decode error changed across thread counts for threads={threads}"
        );
    }
}

#[cfg(feature = "progress")]
struct StreamingOnlyDecodeStream {
    spec: PcmSpec,
    stream_info: StreamInfo,
    samples: Vec<i32>,
    chunk_frames: usize,
    cursor: usize,
    completed_frames: usize,
}

#[cfg(feature = "progress")]
impl StreamingOnlyDecodeStream {
    fn new(spec: PcmSpec, stream_info: StreamInfo, samples: Vec<i32>, chunk_frames: usize) -> Self {
        Self {
            spec,
            stream_info,
            samples,
            chunk_frames,
            cursor: 0,
            completed_frames: 0,
        }
    }
}

#[cfg(feature = "progress")]
impl EncodePcmStream for StreamingOnlyDecodeStream {
    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> flacx::Result<usize> {
        let channels = usize::from(self.spec.channels);
        let remaining_frames =
            usize::try_from(self.spec.total_samples).unwrap() - self.cursor / channels;
        if remaining_frames == 0 {
            return Ok(0);
        }

        let frames = remaining_frames.min(self.chunk_frames).min(max_frames);
        let sample_count = frames * channels;
        let next = self.cursor + sample_count;
        output.extend_from_slice(&self.samples[self.cursor..next]);
        self.cursor = next;
        self.completed_frames += 1;
        Ok(frames)
    }
}

#[cfg(feature = "progress")]
impl DecodePcmStream for StreamingOnlyDecodeStream {
    fn total_input_frames(&self) -> usize {
        usize::try_from(self.spec.total_samples)
            .unwrap()
            .div_ceil(self.chunk_frames)
    }

    fn completed_input_frames(&self) -> usize {
        self.completed_frames
    }

    fn stream_info(&self) -> StreamInfo {
        self.stream_info
    }

    fn take_decoded_samples(&mut self) -> flacx::Result<Option<(Vec<i32>, usize)>> {
        panic!("decode should stream through read_chunk instead of take_decoded_samples")
    }
}

#[test]
fn decode_config_default_threads_matches_available_parallelism() {
    let expected = available_parallelism().map(usize::from).unwrap_or(1);
    assert_eq!(DecodeConfig::default().threads, expected);
}

#[test]
fn decode_config_with_threads_clamps_to_one() {
    assert_eq!(DecodeConfig::default().with_threads(0).threads, 1);
}

#[test]
fn decode_pcm_stream_trait_does_not_expose_profile_hooks() {
    let source = include_str!("../src/read.rs");
    let trait_start = source
        .find("pub trait DecodePcmStream: EncodePcmStream {")
        .unwrap();
    let trait_end = source[trait_start..]
        .find("/// Owned decode-side handoff")
        .map(|offset| trait_start + offset)
        .unwrap();
    let trait_source = &source[trait_start..trait_end];
    assert!(
        !trait_source.contains("fn release_decode_output_buffer"),
        "decode profiling lifecycle must remain internal to preserve the public DecodePcmStream API"
    );
    assert!(
        !trait_source.contains("fn finish_successful_decode_profile"),
        "decode profiling lifecycle must remain internal to preserve the public DecodePcmStream API"
    );
}

#[cfg(feature = "progress")]
#[test]
fn decode_source_prefers_streaming_chunks_over_materialized_samples() {
    let total_samples = LARGE_STREAMING_DECODE_SAMPLE_COUNT;
    let chunk_frames = 4_194_304usize;
    let samples = sample_fixture(1, total_samples);
    let wav = pcm_wav_bytes(16, 1, 44_100, &samples);
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let spec = PcmSpec {
        sample_rate: 44_100,
        channels: 1,
        bits_per_sample: 16,
        total_samples: total_samples as u64,
        bytes_per_sample: 2,
        channel_mask: ordinary_channel_mask(1).expect("mono channel mask"),
    };
    let mut stream_info =
        StreamInfo::new(44_100, 1, 16, total_samples as u64, streaminfo_md5(&flac));
    stream_info.update_block_size(4_096);
    let mut output = Cursor::new(Vec::new());
    let mut progress = Vec::new();
    let mut decoder = DecodeConfig::default()
        .with_threads(1)
        .into_decoder(&mut output);

    let summary = decoder
        .decode_source_with_progress(
            flacx::DecodeSource::new(
                Metadata::new(),
                StreamingOnlyDecodeStream::new(spec, stream_info, samples, chunk_frames),
            ),
            |update| {
                progress.push(update);
                Ok(())
            },
        )
        .unwrap();

    assert_eq!(summary.total_samples, total_samples as u64);
    assert!(progress.len() > 1, "expected multi-chunk progress updates");
    assert_eq!(
        progress.first().unwrap().processed_samples,
        chunk_frames as u64
    );
    assert_eq!(
        progress.last().unwrap().processed_samples,
        total_samples as u64
    );
    assert_eq!(wav_data_bytes(&output.into_inner()), wav_data_bytes(&wav));
}

#[cfg(feature = "progress")]
fn decode_large_streaming_fixture_with_progress(
    threads: usize,
) -> (Vec<u8>, flacx::DecodeSummary, Vec<ProgressSnapshot>) {
    let flac = large_streaming_decode_flac_bytes(threads);
    let reader = read_flac_reader(Cursor::new(&flac)).unwrap();
    let mut output = Cursor::new(Vec::new());
    let mut progress = Vec::new();
    let mut decoder = DecodeConfig::default()
        .with_threads(threads)
        .into_decoder(&mut output);
    let summary = decoder
        .decode_source_with_progress(reader.into_decode_source(), |update| {
            progress.push(update);
            Ok(())
        })
        .unwrap();
    (output.into_inner(), summary, progress)
}

#[cfg(feature = "progress")]
struct LargeStreamingDecodeRun {
    decoded: Vec<u8>,
    summary: flacx::DecodeSummary,
    progress: Vec<ProgressSnapshot>,
}

#[cfg(feature = "progress")]
fn cached_large_streaming_decode_run() -> &'static LargeStreamingDecodeRun {
    static RUN: OnceLock<LargeStreamingDecodeRun> = OnceLock::new();
    RUN.get_or_init(|| {
        let (decoded, summary, progress) = decode_large_streaming_fixture_with_progress(1);
        LargeStreamingDecodeRun {
            decoded,
            summary,
            progress,
        }
    })
}

#[cfg(feature = "progress")]
#[test]
fn large_streaming_decode_fixture_stays_above_eager_threshold() {
    let source = include_str!("../src/decode_output.rs");
    assert!(
        source.contains("const EAGER_DECODE_TOTAL_SAMPLES_THRESHOLD: u64 = 8 * 1024 * 1024;"),
        "keep this guard in sync with the large streaming decode fixture"
    );
    assert!(
        (LARGE_STREAMING_DECODE_SAMPLE_COUNT as u64) > 8 * 1024 * 1024,
        "large streaming decode fixture must remain above the eager materialization threshold"
    );
}

#[cfg(feature = "progress")]
#[test]
fn real_reader_large_decode_prefers_streaming_branch() {
    let run = cached_large_streaming_decode_run();

    assert_eq!(
        wav_data_bytes(&run.decoded),
        wav_data_bytes(&large_streaming_decode_wav_bytes())
    );
    assert!(
        run.progress.len() > 1,
        "real-reader large decode should stream multiple progress updates instead of materializing eagerly"
    );
}

#[cfg(feature = "progress")]
#[test]
fn real_reader_large_decode_progress_keeps_total_frames_zero() {
    let run = cached_large_streaming_decode_run();

    assert_eq!(
        run.summary.total_samples,
        LARGE_STREAMING_DECODE_SAMPLE_COUNT as u64
    );
    assert!(
        !run.progress.is_empty(),
        "expected progress updates for the large streaming fixture"
    );
    assert!(
        run.progress.iter().all(|update| update.total_frames == 0),
        "all progress snapshots should preserve total_frames=0 for the real-reader streaming path"
    );
    assert_eq!(
        run.progress.last().unwrap().processed_samples,
        LARGE_STREAMING_DECODE_SAMPLE_COUNT as u64
    );
}

#[cfg(feature = "progress")]
#[test]
fn streaming_decode_session_reports_bounded_residency() {
    let profile = DecodeProfileGuard::new();
    let flac = large_streaming_decode_flac_bytes(1);
    let reader = read_flac_reader(Cursor::new(&flac)).unwrap();
    let mut output = Cursor::new(Vec::new());
    let mut progress = Vec::new();
    let mut decoder = DecodeConfig::default()
        .with_threads(1)
        .into_decoder(&mut output);

    let summary = decoder
        .decode_source_with_progress(reader.into_decode_source(), |update| {
            progress.push(update);
            Ok(())
        })
        .unwrap();
    let profile_summary = profile.summary();
    let first_chunk_frames = progress.first().unwrap().processed_samples as usize;
    let worker_count = *profile_summary.get("worker_count").unwrap();
    let queue_limit = *profile_summary.get("queue_limit").unwrap();
    let target_pcm_frames = *profile_summary.get("target_pcm_frames").unwrap();

    assert_eq!(
        summary.total_samples,
        LARGE_STREAMING_DECODE_SAMPLE_COUNT as u64
    );
    assert_eq!(worker_count, 1);
    assert!(queue_limit >= worker_count);
    assert!(target_pcm_frames > 0);

    let peak_inflight_packets = *profile_summary.get("peak_inflight_packets").unwrap();
    assert!(
        (1..=queue_limit).contains(&peak_inflight_packets),
        "peak inflight packets should stay within the bounded decode window: {peak_inflight_packets}"
    );

    let peak_inflight_pcm_frames = *profile_summary.get("peak_inflight_pcm_frames").unwrap();
    assert!(
        peak_inflight_pcm_frames >= first_chunk_frames,
        "peak inflight pcm frames should include the decoded chunk handed to the container writer: peak={peak_inflight_pcm_frames}, first_chunk={first_chunk_frames}"
    );
    assert!(
        peak_inflight_pcm_frames <= first_chunk_frames + queue_limit * target_pcm_frames,
        "peak inflight pcm frames should remain bounded by the writer chunk plus the internal decode window: peak={peak_inflight_pcm_frames}, first_chunk={first_chunk_frames}, queue_limit={queue_limit}, target_pcm_frames={target_pcm_frames}"
    );
}

#[cfg(feature = "progress")]
#[test]
fn failed_streaming_decode_does_not_emit_session_summary() {
    let profile = DecodeProfileGuard::new();
    let flac = large_streaming_decode_flac_bytes(1);
    let mut bad_md5 = streaminfo_md5(&flac);
    bad_md5[0] ^= 0xFF;
    let corrupt = rewrite_streaminfo_md5(&flac, bad_md5);
    let reader = read_flac_reader(Cursor::new(&corrupt)).unwrap();
    let mut output = Cursor::new(Vec::new());
    let mut decoder = DecodeConfig::default()
        .with_threads(1)
        .into_decoder(&mut output);

    let error = decoder
        .decode_source(reader.into_decode_source())
        .unwrap_err();

    assert_eq!(error.to_string(), "invalid flac: STREAMINFO MD5 mismatch");
    assert!(
        profile.try_summary().is_none(),
        "streaming decode profile summary should only be emitted after end-to-end decode success"
    );
}

#[cfg(feature = "progress")]
#[test]
fn large_streaming_decode_uses_background_session_and_matches_single_thread_output() {
    let threads = 4;
    let flac = large_streaming_decode_flac_bytes(threads);

    let single_threaded = decode_bytes_with_threads(&flac, 1);
    let multi_threaded = decode_bytes_with_threads(&flac, threads);

    assert_eq!(wav_data_bytes(&single_threaded), wav_data_bytes(&multi_threaded));
}

#[cfg(feature = "progress")]
#[test]
fn matched_large_streaming_decode_stays_bit_exact_across_thread_counts() {
    let flac = large_streaming_decode_flac_bytes(8);
    let single_threaded = decode_bytes_with_threads(&flac, 1);
    let eight_threaded = decode_bytes_with_threads(&flac, 8);

    assert_eq!(single_threaded, eight_threaded);
}

#[cfg(feature = "progress")]
#[test]
fn streaming_decode_source_error_cancels_background_session_without_deadlock() {
    let flac = truncate_bytes(&large_streaming_decode_flac_bytes(4), 4096);

    let error = run_decode_with_timeout(move || decoder_for_threads(4).decode_bytes(&flac))
        .unwrap_err();

    assert!(
        error.to_string().contains("invalid flac") || error.to_string().contains("decode error"),
        "{error}"
    );
}

#[cfg(feature = "progress")]
#[test]
fn streaming_decode_progress_error_cancels_background_session_without_deadlock() {
    let flac = large_streaming_decode_flac_bytes(4);

    let error = run_decode_with_timeout(move || {
        let reader = read_flac_reader(Cursor::new(flac)).unwrap();
        let mut output = Cursor::new(Vec::new());
        let mut decoder = DecodeConfig::default()
            .with_threads(4)
            .into_decoder(&mut output);
        decoder.decode_source_with_progress(reader.into_decode_source(), |progress| {
            if progress.completed_frames >= 2 {
                Err(flacx::Error::Decode("injected progress failure".into()))
            } else {
                Ok(())
            }
        })
    })
    .unwrap_err();

    assert!(error.to_string().contains("injected progress failure"));
}

#[test]
fn round_trips_16bit_mono_wav_bytes_exactly() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::new(EncoderConfig::default().with_level(Level::Level0))
        .encode_bytes(&wav)
        .unwrap();

    assert_round_trips_bytes_exactly(&wav, &flac);
}

#[test]
fn round_trips_16bit_stereo_wav_bytes_exactly() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 6_144));
    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&wav)
        .unwrap();

    assert_round_trips_bytes_exactly(&wav, &flac);
}

#[test]
fn round_trips_24bit_mono_wav_bytes_exactly() {
    let samples: Vec<i32> = (0..5_000)
        .map(|index| ((index * 9_731) % 16_000_000) - 8_000_000)
        .collect();
    let wav = pcm_wav_bytes(24, 1, 96_000, &samples);
    let flac = Encoder::new(EncoderConfig::default().with_threads(3))
        .encode_bytes(&wav)
        .unwrap();

    assert_round_trips_bytes_exactly(&wav, &flac);
}

#[test]
fn round_trips_partial_tail_block_exactly() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 5_111));
    let flac = Encoder::new(
        EncoderConfig::default()
            .with_level(Level::Level0)
            .with_block_size(576),
    )
    .encode_bytes(&wav)
    .unwrap();

    assert_round_trips_bytes_exactly(&wav, &flac);
}

#[test]
fn decodes_variable_blocksize_sample_number_coded_fixture() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 4_352));
    let flac = Encoder::new(
        EncoderConfig::default()
            .with_threads(2)
            .with_block_schedule(vec![576, 1_152, 576, 2_048]),
    )
    .encode_bytes(&wav)
    .unwrap();
    let header = parse_first_flac_frame_header(&flac);

    assert_eq!(
        header.blocking_strategy,
        ParsedFlacBlockingStrategy::Variable
    );
    assert_eq!(
        header.coded_number_kind,
        ParsedFlacCodedNumberKind::SampleNumber
    );
    assert_eq!(header.coded_number_value, 0);
    assert_round_trips_bytes_exactly(&wav, &flac);
}

#[test]
fn decodes_legal_streaminfo_only_sample_rate_from_zero_header_code() {
    let wav = pcm_wav_bytes(16, 1, 700_001, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let header = parse_first_flac_frame_header(&flac);
    let decoded = decode_bytes(&flac).unwrap();
    let format = parse_wav_format(&decoded);

    assert_eq!(header.sample_rate_bits, 0b0000);
    assert_round_trips_bytes_exactly(&wav, &flac);
    assert_eq!(format.sample_rate, 700_001);
}

#[test]
fn decodes_large_block_sizes_above_32768_end_to_end() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 40_000));
    let flac = Encoder::new(EncoderConfig::default().with_block_size(40_000))
        .encode_bytes(&wav)
        .unwrap();
    let header = parse_first_flac_frame_header(&flac);

    assert_eq!(header.block_size_bits, 0b0111);
    assert_round_trips_bytes_exactly(&wav, &flac);
}

#[test]
fn rejects_variable_blocksize_fixture_with_wrong_sample_number() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 4_352));
    let flac = Encoder::new(
        EncoderConfig::default()
            .with_threads(2)
            .with_block_schedule(vec![576, 1_152, 576, 2_048]),
    )
    .encode_bytes(&wav)
    .unwrap();
    let corrupt = corrupt_first_flac_frame_sample_number(&flac, 1);

    assert_decode_error_stable(&corrupt);
}

#[test]
fn round_trips_constant_signal_exactly() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &vec![12_345; 6_144]);
    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&wav)
        .unwrap();

    assert_round_trips_bytes_exactly(&wav, &flac);
}

#[test]
fn decode_file_writes_identical_wav_bytes() {
    let wav = pcm_wav_bytes(16, 1, 32_000, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    fs::write(&input_path, flac).unwrap();

    for threads in decode_thread_variants() {
        let output_path = unique_temp_path("wav");
        let summary = decoder_for_threads(threads)
            .decode_file(&input_path, &output_path)
            .unwrap();

        assert_eq!(summary.total_samples, 2_048);
        assert_eq!(
            wav_data_bytes(&fs::read(&output_path).unwrap()),
            wav_data_bytes(&wav),
            "decode_file changed audio output for threads={threads}"
        );

        let _ = fs::remove_file(output_path);
    }

    let _ = fs::remove_file(input_path);
}

#[cfg(feature = "wav")]
#[test]
fn decode_bytes_can_emit_rf64_when_requested() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();

    let decoded =
        DecodeHarness::new(DecodeConfig::default().with_output_container(PcmContainer::Rf64))
            .decode_bytes(&flac)
            .unwrap();

    assert!(decoded.starts_with(b"RF64"));
    let reencoded = Encoder::default().encode_bytes(&decoded).unwrap();
    let round_tripped = decode_bytes(&reencoded).unwrap();
    assert_eq!(wav_data_bytes(&round_tripped), wav_data_bytes(&wav));
}

#[cfg(feature = "wav")]
#[test]
fn decode_bytes_can_emit_wave64_when_requested() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();

    let decoded =
        DecodeHarness::new(DecodeConfig::default().with_output_container(PcmContainer::Wave64))
            .decode_bytes(&flac)
            .unwrap();

    assert!(is_w64_bytes(&decoded));
    let reencoded = Encoder::default().encode_bytes(&decoded).unwrap();
    let round_tripped = decode_bytes(&reencoded).unwrap();
    assert_eq!(wav_data_bytes(&round_tripped), wav_data_bytes(&wav));
}

#[cfg(feature = "wav")]
#[test]
fn decode_file_infers_wave64_from_output_extension() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 1_024));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("w64");
    fs::write(&input_path, flac).unwrap();

    DecodeHarness::default()
        .decode_file(&input_path, &output_path)
        .unwrap();

    let decoded = fs::read(&output_path).unwrap();
    assert!(is_w64_bytes(&decoded));
    let reencoded = Encoder::default().encode_bytes(&decoded).unwrap();
    let round_tripped = decode_bytes(&reencoded).unwrap();
    assert_eq!(wav_data_bytes(&round_tripped), wav_data_bytes(&wav));

    let _ = fs::remove_file(output_path);
    let _ = fs::remove_file(input_path);
}

#[cfg(feature = "wav")]
#[test]
fn decode_file_infers_rf64_from_output_extension() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 1_024));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("rf64");
    fs::write(&input_path, flac).unwrap();

    DecodeHarness::default()
        .decode_file(&input_path, &output_path)
        .unwrap();

    let decoded = fs::read(&output_path).unwrap();
    assert!(decoded.starts_with(b"RF64"));
    let reencoded = Encoder::default().encode_bytes(&decoded).unwrap();
    let round_tripped = decode_bytes(&reencoded).unwrap();
    assert_eq!(wav_data_bytes(&round_tripped), wav_data_bytes(&wav));

    let _ = fs::remove_file(output_path);
    let _ = fs::remove_file(input_path);
}

#[cfg(feature = "aiff")]
#[test]
fn decode_bytes_can_emit_aiff_when_requested() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();

    let decoded =
        DecodeHarness::new(DecodeConfig::default().with_output_container(PcmContainer::Aiff))
            .decode_bytes(&flac)
            .unwrap();

    assert!(is_aiff_bytes(&decoded));
    let reencoded = Encoder::default().encode_bytes(&decoded).unwrap();
    let round_tripped = decode_bytes(&reencoded).unwrap();
    assert_eq!(wav_data_bytes(&round_tripped), wav_data_bytes(&wav));
}

#[cfg(feature = "aiff")]
#[test]
fn decode_bytes_can_emit_aiff_for_ordinary_multichannel_layouts() {
    let wav = extensible_pcm_wav_bytes(
        16,
        16,
        4,
        48_000,
        ordinary_channel_mask(4).unwrap(),
        &sample_fixture(4, 1_024),
    );
    let flac = Encoder::default().encode_bytes(&wav).unwrap();

    let decoded =
        DecodeHarness::new(DecodeConfig::default().with_output_container(PcmContainer::Aiff))
            .decode_bytes(&flac)
            .unwrap();

    assert!(is_aiff_bytes(&decoded));
    let reencoded = Encoder::default().encode_bytes(&decoded).unwrap();
    let round_tripped = decode_bytes(&reencoded).unwrap();
    assert_eq!(wav_data_bytes(&round_tripped), wav_data_bytes(&wav));
}

#[cfg(feature = "aiff")]
#[test]
fn decode_bytes_can_emit_aifc_when_requested() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();

    let decoded =
        DecodeHarness::new(DecodeConfig::default().with_output_container(PcmContainer::Aifc))
            .decode_bytes(&flac)
            .unwrap();

    assert!(is_aifc_bytes(&decoded));
    let reencoded = Encoder::default().encode_bytes(&decoded).unwrap();
    let round_tripped = decode_bytes(&reencoded).unwrap();
    assert_eq!(wav_data_bytes(&round_tripped), wav_data_bytes(&wav));
}

#[cfg(feature = "caf")]
#[test]
fn decode_bytes_can_emit_caf_when_requested() {
    let wav = extensible_pcm_wav_bytes(
        16,
        16,
        4,
        48_000,
        ordinary_channel_mask(4).unwrap(),
        &sample_fixture(4, 1_024),
    );
    let flac = Encoder::default().encode_bytes(&wav).unwrap();

    let decoded =
        DecodeHarness::new(DecodeConfig::default().with_output_container(PcmContainer::Caf))
            .decode_bytes(&flac)
            .unwrap();

    assert!(is_caf_bytes(&decoded));
    let reencoded = Encoder::default().encode_bytes(&decoded).unwrap();
    let round_tripped = decode_bytes(&reencoded).unwrap();
    assert_eq!(wav_data_bytes(&round_tripped), wav_data_bytes(&wav));
}

#[cfg(all(feature = "aiff", feature = "caf"))]
#[test]
fn decode_file_infers_aiff_family_and_caf_from_output_extension() {
    type OutputCase = (&'static str, fn(&[u8]) -> bool);

    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 1_024));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    fs::write(&input_path, flac).unwrap();

    #[allow(unused_mut)]
    let cases: &[OutputCase] = &[
        #[cfg(feature = "aiff")]
        ("aiff", is_aiff_bytes as fn(&[u8]) -> bool),
        #[cfg(feature = "aiff")]
        ("aifc", is_aifc_bytes as fn(&[u8]) -> bool),
        #[cfg(feature = "caf")]
        ("caf", is_caf_bytes as fn(&[u8]) -> bool),
    ];

    for &(ext, detector) in cases {
        let output_path = unique_temp_path(ext);
        DecodeHarness::default()
            .decode_file(&input_path, &output_path)
            .unwrap();

        let decoded = fs::read(&output_path).unwrap();
        assert!(detector(&decoded), "unexpected output family for .{ext}");
        let reencoded = Encoder::default().encode_bytes(&decoded).unwrap();
        let round_tripped = decode_bytes(&reencoded).unwrap();
        assert_eq!(wav_data_bytes(&round_tripped), wav_data_bytes(&wav));
        let _ = fs::remove_file(output_path);
    }

    let _ = fs::remove_file(input_path);
}

#[test]
fn decode_file_rejects_unsupported_output_extension() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 1_024));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("raw");
    fs::write(&input_path, flac).unwrap();

    let error = DecodeHarness::default()
        .decode_file(&input_path, &output_path)
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("unsupported decode output extension")
    );

    let _ = fs::remove_file(output_path);
    let _ = fs::remove_file(input_path);
}

#[cfg(feature = "aiff")]
#[test]
fn decode_aiff_output_projects_text_and_marker_metadata_without_fxmd() {
    let wav = wav_with_chunks(
        pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 512)),
        &[
            (
                *b"LIST",
                info_list_chunk(&[
                    (*b"INAM", b"Example"),
                    (*b"IART", b"Artist"),
                    (*b"ICMT", b"Note"),
                ]),
            ),
            (*b"cue ", cue_chunk(&[0, 128])),
        ],
    );
    let flac = Encoder::default().encode_bytes(&wav).unwrap();

    let decoded =
        DecodeHarness::new(DecodeConfig::default().with_output_container(PcmContainer::Aiff))
            .decode_bytes(&flac)
            .unwrap();

    assert!(is_aiff_bytes(&decoded));
    assert!(decoded.windows(4).any(|window| window == b"NAME"));
    assert!(decoded.windows(4).any(|window| window == b"AUTH"));
    assert!(decoded.windows(4).any(|window| window == b"ANNO"));
    assert!(decoded.windows(4).any(|window| window == b"MARK"));
    assert!(!decoded.windows(4).any(|window| window == b"fxmd"));
}

#[cfg(feature = "caf")]
#[test]
fn decode_caf_output_projects_info_and_marker_metadata_without_fxmd() {
    let wav = wav_with_chunks(
        pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 512)),
        &[
            (
                *b"LIST",
                info_list_chunk(&[(*b"INAM", b"Example"), (*b"IART", b"Artist")]),
            ),
            (*b"cue ", cue_chunk(&[0, 64])),
        ],
    );
    let flac = Encoder::default().encode_bytes(&wav).unwrap();

    let decoded =
        DecodeHarness::new(DecodeConfig::default().with_output_container(PcmContainer::Caf))
            .decode_bytes(&flac)
            .unwrap();

    assert!(is_caf_bytes(&decoded));
    assert!(decoded.windows(4).any(|window| window == b"info"));
    assert!(decoded.windows(4).any(|window| window == b"mark"));
    assert!(!decoded.windows(4).any(|window| window == b"fxmd"));
}

#[test]
fn failed_decode_file_does_not_leave_accepted_wav_output() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 4_096));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("wav");
    let sentinel = b"keep-existing-output";
    fs::write(&input_path, corrupt_last_frame_crc(&flac)).unwrap();
    fs::write(&output_path, sentinel).unwrap();

    let error = decoder_for_threads(4)
        .decode_file(&input_path, &output_path)
        .unwrap_err();

    let message = error.to_string();
    assert!(
        message.contains("invalid flac") || message.contains("decode error"),
        "unexpected decode cleanup error: {message}"
    );
    assert_eq!(
        fs::read(&output_path).unwrap(),
        sentinel,
        "decode failure should not overwrite existing WAV output"
    );

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn free_decode_helpers_remain_functional() {
    let wav = pcm_wav_bytes(16, 1, 32_000, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    assert_eq!(
        wav_data_bytes(&decode_bytes(&flac).unwrap()),
        wav_data_bytes(&wav)
    );

    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("wav");
    fs::write(&input_path, &flac).unwrap();

    let summary = decode_file(&input_path, &output_path).unwrap();
    assert_eq!(summary.total_samples, 2_048);
    assert_eq!(
        wav_data_bytes(&fs::read(&output_path).unwrap()),
        wav_data_bytes(&wav)
    );

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn rejects_invalid_flac_magic() {
    assert_decode_error_stable(&corrupt_magic(b"fLaC"));
}

#[test]
fn rejects_truncated_streaminfo() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 128));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    assert_decode_error_stable(&truncate_bytes(&flac, 12));
}

#[test]
fn rejects_bad_frame_crc() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    assert_decode_error_stable(&corrupt_last_frame_crc(&flac));
}

#[test]
fn rejects_streaminfo_md5_mismatch() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let mut bad_md5 = streaminfo_md5(&flac);
    bad_md5[0] ^= 0xFF;
    let corrupt = rewrite_streaminfo_md5(&flac, bad_md5);

    assert_eq!(flac_frames(&corrupt), flac_frames(&flac));
    assert_ne!(streaminfo_md5(&corrupt), streaminfo_md5(&flac));

    for threads in decode_thread_variants() {
        let error = decoder_for_threads(threads)
            .decode_bytes(&corrupt)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid flac: STREAMINFO MD5 mismatch",
            "unexpected MD5 mismatch error for threads={threads}"
        );
    }
}

#[test]
fn skips_streaminfo_md5_verification_when_digest_is_all_zeroes() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = rewrite_streaminfo_md5(&flac, [0; 16]);

    assert_round_trips_bytes_exactly(&wav, &flac);
}

#[test]
fn zero_sample_stream_round_trips_with_empty_stream_md5() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &[]);
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    assert_eq!(
        streaminfo_md5(&flac),
        [
            0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8,
            0x42, 0x7e,
        ]
    );

    assert_round_trips_bytes_exactly(&wav, &flac);
}

#[test]
fn decode_uses_seekable_io() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 512));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let mut output = Cursor::new(Vec::new());

    let summary = DecodeHarness::default()
        .decode(Cursor::new(flac), &mut output)
        .unwrap();

    assert_eq!(summary.total_samples, 512);
    assert_eq!(wav_data_bytes(&output.into_inner()), wav_data_bytes(&wav));
}

#[test]
fn restores_vorbis_comments_from_arbitrary_flac_metadata() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(
        &flac,
        &[vorbis_comment_block(&[
            ("ARTIST", "Example Artist"),
            ("TITLE", "Recovered Title"),
            ("UNKNOWN", "ignored"),
        ])],
    );

    let decoded = decode_bytes_with_threads(&flac, 2);

    assert_eq!(
        wav_info_entries(&decoded),
        vec![
            (*b"IART", "Example Artist".to_string()),
            (*b"INAM", "Recovered Title".to_string()),
        ]
    );
}

#[test]
fn restores_cuesheet_metadata_from_arbitrary_flac_input() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 4_096));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(&flac, &[cuesheet_block(&[0, 2_048], 4_096)]);

    let decoded = decode_bytes_with_threads(&flac, 4);

    assert_eq!(wav_cue_points(&decoded), vec![0, 2_048]);
}

#[test]
fn decode_emits_canonical_fxmd_chunk_for_flac_cuesheet() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 4_096));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let raw_cuesheet = rich_cuesheet_payload();
    let flac = replace_flac_optional_metadata(&flac, &[raw_cuesheet_block(&raw_cuesheet)]);

    let decoded = decode_bytes_with_threads(&flac, 2);
    let fxmd_payloads = wav_chunk_payloads(&decoded, *b"fxmd");
    let parsed = parse_fxmd_chunk_payload(&fxmd_payloads[0]);

    assert_eq!(fxmd_payloads.len(), 1);
    assert_eq!(parsed.version, 1);
    assert_eq!(
        parsed
            .records
            .iter()
            .find(|record| record.block_type == 5)
            .expect("cuesheet record present")
            .payload,
        raw_cuesheet
    );
    assert_eq!(wav_cue_points(&decoded), vec![0, 2_048]);
}

#[test]
fn decode_keeps_riff_cue_mirroring_when_cuesheet_is_representable() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 4_096));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(&flac, &[cuesheet_block(&[0, 2_048], 4_096)]);

    let decoded = decode_bytes_with_threads(&flac, 2);

    assert_eq!(wav_chunk_payloads(&decoded, *b"fxmd").len(), 1);
    assert_eq!(wav_cue_points(&decoded), vec![0, 2_048]);
}

#[test]
fn round_trips_application_and_picture_metadata_exactly_via_canonical_fxmd_v1() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let application = application_block(b"opaque-metadata");
    let picture = picture_block(
        "image/png",
        "front cover",
        1,
        1,
        24,
        0,
        &[0x89, 0x50, 0x4E, 0x47],
    );
    let flac = replace_flac_optional_metadata(&flac, &[application.clone(), picture.clone()]);

    let decoded = decode_bytes_with_threads(&flac, 2);
    let fxmd_payloads = wav_chunk_payloads(&decoded, *b"fxmd");
    let parsed = parse_fxmd_chunk_payload(&fxmd_payloads[0]);

    assert_eq!(fxmd_payloads.len(), 1);
    assert_eq!(parsed.version, 1);
    assert_eq!(
        parsed
            .records
            .iter()
            .map(|record| (record.block_type, record.payload.clone()))
            .collect::<Vec<_>>(),
        vec![
            (2, application.payload.clone()),
            (6, picture.payload.clone()),
        ]
    );
    assert!(wav_info_entries(&decoded).is_empty());
    assert!(wav_cue_points(&decoded).is_empty());

    let reencoded = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&decoded)
        .unwrap();
    let reencoded_blocks = flac_metadata_blocks(&reencoded);
    let reencoded_application = reencoded_blocks
        .iter()
        .find(|block| block.block_type == 2)
        .expect("application block present");
    let reencoded_picture = reencoded_blocks
        .iter()
        .find(|block| block.block_type == 6)
        .expect("picture block present");
    let original_blocks = flac_metadata_blocks(&flac);
    let original_application = original_blocks
        .iter()
        .find(|block| block.block_type == 2)
        .expect("application block present");
    let original_picture = original_blocks
        .iter()
        .find(|block| block.block_type == 6)
        .expect("picture block present");

    assert_eq!(reencoded_application.payload, original_application.payload);
    assert_eq!(reencoded_picture.payload, original_picture.payload);
}

#[test]
fn round_trips_fxmd_payload_exactly_through_flac_wav_flac_wav() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(
        &flac,
        &[
            application_block(b"opaque-metadata"),
            picture_block(
                "image/png",
                "front cover",
                1,
                1,
                24,
                0,
                &[0x89, 0x50, 0x4E, 0x47],
            ),
        ],
    );

    let decoded = decode_bytes_with_threads(&flac, 2);
    let fxmd_payload = wav_chunk_payloads(&decoded, *b"fxmd")
        .into_iter()
        .next()
        .expect("fxmd chunk present");
    let wav_with_fxmd = wav_with_chunks(
        pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048)),
        &[(*b"fxmd", fxmd_payload.clone())],
    );

    let reencoded = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&wav_with_fxmd)
        .unwrap();
    let decoded_again = decode_bytes_with_threads(&reencoded, 2);

    assert_eq!(
        wav_chunk_payloads(&decoded_again, *b"fxmd"),
        vec![fxmd_payload]
    );
    assert_eq!(wav_data_bytes(&decoded_again), wav_data_bytes(&wav));
}

#[test]
fn round_trips_unknown_metadata_blocks_opaquely_via_canonical_fxmd_v1() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let unknown = ParsedMetadataBlock {
        is_last: false,
        block_type: 8,
        payload: b"opaque-reserved-block".to_vec(),
    };
    let flac = replace_flac_optional_metadata(&flac, std::slice::from_ref(&unknown));

    let decoded = decode_bytes_with_threads(&flac, 2);
    let parsed = parse_fxmd_chunk_payload(&wav_chunk_payloads(&decoded, *b"fxmd")[0]);
    let reencoded = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&decoded)
        .unwrap();

    assert_eq!(
        parsed
            .records
            .iter()
            .find(|record| record.block_type == 8)
            .expect("unknown metadata record preserved")
            .payload,
        unknown.payload
    );
    assert_eq!(
        flac_metadata_blocks(&reencoded),
        flac_metadata_blocks(&flac)
    );
}

#[test]
fn decode_preserves_exact_cuesheet_even_when_riff_cue_is_partial() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 4_096));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let raw_cuesheet = rich_cuesheet_payload();
    let flac = replace_flac_optional_metadata(&flac, &[raw_cuesheet_block(&raw_cuesheet)]);

    let decoded = decode_bytes_with_threads(&flac, 2);
    let fxmd_payloads = wav_chunk_payloads(&decoded, *b"fxmd");
    let parsed = parse_fxmd_chunk_payload(&fxmd_payloads[0]);

    assert_eq!(fxmd_payloads.len(), 1);
    assert_eq!(
        parsed
            .records
            .iter()
            .find(|record| record.block_type == 5)
            .expect("cuesheet record present")
            .payload,
        raw_cuesheet
    );
}

#[test]
fn drops_unsupported_flac_metadata_blocks_during_decode() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(
        &flac,
        &[
            application_block(b"opaque-metadata"),
            vorbis_comment_block(&[("TITLE", "Kept")]),
        ],
    );

    let decoded = decode_bytes_with_threads(&flac, 1);

    assert_eq!(
        wav_info_entries(&decoded),
        vec![(*b"INAM", "Kept".to_string())]
    );
    assert!(wav_cue_points(&decoded).is_empty());
}

#[test]
fn decode_accepts_valid_seektable_block() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(&flac, &[seektable_block(&[(0, 0, 2_048)])]);

    let decoded =
        DecodeHarness::new(DecodeConfig::default().with_strict_seektable_validation(true))
            .decode_bytes(&flac)
            .unwrap();

    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
}

#[test]
fn decode_tolerates_malformed_seektable_by_default() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(&flac, &[raw_seektable_block(&[0u8; 17])]);

    let decoded = decode_bytes(&flac).unwrap();

    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
}

#[test]
fn decode_rejects_invalid_length_seektable_when_strict() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(&flac, &[raw_seektable_block(&[0u8; 17])]);

    let error = DecodeHarness::new(DecodeConfig::default().with_strict_seektable_validation(true))
        .decode_bytes(&flac)
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("seektable payload length must be a multiple of 18 bytes")
    );
}

#[test]
fn restores_exact_vorbis_comments_into_fxmd_chunk_and_info_mirror() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(
        &flac,
        &[vorbis_comment_block(&[
            ("artist", "Example Artist"),
            ("UNMAPPED", "Opaque"),
            ("TITLE", "Exact Title"),
            ("TITLE", "Duplicate"),
        ])],
    );

    let decoded = decode_bytes_with_threads(&flac, 2);
    let fxmd_payloads = wav_chunk_payloads(&decoded, *b"fxmd");
    let parsed = parse_fxmd_chunk_payload(&fxmd_payloads[0]);
    let vorbis = parsed
        .records
        .iter()
        .find(|record| record.block_type == 4)
        .expect("vorbis record present");

    assert_eq!(fxmd_payloads.len(), 1);
    assert_eq!(
        vorbis_comments(&vorbis.payload),
        vec![
            "artist=Example Artist".to_string(),
            "UNMAPPED=Opaque".to_string(),
            "TITLE=Exact Title".to_string(),
            "TITLE=Duplicate".to_string(),
        ]
    );
    assert_eq!(
        wav_info_entries(&decoded),
        vec![
            (*b"IART", "Example Artist".to_string()),
            (*b"INAM", "Exact Title".to_string()),
            (*b"INAM", "Duplicate".to_string()),
        ]
    );
}

#[test]
fn round_trips_exact_vorbis_comments_via_fxmd_chunk() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(
        &flac,
        &[vorbis_comment_block(&[
            ("artist", "Example Artist"),
            ("UNMAPPED", "Opaque"),
            ("TITLE", "Exact Title"),
            ("TITLE", "Duplicate"),
        ])],
    );

    let decoded = decode_bytes_with_threads(&flac, 2);
    let reencoded = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&decoded)
        .unwrap();
    let blocks = flac_metadata_blocks(&reencoded);
    let vorbis = blocks
        .iter()
        .find(|block| block.block_type == 4)
        .expect("vorbis comment block present");

    assert_eq!(
        support::vorbis_comments(&vorbis.payload),
        vec![
            "artist=Example Artist".to_string(),
            "UNMAPPED=Opaque".to_string(),
            "TITLE=Exact Title".to_string(),
            "TITLE=Duplicate".to_string(),
        ]
    );
}

#[test]
fn decode_rejects_non_ascending_seektable_when_strict() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(
        &flac,
        &[seektable_block(&[(1_024, 128, 1_024), (0, 0, 1_024)])],
    );

    let error = DecodeHarness::new(DecodeConfig::default().with_strict_seektable_validation(true))
        .decode_bytes(&flac)
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("seektable sample numbers must be in ascending order")
    );
}

#[test]
fn decode_rejects_duplicate_seektable_sample_numbers_when_strict() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac =
        replace_flac_optional_metadata(&flac, &[seektable_block(&[(0, 0, 1_024), (0, 64, 1_024)])]);

    let error = DecodeHarness::new(DecodeConfig::default().with_strict_seektable_validation(true))
        .decode_bytes(&flac)
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("seektable sample numbers must be unique")
    );
}

#[test]
fn decode_rejects_seektable_placeholders_not_at_end_when_strict() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(
        &flac,
        &[seektable_block(&[(u64::MAX, 0, 0), (0, 0, 2_048)])],
    );

    let error = DecodeHarness::new(DecodeConfig::default().with_strict_seektable_validation(true))
        .decode_bytes(&flac)
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("seektable placeholder points must appear at the end of the table")
    );
}

#[test]
fn metadata_restoration_round_trips_flacx_supported_subset() {
    let wav = wav_with_chunks(
        pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 4_096)),
        &[
            (
                *b"LIST",
                info_list_chunk(&[
                    (*b"IART", b"Example Artist"),
                    (*b"INAM", b"Round Trip Title"),
                ]),
            ),
            (*b"cue ", cue_chunk(&[0, 2_048])),
        ],
    );
    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&wav)
        .unwrap();

    let decoded = decode_bytes_with_threads(&flac, 2);

    assert_eq!(
        wav_info_entries(&decoded),
        vec![
            (*b"IART", "Example Artist".to_string()),
            (*b"INAM", "Round Trip Title".to_string()),
        ]
    );
    assert_eq!(wav_cue_points(&decoded), vec![0, 2_048]);
}

#[test]
fn restored_metadata_output_is_stable_across_thread_counts() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 4_096));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(
        &flac,
        &[
            vorbis_comment_block(&[("ARTIST", "Example Artist"), ("TITLE", "Stable Title")]),
            cuesheet_block(&[0, 2_048], 4_096),
        ],
    );

    let single_threaded = decode_bytes_with_threads(&flac, 1);
    let multi_threaded = decode_bytes_with_threads(&flac, 4);

    assert_eq!(single_threaded, multi_threaded);
}

#[test]
fn restores_non_ordinary_channel_mask_from_case_insensitive_padded_hex_comment() {
    let wav = extensible_pcm_wav_bytes(
        16,
        16,
        4,
        48_000,
        ordinary_channel_mask(4).unwrap(),
        &sample_fixture(4, 2_048),
    );
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(
        &flac,
        &[vorbis_comment_block(&[(
            "waveformatextensible_channel_mask",
            "0X00012104",
        )])],
    );

    let decoded = decode_bytes_with_threads(&flac, 2);

    assert_eq!(parse_wav_format(&decoded).channel_mask, Some(0x0001_2104));
}

#[test]
fn restores_zero_channel_mask_from_rfc_comment() {
    let wav = extensible_pcm_wav_bytes(
        16,
        16,
        2,
        44_100,
        ordinary_channel_mask(2).unwrap(),
        &sample_fixture(2, 2_048),
    );
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(
        &flac,
        &[vorbis_comment_block(&[(
            "WAVEFORMATEXTENSIBLE_CHANNEL_MASK",
            "0x0",
        )])],
    );

    let decoded = decode_bytes_with_threads(&flac, 1);
    let format = parse_wav_format(&decoded);

    assert_eq!(format.format_tag, 0xFFFE);
    assert_eq!(format.channel_mask, Some(0));
}

#[test]
fn rejects_invalid_waveformatextensible_channel_mask_comment() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(
        &flac,
        &[vorbis_comment_block(&[(
            "WAVEFORMATEXTENSIBLE_CHANNEL_MASK",
            "not-hex",
        )])],
    );

    for threads in decode_thread_variants() {
        let error = decoder_for_threads(threads)
            .decode_bytes(&flac)
            .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("WAVEFORMATEXTENSIBLE_CHANNEL_MASK"),
            "unexpected decode error for threads={threads}: {message}"
        );
    }
}

#[test]
fn strict_channel_mask_provenance_accepts_flacx_marked_non_ordinary_files() {
    let wav = extensible_pcm_wav_bytes(16, 16, 4, 48_000, 0x0001_2104, &sample_fixture(4, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let decoded =
        DecodeHarness::new(DecodeConfig::default().with_strict_channel_mask_provenance(true))
            .decode_bytes(&flac)
            .unwrap();
    let format = parse_wav_format(&decoded);

    assert_eq!(format.format_tag, 0xFFFE);
    assert_eq!(format.channel_mask, Some(0x0001_2104));
    assert_eq!(wav_chunk_payloads(&decoded, *b"fxmd").len(), 1);
}

#[test]
fn strict_channel_mask_provenance_keeps_ordinary_multichannel_fallbacks_compatible() {
    let wav = extensible_pcm_wav_bytes(
        16,
        16,
        4,
        48_000,
        ordinary_channel_mask(4).unwrap(),
        &sample_fixture(4, 2_048),
    );
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let decoded =
        DecodeHarness::new(DecodeConfig::default().with_strict_channel_mask_provenance(true))
            .decode_bytes(&flac)
            .unwrap();
    assert_eq!(
        parse_wav_format(&decoded).channel_mask,
        ordinary_channel_mask(4)
    );
}

#[test]
fn strict_channel_mask_provenance_rejects_unmarked_non_ordinary_masks() {
    let wav = extensible_pcm_wav_bytes(
        16,
        16,
        4,
        48_000,
        ordinary_channel_mask(4).unwrap(),
        &sample_fixture(4, 2_048),
    );
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(
        &flac,
        &[vorbis_comment_block(&[(
            "WAVEFORMATEXTENSIBLE_CHANNEL_MASK",
            "0x00012104",
        )])],
    );

    let error =
        DecodeHarness::new(DecodeConfig::default().with_strict_channel_mask_provenance(true))
            .decode_bytes(&flac)
            .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("FLACX_CHANNEL_LAYOUT_PROVENANCE")
    );
}
