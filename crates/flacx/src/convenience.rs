use std::{
    fs::File,
    io::{BufReader, BufWriter, Cursor, Read, Seek, Write},
    path::Path,
};

use crate::{
    DecodeConfig, EncoderConfig, RecompressConfig, Result,
    decode::DecodeSummary,
    decode_output::{commit_temp_output, open_temp_output},
    encoder::EncodeSummary,
    error::Error,
    input::PcmReaderOptions,
    pcm::PcmContainer,
    read::{FlacReader, FlacReaderOptions, read_flac_reader_with_options},
    recompress::{FlacRecompressSource, RecompressProgressSink, RecompressSummary},
};

pub use crate::{
    inspect_flac_total_samples, inspect_pcm_total_samples, inspect_raw_pcm_total_samples,
    inspect_wav_total_samples,
};

const FILE_READ_BUFFER_CAPACITY: usize = 4 * 1024 * 1024;
const FILE_WRITE_BUFFER_CAPACITY: usize = 10 * 1024 * 1024;

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

pub fn recompress_file<P, Q>(input_path: P, output_path: Q) -> Result<RecompressSummary>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    recompress_file_with_config(&RecompressConfig::default(), input_path, output_path)
}

pub fn recompress_bytes(input: &[u8]) -> Result<Vec<u8>> {
    recompress_bytes_with_config(&RecompressConfig::default(), input)
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
    let input_path = input_path.as_ref();
    let output_path = output_path.as_ref();
    let reader = crate::read_pcm_reader_with_options(
        open_buffered_reader(input_path)?,
        PcmReaderOptions {
            capture_fxmd: config.capture_fxmd,
            strict_fxmd_validation: config.strict_fxmd_validation,
        },
    )?;
    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let mut encoder = config
        .clone()
        .into_encoder(create_buffered_writer(output_path)?);
    encoder.set_metadata(metadata);
    encoder.encode(stream)
}

pub(crate) fn encode_bytes_with_config(config: &EncoderConfig, input: &[u8]) -> Result<Vec<u8>> {
    let reader = crate::read_pcm_reader_with_options(
        Cursor::new(input),
        PcmReaderOptions {
            capture_fxmd: config.capture_fxmd,
            strict_fxmd_validation: config.strict_fxmd_validation,
        },
    )?;
    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let mut encoder = config.clone().into_encoder(Cursor::new(Vec::new()));
    encoder.set_metadata(metadata);
    encoder.encode(stream)?;
    Ok(encoder.into_inner().into_inner())
}

pub(crate) fn recompress_bytes_with_config(
    config: &RecompressConfig,
    input: &[u8],
) -> Result<Vec<u8>> {
    let reader = read_flac_reader_with_options(Cursor::new(input), config.flac_reader_options())?;
    let (writer, _) = recompress_reader_session_with_config_and_progress(
        config,
        reader,
        Cursor::new(Vec::new()),
        &mut crate::progress::NoProgress,
    )?;
    Ok(writer.into_inner())
}

pub(crate) fn recompress_file_with_config<P, Q>(
    config: &RecompressConfig,
    input_path: P,
    output_path: Q,
) -> Result<RecompressSummary>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    let mut progress = crate::progress::NoProgress;
    recompress_file_with_config_and_progress(config, input_path, output_path, &mut progress)
}

pub(crate) fn recompress_file_with_config_and_progress<P, Q, R>(
    config: &RecompressConfig,
    input_path: P,
    output_path: Q,
    progress: &mut R,
) -> Result<RecompressSummary>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
    R: crate::recompress::RecompressProgressSink,
{
    let input_path = input_path.as_ref();
    let output_path = output_path.as_ref();
    let (temp_path, temp_file) = open_temp_output(output_path)?;

    let result = (|| {
        let reader = read_flac_reader_with_options(
            open_buffered_reader(input_path)?,
            config.flac_reader_options(),
        )?;
        let temp_writer = BufWriter::with_capacity(FILE_WRITE_BUFFER_CAPACITY, temp_file);
        let (mut temp_writer, summary) =
            recompress_reader_session_with_config_and_progress(config, reader, temp_writer, progress)?;
        temp_writer.flush()?;
        Ok(summary)
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

pub fn decode_file<P, Q>(input_path: P, output_path: Q) -> Result<DecodeSummary>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    decode_file_with_config(&DecodeConfig::default(), input_path, output_path)
}

pub fn decode_bytes(input: &[u8]) -> Result<Vec<u8>> {
    decode_bytes_with_config(&DecodeConfig::default(), input)
}

pub(crate) fn decode_bytes_with_config(config: &DecodeConfig, input: &[u8]) -> Result<Vec<u8>> {
    let reader = read_flac_reader_with_options(
        Cursor::new(input),
        FlacReaderOptions {
            strict_seektable_validation: config.strict_seektable_validation,
            strict_channel_mask_provenance: config.strict_channel_mask_provenance,
        },
    )?;
    let metadata = reader.metadata().clone();
    let stream = reader.into_pcm_stream();
    let mut decoder = config.clone().into_decoder(Cursor::new(Vec::new()));
    decoder.set_metadata(metadata);
    decoder.decode(stream)?;
    Ok(decoder.into_inner().into_inner())
}

pub(crate) fn decode_file_with_config<P, Q>(
    config: &DecodeConfig,
    input_path: P,
    output_path: Q,
) -> Result<DecodeSummary>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    let mut progress = crate::progress::NoProgress;
    decode_file_with_config_and_progress(config, input_path, output_path, &mut progress)
}

pub(crate) fn decode_file_with_config_and_progress<P, Q, R>(
    config: &DecodeConfig,
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
    let output_container =
        inferred_output_container_from_path(output_path)?.unwrap_or(config.output_container);

    let result =
        (|| {
            let reader = read_flac_reader_with_options(
                open_buffered_reader(input_path)?,
                FlacReaderOptions {
                    strict_seektable_validation: config.strict_seektable_validation,
                    strict_channel_mask_provenance: config.strict_channel_mask_provenance,
                },
            )?;
            let metadata = reader.metadata().clone();
            let stream = reader.into_pcm_stream();
            let mut decoder = config.with_output_container(output_container).into_decoder(
                BufWriter::with_capacity(FILE_WRITE_BUFFER_CAPACITY, temp_file),
            );
            decoder.set_metadata(metadata);
            let summary = decoder.decode_with_sink(stream, progress)?;
            decoder.into_inner().flush()?;
            Ok(summary)
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

fn recompress_reader_session_with_config_and_progress<R, W, P>(
    config: &RecompressConfig,
    reader: FlacReader<R>,
    writer: W,
    progress: &mut P,
) -> Result<(W, RecompressSummary)>
where
    R: Read + Seek,
    W: Write + Seek,
    P: RecompressProgressSink,
{
    let source = FlacRecompressSource::from_reader(reader);
    let mut recompressor = config.clone().into_recompressor(writer);
    let summary = recompressor.recompress_with_sink(source, progress)?;
    Ok((recompressor.into_inner(), summary))
}

fn open_buffered_reader(path: &Path) -> Result<BufReader<File>> {
    Ok(BufReader::with_capacity(
        FILE_READ_BUFFER_CAPACITY,
        File::open(path)?,
    ))
}

fn create_buffered_writer(path: &Path) -> Result<BufWriter<File>> {
    Ok(BufWriter::with_capacity(
        FILE_WRITE_BUFFER_CAPACITY,
        File::create(path)?,
    ))
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
