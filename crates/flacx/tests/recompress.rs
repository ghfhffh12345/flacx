use std::{fs, io::Cursor};

use flacx::{
    FlacReaderOptions, RecompressConfig, RecompressMode, RecompressSummary, builtin,
    read_flac_reader_with_options,
};

#[cfg(feature = "progress")]
use flacx::{RecompressPhase, RecompressProgress};

mod support;
use support::TestDecoder as DecodeHarness;
use support::TestEncoder as Encoder;

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
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
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
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
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
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
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
fn recompress_verifier_keeps_the_full_decode_fast_path_available() {
    let source = include_str!("../src/recompress/verify.rs");
    assert!(source.contains("take_decoded_samples"));
}

#[test]
fn recompress_session_keeps_the_buffered_encode_fast_path() {
    let source = include_str!("../src/recompress/session.rs");
    assert!(source.contains("into_verified_pcm_stream()?"));
    assert!(source.contains("encode_buffered_pcm_with_sink"));
    assert!(!source.contains("encoder.encode(pcm_stream)"));
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
