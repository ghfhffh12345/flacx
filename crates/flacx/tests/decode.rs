use std::{fs, io::Cursor, thread::available_parallelism};

use flacx::{
    DecodeConfig, Decoder, Encoder, EncoderConfig, decode_bytes, decode_file, level::Level,
};

mod support;

use support::{
    ParsedFlacBlockingStrategy, ParsedFlacCodedNumberKind, application_block,
    corrupt_first_flac_frame_sample_number, corrupt_last_frame_crc, corrupt_magic, cue_chunk,
    cuesheet_block, extensible_pcm_wav_bytes, flac_frames, info_list_chunk, ordinary_channel_mask,
    parse_first_flac_frame_header, parse_wav_format, pcm_wav_bytes, raw_seektable_block,
    replace_flac_optional_metadata, rewrite_streaminfo_md5, sample_fixture, seektable_block,
    streaminfo_md5, truncate_bytes, unique_temp_path, vorbis_comment_block, wav_cue_points,
    wav_info_entries, wav_with_chunks,
};

fn decode_thread_variants() -> [usize; 2] {
    [1, DecodeConfig::default().threads.max(2)]
}

fn decoder_for_threads(threads: usize) -> Decoder {
    Decoder::new(DecodeConfig::default().with_threads(threads))
}

fn decode_bytes_with_threads(flac: &[u8], threads: usize) -> Vec<u8> {
    decoder_for_threads(threads).decode_bytes(flac).unwrap()
}

fn assert_round_trips_bytes_exactly(wav: &[u8], flac: &[u8]) {
    for threads in decode_thread_variants() {
        let decoded = decode_bytes_with_threads(flac, threads);
        assert_eq!(
            decoded, wav,
            "decode_bytes changed output for threads={threads}"
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
            fs::read(&output_path).unwrap(),
            wav,
            "decode_file changed output for threads={threads}"
        );

        let _ = fs::remove_file(output_path);
    }

    let _ = fs::remove_file(input_path);
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
    assert_eq!(decode_bytes(&flac).unwrap(), wav);

    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("wav");
    fs::write(&input_path, &flac).unwrap();

    let summary = decode_file(&input_path, &output_path).unwrap();
    assert_eq!(summary.total_samples, 2_048);
    assert_eq!(fs::read(&output_path).unwrap(), wav);

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

    let summary = Decoder::default()
        .decode(Cursor::new(flac), &mut output)
        .unwrap();

    assert_eq!(summary.total_samples, 512);
    assert_eq!(output.into_inner(), wav);
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

    let decoded = Decoder::new(DecodeConfig::default().with_strict_seektable_validation(true))
        .decode_bytes(&flac)
        .unwrap();

    assert_eq!(decoded, wav);
}

#[test]
fn decode_tolerates_malformed_seektable_by_default() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(&flac, &[raw_seektable_block(&[0u8; 17])]);

    let decoded = decode_bytes(&flac).unwrap();

    assert_eq!(decoded, wav);
}

#[test]
fn decode_rejects_invalid_length_seektable_when_strict() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let flac = replace_flac_optional_metadata(&flac, &[raw_seektable_block(&[0u8; 17])]);

    let error = Decoder::new(DecodeConfig::default().with_strict_seektable_validation(true))
        .decode_bytes(&flac)
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("seektable payload length must be a multiple of 18 bytes")
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

    let error = Decoder::new(DecodeConfig::default().with_strict_seektable_validation(true))
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

    let error = Decoder::new(DecodeConfig::default().with_strict_seektable_validation(true))
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

    let error = Decoder::new(DecodeConfig::default().with_strict_seektable_validation(true))
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
    let decoded = Decoder::new(DecodeConfig::default().with_strict_channel_mask_provenance(true))
        .decode_bytes(&flac)
        .unwrap();

    assert_eq!(decoded, wav);
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
    let decoded = Decoder::new(DecodeConfig::default().with_strict_channel_mask_provenance(true))
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

    let error = Decoder::new(DecodeConfig::default().with_strict_channel_mask_provenance(true))
        .decode_bytes(&flac)
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("FLACX_CHANNEL_LAYOUT_PROVENANCE")
    );
}
