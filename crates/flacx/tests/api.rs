use std::{
    cell::RefCell,
    fs,
    io::{Cursor, Read, Seek, SeekFrom},
    rc::Rc,
};

use flacx::{
    DecodePcmStream, DecodeSource, DecodeSummary, EncodePcmStream, EncodeSource, EncoderConfig,
    FlacReaderOptions, Metadata, PcmReader, PcmSpec, RawPcmByteOrder, RawPcmDescriptor,
    RecompressConfig, RecompressMode, WavReader, builtin, inspect_raw_pcm_total_samples,
    level::Level, read_flac_reader, read_flac_reader_with_options, write_pcm_stream,
};

mod support;

#[cfg(all(feature = "aiff", feature = "caf"))]
use flacx::PcmContainer;
use support::TestDecoder;
use support::{
    ParsedFlacBlockingStrategy, ParsedFlacCodedNumberKind, cue_chunk, flac_metadata_blocks,
    info_list_chunk, parse_first_flac_frame_header, parse_vorbis_comment_entries, parse_wav_format,
    pcm_wav_bytes, raw_pcm_fixture, sample_fixture, unique_temp_path, wav_chunk_payloads,
    wav_cue_points, wav_data_bytes, wav_with_chunks,
};
#[cfg(feature = "aiff")]
use support::{aiff_pcm_bytes, is_aifc_bytes, is_aiff_bytes};
#[cfg(feature = "caf")]
use support::{caf_lpcm_bytes, is_caf_bytes};

fn recompress_reader_options(config: RecompressConfig) -> FlacReaderOptions {
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

#[test]
fn builtin_encode_bytes_matches_explicit_reader_session_flow() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let via_builtin = builtin::encode_bytes(&wav).unwrap();

    let reader = PcmReader::new(Cursor::new(&wav)).unwrap();
    let mut encoder = EncoderConfig::default().into_encoder(Cursor::new(Vec::new()));
    let summary = encoder.encode_source(reader.into_source()).unwrap();
    let via_session = encoder.into_inner().into_inner();

    assert_eq!(summary.total_samples, 2_048);
    assert_eq!(via_builtin, via_session);
}

