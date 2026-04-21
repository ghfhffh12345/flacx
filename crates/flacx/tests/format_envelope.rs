#![cfg(feature = "wav")]

use flacx::{EncoderConfig, builtin::decode_bytes};

mod support;
use support::TestEncoder as Encoder;

#[cfg(feature = "aiff")]
use support::{aifc_pcm_bytes, aiff_pcm_bytes};
#[cfg(feature = "caf")]
use support::{caf_bytes_with_options, caf_lpcm_bytes, caf_lpcm_bytes_with_channel_bitmap};
use support::{
    extensible_pcm_wav_bytes, flac_metadata_blocks, ordinary_channel_mask,
    parse_first_flac_frame_header, parse_vorbis_comment_entries, parse_wav_format, pcm_wav_bytes,
    rf64_extensible_pcm_wav_bytes, rf64_from_wav_bytes, rf64_pcm_wav_bytes, sample_fixture,
    w64_extensible_pcm_wav_bytes, w64_pcm_wav_bytes, wav_chunk_payloads, wav_data_bytes,
};

#[cfg(feature = "aiff")]
#[cfg(feature = "caf")]
#[test]
fn keeps_existing_mono_and_stereo_pcm_round_trips_green() {
    let mono_samples = sample_fixture(1, 2_048);
    let stereo_samples = sample_fixture(2, 3_072);
    let cases = [
        (1u16, 16u16, 44_100u32, &mono_samples[..]),
        (2u16, 24u16, 48_000u32, &stereo_samples[..]),
    ];

    for (channels, bits_per_sample, sample_rate, samples) in cases {
        let wav = pcm_wav_bytes(bits_per_sample, channels, sample_rate, samples);
        let flac = Encoder::new(EncoderConfig::default().with_threads(2))
            .encode_bytes(&wav)
            .unwrap();
        let decoded = decode_bytes(&flac).unwrap();

        assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
        let format = parse_wav_format(&decoded);
        assert_eq!(format.format_tag, 1);
        assert_eq!(format.channels, channels);
        assert_eq!(format.bits_per_sample, bits_per_sample);
        assert_eq!(format.valid_bits_per_sample, None);
        assert_eq!(format.channel_mask, None);
    }
}

#[test]
fn encodes_expected_frame_header_bit_depth_codes_for_existing_depths() {
    let cases = [(16u16, 0b100u8), (24u16, 0b110u8)];

    for (bits_per_sample, expected_code) in cases {
        let wav = pcm_wav_bytes(bits_per_sample, 1, 44_100, &sample_fixture(1, 1_024));
        let flac = Encoder::new(EncoderConfig::default().with_threads(1))
            .encode_bytes(&wav)
            .unwrap();
        let header = parse_first_flac_frame_header(&flac);

        assert_eq!(header.sample_size_bits, expected_code);
        assert_eq!(header.channel_assignment_bits, 0b0000);
    }
}

#[test]
fn writes_ordinary_channel_masks_for_representative_multichannel_layouts() {
    let cases = [
        (3u16, 20u16, 24u16, 48_000u32),
        (5u16, 16u16, 16u16, 44_100u32),
        (8u16, 24u16, 24u16, 96_000u32),
    ];

    for (channels, valid_bits, container_bits, sample_rate) in cases {
        let mask = ordinary_channel_mask(channels).unwrap();
        let wav = extensible_pcm_wav_bytes(
            valid_bits,
            container_bits,
            channels,
            sample_rate,
            mask,
            &sample_fixture(channels, 2),
        );
        let format = parse_wav_format(&wav);

        assert_eq!(format.format_tag, 0xFFFE);
        assert_eq!(format.channels, channels);
        assert_eq!(format.bits_per_sample, container_bits);
        assert_eq!(format.valid_bits_per_sample, Some(valid_bits));
        assert_eq!(format.channel_mask, Some(mask));
        assert_eq!(format.sample_rate, sample_rate);
        assert_eq!(
            format.byte_rate,
            sample_rate * u32::from(channels) * u32::from(container_bits / 8)
        );
        assert_eq!(format.block_align, channels * (container_bits / 8));
    }
}

#[test]
fn left_aligns_valid_bits_in_wider_pcm_container() {
    let wav = extensible_pcm_wav_bytes(
        12,
        16,
        2,
        44_100,
        ordinary_channel_mask(2).unwrap(),
        &[0x123, -0x123],
    );
    let data = wav_data_bytes(&wav);

    assert_eq!(&data[..4], &[0x30, 0x12, 0xD0, 0xED]);
    assert_eq!(data[0] & 0x0F, 0);
    assert_eq!(data[2] & 0x0F, 0);
}

