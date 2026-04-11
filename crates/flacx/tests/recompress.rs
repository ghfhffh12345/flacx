use std::{fs, io::Cursor};

use flacx::{RecompressConfig, RecompressMode, Recompressor, builtin::recompress_bytes};

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

#[test]
fn top_level_recompress_bytes_matches_default_recompressor() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();

    let via_function = recompress_bytes(&flac).unwrap();
    let via_recompressor = Recompressor::default().recompress_bytes(&flac).unwrap();

    assert_eq!(via_function, via_recompressor);
}

#[test]
fn recompress_api_accepts_seekable_readers_and_preserves_audio() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 4_096));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let mut output = Cursor::new(Vec::new());

    let summary = Recompressor::new(
        RecompressConfig::default()
            .with_threads(1)
            .with_block_size(576),
    )
    .recompress(Cursor::new(flac), &mut output)
    .unwrap();

    let recompressed = output.into_inner();
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
fn recompress_file_uses_configured_options() {
    let wav = pcm_wav_bytes(16, 1, 32_000, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("flac");
    fs::write(&input_path, &flac).unwrap();

    let summary = Recompressor::new(
        RecompressConfig::default()
            .with_threads(1)
            .with_block_size(576),
    )
    .recompress_file(&input_path, &output_path)
    .unwrap();

    assert_eq!(summary.block_size, 576);
    assert!(output_path.exists());
    let decoded = DecodeHarness::default()
        .decode_bytes(&fs::read(&output_path).unwrap())
        .unwrap();
    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
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

    let recompressed = Recompressor::default().recompress_bytes(&source).unwrap();
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

    let strict = Recompressor::new(RecompressConfig::default().with_mode(RecompressMode::Strict))
        .recompress_bytes(&malformed)
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

#[cfg(feature = "progress")]
#[test]
fn recompress_progress_reports_decode_then_encode_phases() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let mut output = Cursor::new(Vec::new());
    let mut updates = Vec::<RecompressProgress>::new();

    Recompressor::new(RecompressConfig::default().with_threads(1))
        .recompress_with_progress(Cursor::new(flac), &mut output, |progress| {
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
