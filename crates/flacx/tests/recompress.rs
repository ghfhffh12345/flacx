use std::{fs, io::Cursor};

#[cfg(feature = "progress")]
use flacx::{DecodePcmStream, EncodePcmStream, PcmSpec, StreamInfo};
use flacx::{
    FlacReaderOptions, FlacRecompressSource, Metadata, RecompressConfig, RecompressMode,
    RecompressSummary, builtin, read_flac_reader_with_options,
};

#[cfg(feature = "progress")]
use flacx::{RecompressPhase, RecompressProgress};

mod support;
use support::TestDecoder as DecodeHarness;
use support::TestEncoder as Encoder;

#[cfg(feature = "progress")]
use support::{LARGE_STREAMING_DECODE_SAMPLE_COUNT, ordinary_channel_mask, streaminfo_md5};
use support::{
    application_block, flac_metadata_blocks, pcm_wav_bytes, picture_block,
    replace_flac_optional_metadata, sample_fixture, seektable_block, unique_temp_path,
    wav_data_bytes,
};

fn reader_options(config: RecompressConfig) -> FlacReaderOptions {
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

fn recompress_with_config(
    config: RecompressConfig,
    input: &[u8],
) -> flacx::Result<(Vec<u8>, RecompressSummary)> {
    let reader = read_flac_reader_with_options(Cursor::new(input), reader_options(config))?;
    let source = reader.into_recompress_source();
    let mut recompressor = config.into_recompressor(Cursor::new(Vec::new()));
    let summary = recompressor.recompress(source)?;
    Ok((recompressor.into_inner().into_inner(), summary))
}

#[cfg(feature = "progress")]
struct StreamingOnlyRecompressStream {
    spec: PcmSpec,
    stream_info: StreamInfo,
    samples: Vec<i32>,
    chunk_frames: usize,
    total_input_bytes: u64,
    cursor: usize,
    completed_frames: usize,
}

#[cfg(feature = "progress")]
impl StreamingOnlyRecompressStream {
    fn new(
        spec: PcmSpec,
        stream_info: StreamInfo,
        samples: Vec<i32>,
        chunk_frames: usize,
        total_input_bytes: u64,
    ) -> Self {
        Self {
            spec,
            stream_info,
            samples,
            chunk_frames,
            total_input_bytes,
            cursor: 0,
            completed_frames: 0,
        }
    }
}

#[cfg(feature = "progress")]
impl EncodePcmStream for StreamingOnlyRecompressStream {
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
impl DecodePcmStream for StreamingOnlyRecompressStream {
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

    fn input_bytes_processed(&self) -> u64 {
        let total_input_bytes_read = self.total_input_bytes;
        if self.cursor == self.samples.len() {
            total_input_bytes_read
        } else {
            0
        }
    }

    fn take_decoded_samples(&mut self) -> flacx::Result<Option<(Vec<i32>, usize)>> {
        panic!("recompress should verify via read_chunk instead of take_decoded_samples")
    }
}

#[test]
fn builtin_recompress_bytes_matches_explicit_reader_session_flow() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();

    let via_builtin = builtin::recompress_bytes(&flac).unwrap();
    let (via_session, summary) =
        recompress_with_config(RecompressConfig::default(), &flac).unwrap();

    assert_eq!(summary.total_samples, 2_048);
    assert_eq!(via_builtin, via_session);
}

#[test]
fn recompress_api_accepts_reader_first_sources_and_preserves_audio() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 4_096));
    let flac = Encoder::new(flacx::EncoderConfig::default().with_block_size(576))
        .encode_bytes(&wav)
        .unwrap();
    let config = RecompressConfig::default()
        .with_threads(1)
        .with_block_size(576);
    let reader = read_flac_reader_with_options(Cursor::new(flac), reader_options(config)).unwrap();
    let source = reader.into_recompress_source();
    let mut recompressor = config.into_recompressor(Cursor::new(Vec::new()));

    let summary = recompressor.recompress(source).unwrap();
    let recompressed = recompressor.into_inner().into_inner();
    let decoded = DecodeHarness::default()
        .decode_bytes(&recompressed)
        .unwrap();

    assert_eq!(summary.block_size, 576);
    assert_eq!(summary.channels, 2);
    assert_eq!(summary.total_samples, 4_096);
    assert!(recompressed.starts_with(b"fLaC"));
    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
}