#[test]
fn round_trips_representative_multichannel_independent_only_envelopes() {
    let cases = [
        (3u16, 16u16, 16u16, 48_000u32, 0b100u8),
        (5u16, 20u16, 24u16, 44_100u32, 0b101u8),
        (8u16, 24u16, 24u16, 96_000u32, 0b110u8),
    ];

    for (channels, valid_bits, container_bits, sample_rate, expected_sample_size_bits) in cases {
        let samples = sample_fixture(channels, 1_024);
        let wav = extensible_pcm_wav_bytes(
            valid_bits,
            container_bits,
            channels,
            sample_rate,
            ordinary_channel_mask(channels).unwrap(),
            &samples,
        );
        let flac = Encoder::new(EncoderConfig::default().with_threads(2))
            .encode_bytes(&wav)
            .unwrap();
        let decoded = decode_bytes(&flac).unwrap();
        let header = parse_first_flac_frame_header(&flac);
        let format = parse_wav_format(&decoded);

        assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
        assert_eq!(header.channel_assignment_bits, (channels - 1) as u8);
        assert_eq!(header.sample_size_bits, expected_sample_size_bits);
        assert_eq!(format.format_tag, 0xFFFE);
        assert_eq!(format.channels, channels);
        assert_eq!(format.bits_per_sample, container_bits);
        assert_eq!(format.valid_bits_per_sample, Some(valid_bits));
        assert_eq!(format.channel_mask, ordinary_channel_mask(channels));
    }
}

#[test]
fn round_trips_non_ordinary_channel_masks_via_rfc_vorbis_comment() {
    let mask = 0x0001_2104u32;
    let samples = sample_fixture(4, 1_024);
    let wav = extensible_pcm_wav_bytes(16, 16, 4, 48_000, mask, &samples);
    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&wav)
        .unwrap();
    let decoded = decode_bytes(&flac).unwrap();
    let blocks = flac_metadata_blocks(&flac);
    let vorbis = blocks
        .iter()
        .find(|block| block.block_type == 4)
        .expect("vorbis comment block present for non-ordinary mask");
    let comments = parse_vorbis_comment_entries(&vorbis.payload);
    let decoded_format = parse_wav_format(&decoded);

    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
    assert!(comments.iter().any(|(key, value)| {
        key == "WAVEFORMATEXTENSIBLE_CHANNEL_MASK" && value == "0x00012104"
    }));
    assert!(
        comments
            .iter()
            .any(|(key, value)| { key == "FLACX_CHANNEL_LAYOUT_PROVENANCE" && value == "1" })
    );
    assert_eq!(decoded_format.channel_mask, Some(mask));
    assert_eq!(wav_chunk_payloads(&decoded, *b"fxmd").len(), 1);
}

#[test]
fn round_trips_zero_channel_mask_via_rfc_vorbis_comment() {
    let wav = extensible_pcm_wav_bytes(16, 16, 2, 44_100, 0, &[1, -1, 2, -2]);
    let flac = Encoder::new(EncoderConfig::default().with_threads(1))
        .encode_bytes(&wav)
        .unwrap();
    let decoded = decode_bytes(&flac).unwrap();
    let blocks = flac_metadata_blocks(&flac);
    let vorbis = blocks
        .iter()
        .find(|block| block.block_type == 4)
        .expect("vorbis comment block present for zero channel mask");
    let comments = parse_vorbis_comment_entries(&vorbis.payload);
    let decoded_format = parse_wav_format(&decoded);

    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
    assert!(comments.iter().any(|(key, value)| {
        key == "WAVEFORMATEXTENSIBLE_CHANNEL_MASK" && value == "0x00000000"
    }));
    assert!(
        comments
            .iter()
            .any(|(key, value)| { key == "FLACX_CHANNEL_LAYOUT_PROVENANCE" && value == "1" })
    );
    assert_eq!(decoded_format.format_tag, 0xFFFE);
    assert_eq!(decoded_format.channel_mask, Some(0));
    assert_eq!(wav_chunk_payloads(&decoded, *b"fxmd").len(), 1);
}