#[test]
fn reader_session_flow_uses_configured_options() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 4_096));
    let input_path = unique_temp_path("wav");
    let output_path = unique_temp_path("flac");
    fs::write(&input_path, &wav).unwrap();

    let reader = PcmReader::new(fs::File::open(&input_path).unwrap()).unwrap();
    let mut encoder = EncoderConfig::default()
        .with_level(Level::Level0)
        .with_threads(1)
        .with_block_size(576)
        .into_encoder(fs::File::create(&output_path).unwrap());
    let summary = encoder.encode_source(reader.into_source()).unwrap();

    assert_eq!(summary.block_size, 576);
    assert_eq!(summary.channels, 2);
    assert!(output_path.exists());

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn builtin_encode_file_matches_explicit_reader_session_output() {
    let wav = pcm_wav_bytes(16, 1, 32_000, &sample_fixture(1, 2_048));
    let input_path = unique_temp_path("wav");
    let output_path = unique_temp_path("flac");
    fs::write(&input_path, &wav).unwrap();

    let summary = builtin::encode_file(&input_path, &output_path).unwrap();
    let bytes_from_file = fs::read(&output_path).unwrap();

    let reader = PcmReader::new(Cursor::new(&wav)).unwrap();
    let mut encoder = EncoderConfig::default().into_encoder(Cursor::new(Vec::new()));
    encoder.encode_source(reader.into_source()).unwrap();
    let bytes_from_memory = encoder.into_inner().into_inner();

    assert_eq!(summary.total_samples, 2_048);
    assert_eq!(bytes_from_file, bytes_from_memory);

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn builtin_convenience_no_longer_uses_legacy_helpers() {
    let source = include_str!("../src/convenience.rs");
    assert!(!source.contains("read_wav_for_encode_with_config"));
    assert!(!source.contains("encode_buffered_input_with_sink"));
    assert!(!source.contains("decode_flac_to_pcm_with_config"));
    assert!(!source.contains("can_use_wav_family_encode_fastpath"));
    assert!(source.contains("fn recompress_reader_session_with_config_and_progress"));
    assert_eq!(source.matches("into_recompress_source()").count(), 1);
    assert_eq!(
        source
            .matches("recompress_with_sink(source, progress)")
            .count(),
        1
    );
}

#[test]
fn recompress_public_exports_remain_stable() {
    let source = include_str!("../src/lib.rs");
    assert!(source.contains("pub mod builtin {"));
    assert!(source.contains("recompress_bytes, recompress_file,"));
    assert!(source.contains("pub use recompress::{"));
    assert!(source.contains("FlacRecompressSource, RecompressBuilder, RecompressConfig, RecompressMode, RecompressPhase,"));
    assert!(source.contains("RecompressProgress, RecompressSummary, Recompressor,"));
}

#[test]
fn source_api_exports_replace_split_metadata_surface() {
    let lib_source = include_str!("../src/lib.rs");
    let encoder_source = include_str!("../src/encoder.rs");
    let decode_source = include_str!("../src/decode.rs");
    let input_source = include_str!("../src/input.rs");

    assert!(!lib_source.contains("AnyPcmStream,"));
    assert!(!lib_source.contains("PcmReaderOptions,"));
    assert!(!lib_source.contains("read_pcm_reader,"));
    assert!(!lib_source.contains("read_pcm_reader_with_options,"));
    assert!(lib_source.contains("EncodeSource"));
    assert!(lib_source.contains("DecodeSource"));
    assert!(lib_source.contains("pub use metadata::Metadata;"));
    assert!(lib_source.contains("Metadata,"));
    assert!(
        !lib_source.contains("pub use metadata::{DecodeMetadata, EncodeMetadata, WavMetadata};")
    );

    assert!(encoder_source.contains("pub fn encode_source<"));
    assert!(!encoder_source.contains("pub fn set_metadata("));
    assert!(!encoder_source.contains("pub fn with_metadata("));

    assert!(decode_source.contains("pub fn decode_source<"));
    assert!(!decode_source.contains("pub fn set_metadata("));
    assert!(!decode_source.contains("pub fn with_metadata("));

    assert!(input_source.contains("pub fn new(reader: R) -> Result<Self>"));
    assert!(!input_source.contains("pub fn read_pcm_reader<"));
    assert!(!input_source.contains("pub fn read_pcm_reader_with_options<"));
}

#[cfg(feature = "aiff")]
#[test]
fn builtin_encode_file_accepts_aiff_inputs() {
    let aiff = aiff_pcm_bytes(16, 1, 44_100, &sample_fixture(1, 1_024));
    let input_path = unique_temp_path("aiff");
    let output_path = unique_temp_path("flac");
    fs::write(&input_path, &aiff).unwrap();

    let summary = builtin::encode_file(&input_path, &output_path).unwrap();
    assert_eq!(summary.total_samples, 1_024);
    assert!(output_path.exists());

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[cfg(feature = "aiff")]
#[test]
fn explicit_reader_session_flow_accepts_aiff_inputs() {
    let aiff = aiff_pcm_bytes(16, 1, 44_100, &sample_fixture(1, 1_024));
    let via_builtin = builtin::encode_bytes(&aiff).unwrap();

    let reader = PcmReader::new(Cursor::new(&aiff)).unwrap();
    let mut encoder = EncoderConfig::default().into_encoder(Cursor::new(Vec::new()));
    let summary = encoder.encode_source(reader.into_source()).unwrap();
    let via_session = encoder.into_inner().into_inner();

    assert_eq!(summary.total_samples, 1_024);
    assert_eq!(via_builtin, via_session);
}

#[cfg(feature = "caf")]
#[test]
fn builtin_encode_bytes_accepts_caf_inputs() {
    let caf = caf_lpcm_bytes(16, 16, 2, 44_100, true, &sample_fixture(2, 1_024));
    let flac = builtin::encode_bytes(&caf).unwrap();
    assert!(flac.starts_with(b"fLaC"));
}

#[cfg(feature = "caf")]
#[test]
fn explicit_reader_session_flow_accepts_caf_inputs() {
    let caf = caf_lpcm_bytes(16, 16, 2, 44_100, true, &sample_fixture(2, 1_024));
    let via_builtin = builtin::encode_bytes(&caf).unwrap();

    let reader = PcmReader::new(Cursor::new(&caf)).unwrap();
    let mut encoder = EncoderConfig::default().into_encoder(Cursor::new(Vec::new()));
    let summary = encoder.encode_source(reader.into_source()).unwrap();
    let via_session = encoder.into_inner().into_inner();

    assert_eq!(summary.total_samples, 1_024);
    assert_eq!(via_builtin, via_session);
}

#[test]
fn api_accepts_seekable_readers_and_writer_bound_sessions() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 1_024));
    let reader = PcmReader::new(Cursor::new(wav)).unwrap();
    let mut output = Cursor::new(Vec::new());
    let mut encoder = EncoderConfig::default()
        .with_threads(2)
        .into_encoder(&mut output);
    let summary = encoder.encode_source(reader.into_source()).unwrap();

    assert!(summary.frame_count >= 1);
    assert!(output.get_ref().starts_with(b"fLaC"));
}

