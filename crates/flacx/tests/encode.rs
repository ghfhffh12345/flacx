use std::{
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use flacx::{EncodePcmStream, EncoderConfig, PcmSpec, builtin::decode_bytes};

mod support;
use support::TestEncoder as Encoder;

use support::{
    ParsedFlacBlockingStrategy, ParsedFlacCodedNumberKind, cue_chunk, decode_with_ffmpeg,
    ffmpeg_available, flac_metadata_blocks, info_list_chunk, parse_first_flac_frame_header,
    parse_wav_format, pcm_wav_bytes, sample_fixture, vorbis_comments, wav_data_bytes,
    wav_with_chunks,
};

fn require_ffmpeg_or_skip() -> bool {
    if ffmpeg_available() {
        true
    } else {
        eprintln!("skipping ffmpeg oracle test: ffmpeg unavailable in PATH");
        false
    }
}

#[test]
fn patches_streaminfo_after_encoding() {
    let samples = sample_fixture(2, 5_000);
    let wav = pcm_wav_bytes(16, 2, 44_100, &samples);
    let encoder = Encoder::new(EncoderConfig::default().with_threads(2));
    let flac = encoder.encode_bytes(&wav).unwrap();
    let blocks = flac_metadata_blocks(&flac);

    assert_eq!(&flac[..4], b"fLaC");
    assert_eq!(&flac[4..8], &[0x00, 0x00, 0x00, 0x22]);
    let min_block = u16::from_be_bytes([flac[8], flac[9]]);
    let max_block = u16::from_be_bytes([flac[10], flac[11]]);
    let min_frame = u32::from_be_bytes([0, flac[12], flac[13], flac[14]]);
    let max_frame = u32::from_be_bytes([0, flac[15], flac[16], flac[17]]);
    let expected_block_size = encoder.config().block_size;

    assert_eq!(min_block, expected_block_size);
    assert_eq!(max_block, expected_block_size);
    assert!(min_frame > 0);
    assert!(max_frame >= min_frame);
    assert_eq!(blocks[1].block_type, 3);
}

#[test]
fn writes_streaminfo_md5_for_nonempty_pcm() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &[1, -2, 3, -4]);
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let md5 = &flac_metadata_blocks(&flac)[0].payload[18..34];

    assert_eq!(
        md5,
        &[
            0x4e, 0xee, 0x3c, 0x56, 0x22, 0x45, 0x41, 0xfe, 0x00, 0x81, 0x1d, 0x91, 0xd5, 0x24,
            0x24, 0x56,
        ]
    );
}

#[test]
fn writes_empty_stream_md5_for_zero_sample_pcm() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &[]);
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let md5 = &flac_metadata_blocks(&flac)[0].payload[18..34];

    assert_eq!(
        md5,
        &[
            0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8,
            0x42, 0x7e,
        ]
    );
}

#[test]
fn default_encoder_path_remains_fixed_blocksize() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let header = parse_first_flac_frame_header(&flac);

    assert_eq!(header.blocking_strategy, ParsedFlacBlockingStrategy::Fixed);
    assert_eq!(
        header.coded_number_kind,
        ParsedFlacCodedNumberKind::FrameNumber
    );
    assert_eq!(header.coded_number_value, 0);
}

#[test]
fn encodes_legal_streaminfo_only_sample_rate_using_zero_frame_header_code() {
    let wav = pcm_wav_bytes(16, 1, 700_001, &sample_fixture(1, 2_048));
    let flac = Encoder::default().encode_bytes(&wav).unwrap();
    let decoded = decode_bytes(&flac).unwrap();
    let header = parse_first_flac_frame_header(&flac);
    let format = parse_wav_format(&decoded);

    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
    assert_eq!(header.blocking_strategy, ParsedFlacBlockingStrategy::Fixed);
    assert_eq!(
        header.coded_number_kind,
        ParsedFlacCodedNumberKind::FrameNumber
    );
    assert_eq!(header.coded_number_value, 0);
    assert_eq!(header.sample_rate_bits, 0b0000);
    assert_eq!(format.sample_rate, 700_001);
}

#[test]
fn encodes_block_sizes_above_32768_with_extended_block_header_code() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 40_000));
    let flac = Encoder::new(
        EncoderConfig::default()
            .with_threads(2)
            .with_block_size(40_000),
    )
    .encode_bytes(&wav)
    .unwrap();
    let decoded = decode_bytes(&flac).unwrap();
    let header = parse_first_flac_frame_header(&flac);

    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
    assert_eq!(header.blocking_strategy, ParsedFlacBlockingStrategy::Fixed);
    assert_eq!(
        header.coded_number_kind,
        ParsedFlacCodedNumberKind::FrameNumber
    );
    assert_eq!(header.coded_number_value, 0);
    assert_eq!(header.block_size_bits, 0b0111);
    assert_eq!(u16::from_be_bytes([flac[8], flac[9]]), 40_000);
    assert_eq!(u16::from_be_bytes([flac[10], flac[11]]), 40_000);
}

