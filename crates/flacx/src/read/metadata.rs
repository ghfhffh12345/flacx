use super::{Error, FLAC_MAGIC, Result, STREAMINFO_BLOCK_TYPE, StreamInfo, WavMetadata};
use crate::{
    input::ordinary_channel_mask,
    metadata::{
        FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY, SEEKTABLE_BLOCK_TYPE, validate_seektable_payload,
    },
};
use std::io::{ErrorKind, Read};

pub(super) fn resolve_channel_mask(
    channels: u8,
    metadata: &WavMetadata,
    strict_channel_mask_provenance: bool,
) -> Result<u32> {
    let ordinary_mask = ordinary_channel_mask(u16::from(channels))
        .expect("ordinary channel mask must exist after validating 1..8 channels in STREAMINFO");
    if strict_channel_mask_provenance
        && requires_channel_layout_provenance(channels, metadata.channel_mask())
        && !metadata.has_channel_layout_provenance()
    {
        return Err(Error::UnsupportedFlac(format!(
            "strict channel-layout provenance requires {FLACX_CHANNEL_LAYOUT_PROVENANCE_KEY} for {channels}-channel decode"
        )));
    }
    Ok(metadata.channel_mask().unwrap_or(ordinary_mask))
}

pub(super) fn requires_channel_layout_provenance(_channels: u8, channel_mask: Option<u32>) -> bool {
    channel_mask.is_some()
}

/// Inspect a FLAC stream and return the total sample count stored in
/// `STREAMINFO`.
///
/// This helper validates the FLAC marker and first metadata block, then
/// extracts the total sample count without decoding audio frames.
///
/// # Example
///
/// ```no_run
/// use flacx::inspect_flac_total_samples;
/// use std::fs::File;
///
/// let total_samples = inspect_flac_total_samples(File::open("input.flac").unwrap()).unwrap();
/// assert!(total_samples > 0);
/// ```
pub fn inspect_flac_total_samples<R: Read>(mut reader: R) -> Result<u64> {
    let mut magic = [0u8; 4];
    read_exact_or_invalid(&mut reader, &mut magic, "file is too short")?;
    if &magic != FLAC_MAGIC {
        return Err(Error::InvalidFlac("expected fLaC stream marker"));
    }

    let mut header = [0u8; 4];
    read_exact_or_invalid(
        &mut reader,
        &mut header,
        "metadata block header is truncated",
    )?;
    let block_type = header[0] & 0x7f;
    let block_len = u32::from_be_bytes([0, header[1], header[2], header[3]]) as usize;
    if block_type != STREAMINFO_BLOCK_TYPE || block_len != 34 {
        return Err(Error::InvalidFlac(
            "first metadata block must be a 34-byte STREAMINFO block",
        ));
    }

    let mut raw = [0u8; 34];
    read_exact_or_invalid(&mut reader, &mut raw, "metadata block body is truncated")?;
    Ok(StreamInfo::from_bytes(raw).total_samples)
}

pub(super) fn parse_metadata(
    bytes: &[u8],
    strict_seektable_validation: bool,
) -> Result<(StreamInfo, WavMetadata, usize)> {
    if bytes.len() < 8 {
        return Err(Error::InvalidFlac("file is too short"));
    }
    if &bytes[..4] != FLAC_MAGIC {
        return Err(Error::InvalidFlac("expected fLaC stream marker"));
    }

    let mut offset = 4usize;
    let mut saw_streaminfo = false;
    let mut stream_info = None;
    let mut metadata = WavMetadata::default();
    let mut saw_seektable = false;
    loop {
        if offset + 4 > bytes.len() {
            return Err(Error::InvalidFlac("metadata block header is truncated"));
        }
        let header = bytes[offset];
        let is_last = header & 0x80 != 0;
        let block_type = header & 0x7f;
        let block_len =
            u32::from_be_bytes([0, bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
                as usize;
        offset += 4;
        if offset + block_len > bytes.len() {
            return Err(Error::InvalidFlac("metadata block body is truncated"));
        }

        if !saw_streaminfo {
            if block_type != STREAMINFO_BLOCK_TYPE || block_len != 34 {
                return Err(Error::InvalidFlac(
                    "first metadata block must be a 34-byte STREAMINFO block",
                ));
            }
            let mut raw = [0u8; 34];
            raw.copy_from_slice(&bytes[offset..offset + 34]);
            stream_info = Some(StreamInfo::from_bytes(raw));
            saw_streaminfo = true;
        } else if block_type == SEEKTABLE_BLOCK_TYPE {
            let seektable_result = validate_seektable_payload(&bytes[offset..offset + block_len]);
            let seektable_is_valid = seektable_result.is_ok();
            if strict_seektable_validation {
                seektable_result?;
                if saw_seektable {
                    return Err(Error::InvalidFlac(
                        "stream must not contain more than one seektable metadata block",
                    ));
                }
            }
            if seektable_is_valid {
                metadata.ingest_flac_metadata_block(
                    block_type,
                    &bytes[offset..offset + block_len],
                    stream_info
                        .expect("streaminfo parsed before optional metadata")
                        .total_samples,
                    stream_info
                        .expect("streaminfo parsed before optional metadata")
                        .channels,
                )?;
            }
            saw_seektable = true;
        } else {
            metadata.ingest_flac_metadata_block(
                block_type,
                &bytes[offset..offset + block_len],
                stream_info
                    .expect("streaminfo parsed before optional metadata")
                    .total_samples,
                stream_info
                    .expect("streaminfo parsed before optional metadata")
                    .channels,
            )?;
        }

        offset += block_len;
        if is_last {
            break;
        }
    }

    Ok((
        stream_info.ok_or(Error::InvalidFlac("missing STREAMINFO block"))?,
        metadata,
        offset,
    ))
}

fn read_exact_or_invalid<R: Read>(
    reader: &mut R,
    buffer: &mut [u8],
    truncated_message: &'static str,
) -> Result<()> {
    reader.read_exact(buffer).map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            Error::InvalidFlac(truncated_message)
        } else {
            error.into()
        }
    })
}