#[test]
fn builtin_decode_bytes_matches_default_decoder() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = builtin::encode_bytes(&wav).unwrap();

    let via_function = builtin::decode_bytes(&flac).unwrap();
    let via_decoder = TestDecoder::default().decode_bytes(&flac).unwrap();
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
    let flac = builtin::encode_bytes(&wav).unwrap();
    let reader = read_flac_reader(Cursor::new(flac)).unwrap();
    let mut decoder = flacx::DecodeConfig::default().into_decoder(Cursor::new(Vec::new()));
    let summary = decoder.decode_source(reader.into_decode_source()).unwrap();

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
    let decoded = decoder.into_inner().into_inner();
    let format = parse_wav_format(&decoded);
    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
    assert_eq!(format.channels, 2);
    assert_eq!(format.sample_rate, 48_000);
    assert_eq!(format.bits_per_sample, 24);
}

#[test]
fn explicit_recompress_reader_session_flow_preserves_audio() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 1_536));
    let flac = builtin::encode_bytes(&wav).unwrap();
    let config = RecompressConfig::default()
        .with_threads(1)
        .with_block_size(576);
    let reader =
        read_flac_reader_with_options(Cursor::new(flac), recompress_reader_options(config))
            .unwrap();
    let spec = reader.spec();
    let source = reader.into_recompress_source();
    let mut recompressor = config.into_recompressor(Cursor::new(Vec::new()));

    assert_eq!(source.total_samples(), 1_536);
    assert_eq!(source.spec(), spec);

    let summary = recompressor.recompress(source).unwrap();
    let recompressed = recompressor.into_inner().into_inner();
    let decoded = TestDecoder::default().decode_bytes(&recompressed).unwrap();

    assert_eq!(summary.block_size, 576);
    assert_eq!(summary.total_samples, 1_536);
    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
}

#[test]
fn builtin_recompress_file_matches_explicit_reader_session_output() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 1_536));
    let flac = builtin::encode_bytes(&wav).unwrap();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("flac");
    fs::write(&input_path, &flac).unwrap();

    let builtin_summary = builtin::recompress_file(&input_path, &output_path).unwrap();
    let builtin_bytes = fs::read(&output_path).unwrap();

    let config = RecompressConfig::default();
    let reader =
        read_flac_reader_with_options(Cursor::new(flac), recompress_reader_options(config))
            .unwrap();
    let source = reader.into_recompress_source();
    let mut recompressor = config.into_recompressor(Cursor::new(Vec::new()));
    let explicit_summary = recompressor.recompress(source).unwrap();
    let explicit_bytes = recompressor.into_inner().into_inner();

    assert_eq!(builtin_summary, explicit_summary);
    assert_eq!(builtin_bytes, explicit_bytes);

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn flac_reader_stream_starts_before_all_frames_are_decoded() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 16_384));
    let reader = PcmReader::new(Cursor::new(&wav)).unwrap();
    let mut encoder = EncoderConfig::default()
        .with_block_size(576)
        .into_encoder(Cursor::new(Vec::new()));
    encoder.encode_source(reader.into_source()).unwrap();
    let flac = encoder.into_inner().into_inner();

    let reader = read_flac_reader(Cursor::new(flac)).unwrap();
    let (_, mut stream) = reader.into_decode_source().into_parts();
    assert_eq!(stream.completed_input_frames(), 0);

    let mut first_chunk = Vec::new();
    let frames = stream.read_chunk(4_096, &mut first_chunk).unwrap();
    assert!(frames > 0);
    assert!(frames < 16_384);
    assert!(stream.completed_input_frames() > 0);

    let reader = read_flac_reader(Cursor::new(builtin::encode_bytes(&wav).unwrap())).unwrap();
    let mut decoder = flacx::DecodeConfig::default().into_decoder(Cursor::new(Vec::new()));
    let summary = decoder.decode_source(reader.into_decode_source()).unwrap();
    assert_eq!(summary.total_samples, 16_384);
}