#[test]
fn recompress_source_new_with_scratch_metadata_preserves_audio_and_verifies_md5() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 4_096));
    let flac = Encoder::new(flacx::EncoderConfig::default().with_block_size(576))
        .encode_bytes(&wav)
        .unwrap();
    let config = RecompressConfig::default()
        .with_threads(1)
        .with_block_size(576);
    let reader = read_flac_reader_with_options(Cursor::new(&flac), reader_options(config)).unwrap();
    let expected_md5 = reader.stream_info().md5;
    let (_, stream) = reader.into_decode_source().into_parts();
    let mut metadata = Metadata::new();
    metadata.add_comment("TITLE", "Scratch Recompress");
    let source = FlacRecompressSource::new(metadata, stream, expected_md5);
    let mut recompressor = config.into_recompressor(Cursor::new(Vec::new()));

    let summary = recompressor.recompress(source).unwrap();
    let recompressed = recompressor.into_inner().into_inner();
    let decoded = DecodeHarness::default()
        .decode_bytes(&recompressed)
        .unwrap();
    let blocks = flac_metadata_blocks(&recompressed);

    assert_eq!(summary.total_samples, 4_096);
    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
    assert!(blocks.iter().any(|block| block.block_type == 4));
}

#[test]
fn reader_metadata_reused_via_recompress_source_new_matches_reader_into_recompress_source_bytes() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 4_096));
    let flac = Encoder::new(flacx::EncoderConfig::default().with_block_size(576))
        .encode_bytes(&wav)
        .unwrap();
    let config = RecompressConfig::default()
        .with_threads(1)
        .with_level(flacx::level::Level::Level0)
        .with_block_size(576);

    let baseline_reader =
        read_flac_reader_with_options(Cursor::new(&flac), reader_options(config)).unwrap();
    let mut baseline_recompressor = config.into_recompressor(Cursor::new(Vec::new()));
    let baseline_summary = baseline_recompressor
        .recompress(baseline_reader.into_recompress_source())
        .unwrap();
    let baseline_output = baseline_recompressor.into_inner().into_inner();

    let reader = read_flac_reader_with_options(Cursor::new(&flac), reader_options(config)).unwrap();
    let expected_md5 = reader.stream_info().md5;
    let metadata = reader.metadata().clone();
    let (_, stream) = reader.into_decode_source().into_parts();
    let source = FlacRecompressSource::new(metadata, stream, expected_md5);
    let mut candidate_recompressor = config.into_recompressor(Cursor::new(Vec::new()));
    let candidate_summary = candidate_recompressor.recompress(source).unwrap();
    let candidate_output = candidate_recompressor.into_inner().into_inner();

    assert_eq!(candidate_summary, baseline_summary);
    assert_eq!(candidate_output, baseline_output);
}

#[test]
fn builtin_recompress_file_matches_explicit_reader_session_output() {
    let wav = pcm_wav_bytes(16, 1, 32_000, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("flac");
    fs::write(&input_path, &flac).unwrap();

    let summary = builtin::recompress_file(&input_path, &output_path).unwrap();
    let bytes_from_file = fs::read(&output_path).unwrap();
    let (bytes_from_memory, session_summary) =
        recompress_with_config(RecompressConfig::default(), &flac).unwrap();

    assert_eq!(summary.total_samples, 2_048);
    assert_eq!(summary, session_summary);
    assert_eq!(bytes_from_file, bytes_from_memory);

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn recompress_is_deterministic_for_repeat_runs() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 4_096));
    let flac = Encoder::new(flacx::EncoderConfig::default().with_block_size(576))
        .encode_bytes(&wav)
        .unwrap();
    let config = RecompressConfig::default()
        .with_threads(1)
        .with_level(flacx::level::Level::Level0)
        .with_block_size(576);

    let first = recompress_with_config(config, &flac).unwrap();
    let second = recompress_with_config(config, &flac).unwrap();

    assert_eq!(first.1, second.1);
    assert_eq!(first.0, second.0);
}