#[test]
fn encodes_variable_blocksize_schedule_with_sample_number_coded_headers() {
    let wav = pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 4_352));
    let encoder = Encoder::new(
        EncoderConfig::default()
            .with_threads(2)
            .with_block_schedule(vec![576, 1_152, 576, 2_048]),
    );
    let flac = encoder.encode_bytes(&wav).unwrap();
    let decoded = decode_bytes(&flac).unwrap();
    let header = parse_first_flac_frame_header(&flac);

    assert_eq!(wav_data_bytes(&decoded), wav_data_bytes(&wav));
    assert_eq!(
        header.blocking_strategy,
        ParsedFlacBlockingStrategy::Variable
    );
    assert_eq!(
        header.coded_number_kind,
        ParsedFlacCodedNumberKind::SampleNumber
    );
    assert_eq!(header.coded_number_value, 0);

    let min_block = u16::from_be_bytes([flac[8], flac[9]]);
    let max_block = u16::from_be_bytes([flac[10], flac[11]]);
    assert_eq!(min_block, 576);
    assert_eq!(max_block, 2_048);
}

#[test]
fn produces_identical_output_across_thread_counts() {
    let samples = sample_fixture(2, 8_192);
    let wav = pcm_wav_bytes(16, 2, 44_100, &samples);

    let single_threaded = Encoder::new(EncoderConfig::default().with_threads(1))
        .encode_bytes(&wav)
        .unwrap();
    let multi_threaded = Encoder::new(EncoderConfig::default().with_threads(4))
        .encode_bytes(&wav)
        .unwrap();

    assert_eq!(single_threaded, multi_threaded);
}

#[test]
fn reference_identity_matrix_repeats_exact_encode_bytes() {
    struct IdentityCase {
        label: &'static str,
        wav: Vec<u8>,
        config: EncoderConfig,
    }

    let metadata_wav = wav_with_chunks(
        pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 8_192)),
        &[
            (
                *b"LIST",
                info_list_chunk(&[
                    (*b"IART", b"Example Artist"),
                    (*b"INAM", b"Identity Matrix Title"),
                ]),
            ),
            (*b"cue ", cue_chunk(&[0, 4_096])),
        ],
    );

    let cases = vec![
        IdentityCase {
            label: "bench-mono-default",
            wav: pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 4_096)),
            config: EncoderConfig::default().with_threads(2),
        },
        IdentityCase {
            label: "bench-stereo-medium-default",
            wav: pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 8_192)),
            config: EncoderConfig::default().with_threads(2),
        },
        IdentityCase {
            label: "bench-stereo-large-default",
            wav: pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 16_384)),
            config: EncoderConfig::default().with_threads(2),
        },
        IdentityCase {
            label: "level0-block576",
            wav: pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 4_608)),
            config: EncoderConfig::default()
                .with_threads(1)
                .with_level(flacx::level::Level::Level0)
                .with_block_size(576),
        },
        IdentityCase {
            label: "variable-block-schedule",
            wav: pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 4_352)),
            config: EncoderConfig::default()
                .with_threads(2)
                .with_block_schedule(vec![576, 1_152, 576, 2_048]),
        },
        IdentityCase {
            label: "metadata-bearing-wav",
            wav: metadata_wav,
            config: EncoderConfig::default().with_threads(2),
        },
    ];

    for case in cases {
        let first = Encoder::new(case.config.clone())
            .encode_bytes(&case.wav)
            .unwrap_or_else(|error| panic!("{} first encode failed: {error}", case.label));
        let second = Encoder::new(case.config)
            .encode_bytes(&case.wav)
            .unwrap_or_else(|error| panic!("{} second encode failed: {error}", case.label));
        let decoded = decode_bytes(&first)
            .unwrap_or_else(|error| panic!("{} decode failed: {error}", case.label));

        assert_eq!(first, second, "{}", case.label);
        assert_eq!(
            wav_data_bytes(&decoded),
            wav_data_bytes(&case.wav),
            "{}",
            case.label
        );
    }
}