#[test]
fn flac_reader_stream_single_thread_chunk_decode_round_trips_full_stream() {
    let expected_samples = sample_fixture(2, 16_384);
    let wav = pcm_wav_bytes(16, 2, 44_100, &expected_samples);
    let reader = PcmReader::new(Cursor::new(&wav)).unwrap();
    let mut encoder = EncoderConfig::default()
        .with_block_size(576)
        .into_encoder(Cursor::new(Vec::new()));
    encoder.encode_source(reader.into_source()).unwrap();
    let flac = encoder.into_inner().into_inner();

    let reader = read_flac_reader(Cursor::new(flac)).unwrap();
    let (_, mut stream) = reader.into_decode_source().into_parts();
    stream.set_threads(1);

    let mut decoded_samples = Vec::new();
    let mut chunk_reads = 0usize;
    while stream.read_chunk(4_096, &mut decoded_samples).unwrap() != 0 {
        chunk_reads += 1;
    }

    assert_eq!(decoded_samples, expected_samples);
    assert!(chunk_reads > 1);
    assert!(stream.completed_input_frames() > 0);
}

#[test]
fn wav_reader_exposes_spec_before_stream_consumption() {
    let wav = pcm_wav_bytes(24, 2, 48_000, &sample_fixture(2, 256));
    let reader = WavReader::new(Cursor::new(&wav)).unwrap();
    let spec: PcmSpec = reader.spec();
    let (_, mut stream) = reader.into_source().into_parts();
    let mut samples = Vec::new();
    let frames = stream.read_chunk(256, &mut samples).unwrap();

    assert_eq!(spec.sample_rate, 48_000);
    assert_eq!(spec.channels, 2);
    assert_eq!(spec.bits_per_sample, 24);
    assert_eq!(frames, 256);
    assert_eq!(samples.len(), 512);
}

#[test]
fn wav_reader_stream_appends_into_existing_output_buffer() {
    let expected_samples = sample_fixture(2, 4);
    let wav = pcm_wav_bytes(16, 2, 44_100, &expected_samples);
    let reader = WavReader::new(Cursor::new(&wav)).unwrap();
    let (_, mut stream) = reader.into_source().into_parts();
    let mut output = vec![-999];

    assert_eq!(stream.read_chunk(2, &mut output).unwrap(), 2);
    assert_eq!(stream.read_chunk(2, &mut output).unwrap(), 2);
    assert_eq!(stream.read_chunk(2, &mut output).unwrap(), 0);

    assert_eq!(output[0], -999);
    assert_eq!(&output[1..], expected_samples.as_slice());
}

