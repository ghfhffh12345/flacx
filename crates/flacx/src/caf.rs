use std::io::{Read, Seek, SeekFrom};

use crate::{
    error::{Error, Result},
    pcm::is_supported_channel_mask,
    raw::{RawPcmByteOrder, RawPcmDescriptor, total_samples_from_byte_len_with_descriptor},
};

const CAF_MAGIC: [u8; 4] = *b"caff";
const CAF_VERSION: u16 = 1;
const DESC_CHUNK_ID: [u8; 4] = *b"desc";
const DATA_CHUNK_ID: [u8; 4] = *b"data";
const CHAN_CHUNK_ID: [u8; 4] = *b"chan";
const PAKT_CHUNK_ID: [u8; 4] = *b"pakt";
const LPCM_FORMAT_ID: [u8; 4] = *b"lpcm";
const CAF_LAYOUT_TAG_USE_CHANNEL_BITMAP: u32 = 0x0001_0000;
const CAF_FLAG_IS_FLOAT: u32 = 1 << 0;
const CAF_FLAG_IS_LITTLE_ENDIAN: u32 = 1 << 1;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedCaf {
    descriptor: RawPcmDescriptor,
    data_offset: u64,
    data_size: u64,
    data: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CafChannelLayout {
    layout_tag: u32,
    channel_bitmap: u32,
    number_channel_descriptions: u32,
}

/// Reader façade for CAF encode inputs.
#[derive(Debug, Clone)]
pub struct CafReader<R> {
    reader: R,
    spec: crate::input::PcmSpec,
    metadata: crate::metadata::Metadata,
    descriptor: RawPcmDescriptor,
}

impl<R: Read + Seek> CafReader<R> {
    /// Parse a CAF reader into a reusable encode-side façade.
    pub fn new(mut reader: R) -> Result<Self> {
        let parsed = parse_caf(&mut reader, false)?;
        reader.seek(SeekFrom::Start(parsed.data_offset))?;
        let total_samples =
            total_samples_from_byte_len_with_descriptor(parsed.data_size, parsed.descriptor)?;
        Ok(Self {
            reader,
            spec: crate::input::PcmSpec {
                sample_rate: parsed.descriptor.sample_rate,
                channels: parsed.descriptor.channels,
                bits_per_sample: parsed.descriptor.valid_bits_per_sample,
                total_samples,
                bytes_per_sample: u16::from(parsed.descriptor.container_bits_per_sample) / 8,
                channel_mask: parsed.descriptor.channel_mask.unwrap_or_default(),
            },
            metadata: crate::metadata::Metadata::default(),
            descriptor: parsed.descriptor,
        })
    }

    /// Return the parsed PCM stream specification.
    #[must_use]
    pub fn spec(&self) -> crate::input::PcmSpec {
        self.spec
    }

    /// Return metadata staged for encode-side preservation.
    #[must_use]
    pub fn metadata(&self) -> &crate::metadata::Metadata {
        &self.metadata
    }

    /// Convert this reader into an owned encode source.
    pub fn into_source(self) -> crate::input::EncodeSource<impl crate::input::EncodePcmStream> {
        let (metadata, stream) = self.into_session_parts();
        crate::input::EncodeSource::new(metadata, stream)
    }

    #[allow(dead_code)]
    pub(crate) fn into_pcm_stream(self) -> CafPcmStream<R> {
        self.into_session_parts().1
    }

    pub(crate) fn into_session_parts(self) -> (crate::metadata::Metadata, CafPcmStream<R>) {
        let Self {
            reader,
            spec,
            metadata,
            descriptor,
        } = self;
        (
            metadata,
            CafPcmStream::new(reader, descriptor, spec.total_samples)
                .expect("validated CAF descriptor remains valid"),
        )
    }
}

/// Single-pass PCM stream produced by [`CafReader`].
#[derive(Debug)]
pub struct CafPcmStream<R> {
    inner: crate::raw::RawPcmStream<R>,
}

impl<R: Read + Seek> CafPcmStream<R> {
    /// Directly construct a CAF LPCM stream from a positioned payload reader,
    /// explicit descriptor, and known per-channel sample count.
    pub fn new(reader: R, descriptor: RawPcmDescriptor, total_samples: u64) -> Result<Self> {
        validate_direct_descriptor(descriptor)?;
        Ok(Self {
            inner: crate::raw::RawPcmStream::new(reader, descriptor, total_samples)?,
        })
    }
}

impl<R: Read + Seek> crate::input::EncodePcmStream for CafPcmStream<R> {
    fn spec(&self) -> crate::input::PcmSpec {
        self.inner.spec()
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize> {
        self.inner.read_chunk(max_frames, output)
    }

    #[cfg(feature = "progress")]
    fn input_bytes_processed(&self) -> u64 {
        self.inner.input_bytes_processed()
    }
}

fn validate_direct_descriptor(descriptor: RawPcmDescriptor) -> Result<()> {
    if matches!(descriptor.channel_mask, Some(0)) {
        return Err(Error::UnsupportedWav(
            "CAF channel layout mappings must use a supported non-zero channel bitmap".into(),
        ));
    }
    let _ = total_samples_from_byte_len_with_descriptor(0, descriptor)?;
    Ok(())
}

pub(crate) fn inspect_caf_total_samples<R: Read + Seek>(reader: &mut R) -> Result<u64> {
    let parsed = parse_caf(reader, false)?;
    total_samples_from_byte_len_with_descriptor(parsed.data_size, parsed.descriptor)
}

fn parse_caf<R: Read + Seek>(reader: &mut R, read_data: bool) -> Result<ParsedCaf> {
    let mut header = [0u8; 8];
    reader.read_exact(&mut header)?;
    if header[..4] != CAF_MAGIC {
        return Err(Error::InvalidWav("expected CAF header"));
    }
    let version = u16::from_be_bytes(header[4..6].try_into().expect("fixed version"));
    if version != CAF_VERSION {
        return Err(Error::UnsupportedWav(format!(
            "CAF version {version} is not supported"
        )));
    }
    let flags = u16::from_be_bytes(header[6..8].try_into().expect("fixed flags"));
    if flags != 0 {
        return Err(Error::UnsupportedWav(format!(
            "CAF header flags {flags:#06x} are not supported"
        )));
    }

    let desc_header = read_chunk_header(reader)?;
    if desc_header.chunk_id != DESC_CHUNK_ID {
        return Err(Error::InvalidWav(
            "CAF Audio Description chunk must follow the file header",
        ));
    }
    let desc_chunk_start = reader.stream_position()?;
    let descriptor = read_audio_description_chunk(reader, desc_header.size)?;
    seek_forward(
        reader,
        desc_chunk_start,
        u64::try_from(desc_header.size).expect("desc size validated"),
    )?;

    let mut data_offset = None;
    let mut data_size = None;
    let mut data = None;
    let mut channel_layout = None;

    loop {
        let chunk_header = match read_chunk_header(reader) {
            Ok(header) => header,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error),
        };
        let chunk_start = reader.stream_position()?;
        let chunk_size = u64::try_from(chunk_header.size).map_err(|_| {
            Error::UnsupportedWav("negative CAF chunk sizes are not supported".into())
        })?;

        match chunk_header.chunk_id {
            DATA_CHUNK_ID => {
                if chunk_header.size < 4 {
                    return Err(Error::InvalidWav("CAF data chunk is too short"));
                }
                let _edit_count = read_u32_be(reader)?;
                let audio_size = chunk_size - 4;
                data_size = Some(audio_size);
                let payload_offset = reader.stream_position()?;
                data_offset = Some(payload_offset);
                if read_data {
                    let data_len = usize::try_from(audio_size).map_err(|_| {
                        Error::UnsupportedWav("PCM payload exceeds memory-addressable size".into())
                    })?;
                    let mut payload = vec![0u8; data_len];
                    reader.read_exact(&mut payload)?;
                    data = Some(payload);
                } else {
                    seek_forward(reader, chunk_start, chunk_size)?;
                }
            }
            CHAN_CHUNK_ID => {
                channel_layout = Some(read_channel_layout_chunk(reader, chunk_size)?);
                seek_forward(reader, chunk_start, chunk_size)?;
            }
            PAKT_CHUNK_ID => {
                return Err(Error::UnsupportedWav(
                    "CAF packet table chunks are not supported in Stage 3".into(),
                ));
            }
            _ => {
                seek_forward(reader, chunk_start, chunk_size)?;
            }
        }
    }

    let channel_mask = resolve_channel_mask(descriptor.channels, channel_layout)?;
    let mut descriptor = descriptor;
    descriptor.channel_mask = channel_mask;

    Ok(ParsedCaf {
        descriptor,
        data_offset: data_offset.ok_or(Error::InvalidWav("missing CAF data chunk"))?,
        data_size: data_size.ok_or(Error::InvalidWav("missing CAF data chunk"))?,
        data,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChunkHeader {
    chunk_id: [u8; 4],
    size: i64,
}

fn read_chunk_header<R: Read>(reader: &mut R) -> Result<ChunkHeader> {
    let mut header = [0u8; 12];
    reader.read_exact(&mut header)?;
    Ok(ChunkHeader {
        chunk_id: header[..4].try_into().expect("fixed chunk id"),
        size: i64::from_be_bytes(header[4..12].try_into().expect("fixed chunk size")),
    })
}

fn read_audio_description_chunk<R: Read>(
    reader: &mut R,
    chunk_size: i64,
) -> Result<RawPcmDescriptor> {
    if chunk_size < 32 {
        return Err(Error::InvalidWav(
            "CAF Audio Description chunk is too short",
        ));
    }
    let sample_rate = read_f64_be(reader)?;
    if !sample_rate.is_finite() || sample_rate <= 0.0 || sample_rate.fract() != 0.0 {
        return Err(Error::UnsupportedWav(
            "CAF sample rates must be positive whole numbers".into(),
        ));
    }
    let sample_rate = u32::try_from(sample_rate as u64)
        .map_err(|_| Error::UnsupportedWav("CAF sample rate exceeds supported range".into()))?;

    let mut format_id = [0u8; 4];
    reader.read_exact(&mut format_id)?;
    if format_id != LPCM_FORMAT_ID {
        return Err(Error::UnsupportedWav(format!(
            "CAF format '{}' is not supported in Stage 3",
            String::from_utf8_lossy(&format_id)
        )));
    }

    let format_flags = read_u32_be(reader)?;
    if format_flags & CAF_FLAG_IS_FLOAT != 0 {
        return Err(Error::UnsupportedWav("float CAF is not supported".into()));
    }
    if format_flags & !(CAF_FLAG_IS_FLOAT | CAF_FLAG_IS_LITTLE_ENDIAN) != 0 {
        return Err(Error::UnsupportedWav(format!(
            "CAF format flags {format_flags:#010x} are not supported"
        )));
    }

    let bytes_per_packet = read_u32_be(reader)?;
    let frames_per_packet = read_u32_be(reader)?;
    let channels = read_u32_be(reader)?;
    let bits_per_channel = read_u32_be(reader)?;

    if frames_per_packet != 1 {
        return Err(Error::UnsupportedWav(format!(
            "CAF frames/packet must be 1 for Stage 3 linear PCM, found {frames_per_packet}"
        )));
    }
    if channels == 0 {
        return Err(Error::InvalidWav("CAF channel count must be non-zero"));
    }
    if bytes_per_packet == 0 {
        return Err(Error::UnsupportedWav(
            "CAF bytes/packet must be non-zero for Stage 3 linear PCM".into(),
        ));
    }
    if bytes_per_packet % channels != 0 {
        return Err(Error::InvalidWav(
            "CAF bytes/packet does not align to the channel count",
        ));
    }
    let bytes_per_sample = bytes_per_packet / channels;
    let container_bits_per_sample = bytes_per_sample
        .checked_mul(8)
        .ok_or_else(|| Error::UnsupportedWav("CAF container width overflows".into()))?;

    let byte_order = if format_flags & CAF_FLAG_IS_LITTLE_ENDIAN != 0 {
        RawPcmByteOrder::LittleEndian
    } else {
        RawPcmByteOrder::BigEndian
    };

    Ok(RawPcmDescriptor {
        sample_rate,
        channels: u8::try_from(channels).map_err(|_| {
            Error::UnsupportedWav("CAF channel count exceeds supported range".into())
        })?,
        valid_bits_per_sample: u8::try_from(bits_per_channel).map_err(|_| {
            Error::UnsupportedWav("CAF bits/channel exceeds supported range".into())
        })?,
        container_bits_per_sample: u8::try_from(container_bits_per_sample).map_err(|_| {
            Error::UnsupportedWav("CAF container width exceeds supported range".into())
        })?,
        byte_order,
        channel_mask: None,
    })
}

fn read_channel_layout_chunk<R: Read>(reader: &mut R, chunk_size: u64) -> Result<CafChannelLayout> {
    if chunk_size < 12 {
        return Err(Error::InvalidWav("CAF channel layout chunk is too short"));
    }
    Ok(CafChannelLayout {
        layout_tag: read_u32_be(reader)?,
        channel_bitmap: read_u32_be(reader)?,
        number_channel_descriptions: read_u32_be(reader)?,
    })
}

fn resolve_channel_mask(
    channels: u8,
    channel_layout: Option<CafChannelLayout>,
) -> Result<Option<u32>> {
    let Some(channel_layout) = channel_layout else {
        return if channels <= 2 {
            Ok(None)
        } else {
            Err(Error::UnsupportedWav(
                "CAF 3..8 channel inputs require a supported channel layout mapping".into(),
            ))
        };
    };
    if channel_layout.layout_tag != CAF_LAYOUT_TAG_USE_CHANNEL_BITMAP
        || channel_layout.number_channel_descriptions != 0
    {
        return if channels <= 2 {
            Ok(None)
        } else {
            Err(Error::UnsupportedWav(
                "CAF channel layout mappings must use a supported channel bitmap in Stage 3".into(),
            ))
        };
    }
    if channel_layout.channel_bitmap.count_ones() != u32::from(channels)
        || !is_supported_channel_mask(u16::from(channels), channel_layout.channel_bitmap)
    {
        return Err(Error::UnsupportedWav(
            "CAF channel bitmap does not map to a supported channel layout".into(),
        ));
    }
    Ok(Some(channel_layout.channel_bitmap))
}

fn read_u32_be<R: Read>(reader: &mut R) -> Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_f64_be<R: Read>(reader: &mut R) -> Result<f64> {
    let mut bytes = [0u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(f64::from_bits(u64::from_be_bytes(bytes)))
}

fn seek_forward<R: Seek>(reader: &mut R, chunk_start: u64, chunk_size: u64) -> Result<()> {
    let target = chunk_start
        .checked_add(chunk_size)
        .ok_or(Error::InvalidWav("chunk length overflows the file cursor"))?;
    reader.seek(SeekFrom::Start(target))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{
        CAF_LAYOUT_TAG_USE_CHANNEL_BITMAP, CAF_MAGIC, CHAN_CHUNK_ID, CafPcmStream, CafReader,
        DATA_CHUNK_ID, DESC_CHUNK_ID, LPCM_FORMAT_ID, inspect_caf_total_samples,
    };
    use crate::{RawPcmByteOrder, RawPcmDescriptor, input::EncodePcmStream};

    fn append_chunk(bytes: &mut Vec<u8>, chunk_id: [u8; 4], payload: &[u8]) {
        bytes.extend_from_slice(&chunk_id);
        bytes.extend_from_slice(&(payload.len() as i64).to_be_bytes());
        bytes.extend_from_slice(payload);
    }

    fn caf_lpcm_bytes(
        sample_rate: u32,
        channels: u32,
        valid_bits_per_sample: u32,
        container_bits_per_sample: u32,
        little_endian: bool,
        samples: &[i32],
    ) -> Vec<u8> {
        let bytes_per_sample = container_bits_per_sample / 8;
        let bytes_per_packet = channels * bytes_per_sample;
        let mut desc = Vec::new();
        desc.extend_from_slice(&(sample_rate as f64).to_bits().to_be_bytes());
        desc.extend_from_slice(&LPCM_FORMAT_ID);
        desc.extend_from_slice(&(u32::from(little_endian) << 1).to_be_bytes());
        desc.extend_from_slice(&bytes_per_packet.to_be_bytes());
        desc.extend_from_slice(&1u32.to_be_bytes());
        desc.extend_from_slice(&channels.to_be_bytes());
        desc.extend_from_slice(&valid_bits_per_sample.to_be_bytes());

        let mut data = Vec::new();
        data.extend_from_slice(&0u32.to_be_bytes());
        for &sample in samples {
            let shifted = if valid_bits_per_sample == container_bits_per_sample {
                sample
            } else {
                sample << (container_bits_per_sample - valid_bits_per_sample)
            };
            match container_bits_per_sample {
                16 => {
                    let value = shifted as i16;
                    if little_endian {
                        data.extend_from_slice(&value.to_le_bytes());
                    } else {
                        data.extend_from_slice(&value.to_be_bytes());
                    }
                }
                24 => {
                    let value = shifted as u32;
                    let chunk = if little_endian {
                        [
                            (value & 0xff) as u8,
                            ((value >> 8) & 0xff) as u8,
                            ((value >> 16) & 0xff) as u8,
                        ]
                    } else {
                        [
                            ((value >> 16) & 0xff) as u8,
                            ((value >> 8) & 0xff) as u8,
                            (value & 0xff) as u8,
                        ]
                    };
                    data.extend_from_slice(&chunk);
                }
                32 => {
                    if little_endian {
                        data.extend_from_slice(&shifted.to_le_bytes());
                    } else {
                        data.extend_from_slice(&shifted.to_be_bytes());
                    }
                }
                _ => unreachable!(),
            }
        }

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&CAF_MAGIC);
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        append_chunk(&mut bytes, DESC_CHUNK_ID, &desc);
        append_chunk(&mut bytes, DATA_CHUNK_ID, &data);
        bytes
    }

    #[test]
    fn inspects_and_reads_stereo_caf_lpcm() {
        let bytes = caf_lpcm_bytes(44_100, 2, 16, 16, false, &[1, -2, 3, -4]);
        assert_eq!(
            inspect_caf_total_samples(&mut Cursor::new(&bytes)).unwrap(),
            2
        );
        let reader = CafReader::new(Cursor::new(bytes)).unwrap();
        let mut stream = reader.into_pcm_stream();
        let mut samples = Vec::new();
        stream.read_chunk(2, &mut samples).unwrap();
        assert_eq!(samples, vec![1, -2, 3, -4]);
    }

    #[test]
    fn rejects_packet_tables_in_stage_three() {
        let mut bytes = caf_lpcm_bytes(44_100, 2, 16, 16, true, &[1, -2, 3, -4]);
        append_chunk(&mut bytes, *b"pakt", &[0; 24]);
        let error = CafReader::new(Cursor::new(bytes)).unwrap_err();
        assert!(error.to_string().contains("packet table"));
    }

    #[test]
    fn rejects_multichannel_without_supported_layout_mapping() {
        let bytes = caf_lpcm_bytes(48_000, 4, 16, 16, true, &[0; 16]);
        let error = CafReader::new(Cursor::new(bytes)).unwrap_err();
        assert!(error.to_string().contains("channel layout"));
    }

    #[test]
    fn accepts_multichannel_bitmap_layouts_with_supported_masks() {
        let mut bytes = caf_lpcm_bytes(48_000, 4, 16, 16, true, &[0; 16]);
        let mut chan = Vec::new();
        chan.extend_from_slice(&CAF_LAYOUT_TAG_USE_CHANNEL_BITMAP.to_be_bytes());
        chan.extend_from_slice(&0x0033u32.to_be_bytes());
        chan.extend_from_slice(&0u32.to_be_bytes());
        append_chunk(&mut bytes, CHAN_CHUNK_ID, &chan);
        let parsed = CafReader::new(Cursor::new(bytes)).unwrap();
        assert_eq!(parsed.spec().channel_mask, 0x0033);
    }

    #[test]
    fn directly_constructs_caf_stream_from_descriptor() {
        let descriptor = RawPcmDescriptor {
            sample_rate: 48_000,
            channels: 4,
            valid_bits_per_sample: 16,
            container_bits_per_sample: 16,
            byte_order: RawPcmByteOrder::LittleEndian,
            channel_mask: Some(0x0033),
        };
        let data = [1i16, -2, 3, -4, 5, -6, 7, -8]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect::<Vec<_>>();

        let mut stream = CafPcmStream::new(Cursor::new(data), descriptor, 2).unwrap();
        let mut samples = Vec::new();
        stream.read_chunk(2, &mut samples).unwrap();

        assert_eq!(stream.spec().channel_mask, 0x0033);
        assert_eq!(samples, vec![1, -2, 3, -4, 5, -6, 7, -8]);
    }

    #[test]
    fn rejects_zero_bitmap_in_direct_caf_descriptor() {
        let descriptor = RawPcmDescriptor {
            sample_rate: 44_100,
            channels: 2,
            valid_bits_per_sample: 16,
            container_bits_per_sample: 16,
            byte_order: RawPcmByteOrder::BigEndian,
            channel_mask: Some(0),
        };
        let error = CafPcmStream::new(Cursor::new(vec![0; 4]), descriptor, 1).unwrap_err();
        assert!(error.to_string().contains("channel bitmap"));
    }
}
