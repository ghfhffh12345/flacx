use std::{fs, io::Cursor};

use flacx::{
    DecodeSummary, Decoder, Encoder, EncoderConfig, PcmSpec, PcmStream, RawPcmByteOrder,
    RawPcmDescriptor, convenience, decode_bytes, encode_file, inspect_raw_pcm_total_samples,
    level::Level, read_pcm_stream, write_pcm_stream,
};

mod support;

use support::{
    parse_wav_format, pcm_wav_bytes, raw_pcm_fixture, sample_fixture, unique_temp_path,
    wav_data_bytes,
};

#[test]
fn convenience_encode_bytes_matches_default_encoder() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let via_module = convenience::encode_bytes(&wav).unwrap();
    let via_encoder = Encoder::default().encode_bytes(&wav).unwrap();
    assert_eq!(via_module, via_encoder);
}

#[test]
fn encode_file_uses_configured_options() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 4_096));
    let input_path = unique_temp_path("wav");
    let output_path = unique_temp_path("flac");
    fs::write(&input_path, &wav).unwrap();

    let summary = Encoder::new(
        EncoderConfig::default()
            .with_level(Level::Level0)
            .with_threads(1)
            .with_block_size(576),
    )
    .encode_file(&input_path, &output_path)
    .unwrap();

    assert_eq!(summary.block_size, 576);
    assert_eq!(summary.channels, 2);
    assert!(output_path.exists());

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn convenience_encode_file_matches_default_encoder_output() {
    let wav = pcm_wav_bytes(16, 1, 32_000, &sample_fixture(1, 2_048));
    let input_path = unique_temp_path("wav");
    let output_path = unique_temp_path("flac");
    fs::write(&input_path, &wav).unwrap();

    let summary = encode_file(&input_path, &output_path).unwrap();
    let bytes_from_file = fs::read(&output_path).unwrap();
    let bytes_from_memory = Encoder::default().encode_bytes(&wav).unwrap();

    assert_eq!(summary.total_samples, 2_048);
    assert_eq!(bytes_from_file, bytes_from_memory);

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn api_accepts_seekable_readers_and_writers() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 1_024));
    let mut output = Cursor::new(Vec::new());
    let summary = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode(Cursor::new(wav), &mut output)
        .unwrap();

    assert!(summary.frame_count >= 1);
    assert!(output.get_ref().starts_with(b"fLaC"));
}

#[test]
fn convenience_decode_bytes_matches_default_decoder() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();

    let via_function = decode_bytes(&flac).unwrap();
    let via_decoder = Decoder::default().decode_bytes(&flac).unwrap();
    let format = parse_wav_format(&via_decoder);

    assert_eq!(via_function, via_decoder);
    assert_eq!(wav_data_bytes(&via_decoder), wav_data_bytes(&wav));
    assert_eq!(format.channels, 1);
    assert_eq!(format.sample_rate, 44_100);
    assert_eq!(format.bits_per_sample, 16);
}

#[test]
fn decode_api_accepts_seekable_readers_and_returns_summary() {
    let wav = pcm_wav_bytes(24, 2, 48_000, &sample_fixture(2, 3_000));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let mut output = Cursor::new(Vec::new());

    let summary = Decoder::default()
        .decode(Cursor::new(flac), &mut output)
        .unwrap();

    assert_eq!(
        summary,
        DecodeSummary {
            frame_count: summary.frame_count,
            total_samples: 3_000,
            block_size: summary.block_size,
            min_frame_size: summary.min_frame_size,
            max_frame_size: summary.max_frame_size,
            min_block_size: summary.min_block_size,
            max_block_size: summary.max_block_size,
            sample_rate: 48_000,
            channels: 2,
            bits_per_sample: 24,
        }
    );
    let decoded = output.into_inner();
    let format = parse_wav_format(&decoded);
    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
    assert_eq!(format.channels, 2);
    assert_eq!(format.sample_rate, 48_000);
    assert_eq!(format.bits_per_sample, 24);
}

#[test]
fn typed_pcm_aliases_are_usable_from_the_public_api() {
    let wav = pcm_wav_bytes(24, 2, 48_000, &sample_fixture(2, 256));
    let stream: PcmStream = read_pcm_stream(Cursor::new(&wav)).unwrap();
    let spec: PcmSpec = stream.spec;

    assert_eq!(spec.sample_rate, 48_000);
    assert_eq!(spec.channels, 2);
    assert_eq!(spec.bits_per_sample, 24);
    assert_eq!(stream.samples.len(), 512);
}

#[test]
fn explicit_pcm_stream_pipeline_round_trips_without_convenience_inference() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 1_024));
    let stream = read_pcm_stream(Cursor::new(&wav)).unwrap();

    let mut flac = Cursor::new(Vec::new());
    let encode_summary = Encoder::default()
        .encode_pcm_stream(&stream, &mut flac)
        .unwrap();
    let decoded_stream = Decoder::default()
        .decode_pcm_stream(Cursor::new(flac.into_inner()))
        .unwrap();
    let mut roundtrip = Cursor::new(Vec::new());
    write_pcm_stream(&mut roundtrip, &decoded_stream, flacx::PcmContainer::Wave).unwrap();

    assert_eq!(encode_summary.total_samples, 1_024);
    assert_eq!(decoded_stream.spec.sample_rate, 44_100);
    assert_eq!(decoded_stream.spec.channels, 2);
    assert_eq!(decoded_stream.samples, stream.samples);
    assert_eq!(wav_data_bytes(roundtrip.get_ref()), wav_data_bytes(&wav));
}

#[test]
fn decode_builder_supports_strict_channel_mask_provenance() {
    let config = flacx::DecodeConfig::builder()
        .threads(2)
        .strict_channel_mask_provenance(true)
        .build();

    assert_eq!(
        config,
        flacx::DecodeConfig::default()
            .with_threads(2)
            .with_strict_channel_mask_provenance(true)
    );
}

#[test]
fn raw_api_round_trips_with_explicit_descriptor() {
    let samples = sample_fixture(2, 1_024);
    let (raw_bytes, descriptor) = raw_pcm_fixture(
        44_100,
        2,
        16,
        16,
        RawPcmByteOrder::LittleEndian,
        None,
        &samples,
    );
    let mut output = Cursor::new(Vec::new());
    let summary = Encoder::default()
        .encode_raw(Cursor::new(&raw_bytes), &mut output, descriptor)
        .unwrap();
    let flac = output.into_inner();
    let decoded = decode_bytes(&flac).unwrap();
    let expected = pcm_wav_bytes(16, 2, 44_100, &samples);

    assert_eq!(summary.total_samples, 1_024);
    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&expected));
    assert_eq!(parse_wav_format(&decoded).channels, 2);
    assert_eq!(
        inspect_raw_pcm_total_samples(Cursor::new(&raw_bytes), descriptor).unwrap(),
        1_024
    );
}

#[test]
fn raw_api_rejects_missing_multichannel_channel_mask() {
    let descriptor = RawPcmDescriptor {
        sample_rate: 48_000,
        channels: 4,
        valid_bits_per_sample: 16,
        container_bits_per_sample: 16,
        byte_order: RawPcmByteOrder::LittleEndian,
        channel_mask: None,
    };
    let error = inspect_raw_pcm_total_samples(Cursor::new(vec![0u8; 16]), descriptor).unwrap_err();
    assert!(error.to_string().contains("channel mask"));
}