#[test]
fn raw_reader_stream_appends_into_existing_output_buffer() {
    let expected_samples = sample_fixture(2, 4);
    let (raw_bytes, descriptor) = raw_pcm_fixture(
        44_100,
        2,
        16,
        16,
        RawPcmByteOrder::LittleEndian,
        None,
        &expected_samples,
    );
    let reader = flacx::RawPcmReader::new(Cursor::new(raw_bytes), descriptor).unwrap();
    let mut stream = reader.into_pcm_stream().unwrap();
    let mut output = vec![-999];

    assert_eq!(stream.read_chunk(1, &mut output).unwrap(), 1);
    assert_eq!(stream.read_chunk(3, &mut output).unwrap(), 3);
    assert_eq!(stream.read_chunk(1, &mut output).unwrap(), 0);

    assert_eq!(output[0], -999);
    assert_eq!(&output[1..], expected_samples.as_slice());
}

#[test]
fn explicit_reader_session_pipeline_round_trips_without_builtin_inference() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 1_024));
    let reader = WavReader::new(Cursor::new(&wav)).unwrap();

    let mut encoder = EncoderConfig::default().into_encoder(Cursor::new(Vec::new()));
    let encode_summary = encoder.encode_source(reader.into_source()).unwrap();
    let decoded_reader = read_flac_reader(Cursor::new(encoder.into_inner().into_inner())).unwrap();
    let decoded_spec = decoded_reader.spec();
    let (_, mut decoded_stream) = decoded_reader.into_decode_source().into_parts();
    let mut decoded_samples = Vec::new();
    while decoded_stream
        .read_chunk(4_096, &mut decoded_samples)
        .unwrap()
        != 0
    {}
    let mut roundtrip = Cursor::new(Vec::new());
    write_pcm_stream(
        &mut roundtrip,
        &flacx::PcmStream {
            spec: decoded_spec,
            samples: decoded_samples,
        },
        flacx::PcmContainer::Wave,
    )
    .unwrap();

    assert_eq!(encode_summary.total_samples, 1_024);
    assert_eq!(decoded_spec.sample_rate, 44_100);
    assert_eq!(decoded_spec.channels, 2);
    assert_eq!(wav_data_bytes(roundtrip.get_ref()), wav_data_bytes(&wav));
}

#[test]
fn explicit_reader_session_supports_variable_block_schedule_semantics() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 4_352));
    let reader = WavReader::new(Cursor::new(&wav)).unwrap();
    let mut encoder = EncoderConfig::default()
        .with_threads(2)
        .with_block_schedule(vec![576, 1_152, 576, 2_048])
        .into_encoder(Cursor::new(Vec::new()));

    let summary = encoder.encode_source(reader.into_source()).unwrap();
    let flac = encoder.into_inner().into_inner();
    let decoded = builtin::decode_bytes(&flac).unwrap();
    let header = parse_first_flac_frame_header(&flac);

    assert_eq!(summary.total_samples, 4_352);
    assert_eq!(summary.frame_count, 4);
    assert_eq!(summary.min_block_size, 576);
    assert_eq!(summary.max_block_size, 2_048);
    assert_eq!(
        header.blocking_strategy,
        ParsedFlacBlockingStrategy::Variable
    );
    assert_eq!(
        header.coded_number_kind,
        ParsedFlacCodedNumberKind::SampleNumber
    );
    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
}

#[test]
fn encode_source_constructor_preserves_explicit_metadata_and_stream_composition() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 1_024));
    let (metadata, stream) = WavReader::new(Cursor::new(&wav))
        .unwrap()
        .into_source()
        .into_parts();
    let mut encoder = EncoderConfig::default().into_encoder(Cursor::new(Vec::new()));
    let summary = encoder
        .encode_source(EncodeSource::new(metadata, stream))
        .unwrap();
    let decoded = builtin::decode_bytes(&encoder.into_inner().into_inner()).unwrap();

    assert_eq!(summary.total_samples, 1_024);
    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
}

#[test]
fn encode_source_new_with_scratch_metadata_emits_expected_flac_and_summary() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 1_024));
    let (_, stream) = WavReader::new(Cursor::new(&wav))
        .unwrap()
        .into_source()
        .into_parts();
    let mut metadata = Metadata::new();
    metadata.add_comment("TITLE", "Scratch Title");
    metadata.set_cue_points([0, 512]);

    let mut encoder = EncoderConfig::default().into_encoder(Cursor::new(Vec::new()));
    let summary = encoder
        .encode_source(EncodeSource::new(metadata, stream))
        .unwrap();
    let flac = encoder.into_inner().into_inner();
    let blocks = flac_metadata_blocks(&flac);
    let vorbis = blocks.iter().find(|block| block.block_type == 4).unwrap();

    assert_eq!(summary.total_samples, 1_024);
    assert!(blocks.iter().any(|block| block.block_type == 5));
    assert!(
        parse_vorbis_comment_entries(&vorbis.payload)
            .iter()
            .any(|(key, value)| key == "TITLE" && value == "Scratch Title")
    );
}