#[test]
fn recompress_produces_identical_output_across_thread_counts() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 8_192));
    let flac = Encoder::new(flacx::EncoderConfig::default().with_block_size(576))
        .encode_bytes(&wav)
        .unwrap();
    let single_threaded = recompress_with_config(
        RecompressConfig::default()
            .with_threads(1)
            .with_level(flacx::level::Level::Level0)
            .with_block_size(576),
        &flac,
    )
    .unwrap();
    let multi_threaded = recompress_with_config(
        RecompressConfig::default()
            .with_threads(4)
            .with_level(flacx::level::Level::Level0)
            .with_block_size(576),
        &flac,
    )
    .unwrap();

    assert_eq!(single_threaded.1, multi_threaded.1);
    assert_eq!(single_threaded.0, multi_threaded.0);
}

#[test]
fn recompress_preserves_optional_metadata_blocks() {
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
    let source = replace_flac_optional_metadata(&flac, &[application.clone(), picture.clone()]);

    let (recompressed, _) = recompress_with_config(RecompressConfig::default(), &source).unwrap();
    let blocks = flac_metadata_blocks(&recompressed);

    let recompressed_application = blocks
        .iter()
        .find(|block| block.block_type == 2)
        .expect("application block present");
    let recompressed_picture = blocks
        .iter()
        .find(|block| block.block_type == 6)
        .expect("picture block present");

    assert_eq!(recompressed_application.payload, application.payload);
    assert_eq!(recompressed_picture.payload, picture.payload);
}

#[test]
fn recompress_strict_mode_rejects_invalid_seektable() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let malformed = replace_flac_optional_metadata(
        &flac,
        &[seektable_block(&[(128, 1_024, 128), (64, 0, 64)])],
    );

    let strict = recompress_with_config(
        RecompressConfig::default().with_mode(RecompressMode::Strict),
        &malformed,
    )
    .unwrap_err();

    assert!(
        strict
            .to_string()
            .contains("seektable sample numbers must be in ascending order")
    );
}

#[test]
fn recompress_builder_matches_fluent_config() {
    let builder = RecompressConfig::builder()
        .mode(RecompressMode::Strict)
        .level(flacx::level::Level::Level0)
        .threads(4)
        .block_size(576)
        .build();

    let fluent = RecompressConfig::default()
        .with_mode(RecompressMode::Strict)
        .with_level(flacx::level::Level::Level0)
        .with_threads(4)
        .with_block_size(576);

    assert_eq!(builder, fluent);
    assert_eq!(builder.mode(), RecompressMode::Strict);
    assert_eq!(builder.level(), flacx::level::Level::Level0);
    assert_eq!(builder.threads(), 4);
    assert_eq!(builder.block_size(), Some(576));
}

#[test]
fn recompress_verifier_limits_take_decoded_samples_to_bounded_small_inputs() {
    let verify_source = include_str!("../src/recompress/verify.rs");
    let session_source = include_str!("../src/recompress/session.rs");
    assert!(verify_source.contains("into_verified_pcm_stream"));
    assert!(session_source.contains("EAGER_RECOMPRESS_TOTAL_SAMPLES_THRESHOLD"));
}

#[test]
fn recompress_session_avoids_buffered_encode_handoff() {
    let source = include_str!("../src/recompress/session.rs");
    assert!(source.contains("into_verified_pcm_stream()?"));
    assert!(source.contains("into_encode_parts()"));
    assert!(source.contains("BufferedRecompressPcmStream"));
    assert!(source.contains("CountedEncodePcmStream::new"));
    assert!(!source.contains("encode_buffered_pcm_with_sink"));
}

#[cfg(feature = "progress")]
#[test]
fn recompress_source_streams_verified_pcm_without_materializing_samples() {
    let total_samples = LARGE_STREAMING_DECODE_SAMPLE_COUNT;
    let chunk_frames = 4_194_304usize;
    let samples = sample_fixture(1, total_samples);
    let wav = pcm_wav_bytes(16, 1, 44_100, &samples);
    let flac = Encoder::new(flacx::EncoderConfig::default().with_block_size(576))
        .encode_bytes(&wav)
        .unwrap();
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
    stream_info.update_block_size(512);
    let mut updates = Vec::<RecompressProgress>::new();
    let mut recompressor = RecompressConfig::default()
        .with_threads(1)
        .with_block_size(576)
        .into_recompressor(Cursor::new(Vec::new()));

    let summary = recompressor
        .recompress_with_progress(
            FlacRecompressSource::new(
                Metadata::new(),
                StreamingOnlyRecompressStream::new(
                    spec,
                    stream_info,
                    samples,
                    chunk_frames,
                    flac.len() as u64,
                ),
                streaminfo_md5(&flac),
            ),
            |progress| {
                updates.push(progress);
                Ok(())
            },
        )
        .unwrap();

    let decoded = DecodeHarness::default()
        .decode_bytes(&recompressor.into_inner().into_inner())
        .unwrap();
    assert_eq!(summary.total_samples, total_samples as u64);
    assert_eq!(updates.first().unwrap().phase, RecompressPhase::Decode);
    assert!(
        updates
            .iter()
            .any(|progress| progress.phase == RecompressPhase::Encode)
    );
    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
}

