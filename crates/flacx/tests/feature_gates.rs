#[cfg(any(not(feature = "aiff"), not(feature = "caf")))]
use flacx::{DecodeConfig, EncoderConfig, PcmContainer, PcmReader, RawPcmByteOrder, RawPcmReader};

mod support;
#[cfg(any(not(feature = "aiff"), not(feature = "caf")))]
use support::TestDecoder as DecodeHarness;
#[cfg(any(not(feature = "aiff"), not(feature = "caf")))]
use support::TestEncoder as Encoder;

#[cfg(not(feature = "caf"))]
use support::caf_lpcm_bytes;
#[cfg(not(feature = "aiff"))]
use support::{aifc_pcm_bytes, aiff_pcm_bytes};
#[cfg(any(not(feature = "aiff"), not(feature = "caf")))]
use support::{raw_pcm_fixture, sample_fixture, unique_temp_path};

#[cfg(any(not(feature = "aiff"), not(feature = "caf")))]
fn flac_fixture() -> Vec<u8> {
    let (raw_bytes, descriptor) = raw_pcm_fixture(
        44_100,
        1,
        16,
        16,
        RawPcmByteOrder::LittleEndian,
        None,
        &sample_fixture(1, 256),
    );
    let reader = RawPcmReader::new(std::io::Cursor::new(raw_bytes), descriptor).unwrap();
    let mut encoder = EncoderConfig::default().into_encoder(std::io::Cursor::new(Vec::new()));
    encoder.encode(reader.into_pcm_stream().unwrap()).unwrap();
    encoder.into_inner().into_inner()
}

#[cfg(not(feature = "aiff"))]
#[test]
fn encode_rejects_aiff_family_inputs_when_feature_is_disabled() {
    for input in [
        aiff_pcm_bytes(16, 1, 44_100, &sample_fixture(1, 128)),
        aifc_pcm_bytes(*b"NONE", 16, 1, 44_100, &sample_fixture(1, 128)),
    ] {
        let error = Encoder::default().encode_bytes(&input).unwrap_err();
        assert!(error.to_string().contains("pcm container"));
        assert!(error.to_string().contains("`aiff` cargo feature"));
    }
}

#[cfg(not(feature = "aiff"))]
#[test]
fn pcm_reader_new_rejects_aiff_family_inputs_when_feature_is_disabled() {
    for input in [
        aiff_pcm_bytes(16, 1, 44_100, &sample_fixture(1, 128)),
        aifc_pcm_bytes(*b"NONE", 16, 1, 44_100, &sample_fixture(1, 128)),
    ] {
        let error = PcmReader::new(std::io::Cursor::new(input)).unwrap_err();
        assert!(error.to_string().contains("pcm container"));
        assert!(error.to_string().contains("`aiff` cargo feature"));
    }
}

#[cfg(not(feature = "caf"))]
#[test]
fn encode_rejects_caf_inputs_when_feature_is_disabled() {
    let input = caf_lpcm_bytes(16, 16, 2, 44_100, false, &sample_fixture(2, 128));
    let error = Encoder::default().encode_bytes(&input).unwrap_err();
    assert!(error.to_string().contains("pcm container"));
    assert!(error.to_string().contains("`caf` cargo feature"));
}

#[cfg(not(feature = "caf"))]
#[test]
fn pcm_reader_new_rejects_caf_inputs_when_feature_is_disabled() {
    let input = caf_lpcm_bytes(16, 16, 2, 44_100, false, &sample_fixture(2, 128));
    let error = PcmReader::new(std::io::Cursor::new(input)).unwrap_err();
    assert!(error.to_string().contains("pcm container"));
    assert!(error.to_string().contains("`caf` cargo feature"));
}

#[cfg(not(feature = "aiff"))]
#[test]
fn decode_rejects_aiff_family_outputs_when_feature_is_disabled() {
    let flac = flac_fixture();

    for container in [PcmContainer::Aiff, PcmContainer::Aifc] {
        let error = DecodeHarness::new(DecodeConfig::default().with_output_container(container))
            .decode_bytes(&flac)
            .unwrap_err();
        assert!(error.to_string().contains("pcm container"));
        assert!(error.to_string().contains("`aiff` cargo feature"));
    }
}

#[cfg(not(feature = "caf"))]
#[test]
fn decode_rejects_caf_output_when_feature_is_disabled() {
    let flac = flac_fixture();

    let error =
        DecodeHarness::new(DecodeConfig::default().with_output_container(PcmContainer::Caf))
            .decode_bytes(&flac)
            .unwrap_err();
    assert!(error.to_string().contains("pcm container"));
    assert!(error.to_string().contains("`caf` cargo feature"));
}

#[cfg(not(feature = "aiff"))]
#[test]
fn decode_file_rejects_aiff_extensions_when_feature_is_disabled() {
    let flac = flac_fixture();
    let input_path = unique_temp_path("flac");
    std::fs::write(&input_path, flac).unwrap();

    for ext in ["aiff", "aifc"] {
        let output_path = unique_temp_path(ext);
        let error = DecodeHarness::default()
            .decode_file(&input_path, &output_path)
            .unwrap_err();
        assert!(error.to_string().contains("pcm container"));
        assert!(error.to_string().contains("`aiff` cargo feature"));
    }

    let _ = std::fs::remove_file(input_path);
}

#[cfg(not(feature = "caf"))]
#[test]
fn decode_file_rejects_caf_extension_when_feature_is_disabled() {
    let flac = flac_fixture();
    let input_path = unique_temp_path("flac");
    let output_path = unique_temp_path("caf");
    std::fs::write(&input_path, flac).unwrap();

    let error = DecodeHarness::default()
        .decode_file(&input_path, &output_path)
        .unwrap_err();
    assert!(error.to_string().contains("pcm container"));
    assert!(error.to_string().contains("`caf` cargo feature"));

    let _ = std::fs::remove_file(output_path);
    let _ = std::fs::remove_file(input_path);
}