#[cfg(feature = "aiff")]
#[test]
fn round_trips_stage_two_aiff_and_aifc_inputs_through_existing_encode_path() {
    let cases = [
        (
            aiff_pcm_bytes(24, 3, 48_000, &sample_fixture(3, 512)),
            extensible_pcm_wav_bytes(
                24,
                24,
                3,
                48_000,
                ordinary_channel_mask(3).unwrap(),
                &sample_fixture(3, 512),
            ),
            24u16,
            3u16,
        ),
        (
            aiff_pcm_bytes(32, 1, 44_100, &sample_fixture(1, 1_024)),
            pcm_wav_bytes(32, 1, 44_100, &sample_fixture(1, 1_024)),
            32u16,
            1u16,
        ),
        (
            aifc_pcm_bytes(*b"NONE", 20, 4, 96_000, &sample_fixture(4, 256)),
            extensible_pcm_wav_bytes(
                20,
                24,
                4,
                96_000,
                ordinary_channel_mask(4).unwrap(),
                &sample_fixture(4, 256),
            ),
            20u16,
            4u16,
        ),
        (
            aifc_pcm_bytes(*b"sowt", 16, 2, 44_100, &sample_fixture(2, 1_024)),
            pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 1_024)),
            16u16,
            2u16,
        ),
    ];

    for (input, reference_wav, expected_valid_bits, expected_channels) in cases {
        let flac = Encoder::new(EncoderConfig::default().with_threads(2))
            .encode_bytes(&input)
            .unwrap();
        let decoded = decode_bytes(&flac).unwrap();
        let format = parse_wav_format(&decoded);

        assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&reference_wav));
        assert_eq!(format.channels, expected_channels);
        assert_eq!(
            format.sample_rate,
            parse_wav_format(&reference_wav).sample_rate
        );
        assert_eq!(
            format
                .valid_bits_per_sample
                .unwrap_or(format.bits_per_sample),
            expected_valid_bits
        );
    }
}

#[cfg(feature = "aiff")]
#[test]
fn rejects_stage_two_aifc_inputs_outside_the_exact_allowlist() {
    let reject_cases = [
        aifc_pcm_bytes(*b"ACE2", 16, 1, 44_100, &sample_fixture(1, 8)),
        aifc_pcm_bytes(*b"ACE8", 16, 1, 44_100, &sample_fixture(1, 8)),
        aifc_pcm_bytes(*b"MAC3", 16, 1, 44_100, &sample_fixture(1, 8)),
        aifc_pcm_bytes(*b"MAC6", 16, 1, 44_100, &sample_fixture(1, 8)),
        aifc_pcm_bytes(*b"fl32", 32, 1, 44_100, &sample_fixture(1, 8)),
        aifc_pcm_bytes(*b"sowt", 24, 1, 44_100, &sample_fixture(1, 8)),
        aifc_pcm_bytes(*b"????", 16, 1, 44_100, &sample_fixture(1, 8)),
    ];

    for input in reject_cases {
        let error = Encoder::default().encode_bytes(&input).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("AIFC") || message.contains("float"),
            "unexpected error: {message}"
        );
    }
}

#[cfg(feature = "caf")]
#[test]
fn round_trips_stage_three_caf_lpcm_inputs_through_existing_encode_path() {
    let cases = [
        (
            caf_lpcm_bytes(16, 16, 1, 44_100, false, &sample_fixture(1, 1_024)),
            pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 1_024)),
        ),
        (
            caf_lpcm_bytes(24, 24, 2, 48_000, true, &sample_fixture(2, 512)),
            pcm_wav_bytes(24, 2, 48_000, &sample_fixture(2, 512)),
        ),
        (
            caf_lpcm_bytes(24, 32, 2, 96_000, false, &sample_fixture(2, 256)),
            pcm_wav_bytes(24, 2, 96_000, &sample_fixture(2, 256)),
        ),
        (
            caf_lpcm_bytes_with_channel_bitmap(
                16,
                16,
                4,
                48_000,
                true,
                ordinary_channel_mask(4).unwrap(),
                &sample_fixture(4, 256),
            ),
            extensible_pcm_wav_bytes(
                16,
                16,
                4,
                48_000,
                ordinary_channel_mask(4).unwrap(),
                &sample_fixture(4, 256),
            ),
        ),
    ];

    for (input, reference_wav) in cases {
        let flac = Encoder::new(EncoderConfig::default().with_threads(2))
            .encode_bytes(&input)
            .unwrap();
        let decoded = decode_bytes(&flac).unwrap();

        assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&reference_wav));
        assert_eq!(
            parse_wav_format(&decoded).sample_rate,
            parse_wav_format(&reference_wav).sample_rate
        );
    }
}