#[cfg(feature = "progress")]
#[test]
fn recompress_progress_reports_decode_then_encode_phases() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let config = RecompressConfig::default().with_threads(1);
    let reader = read_flac_reader_with_options(Cursor::new(flac), reader_options(config)).unwrap();
    let source = reader.into_recompress_source();
    let mut recompressor = config.into_recompressor(Cursor::new(Vec::new()));
    let mut updates = Vec::<RecompressProgress>::new();

    recompressor
        .recompress_with_progress(source, |progress| {
            updates.push(progress);
            Ok(())
        })
        .unwrap();

    assert!(!updates.is_empty());
    assert_eq!(updates.first().unwrap().phase, RecompressPhase::Decode);
    assert!(
        updates
            .iter()
            .any(|progress| progress.phase == RecompressPhase::Encode)
    );

    let mut saw_encode = false;
    let mut previous_overall = 0u64;
    for progress in updates {
        if progress.phase == RecompressPhase::Encode {
            saw_encode = true;
        }
        if saw_encode {
            assert_eq!(progress.phase, RecompressPhase::Encode);
        }
        assert!(progress.overall_processed_samples >= previous_overall);
        previous_overall = progress.overall_processed_samples;
    }
    assert_eq!(previous_overall, 4_096);
}

#[cfg(feature = "progress")]
#[test]
fn recompress_progress_reports_exact_phase_and_overall_output_bytes() {
    let total_samples = LARGE_STREAMING_DECODE_SAMPLE_COUNT;
    let samples = sample_fixture(1, total_samples);
    let wav = pcm_wav_bytes(16, 1, 44_100, &samples);
    let flac = Encoder::new(flacx::EncoderConfig::default().with_block_size(576))
        .encode_bytes(&wav)
        .unwrap();
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
    stream_info.update_block_size(512);
    let source = FlacRecompressSource::new(
        Metadata::new(),
        StreamingOnlyRecompressStream::new(
            spec,
            stream_info,
            samples,
            4_194_304,
            flac.len() as u64,
        ),
        streaminfo_md5(&flac),
    );
    let config = RecompressConfig::default()
        .with_threads(1)
        .with_block_size(576);
    let mut recompressor = config.into_recompressor(Cursor::new(Vec::new()));
    let mut updates = Vec::<RecompressProgress>::new();

    recompressor
        .recompress_with_progress(source, |progress| {
            updates.push(progress);
            Ok(())
        })
        .unwrap();

    let recompressed = recompressor.into_inner().into_inner();
    let last = updates.last().unwrap();
    assert_eq!(
        last.phase_input_bytes_read,
        wav_data_bytes(&wav).len() as u64
    );
    assert_eq!(
        last.phase_output_bytes_written,
        recompressed.len() as u64
    );
    assert_eq!(
        last.overall_output_bytes_written,
        recompressed.len() as u64
    );
}

#[cfg(feature = "progress")]
#[test]
fn recompress_progress_reports_exact_overall_input_bytes_across_decode_and_encode() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let config = RecompressConfig::default().with_threads(1);
    let reader = read_flac_reader_with_options(Cursor::new(&flac), reader_options(config)).unwrap();
    let source = reader.into_recompress_source();
    let mut recompressor = config.into_recompressor(Cursor::new(Vec::new()));
    let mut updates = Vec::<RecompressProgress>::new();

    recompressor
        .recompress_with_progress(source, |progress| {
            updates.push(progress);
            Ok(())
        })
        .unwrap();

    let last = updates.last().unwrap();
    assert_eq!(
        last.overall_input_bytes_read,
        flac.len() as u64 + wav_data_bytes(&wav).len() as u64
    );
}