#[test]
fn shared_metadata_api_replaces_public_split_metadata_and_exposes_no_raw_authoring_surface() {
    let lib_source = include_str!("../src/lib.rs");
    let metadata_source = include_str!("../src/metadata.rs");

    assert!(lib_source.contains("pub use metadata::Metadata;"));
    assert!(!lib_source.contains("EncodeMetadata"));
    assert!(!lib_source.contains("DecodeMetadata"));
    assert!(metadata_source.contains("pub struct Metadata"));
    assert!(!metadata_source.contains("pub struct EncodeMetadata"));
    assert!(!metadata_source.contains("pub struct DecodeMetadata"));
    assert!(!metadata_source.contains("pub fn from_fxmd"));
    assert!(!metadata_source.contains("pub fn raw_"));
}

#[cfg(feature = "progress")]
#[test]
fn explicit_reader_session_progress_matches_default_output_for_variable_schedule() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 4_352));
    let config = EncoderConfig::default()
        .with_threads(2)
        .with_block_schedule(vec![576, 1_152, 576, 2_048]);

    let baseline_reader = PcmReader::new(Cursor::new(&wav)).unwrap();
    let mut baseline_encoder = config.clone().into_encoder(Cursor::new(Vec::new()));
    let expected_summary = baseline_encoder
        .encode_source(baseline_reader.into_source())
        .unwrap();
    let expected_output = baseline_encoder.into_inner().into_inner();

    let progress_reader = PcmReader::new(Cursor::new(&wav)).unwrap();
    let mut progress_encoder = config.into_encoder(Cursor::new(Vec::new()));

    let mut updates = Vec::new();
    let summary = progress_encoder
        .encode_source_with_progress(progress_reader.into_source(), |progress| {
            updates.push(progress);
            Ok(())
        })
        .unwrap();
    let output = progress_encoder.into_inner().into_inner();

    assert_eq!(summary, expected_summary);
    assert_eq!(output, expected_output);
    assert_eq!(summary.frame_count, 4);
    assert_eq!(summary.total_samples, 4_352);
    assert!(!updates.is_empty());
    assert!(updates.iter().all(|progress| progress.total_frames == 4));
    assert_eq!(
        updates.last().unwrap().processed_samples,
        summary.total_samples
    );
    assert_eq!(
        updates.last().unwrap().completed_frames,
        summary.frame_count
    );
    assert!(
        updates
            .windows(2)
            .all(|pair| pair[0].processed_samples <= pair[1].processed_samples)
    );
    assert!(
        updates
            .windows(2)
            .all(|pair| pair[0].completed_frames <= pair[1].completed_frames)
    );
}

#[test]
fn decode_source_constructor_preserves_explicit_metadata_and_stream_composition() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 1_024));
    let flac = builtin::encode_bytes(&wav).unwrap();
    let (metadata, stream) = read_flac_reader(Cursor::new(&flac))
        .unwrap()
        .into_decode_source()
        .into_parts();
    let mut decoder = flacx::DecodeConfig::default().into_decoder(Cursor::new(Vec::new()));
    let summary = decoder
        .decode_source(DecodeSource::new(metadata, stream))
        .unwrap();
    let decoded = decoder.into_inner().into_inner();

    assert_eq!(summary.total_samples, 1_024);
    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
}