#[cfg(feature = "caf")]
#[test]
fn rejects_stage_three_caf_inputs_outside_the_allowlist() {
    let reject_cases = [
        caf_bytes_with_options(
            *b"alac",
            0,
            16,
            16,
            2,
            44_100,
            &sample_fixture(2, 8),
            false,
            false,
            true,
            true,
        ),
        caf_bytes_with_options(
            *b"lpcm",
            1,
            32,
            32,
            2,
            44_100,
            &sample_fixture(2, 8),
            false,
            false,
            true,
            true,
        ),
        caf_bytes_with_options(
            *b"lpcm",
            0,
            16,
            16,
            2,
            44_100,
            &sample_fixture(2, 8),
            true,
            false,
            true,
            true,
        ),
        caf_bytes_with_options(
            *b"lpcm",
            0,
            16,
            16,
            2,
            44_100,
            &sample_fixture(2, 8),
            false,
            true,
            true,
            true,
        ),
        caf_lpcm_bytes(16, 16, 4, 48_000, true, &sample_fixture(4, 8)),
        caf_bytes_with_options(
            *b"lpcm",
            0,
            16,
            16,
            2,
            44_100,
            &sample_fixture(2, 8),
            false,
            false,
            false,
            true,
        ),
        caf_bytes_with_options(
            *b"lpcm",
            0,
            16,
            16,
            2,
            44_100,
            &sample_fixture(2, 8),
            false,
            false,
            true,
            false,
        ),
    ];

    for input in reject_cases {
        let error = Encoder::default().encode_bytes(&input).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("CAF") || message.contains("channel layout"),
            "unexpected error: {message}"
        );
    }
}

#[test]
fn round_trips_rf64_pcm_input_through_existing_encode_path() {
    let samples = sample_fixture(2, 2_048);
    let rf64 = rf64_pcm_wav_bytes(16, 2, 44_100, &samples);
    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&rf64)
        .unwrap();
    let decoded = decode_bytes(&flac).unwrap();
    let decoded_format = parse_wav_format(&decoded);

    assert_eq!(
        wav_data_bytes(&decoded),
        wav_data_bytes(&pcm_wav_bytes(16, 2, 44_100, &samples))
    );
    assert_eq!(decoded_format.format_tag, 1);
    assert_eq!(decoded_format.channels, 2);
    assert_eq!(decoded_format.bits_per_sample, 16);
}

#[test]
fn round_trips_rf64_extensible_multichannel_pcm() {
    let samples = sample_fixture(4, 1_024);
    let mask = ordinary_channel_mask(4).unwrap();
    let rf64 = rf64_extensible_pcm_wav_bytes(20, 24, 4, 48_000, mask, &samples);
    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&rf64)
        .unwrap();
    let decoded = decode_bytes(&flac).unwrap();
    let decoded_format = parse_wav_format(&decoded);

    assert_eq!(
        wav_data_bytes(&decoded),
        wav_data_bytes(&extensible_pcm_wav_bytes(20, 24, 4, 48_000, mask, &samples))
    );
    assert_eq!(decoded_format.format_tag, 0xFFFE);
    assert_eq!(decoded_format.channels, 4);
    assert_eq!(decoded_format.valid_bits_per_sample, Some(20));
    assert_eq!(decoded_format.channel_mask, Some(mask));
}

#[test]
fn rejects_malformed_rf64_missing_ds64() {
    let samples = sample_fixture(2, 256);
    let wav = pcm_wav_bytes(16, 2, 44_100, &samples);
    let mut rf64 = rf64_from_wav_bytes(&wav, 256);
    rf64.drain(12..48);

    let error = Encoder::default().encode_bytes(&rf64).unwrap_err();
    assert!(error.to_string().contains("ds64"));
}

#[test]
fn round_trips_w64_pcm_input_through_existing_encode_path() {
    let samples = sample_fixture(2, 2_048);
    let w64 = w64_pcm_wav_bytes(16, 2, 44_100, &samples);
    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&w64)
        .unwrap();
    let decoded = decode_bytes(&flac).unwrap();

    assert_eq!(
        wav_data_bytes(&decoded),
        wav_data_bytes(&pcm_wav_bytes(16, 2, 44_100, &samples))
    );
}

#[test]
fn round_trips_w64_extensible_multichannel_pcm() {
    let samples = sample_fixture(3, 1_024);
    let mask = ordinary_channel_mask(3).unwrap();
    let w64 = w64_extensible_pcm_wav_bytes(20, 24, 3, 48_000, mask, &samples);
    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&w64)
        .unwrap();
    let decoded = decode_bytes(&flac).unwrap();
    let decoded_format = parse_wav_format(&decoded);

    assert_eq!(
        wav_data_bytes(&decoded),
        wav_data_bytes(&extensible_pcm_wav_bytes(20, 24, 3, 48_000, mask, &samples))
    );
    assert_eq!(decoded_format.format_tag, 0xFFFE);
    assert_eq!(decoded_format.channels, 3);
    assert_eq!(decoded_format.valid_bits_per_sample, Some(20));
}
