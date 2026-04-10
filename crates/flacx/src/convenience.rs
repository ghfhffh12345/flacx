use std::{fs::File, io::Cursor, path::Path};

use crate::{
    EncoderConfig, Result,
    decode::{DecodeSummary, Decoder},
    decode_output::{commit_temp_output, open_temp_output},
    encoder::EncodeSummary,
    error::Error,
    input::read_pcm_reader_with_config,
    pcm::PcmContainer,
};

pub use crate::recompress::{recompress_bytes, recompress_file};
pub use crate::{
    inspect_flac_total_samples, inspect_pcm_total_samples, inspect_raw_pcm_total_samples,
    inspect_wav_total_samples,
};

pub fn encode_file<P, Q>(input_path: P, output_path: Q) -> Result<EncodeSummary>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    encode_file_with_config(&EncoderConfig::default(), input_path, output_path)
}

pub fn encode_bytes(input: &[u8]) -> Result<Vec<u8>> {
    encode_bytes_with_config(&EncoderConfig::default(), input)
}

pub(crate) fn encode_file_with_config<P, Q>(
    config: &EncoderConfig,
    input_path: P,
    output_path: Q,
) -> Result<EncodeSummary>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    let reader = read_pcm_reader_with_config(File::open(input_path)?, config)?;
    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let mut encoder = config.clone().into_encoder(File::create(output_path)?);
    encoder.set_metadata(metadata);
    encoder.encode(stream)
}

pub(crate) fn encode_bytes_with_config(config: &EncoderConfig, input: &[u8]) -> Result<Vec<u8>> {
    let reader = read_pcm_reader_with_config(Cursor::new(input), config)?;
    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let mut encoder = config.clone().into_encoder(Cursor::new(Vec::new()));
    encoder.set_metadata(metadata);
    encoder.encode(stream)?;
    Ok(encoder.into_inner().into_inner())
}

pub fn decode_file<P, Q>(input_path: P, output_path: Q) -> Result<DecodeSummary>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    decode_file_with_decoder(&Decoder::default(), input_path, output_path)
}

pub fn decode_bytes(input: &[u8]) -> Result<Vec<u8>> {
    decode_bytes_with_decoder(&Decoder::default(), input)
}

pub(crate) fn decode_bytes_with_decoder(decoder: &Decoder, input: &[u8]) -> Result<Vec<u8>> {
    let mut output = Cursor::new(Vec::new());
    decoder.decode(Cursor::new(input), &mut output)?;
    Ok(output.into_inner())
}

pub(crate) fn decode_file_with_decoder<P, Q>(
    decoder: &Decoder,
    input_path: P,
    output_path: Q,
) -> Result<DecodeSummary>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    let mut progress = crate::progress::NoProgress;
    decode_file_with_decoder_and_progress(decoder, input_path, output_path, &mut progress)
}

pub(crate) fn decode_file_with_decoder_and_progress<P, Q, R>(
    decoder: &Decoder,
    input_path: P,
    output_path: Q,
    progress: &mut R,
) -> Result<DecodeSummary>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
    R: crate::progress::ProgressSink,
{
    let input_path = input_path.as_ref();
    let output_path = output_path.as_ref();
    let (temp_path, temp_file) = open_temp_output(output_path)?;
    let output_container = inferred_output_container_from_path(output_path)?
        .unwrap_or(decoder.config().output_container);

    let result = (|| {
        let input = File::open(input_path)?;
        let mut temp_file = temp_file;
        decoder.decode_with_output_container(input, &mut temp_file, output_container, progress)
    })();
    match result {
        Ok(summary) => {
            if let Err(error) = commit_temp_output(&temp_path, output_path) {
                let _ = std::fs::remove_file(&temp_path);
                return Err(error);
            }
            Ok(summary)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temp_path);
            Err(error)
        }
    }
}

fn inferred_output_container_from_path(path: &Path) -> Result<Option<PcmContainer>> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("rf64") => wav_output_container(PcmContainer::Rf64),
        Some(ext) if ext.eq_ignore_ascii_case("w64") => wav_output_container(PcmContainer::Wave64),
        Some(ext) if ext.eq_ignore_ascii_case("aif") => aiff_output_container(PcmContainer::Aiff),
        Some(ext) if ext.eq_ignore_ascii_case("aiff") => aiff_output_container(PcmContainer::Aiff),
        Some(ext) if ext.eq_ignore_ascii_case("aifc") => aiff_output_container(PcmContainer::Aifc),
        Some(ext) if ext.eq_ignore_ascii_case("caf") => caf_output_container(),
        Some(ext) if ext.eq_ignore_ascii_case("wav") => wav_output_container(PcmContainer::Wave),
        Some(ext) => Err(Error::Decode(format!(
            "unsupported decode output extension '.{ext}'"
        ))),
        None => Ok(None),
    }
}

#[cfg(feature = "wav")]
fn wav_output_container(container: PcmContainer) -> Result<Option<PcmContainer>> {
    Ok(Some(container))
}

#[cfg(not(feature = "wav"))]
fn wav_output_container(_container: PcmContainer) -> Result<Option<PcmContainer>> {
    Err(Error::Decode(
        "RIFF/WAVE family decode output requires the `wav` cargo feature".into(),
    ))
}

#[cfg(feature = "aiff")]
fn aiff_output_container(container: PcmContainer) -> Result<Option<PcmContainer>> {
    Ok(Some(container))
}

#[cfg(not(feature = "aiff"))]
fn aiff_output_container(_container: PcmContainer) -> Result<Option<PcmContainer>> {
    Err(Error::Decode(
        "AIFF/AIFC decode output requires the `aiff` cargo feature".into(),
    ))
}

#[cfg(feature = "caf")]
fn caf_output_container() -> Result<Option<PcmContainer>> {
    Ok(Some(PcmContainer::Caf))
}

#[cfg(not(feature = "caf"))]
fn caf_output_container() -> Result<Option<PcmContainer>> {
    Err(Error::Decode(
        "CAF decode output requires the `caf` cargo feature".into(),
    ))
}