#[test]
fn decode_source_new_with_scratch_metadata_emits_expected_container_bytes() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 1_024));
    let flac = builtin::encode_bytes(&wav).unwrap();
    let (_, stream) = read_flac_reader(Cursor::new(&flac))
        .unwrap()
        .into_decode_source()
        .into_parts();
    let mut metadata = Metadata::new();
    metadata.add_comment("ARTIST", "Scratch Artist");
    metadata.set_cue_points([128, 512]);

    let mut decoder = flacx::DecodeConfig::default().into_decoder(Cursor::new(Vec::new()));
    let summary = decoder
        .decode_source(DecodeSource::new(metadata, stream))
        .unwrap();
    let decoded = decoder.into_inner().into_inner();
    let list_chunks = wav_chunk_payloads(&decoded, *b"LIST");

    assert_eq!(summary.total_samples, 1_024);
    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
    assert_eq!(wav_cue_points(&decoded), vec![128, 512]);
    assert_eq!(list_chunks.len(), 1);
    assert!(list_chunks[0].windows(4).any(|window| window == b"IART"));
}

#[test]
fn reader_metadata_reused_via_encode_source_new_matches_reader_into_source_bytes() {
    let wav = wav_with_chunks(
        pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 1_024)),
        &[
            (*b"LIST", info_list_chunk(&[(*b"INAM", b"Reuse Title")])),
            (*b"cue ", cue_chunk(&[0, 512])),
        ],
    );

    let baseline_reader = WavReader::new(Cursor::new(&wav)).unwrap();
    let mut baseline_encoder = EncoderConfig::default().into_encoder(Cursor::new(Vec::new()));
    let baseline_summary = baseline_encoder
        .encode_source(baseline_reader.into_source())
        .unwrap();
    let baseline_flac = baseline_encoder.into_inner().into_inner();

    let reader = WavReader::new(Cursor::new(&wav)).unwrap();
    let metadata = reader.metadata().clone();
    let (_, stream) = reader.into_source().into_parts();
    let mut candidate_encoder = EncoderConfig::default().into_encoder(Cursor::new(Vec::new()));
    let candidate_summary = candidate_encoder
        .encode_source(EncodeSource::new(metadata, stream))
        .unwrap();
    let candidate_flac = candidate_encoder.into_inner().into_inner();

    assert_eq!(candidate_summary, baseline_summary);
    assert_eq!(candidate_flac, baseline_flac);
}

#[test]
fn reader_metadata_reused_via_decode_source_new_matches_reader_into_decode_source_bytes() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 1_024));
    let flac = builtin::encode_bytes(&wav).unwrap();

    let baseline_reader = read_flac_reader(Cursor::new(&flac)).unwrap();
    let mut baseline_decoder = flacx::DecodeConfig::default().into_decoder(Cursor::new(Vec::new()));
    let baseline_summary = baseline_decoder
        .decode_source(baseline_reader.into_decode_source())
        .unwrap();
    let baseline_output = baseline_decoder.into_inner().into_inner();

    let reader = read_flac_reader(Cursor::new(&flac)).unwrap();
    let metadata = reader.metadata().clone();
    let (_, stream) = reader.into_decode_source().into_parts();
    let mut candidate_decoder =
        flacx::DecodeConfig::default().into_decoder(Cursor::new(Vec::new()));
    let candidate_summary = candidate_decoder
        .decode_source(DecodeSource::new(metadata, stream))
        .unwrap();
    let candidate_output = candidate_decoder.into_inner().into_inner();

    assert_eq!(candidate_summary, baseline_summary);
    assert_eq!(candidate_output, baseline_output);
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
fn raw_descriptor_fixture_still_counts_samples_explicitly() {
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

#[cfg(all(feature = "aiff", feature = "caf"))]
#[test]
fn read_pcm_reader_dispatches_family_peers_without_wav_bias() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 64));
    let aiff = aiff_pcm_bytes(16, 1, 44_100, &sample_fixture(1, 64));
    let caf = caf_lpcm_bytes(16, 16, 1, 44_100, true, &sample_fixture(1, 64));

    let wav_reader = PcmReader::new(Cursor::new(&wav)).unwrap();
    let aiff_reader = PcmReader::new(Cursor::new(&aiff)).unwrap();
    let caf_reader = PcmReader::new(Cursor::new(&caf)).unwrap();

    assert!(matches!(wav_reader, PcmReader::Wav(_)));
    assert!(matches!(aiff_reader, PcmReader::Aiff(_)));
    assert!(matches!(caf_reader, PcmReader::Caf(_)));
}

