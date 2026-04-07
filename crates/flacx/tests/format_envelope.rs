use flacx::{Encoder, EncoderConfig, decode_bytes};

mod support;

use support::{
    extensible_pcm_wav_bytes, flac_metadata_blocks, ordinary_channel_mask,
    parse_first_flac_frame_header, parse_vorbis_comment_entries, parse_wav_format, pcm_wav_bytes,
    sample_fixture, wav_chunk_payloads, wav_data_bytes,
};

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

        assert_eq!(decoded, wav);
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

        assert_eq!(decoded, wav);
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
    assert_eq!(wav_chunk_payloads(&decoded, *b"fxvc").len(), 1);
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
    assert_eq!(wav_chunk_payloads(&decoded, *b"fxvc").len(), 1);
}
