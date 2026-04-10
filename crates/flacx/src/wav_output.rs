use std::{io::Write, sync::mpsc, thread};

#[cfg(feature = "aiff")]
use crate::aiff_output::{AiffContainer, write_aiff_with_metadata_and_md5};
#[cfg(feature = "caf")]
use crate::caf_output::write_caf;
use crate::{
    error::{Error, Result},
    input::{
        PcmEnvelope, WavSpec, append_encoded_sample, container_bits_from_valid_bits,
        ordinary_channel_mask,
    },
    md5::Md5,
    metadata::{FXMD_CHUNK_ID, WavMetadata},
    pcm::{PcmContainer, is_supported_channel_mask},
};

const PCM_FMT_CHUNK_SIZE: u32 = 16;
const EXTENSIBLE_FMT_CHUNK_SIZE: u32 = 40;
const PCM_SUBFORMAT_GUID: [u8; 16] = [
    0x01, 0x00, 0x00, 0x00, // PCM subformat
    0x00, 0x00, 0x10, 0x00, // GUID data2/data3
    0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71, // GUID data4
];
const RF64_PLACEHOLDER_SIZE: u32 = 0xFFFF_FFFF;
const W64_RIFF_GUID: [u8; 16] = [
    0x72, 0x69, 0x66, 0x66, 0x2E, 0x91, 0xCF, 0x11, 0xA5, 0xD6, 0x28, 0xDB, 0x04, 0xC1, 0x00, 0x00,
];
const W64_CHUNK_GUID_SUFFIX: [u8; 12] = [
    0xF3, 0xAC, 0xD3, 0x11, 0x8C, 0xD1, 0x00, 0xC0, 0x4F, 0x8E, 0xDB, 0x8A,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WavMetadataWriteOptions {
    pub(crate) emit_fxmd: bool,
    pub(crate) container: PcmContainer,
}

impl Default for WavMetadataWriteOptions {
    fn default() -> Self {
        Self {
            emit_fxmd: true,
            container: PcmContainer::Auto,
        }
    }
}

#[allow(dead_code)]
fn wav_feature_disabled_error() -> Error {
    Error::UnsupportedWav("RIFF/WAVE family output requires the `wav` cargo feature".into())
}

#[allow(dead_code)]
fn aiff_feature_disabled_error() -> Error {
    Error::UnsupportedWav("AIFF/AIFC output requires the `aiff` cargo feature".into())
}

#[allow(dead_code)]
fn caf_feature_disabled_error() -> Error {
    Error::UnsupportedWav("CAF output requires the `caf` cargo feature".into())
}

#[allow(dead_code)]
pub(crate) fn write_wav<W: Write>(writer: &mut W, spec: WavSpec, samples: &[i32]) -> Result<()> {
    write_wav_with_metadata(writer, spec, samples, &WavMetadata::default())
}

pub(crate) fn write_wav_with_metadata<W: Write>(
    writer: &mut W,
    spec: WavSpec,
    samples: &[i32],
    metadata: &WavMetadata,
) -> Result<()> {
    write_wav_with_metadata_and_md5_with_options(
        writer,
        spec,
        samples,
        metadata,
        WavMetadataWriteOptions::default(),
    )
    .map(|_| ())
}

pub(crate) fn write_wav_with_metadata_and_md5_with_options<W: Write>(
    writer: &mut W,
    spec: WavSpec,
    samples: &[i32],
    metadata: &WavMetadata,
    options: WavMetadataWriteOptions,
) -> Result<[u8; 16]> {
    match options.container {
        PcmContainer::Aiff => {
            return write_aiff(writer, spec, samples, metadata);
        }
        PcmContainer::Aifc => {
            return write_aifc(writer, spec, samples, metadata);
        }
        PcmContainer::Caf => {
            return write_caf_container(writer, spec, samples, metadata);
        }
        PcmContainer::Auto => {
            #[cfg(not(feature = "wav"))]
            {
                #[cfg(feature = "aiff")]
                {
                    return write_aiff_with_metadata_and_md5(
                        writer,
                        spec,
                        samples,
                        metadata,
                        AiffContainer::Aiff,
                    );
                }
                #[cfg(all(not(feature = "aiff"), feature = "caf"))]
                {
                    return write_caf(writer, spec, samples, metadata);
                }
                #[cfg(all(not(feature = "aiff"), not(feature = "caf")))]
                {
                    return Err(wav_feature_disabled_error());
                }
            }
        }
        _ => {}
    }

    #[cfg(not(feature = "wav"))]
    {
        return Err(wav_feature_disabled_error());
    }

    if !(1..=8).contains(&spec.channels) {
        return Err(Error::UnsupportedWav(format!(
            "only the ordinary 1..8 channel envelope is supported, found {} channels",
            spec.channels
        )));
    }
    if !matches!(spec.bytes_per_sample, 1..=4) {
        return Err(Error::UnsupportedWav(format!(
            "only byte-aligned PCM containers are supported, found {} bytes/sample",
            spec.bytes_per_sample
        )));
    }
    if !(4..=32).contains(&spec.bits_per_sample) {
        return Err(Error::UnsupportedWav(format!(
            "only FLAC-native 4..32 valid bits/sample are supported, found {}",
            spec.bits_per_sample
        )));
    }
    if !samples.len().is_multiple_of(usize::from(spec.channels)) {
        return Err(Error::Decode(
            "decoded samples are not aligned to the channel count".into(),
        ));
    }

    let container_bits_per_sample = container_bits_from_valid_bits(u16::from(spec.bits_per_sample));
    if spec.bytes_per_sample * 8 != container_bits_per_sample {
        return Err(Error::UnsupportedWav(format!(
            "bytes/sample does not match the chosen container width for {} valid bits/sample",
            spec.bits_per_sample
        )));
    }

    let ordinary_mask = ordinary_channel_mask(u16::from(spec.channels)).ok_or_else(|| {
        Error::UnsupportedWav(format!(
            "no ordinary channel mask exists for {} channels",
            spec.channels
        ))
    })?;
    let channel_mask = spec.channel_mask;
    if !is_supported_channel_mask(u16::from(spec.channels), channel_mask) {
        return Err(Error::UnsupportedWav(format!(
            "channel mask {channel_mask:#010x} is not supported on output for {} channels",
            spec.channels
        )));
    }
    let envelope = PcmEnvelope {
        channels: u16::from(spec.channels),
        valid_bits_per_sample: u16::from(spec.bits_per_sample),
        container_bits_per_sample,
        channel_mask,
    };
    let use_canonical_pcm = spec.channels <= 2
        && envelope.valid_bits_per_sample == envelope.container_bits_per_sample
        && channel_mask == ordinary_mask;
    let block_align = usize::from(spec.channels) * usize::from(container_bits_per_sample / 8);
    let data_bytes = (samples.len() as u64) * u64::from(container_bits_per_sample / 8);
    let fmt_payload = fmt_chunk_payload(
        spec,
        container_bits_per_sample,
        block_align,
        channel_mask,
        use_canonical_pcm,
    );
    let metadata_chunks = wav_metadata_chunks(metadata, options.emit_fxmd);
    let resolved_container = resolve_pcm_container(
        options.container,
        fmt_payload.len(),
        &metadata_chunks,
        data_bytes,
    )?;

    let streaminfo_md5 = match resolved_container {
        PcmContainer::Wave => {
            write_wave_header_and_chunks(writer, &fmt_payload, &metadata_chunks, data_bytes)?;
            write_sample_bytes(writer, samples, envelope)?
        }
        PcmContainer::Rf64 => {
            write_rf64_header_and_chunks(
                writer,
                spec.total_samples,
                &fmt_payload,
                &metadata_chunks,
                data_bytes,
            )?;
            write_sample_bytes(writer, samples, envelope)?
        }
        PcmContainer::Wave64 => {
            write_w64_header_and_chunks(writer, &fmt_payload, &metadata_chunks, data_bytes)?;
            let md5 = write_sample_bytes(writer, samples, envelope)?;
            let padding = w64_padding(data_bytes as usize);
            if padding != 0 {
                writer.write_all(&vec![0u8; padding])?;
            }
            md5
        }
        PcmContainer::Aiff | PcmContainer::Aifc | PcmContainer::Caf => {
            unreachable!(
                "non-RIFF containers should have been dispatched before RIFF writer resolution"
            )
        }
        PcmContainer::Auto => unreachable!("auto container should resolve before writing"),
    };

    Ok(streaminfo_md5)
}

fn fmt_chunk_payload(
    spec: WavSpec,
    container_bits_per_sample: u16,
    block_align: usize,
    channel_mask: u32,
    use_canonical_pcm: bool,
) -> Vec<u8> {
    let mut payload = Vec::with_capacity(if use_canonical_pcm {
        PCM_FMT_CHUNK_SIZE as usize
    } else {
        EXTENSIBLE_FMT_CHUNK_SIZE as usize
    });
    payload.extend_from_slice(&(if use_canonical_pcm { 1u16 } else { 0xFFFEu16 }).to_le_bytes());
    payload.extend_from_slice(&(u16::from(spec.channels)).to_le_bytes());
    payload.extend_from_slice(&spec.sample_rate.to_le_bytes());
    payload.extend_from_slice(&(spec.sample_rate * block_align as u32).to_le_bytes());
    payload.extend_from_slice(&(block_align as u16).to_le_bytes());
    payload.extend_from_slice(&container_bits_per_sample.to_le_bytes());

    if !use_canonical_pcm {
        payload.extend_from_slice(&22u16.to_le_bytes());
        payload.extend_from_slice(&(u16::from(spec.bits_per_sample)).to_le_bytes());
        payload.extend_from_slice(&channel_mask.to_le_bytes());
        payload.extend_from_slice(&PCM_SUBFORMAT_GUID);
    }

    payload
}

fn wav_metadata_chunks(metadata: &WavMetadata, emit_fxmd: bool) -> Vec<([u8; 4], Vec<u8>)> {
    if metadata.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    if emit_fxmd && let Some(payload) = metadata.unified_chunk_payload() {
        chunks.push((FXMD_CHUNK_ID, payload));
    }
    if let Some(payload) = metadata.list_info_chunk_payload() {
        chunks.push((*b"LIST", payload));
    }
    if let Some(payload) = metadata.cue_chunk_payload() {
        chunks.push((*b"cue ", payload));
    }
    chunks
}

fn resolve_pcm_container(
    requested: PcmContainer,
    fmt_payload_len: usize,
    metadata_chunks: &[([u8; 4], Vec<u8>)],
    data_bytes: u64,
) -> Result<PcmContainer> {
    #[cfg(not(feature = "wav"))]
    {
        return resolve_pcm_container_without_wav(requested);
    }

    let wave_riff_size = 4u64
        + riff_chunk_serialized_size(fmt_payload_len)
        + metadata_chunks
            .iter()
            .map(|(_, payload)| riff_chunk_serialized_size(payload.len()))
            .sum::<u64>()
        + 8
        + data_bytes;

    match requested {
        PcmContainer::Auto => {
            ensure_output_container_enabled(PcmContainer::Auto)?;
            if wave_riff_size <= u64::from(u32::MAX) {
                Ok(PcmContainer::Wave)
            } else {
                Ok(PcmContainer::Rf64)
            }
        }
        PcmContainer::Wave => {
            ensure_output_container_enabled(PcmContainer::Wave)?;
            if wave_riff_size > u64::from(u32::MAX) {
                Err(Error::UnsupportedWav(
                    "decoded WAV output exceeds RIFF size limits; use RF64 or Wave64".into(),
                ))
            } else {
                Ok(PcmContainer::Wave)
            }
        }
        PcmContainer::Rf64 => {
            ensure_output_container_enabled(PcmContainer::Rf64)?;
            Ok(PcmContainer::Rf64)
        }
        PcmContainer::Wave64 => {
            ensure_output_container_enabled(PcmContainer::Wave64)?;
            Ok(PcmContainer::Wave64)
        }
        PcmContainer::Aiff => {
            ensure_output_container_enabled(PcmContainer::Aiff)?;
            Ok(PcmContainer::Aiff)
        }
        PcmContainer::Aifc => {
            ensure_output_container_enabled(PcmContainer::Aifc)?;
            Ok(PcmContainer::Aifc)
        }
        PcmContainer::Caf => {
            ensure_output_container_enabled(PcmContainer::Caf)?;
            Ok(PcmContainer::Caf)
        }
    }
}

fn feature_disabled_output_error(container: PcmContainer) -> Error {
    Error::UnsupportedWav(format!(
        "{} output requires the `{}` cargo feature",
        container.family_label(),
        container.feature_name()
    ))
}

pub(crate) fn ensure_output_container_enabled(container: PcmContainer) -> Result<()> {
    if container.is_enabled() {
        Ok(())
    } else {
        Err(feature_disabled_output_error(container))
    }
}

#[cfg(feature = "aiff")]
fn write_aiff<W: Write>(
    writer: &mut W,
    spec: WavSpec,
    samples: &[i32],
    metadata: &WavMetadata,
) -> Result<[u8; 16]> {
    write_aiff_with_metadata_and_md5(writer, spec, samples, metadata, AiffContainer::Aiff)
}

#[cfg(not(feature = "aiff"))]
fn write_aiff<W: Write>(
    _writer: &mut W,
    _spec: WavSpec,
    _samples: &[i32],
    _metadata: &WavMetadata,
) -> Result<[u8; 16]> {
    Err(feature_disabled_output_error(PcmContainer::Aiff))
}

#[cfg(feature = "aiff")]
fn write_aifc<W: Write>(
    writer: &mut W,
    spec: WavSpec,
    samples: &[i32],
    metadata: &WavMetadata,
) -> Result<[u8; 16]> {
    write_aiff_with_metadata_and_md5(writer, spec, samples, metadata, AiffContainer::AifcNone)
}

#[cfg(not(feature = "aiff"))]
fn write_aifc<W: Write>(
    _writer: &mut W,
    _spec: WavSpec,
    _samples: &[i32],
    _metadata: &WavMetadata,
) -> Result<[u8; 16]> {
    Err(feature_disabled_output_error(PcmContainer::Aifc))
}

#[cfg(feature = "caf")]
fn write_caf_container<W: Write>(
    writer: &mut W,
    spec: WavSpec,
    samples: &[i32],
    metadata: &WavMetadata,
) -> Result<[u8; 16]> {
    write_caf(writer, spec, samples, metadata)
}

#[cfg(not(feature = "caf"))]
fn write_caf_container<W: Write>(
    _writer: &mut W,
    _spec: WavSpec,
    _samples: &[i32],
    _metadata: &WavMetadata,
) -> Result<[u8; 16]> {
    Err(feature_disabled_output_error(PcmContainer::Caf))
}

fn write_wave_header_and_chunks<W: Write>(
    writer: &mut W,
    fmt_payload: &[u8],
    metadata_chunks: &[([u8; 4], Vec<u8>)],
    data_bytes: u64,
) -> Result<()> {
    let riff_size = 4u64
        + riff_chunk_serialized_size(fmt_payload.len())
        + metadata_chunks
            .iter()
            .map(|(_, payload)| riff_chunk_serialized_size(payload.len()))
            .sum::<u64>()
        + 8
        + data_bytes;
    let riff_size = u32::try_from(riff_size)
        .map_err(|_| Error::UnsupportedWav("RIFF output exceeds 4 GiB".into()))?;

    writer.write_all(b"RIFF")?;
    writer.write_all(&riff_size.to_le_bytes())?;
    writer.write_all(b"WAVE")?;
    write_riff_chunk(writer, b"fmt ", fmt_payload)?;
    for (chunk_id, payload) in metadata_chunks {
        write_riff_chunk(writer, chunk_id, payload)?;
    }
    writer.write_all(b"data")?;
    writer.write_all(
        &u32::try_from(data_bytes)
            .map_err(|_| Error::UnsupportedWav("RIFF data chunk exceeds 4 GiB".into()))?
            .to_le_bytes(),
    )?;
    Ok(())
}

fn write_rf64_header_and_chunks<W: Write>(
    writer: &mut W,
    total_samples: u64,
    fmt_payload: &[u8],
    metadata_chunks: &[([u8; 4], Vec<u8>)],
    data_bytes: u64,
) -> Result<()> {
    let file_size = 12u64
        + 36
        + riff_chunk_serialized_size(fmt_payload.len())
        + metadata_chunks
            .iter()
            .map(|(_, payload)| riff_chunk_serialized_size(payload.len()))
            .sum::<u64>()
        + 8
        + data_bytes;

    writer.write_all(b"RF64")?;
    writer.write_all(&RF64_PLACEHOLDER_SIZE.to_le_bytes())?;
    writer.write_all(b"WAVE")?;
    writer.write_all(b"ds64")?;
    writer.write_all(&28u32.to_le_bytes())?;
    writer.write_all(&(file_size - 8).to_le_bytes())?;
    writer.write_all(&data_bytes.to_le_bytes())?;
    writer.write_all(&total_samples.to_le_bytes())?;
    writer.write_all(&0u32.to_le_bytes())?;

    write_riff_chunk(writer, b"fmt ", fmt_payload)?;
    for (chunk_id, payload) in metadata_chunks {
        write_riff_chunk(writer, chunk_id, payload)?;
    }
    writer.write_all(b"data")?;
    writer.write_all(&RF64_PLACEHOLDER_SIZE.to_le_bytes())?;
    Ok(())
}

fn write_w64_header_and_chunks<W: Write>(
    writer: &mut W,
    fmt_payload: &[u8],
    metadata_chunks: &[([u8; 4], Vec<u8>)],
    data_bytes: u64,
) -> Result<()> {
    let total_size = 16u64
        + 8
        + 16
        + w64_chunk_serialized_size(fmt_payload.len() as u64)
        + metadata_chunks
            .iter()
            .map(|(_, payload)| w64_chunk_serialized_size(payload.len() as u64))
            .sum::<u64>()
        + w64_chunk_serialized_size(data_bytes);

    writer.write_all(&W64_RIFF_GUID)?;
    writer.write_all(&total_size.to_le_bytes())?;
    writer.write_all(&w64_chunk_guid(*b"wave"))?;
    write_w64_chunk(writer, *b"fmt ", fmt_payload)?;
    for (chunk_id, payload) in metadata_chunks {
        write_w64_chunk(writer, *chunk_id, payload)?;
    }
    writer.write_all(&w64_chunk_guid(*b"data"))?;
    writer.write_all(&(24u64 + data_bytes).to_le_bytes())?;
    Ok(())
}

fn riff_chunk_serialized_size(payload_len: usize) -> u64 {
    let payload_len = payload_len as u64;
    8 + payload_len + (payload_len % 2)
}

fn w64_chunk_serialized_size(payload_len: u64) -> u64 {
    let chunk_size = 24 + payload_len;
    chunk_size + ((8 - (chunk_size % 8)) % 8)
}

fn write_riff_chunk<W: Write>(writer: &mut W, id: &[u8; 4], payload: &[u8]) -> Result<()> {
    writer.write_all(id)?;
    writer.write_all(&(payload.len() as u32).to_le_bytes())?;
    writer.write_all(payload)?;
    if !payload.len().is_multiple_of(2) {
        writer.write_all(&[0])?;
    }
    Ok(())
}

fn write_w64_chunk<W: Write>(writer: &mut W, id: [u8; 4], payload: &[u8]) -> Result<()> {
    writer.write_all(&w64_chunk_guid(id))?;
    writer.write_all(&(24u64 + payload.len() as u64).to_le_bytes())?;
    writer.write_all(payload)?;
    let padding = w64_padding(payload.len());
    if padding != 0 {
        writer.write_all(&vec![0u8; padding])?;
    }
    Ok(())
}

fn w64_padding(payload_len: usize) -> usize {
    let chunk_size = 24 + payload_len;
    (8 - (chunk_size % 8)) % 8
}

fn w64_chunk_guid(chunk_id: [u8; 4]) -> [u8; 16] {
    let mut guid = [0u8; 16];
    guid[..4].copy_from_slice(&chunk_id);
    guid[4..].copy_from_slice(&W64_CHUNK_GUID_SUFFIX);
    guid
}

fn write_sample_bytes<W: Write>(
    writer: &mut W,
    samples: &[i32],
    envelope: PcmEnvelope,
) -> Result<[u8; 16]> {
    const CHUNK_CAPACITY: usize = 64 * 1024;
    let (hash_sender, hash_receiver) = mpsc::sync_channel::<Vec<u8>>(2);
    let hash_worker = thread::spawn(move || {
        let mut md5 = Md5::new();
        for chunk in hash_receiver {
            md5.update(&chunk);
        }
        md5.finalize()
    });
    let mut buffer = Vec::with_capacity(CHUNK_CAPACITY);

    for &sample in samples {
        append_encoded_sample(&mut buffer, sample, envelope)?;
        if buffer.len() >= CHUNK_CAPACITY {
            writer.write_all(&buffer)?;
            hash_sender
                .send(std::mem::replace(
                    &mut buffer,
                    Vec::with_capacity(CHUNK_CAPACITY),
                ))
                .map_err(|_| Error::Thread("streaminfo md5 worker stopped".into()))?;
        }
    }

    if !buffer.is_empty() {
        writer.write_all(&buffer)?;
        hash_sender
            .send(buffer)
            .map_err(|_| Error::Thread("streaminfo md5 worker stopped".into()))?;
    }
    drop(hash_sender);

    hash_worker
        .join()
        .map_err(|_| Error::Thread("streaminfo md5 worker panicked".into()))
}

#[cfg(test)]
mod tests {
    use crate::{
        PcmContainer,
        input::{WavSpec, ordinary_channel_mask},
        metadata::WavMetadata,
    };

    use super::{
        WavMetadataWriteOptions, write_wav, write_wav_with_metadata,
        write_wav_with_metadata_and_md5_with_options,
    };

    fn parse_chunk_layout(wav: &[u8]) -> Vec<([u8; 4], u32)> {
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        let mut offset = 12usize;
        let mut chunks = Vec::new();
        while offset + 8 <= wav.len() {
            let id = wav[offset..offset + 4]
                .try_into()
                .expect("fixed wav chunk id slice");
            let size = u32::from_le_bytes(
                wav[offset + 4..offset + 8]
                    .try_into()
                    .expect("fixed wav chunk size slice"),
            );
            chunks.push((id, size));
            offset += 8 + size as usize;
            if !size.is_multiple_of(2) {
                offset += 1;
            }
        }
        chunks
    }

    fn synthetic_cuesheet_payload(track_offsets: &[u64], lead_out_offset: u64) -> Vec<u8> {
        let mut payload = vec![0u8; 128];
        payload.extend_from_slice(&0u64.to_be_bytes());
        payload.push(0);
        payload.extend_from_slice(&[0u8; 258]);
        payload.push((track_offsets.len() + 1) as u8);
        for (index, &offset) in track_offsets.iter().enumerate() {
            payload.extend_from_slice(&offset.to_be_bytes());
            payload.push((index + 1) as u8);
            payload.extend_from_slice(&[0u8; 12]);
            payload.push(0);
            payload.extend_from_slice(&[0u8; 13]);
            payload.push(1);
            payload.extend_from_slice(&0u64.to_be_bytes());
            payload.push(1);
            payload.extend_from_slice(&[0u8; 3]);
        }
        payload.extend_from_slice(&lead_out_offset.to_be_bytes());
        payload.push(170);
        payload.extend_from_slice(&[0u8; 12]);
        payload.push(0);
        payload.extend_from_slice(&[0u8; 13]);
        payload.push(0);
        payload
    }

    #[test]
    fn writes_canonical_16bit_wav() {
        let spec = WavSpec {
            sample_rate: 44_100,
            channels: 2,
            bits_per_sample: 16,
            total_samples: 2,
            bytes_per_sample: 2,
            channel_mask: ordinary_channel_mask(2u16).unwrap(),
        };
        let samples = [1, -2, 3, -4];
        let mut wav = Vec::new();

        write_wav(&mut wav, spec, &samples).unwrap();

        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(
            parse_chunk_layout(&wav),
            vec![(*b"fmt ", 16), (*b"data", 8)]
        );
    }

    #[test]
    fn writes_extensible_wav_for_padded_container() {
        let spec = WavSpec {
            sample_rate: 48_000,
            channels: 2,
            bits_per_sample: 12,
            total_samples: 2,
            bytes_per_sample: 2,
            channel_mask: ordinary_channel_mask(2u16).unwrap(),
        };
        let samples = [0x123, -0x123];
        let mut wav = Vec::new();

        write_wav(&mut wav, spec, &samples).unwrap();

        assert_eq!(
            parse_chunk_layout(&wav),
            vec![(*b"fmt ", 40), (*b"data", 4)]
        );
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 0xFFFE);
    }

    #[test]
    fn metadata_wav_layout_is_fixed_and_padded() {
        let spec = WavSpec {
            sample_rate: 44_100,
            channels: 1,
            bits_per_sample: 16,
            total_samples: 2,
            bytes_per_sample: 2,
            channel_mask: ordinary_channel_mask(1u16).unwrap(),
        };
        let samples = [1, -2];
        let mut metadata = WavMetadata::default();
        metadata
            .ingest_flac_metadata_block(
                4,
                &[
                    0, 0, 0, 0, // vendor len
                    1, 0, 0, 0, // comments
                    9, 0, 0, 0, // len
                    b'T', b'I', b'T', b'L', b'E', b'=', b'O', b'd', b'd',
                ],
                2,
                1,
            )
            .unwrap();
        metadata
            .ingest_flac_metadata_block(5, &synthetic_cuesheet_payload(&[0], 2), 2, 1)
            .unwrap();

        let mut wav = Vec::new();
        write_wav_with_metadata(&mut wav, spec, &samples, &metadata).unwrap();

        let chunks = parse_chunk_layout(&wav);
        assert_eq!(
            chunks.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![*b"fmt ", *b"fxmd", *b"LIST", *b"cue ", *b"data"]
        );

        let mut list_index = None;
        let mut offset = 12usize;
        while offset + 8 <= wav.len() {
            let id: [u8; 4] = wav[offset..offset + 4].try_into().unwrap();
            let size = u32::from_le_bytes(wav[offset + 4..offset + 8].try_into().unwrap()) as usize;
            if id == *b"LIST" {
                list_index = Some(offset);
                break;
            }
            offset += 8 + size;
            if !size.is_multiple_of(2) {
                offset += 1;
            }
        }

        let list_index = list_index.expect("list chunk present");
        let list_size = u32::from_le_bytes(wav[list_index + 4..list_index + 8].try_into().unwrap());
        assert_eq!(list_size, 16);
        let padded_byte = wav[list_index + 8 + list_size as usize - 1];
        assert_eq!(padded_byte, 0);
    }

    #[test]
    fn metadata_output_can_omit_fxmd_while_preserving_other_chunks() {
        let spec = WavSpec {
            sample_rate: 44_100,
            channels: 1,
            bits_per_sample: 16,
            total_samples: 2,
            bytes_per_sample: 2,
            channel_mask: ordinary_channel_mask(1u16).unwrap(),
        };
        let samples = [1, -2];
        let mut metadata = WavMetadata::default();
        metadata
            .ingest_flac_metadata_block(
                4,
                &[
                    0, 0, 0, 0, // vendor len
                    1, 0, 0, 0, // comments
                    9, 0, 0, 0, // len
                    b'T', b'I', b'T', b'L', b'E', b'=', b'O', b'd', b'd',
                ],
                2,
                1,
            )
            .unwrap();
        metadata
            .ingest_flac_metadata_block(5, &synthetic_cuesheet_payload(&[0], 2), 2, 1)
            .unwrap();

        let mut wav = Vec::new();
        write_wav_with_metadata_and_md5_with_options(
            &mut wav,
            spec,
            &samples,
            &metadata,
            WavMetadataWriteOptions {
                emit_fxmd: false,
                container: PcmContainer::Auto,
            },
        )
        .unwrap();

        let chunks = parse_chunk_layout(&wav);
        assert_eq!(
            chunks.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![*b"fmt ", *b"LIST", *b"cue ", *b"data"]
        );
    }

    #[test]
    fn writes_non_ordinary_channel_masks_in_extensible_fmt() {
        let spec = WavSpec {
            sample_rate: 48_000,
            channels: 4,
            bits_per_sample: 16,
            total_samples: 2,
            bytes_per_sample: 2,
            channel_mask: 0x0001_2104,
        };
        let samples = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut wav = Vec::new();

        write_wav(&mut wav, spec, &samples).unwrap();

        assert_eq!(
            parse_chunk_layout(&wav),
            vec![(*b"fmt ", 40), (*b"data", 16)]
        );
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 0xFFFE);
        assert_eq!(
            u32::from_le_bytes(wav[40..44].try_into().unwrap()),
            0x0001_2104
        );
    }

    #[test]
    fn writes_zero_channel_mask_in_extensible_fmt() {
        let spec = WavSpec {
            sample_rate: 44_100,
            channels: 2,
            bits_per_sample: 16,
            total_samples: 1,
            bytes_per_sample: 2,
            channel_mask: 0,
        };
        let samples = [1, -1];
        let mut wav = Vec::new();

        write_wav(&mut wav, spec, &samples).unwrap();

        assert_eq!(
            parse_chunk_layout(&wav),
            vec![(*b"fmt ", 40), (*b"data", 4)]
        );
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 0);
    }
}
