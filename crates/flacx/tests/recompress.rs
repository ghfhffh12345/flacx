use std::{fs, io::Cursor};

use flacx::{
    DecodeConfig, Decoder, Encoder, EncoderConfig, RecompressConfig, Recompressor, recompress_bytes,
};

mod support;

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
        RecompressConfig::default().with_encode_config(
            EncoderConfig::default()
                .with_threads(1)
                .with_block_size(576),
        ),
    )
    .recompress(Cursor::new(flac), &mut output)
    .unwrap();

    let recompressed = output.into_inner();
    let decoded = Decoder::default().decode_bytes(&recompressed).unwrap();

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
        RecompressConfig::default().with_encode_config(
            EncoderConfig::default()
                .with_threads(1)
                .with_block_size(576),
        ),
    )
    .recompress_file(&input_path, &output_path)
    .unwrap();

    assert_eq!(summary.block_size, 576);
    assert!(output_path.exists());
    let decoded = Decoder::default()
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

    let strict = Recompressor::new(
        RecompressConfig::default()
            .with_decode_config(DecodeConfig::default().with_strict_seektable_validation(true)),
    )
    .recompress_bytes(&malformed)
    .unwrap_err();

    assert!(
        strict
            .to_string()
            .contains("seektable sample numbers must be in ascending order")
    );
}
