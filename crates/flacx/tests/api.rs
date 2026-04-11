use std::{
    cell::RefCell,
    fs,
    io::{Cursor, Read, Seek, SeekFrom},
    rc::Rc,
};

use flacx::{
    DecodePcmStream, DecodeSummary, EncodePcmStream, EncoderConfig, FlacReaderOptions,
    FlacRecompressSource, PcmSpec, RawPcmByteOrder, RawPcmDescriptor, RecompressConfig,
    RecompressMode, WavReader, builtin, inspect_raw_pcm_total_samples, level::Level,
    read_flac_reader, read_flac_reader_with_options, read_pcm_reader, write_pcm_stream,
};

mod support;

use support::TestDecoder;
use support::{
    parse_wav_format, pcm_wav_bytes, raw_pcm_fixture, sample_fixture, unique_temp_path,
    wav_data_bytes,
};

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

    let reader = read_pcm_reader(Cursor::new(&wav)).unwrap();
    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let mut encoder = EncoderConfig::default().into_encoder(Cursor::new(Vec::new()));
    encoder.set_metadata(metadata);
    let summary = encoder.encode(stream).unwrap();
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

    let reader = read_pcm_reader(fs::File::open(&input_path).unwrap()).unwrap();
    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let mut encoder = EncoderConfig::default()
        .with_level(Level::Level0)
        .with_threads(1)
        .with_block_size(576)
        .into_encoder(fs::File::create(&output_path).unwrap());
    encoder.set_metadata(metadata);
    let summary = encoder.encode(stream).unwrap();

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

    let reader = read_pcm_reader(Cursor::new(&wav)).unwrap();
    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let mut encoder = EncoderConfig::default().into_encoder(Cursor::new(Vec::new()));
    encoder.set_metadata(metadata);
    encoder.encode(stream).unwrap();
    let bytes_from_memory = encoder.into_inner().into_inner();

    assert_eq!(summary.total_samples, 2_048);
    assert_eq!(bytes_from_file, bytes_from_memory);

    let _ = fs::remove_file(input_path);
    let _ = fs::remove_file(output_path);
}

#[test]
fn api_accepts_seekable_readers_and_writer_bound_sessions() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 1_024));
    let reader = read_pcm_reader(Cursor::new(wav)).unwrap();
    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let mut output = Cursor::new(Vec::new());
    let mut encoder = EncoderConfig::default()
        .with_threads(2)
        .into_encoder(&mut output);
    encoder.set_metadata(metadata);
    let summary = encoder.encode(stream).unwrap();

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
    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let mut decoder = flacx::DecodeConfig::default().into_decoder(Cursor::new(Vec::new()));
    decoder.set_metadata(metadata);
    let summary = decoder.decode(stream).unwrap();

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
    let source = FlacRecompressSource::from_reader(reader);
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
fn flac_reader_stream_starts_before_all_frames_are_decoded() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 16_384));
    let reader = read_pcm_reader(Cursor::new(&wav)).unwrap();
    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let mut encoder = EncoderConfig::default()
        .with_block_size(576)
        .into_encoder(Cursor::new(Vec::new()));
    encoder.set_metadata(metadata);
    encoder.encode(stream).unwrap();
    let flac = encoder.into_inner().into_inner();

    let reader = read_flac_reader(Cursor::new(flac)).unwrap();
    let mut stream = reader.into_pcm_stream();
    assert_eq!(stream.completed_input_frames(), 0);

    let mut first_chunk = Vec::new();
    let frames = stream.read_chunk(4_096, &mut first_chunk).unwrap();
    assert!(frames > 0);
    assert!(frames < 16_384);
    assert!(stream.completed_input_frames() > 0);

    let reader = read_flac_reader(Cursor::new(builtin::encode_bytes(&wav).unwrap())).unwrap();
    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let mut decoder = flacx::DecodeConfig::default().into_decoder(Cursor::new(Vec::new()));
    decoder.set_metadata(metadata);
    let summary = decoder.decode(stream).unwrap();
    assert_eq!(summary.total_samples, 16_384);
}

#[test]
fn wav_reader_exposes_spec_before_stream_consumption() {
    let wav = pcm_wav_bytes(24, 2, 48_000, &sample_fixture(2, 256));
    let reader = WavReader::new(Cursor::new(&wav)).unwrap();
    let spec: PcmSpec = reader.spec();
    let mut stream = reader.into_pcm_stream();
    let mut samples = Vec::new();
    let frames = stream.read_chunk(256, &mut samples).unwrap();

    assert_eq!(spec.sample_rate, 48_000);
    assert_eq!(spec.channels, 2);
    assert_eq!(spec.bits_per_sample, 24);
    assert_eq!(frames, 256);
    assert_eq!(samples.len(), 512);
}

#[test]
fn explicit_reader_session_pipeline_round_trips_without_builtin_inference() {
    let wav = pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 1_024));
    let reader = WavReader::new(Cursor::new(&wav)).unwrap();
    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();

    let mut encoder = EncoderConfig::default().into_encoder(Cursor::new(Vec::new()));
    encoder.set_metadata(metadata);
    let encode_summary = encoder.encode(stream).unwrap();
    let decoded_reader = read_flac_reader(Cursor::new(encoder.into_inner().into_inner())).unwrap();
    let decoded_spec = decoded_reader.spec();
    let mut decoded_stream = decoded_reader.into_pcm_stream();
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

    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let mut encoder = EncoderConfig::default().into_encoder(Cursor::new(Vec::new()));
    encoder.set_metadata(metadata);
    assert!(
        *bytes_read.borrow() < wav_len,
        "binding the writer-owning encoder session should not consume the full payload"
    );

    encoder.encode(stream).unwrap();
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

    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let mut decoder = flacx::DecodeConfig::default().into_decoder(Cursor::new(Vec::new()));
    decoder.set_metadata(metadata);
    assert!(
        *bytes_read.borrow() < flac_len,
        "binding the writer-owning decoder session should not consume the full input payload"
    );

    decoder.decode(stream).unwrap();
    assert_eq!(
        *bytes_read.borrow(),
        flac_len,
        "the full FLAC payload should only be consumed while driving the decode stream through the session"
    );
}