#[test]
fn preserves_list_info_text_metadata_as_vorbis_comments() {
    let wav = wav_with_chunks(
        pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048)),
        &[(
            *b"LIST",
            info_list_chunk(&[
                (*b"IART", b"Example Artist"),
                (*b"INAM", b"Metadata Song"),
                (*b"IZZZ", b"ignored"),
            ]),
        )],
    );

    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&wav)
        .unwrap();
    let blocks = flac_metadata_blocks(&flac);

    assert_eq!(
        blocks
            .iter()
            .map(|block| block.block_type)
            .collect::<Vec<_>>(),
        vec![0, 3, 4]
    );
    assert_eq!(
        vorbis_comments(&blocks[2].payload),
        vec![
            "ARTIST=Example Artist".to_string(),
            "TITLE=Metadata Song".to_string(),
        ]
    );
}

#[test]
fn preserves_representable_cue_points_as_cuesheet_tracks() {
    let wav = wav_with_chunks(
        pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 4_096)),
        &[(*b"cue ", cue_chunk(&[0, 2_048]))],
    );

    let flac = Encoder::new(EncoderConfig::default().with_threads(3))
        .encode_bytes(&wav)
        .unwrap();
    let blocks = flac_metadata_blocks(&flac);

    assert_eq!(
        blocks
            .iter()
            .map(|block| block.block_type)
            .collect::<Vec<_>>(),
        vec![0, 3, 5]
    );
    assert_eq!(
        blocks[2].payload[395], 3,
        "two cue-derived tracks plus lead-out"
    );
}

#[test]
fn drops_unmappable_metadata_chunks_in_output() {
    let bext_payload = vec![0x42; 602];
    let wav = wav_with_chunks(
        pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048)),
        &[
            (*b"bext", bext_payload),
            (*b"LIST", info_list_chunk(&[(*b"INAM", b"Kept Title")])),
        ],
    );

    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&wav)
        .unwrap();
    let blocks = flac_metadata_blocks(&flac);

    assert_eq!(
        blocks
            .iter()
            .map(|block| block.block_type)
            .collect::<Vec<_>>(),
        vec![0, 3, 4]
    );
    assert_eq!(
        vorbis_comments(&blocks[2].payload),
        vec!["TITLE=Kept Title".to_string()]
    );
}

#[test]
fn preserves_metadata_deterministically_across_thread_counts() {
    let wav = wav_with_chunks(
        pcm_wav_bytes(16, 2, 44_100, &sample_fixture(2, 8_192)),
        &[
            (
                *b"LIST",
                info_list_chunk(&[
                    (*b"IART", b"Example Artist"),
                    (*b"INAM", b"Thread-Stable Title"),
                ]),
            ),
            (*b"cue ", cue_chunk(&[0, 4_096])),
        ],
    );

    let single_threaded = Encoder::new(EncoderConfig::default().with_threads(1))
        .encode_bytes(&wav)
        .unwrap();
    let multi_threaded = Encoder::new(EncoderConfig::default().with_threads(4))
        .encode_bytes(&wav)
        .unwrap();

    assert_eq!(single_threaded, multi_threaded);
}

#[test]
fn legacy_fxvc_fxcs_chunks_are_ignored_like_unknown_wav_chunks() {
    let wav = wav_with_chunks(
        pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 2_048)),
        &[(*b"fxvc", vec![1, 2, 3, 4]), (*b"fxcs", vec![5, 6, 7, 8])],
    );

    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&wav)
        .unwrap();
    let blocks = flac_metadata_blocks(&flac);
    assert!(!blocks.iter().any(|block| block.block_type == 4));
    assert!(!blocks.iter().any(|block| block.block_type == 5));

    let decoded = decode_bytes(&flac).unwrap();
    let reencoded = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&decoded)
        .unwrap();
    let reencoded_blocks = flac_metadata_blocks(&reencoded);

    assert!(!reencoded_blocks.iter().any(|block| block.block_type == 4));
    assert!(!reencoded_blocks.iter().any(|block| block.block_type == 5));
}

#[test]
fn encodes_riff_cue_when_no_canonical_private_chunk_exists() {
    let wav = wav_with_chunks(
        pcm_wav_bytes(16, 1, 44_100, &sample_fixture(1, 4_096)),
        &[(*b"cue ", cue_chunk(&[0, 2_048]))],
    );

    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&wav)
        .unwrap();
    let blocks = flac_metadata_blocks(&flac);
    let cuesheet = blocks
        .iter()
        .find(|block| block.block_type == 5)
        .expect("cuesheet block present");

    assert_eq!(
        cuesheet.payload,
        support::cuesheet_block(&[0, 2_048], 4_096).payload
    );
}