#[cfg(all(feature = "aiff", feature = "caf"))]
#[test]
fn explicit_decode_sessions_can_emit_peer_family_outputs() {
    type DecodePeerCase = (PcmContainer, fn(&[u8]) -> bool, &'static str);

    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 1_024));
    let flac = builtin::encode_bytes(&wav).unwrap();
    let cases: &[DecodePeerCase] = &[
        (PcmContainer::Aiff, is_aiff_bytes, "aiff"),
        (PcmContainer::Aifc, is_aifc_bytes, "aifc"),
        (PcmContainer::Caf, is_caf_bytes, "caf"),
    ];

    for &(container, detector, label) in cases {
        let reader = read_flac_reader(Cursor::new(&flac)).unwrap();
        let mut decoder = flacx::DecodeConfig::default()
            .with_output_container(container)
            .into_decoder(Cursor::new(Vec::new()));
        let summary = decoder.decode_source(reader.into_decode_source()).unwrap();
        let decoded = decoder.into_inner().into_inner();

        assert_eq!(
            summary.total_samples, 1_024,
            "unexpected summary for {label}"
        );
        assert!(detector(&decoded), "unexpected output family for {label}");

        let reencoded = builtin::encode_bytes(&decoded).unwrap();
        let round_tripped = builtin::decode_bytes(&reencoded).unwrap();
        assert_eq!(
            wav_data_bytes(&round_tripped),
            wav_data_bytes(&wav),
            "explicit decode session changed audio bytes for {label}"
        );
    }
}

#[derive(Clone)]
struct CountingCursor {
    inner: Cursor<Vec<u8>>,
    bytes_read: Rc<RefCell<usize>>,
}

impl CountingCursor {
    fn new(bytes: Vec<u8>) -> (Self, Rc<RefCell<usize>>) {
        let bytes_read = Rc::new(RefCell::new(0usize));
        (
            Self {
                inner: Cursor::new(bytes),
                bytes_read: Rc::clone(&bytes_read),
            },
            bytes_read,
        )
    }
}

impl Read for CountingCursor {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        *self.bytes_read.borrow_mut() += read;
        Ok(read)
    }
}

impl Seek for CountingCursor {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn encoder_session_starts_before_full_payload_consumption() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 4_096));
    let wav_len = wav.len();
    let (counting_reader, bytes_read) = CountingCursor::new(wav.clone());

    let reader = WavReader::new(counting_reader).unwrap();
    assert!(
        *bytes_read.borrow() < wav_len,
        "reader construction should not consume the full payload"
    );

    let mut encoder = EncoderConfig::default().into_encoder(Cursor::new(Vec::new()));
    assert!(
        *bytes_read.borrow() < wav_len,
        "binding the writer-owning encoder session should not consume the full payload"
    );

    encoder.encode_source(reader.into_source()).unwrap();
    assert_eq!(
        *bytes_read.borrow(),
        wav_len,
        "the full payload should only be consumed while driving the PCM stream through encode"
    );
}

#[test]
fn decoder_session_starts_before_full_flac_payload_consumption() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 16_384));
    let flac = builtin::encode_bytes(&wav).unwrap();
    let flac_len = flac.len();
    let (counting_reader, bytes_read) = CountingCursor::new(flac);

    let reader = read_flac_reader(counting_reader).unwrap();
    assert!(
        *bytes_read.borrow() < flac_len,
        "flac reader construction should not consume the full input payload"
    );

    let mut decoder = flacx::DecodeConfig::default().into_decoder(Cursor::new(Vec::new()));
    assert!(
        *bytes_read.borrow() < flac_len,
        "binding the writer-owning decoder session should not consume the full input payload"
    );

    decoder.decode_source(reader.into_decode_source()).unwrap();
    assert_eq!(
        *bytes_read.borrow(),
        flac_len,
        "the full FLAC payload should only be consumed while driving the decode stream through the session"
    );
}