#[test]
fn round_trips_16bit_stereo_with_ffmpeg_oracle() {
    if !require_ffmpeg_or_skip() {
        return;
    }
    let samples = sample_fixture(2, 6_144);
    let wav = pcm_wav_bytes(16, 2, 44_100, &samples);
    let flac = Encoder::new(EncoderConfig::default().with_threads(4))
        .encode_bytes(&wav)
        .unwrap();

    let decoded = decode_with_ffmpeg(&flac, 16);
    assert_eq!(decoded, samples);
}

#[test]
fn round_trips_24bit_mono_with_ffmpeg_oracle() {
    if !require_ffmpeg_or_skip() {
        return;
    }
    let samples: Vec<i32> = (0..5_000)
        .map(|index| ((index * 9_731) % 16_000_000) - 8_000_000)
        .collect();
    let wav = pcm_wav_bytes(24, 1, 96_000, &samples);
    let flac = Encoder::new(EncoderConfig::default().with_threads(3))
        .encode_bytes(&wav)
        .unwrap();

    let decoded = decode_with_ffmpeg(&flac, 24);
    assert_eq!(decoded, samples);
}

#[test]
fn round_trips_constant_16bit_mono_with_ffmpeg_oracle() {
    if !require_ffmpeg_or_skip() {
        return;
    }
    let samples = vec![12_345; 6_144];
    let wav = pcm_wav_bytes(16, 1, 44_100, &samples);
    let flac = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode_bytes(&wav)
        .unwrap();

    let decoded = decode_with_ffmpeg(&flac, 16);
    assert_eq!(decoded, samples);
}

#[test]
fn public_api_requires_seekable_io_but_accepts_cursor_inputs() {
    let samples = sample_fixture(1, 2_048);
    let wav = pcm_wav_bytes(16, 1, 32_000, &samples);
    let mut output = Cursor::new(Vec::new());

    let summary = Encoder::new(EncoderConfig::default().with_threads(2))
        .encode(Cursor::new(wav), &mut output)
        .unwrap();

    assert_eq!(summary.total_samples, 2_048);
    assert!(summary.frame_count >= 1);
}

struct StreamingProbeEncodeStream {
    spec: PcmSpec,
    samples: Vec<i32>,
    chunk_frames: usize,
    requested_frames: Arc<AtomicUsize>,
    read_calls: Arc<AtomicUsize>,
    cursor: usize,
}

impl StreamingProbeEncodeStream {
    fn new(
        spec: PcmSpec,
        samples: Vec<i32>,
        chunk_frames: usize,
        requested_frames: Arc<AtomicUsize>,
        read_calls: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            spec,
            samples,
            chunk_frames,
            requested_frames,
            read_calls,
            cursor: 0,
        }
    }
}

impl EncodePcmStream for StreamingProbeEncodeStream {
    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> flacx::Result<usize> {
        self.read_calls.fetch_add(1, Ordering::Relaxed);
        self.requested_frames.store(max_frames, Ordering::Relaxed);

        assert!(
            max_frames < usize::try_from(self.spec.total_samples).unwrap(),
            "encode requested the full PCM stream in one read_chunk call"
        );

        let remaining_frames = usize::try_from(self.spec.total_samples).unwrap()
            - self.cursor / usize::from(self.spec.channels);
        if remaining_frames == 0 {
            return Ok(0);
        }

        let frames = remaining_frames.min(self.chunk_frames).min(max_frames);
        let sample_count = frames * usize::from(self.spec.channels);
        let next = self.cursor + sample_count;
        output.extend_from_slice(&self.samples[self.cursor..next]);
        self.cursor = next;
        Ok(frames)
    }
}

struct ValidationProbeEncodeStream {
    spec: PcmSpec,
    samples: Vec<i32>,
    chunk_frames: usize,
    extra_frames_after_eof: usize,
    cursor: usize,
}

impl ValidationProbeEncodeStream {
    fn new(
        spec: PcmSpec,
        samples: Vec<i32>,
        chunk_frames: usize,
        extra_frames_after_eof: usize,
    ) -> Self {
        Self {
            spec,
            samples,
            chunk_frames,
            extra_frames_after_eof,
            cursor: 0,
        }
    }
}

impl EncodePcmStream for ValidationProbeEncodeStream {
    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> flacx::Result<usize> {
        let channels = usize::from(self.spec.channels);
        let available_frames = self.samples.len() / channels - self.cursor / channels;
        if available_frames > 0 {
            let frames = available_frames.min(self.chunk_frames).min(max_frames);
            let sample_count = frames * channels;
            let next = self.cursor + sample_count;
            output.extend_from_slice(&self.samples[self.cursor..next]);
            self.cursor = next;
            return Ok(frames);
        }

        if self.extra_frames_after_eof > 0 && max_frames > 0 {
            self.extra_frames_after_eof -= 1;
            output.extend(std::iter::repeat_n(0, channels));
            return Ok(1);
        }

        Ok(0)
    }
}

#[test]
fn encode_uses_bounded_pcm_reads_for_multi_frame_inputs() {
    let total_samples = 576 * 300;
    let spec = PcmSpec {
        sample_rate: 44_100,
        channels: 1,
        bits_per_sample: 16,
        total_samples: total_samples as u64,
        bytes_per_sample: 2,
        channel_mask: 0,
    };
    let samples = sample_fixture(1, usize::try_from(spec.total_samples).unwrap());
    let requested_frames = Arc::new(AtomicUsize::new(0));
    let read_calls = Arc::new(AtomicUsize::new(0));
    let stream = StreamingProbeEncodeStream::new(
        spec,
        samples,
        total_samples,
        Arc::clone(&requested_frames),
        Arc::clone(&read_calls),
    );
    let mut output = Cursor::new(Vec::new());

    let mut encoder = EncoderConfig::default()
        .with_threads(1)
        .with_block_schedule(vec![576; 300])
        .into_encoder(&mut output);
    let summary = encoder.encode(stream).unwrap();

    assert_eq!(summary.total_samples, spec.total_samples);
    assert!(
        read_calls.load(Ordering::Relaxed) > 1,
        "expected multiple bounded read_chunk calls"
    );
    assert!(
        requested_frames.load(Ordering::Relaxed) < usize::try_from(spec.total_samples).unwrap(),
        "final read request should stay below the full input length"
    );
}

#[test]
fn encode_rejects_early_eof_during_chunked_reads() {
    let total_samples = 576 * 300;
    let spec = PcmSpec {
        sample_rate: 44_100,
        channels: 1,
        bits_per_sample: 16,
        total_samples: total_samples as u64,
        bytes_per_sample: 2,
        channel_mask: 0,
    };
    let truncated_samples = sample_fixture(1, total_samples - 576);
    let stream = ValidationProbeEncodeStream::new(spec, truncated_samples, total_samples, 0);
    let mut output = Cursor::new(Vec::new());

    let error = EncoderConfig::default()
        .with_threads(1)
        .with_block_schedule(vec![576; 300])
        .into_encoder(&mut output)
        .encode(stream)
        .unwrap_err();

    assert!(
        error.to_string().contains("PCM stream ended early"),
        "{error}"
    );
}

#[test]
fn encode_rejects_extra_input_after_chunked_reads_complete() {
    let total_samples = 576 * 300;
    let spec = PcmSpec {
        sample_rate: 44_100,
        channels: 1,
        bits_per_sample: 16,
        total_samples: total_samples as u64,
        bytes_per_sample: 2,
        channel_mask: 0,
    };
    let samples = sample_fixture(1, total_samples);
    let stream = ValidationProbeEncodeStream::new(spec, samples, total_samples, 1);
    let mut output = Cursor::new(Vec::new());

    let error = EncoderConfig::default()
        .with_threads(1)
        .with_block_schedule(vec![576; 300])
        .into_encoder(&mut output)
        .encode(stream)
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("PCM stream produced more frames than declared in the spec"),
        "{error}"
    );
}

#[cfg(feature = "progress")]
#[test]
fn progress_encode_path_matches_default_output_and_reports_monotonic_updates() {
    let samples = sample_fixture(2, 5_111);
    let wav = pcm_wav_bytes(16, 2, 44_100, &samples);
    let encoder = Encoder::new(
        EncoderConfig::default()
            .with_threads(2)
            .with_block_size(576),
    );
    let expected = encoder.encode_bytes(&wav).unwrap();

    let mut output = Cursor::new(Vec::new());
    let mut progress_updates = Vec::new();
    let summary = encoder
        .encode_with_progress(Cursor::new(&wav), &mut output, |progress| {
            progress_updates.push(progress);
            Ok(())
        })
        .unwrap();

    assert_eq!(output.into_inner(), expected);
    assert_eq!(summary.total_samples, 5_111);
    assert!(!progress_updates.is_empty());
    assert_eq!(
        progress_updates.last().unwrap().processed_samples,
        summary.total_samples
    );
    assert!(
        progress_updates
            .windows(2)
            .all(|pair| pair[0].processed_samples <= pair[1].processed_samples)
    );
    assert!(
        progress_updates
            .windows(2)
            .all(|pair| pair[0].completed_frames <= pair[1].completed_frames)
    );
}
