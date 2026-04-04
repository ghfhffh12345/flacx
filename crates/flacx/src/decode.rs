use std::{
    fs::File,
    io::{Cursor, Read, Seek, Write},
    path::Path,
};

use crate::{error::Result, read::read_flac, stream_info::StreamInfo, wav_output::write_wav};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeSummary {
    pub frame_count: usize,
    pub total_samples: u64,
    pub block_size: u16,
    pub min_frame_size: u32,
    pub max_frame_size: u32,
    pub min_block_size: u16,
    pub max_block_size: u16,
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
}

/// Primary library façade for FLAC-to-WAV conversion.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Decoder;

impl Decoder {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    pub fn decode<R, W>(&self, input: R, mut output: W) -> Result<DecodeSummary>
    where
        R: Read + Seek,
        W: Write + Seek,
    {
        let (wav, stream_info, frame_count) = read_flac(input)?;
        write_wav(&mut output, wav.spec, &wav.samples)?;
        Ok(summary_from_stream_info(stream_info, frame_count))
    }

    pub fn decode_file<P, Q>(&self, input_path: P, output_path: Q) -> Result<DecodeSummary>
    where
        P: AsRef<Path>,
        Q: AsRef<Path>,
    {
        self.decode(File::open(input_path)?, File::create(output_path)?)
    }

    pub fn decode_bytes(&self, input: &[u8]) -> Result<Vec<u8>> {
        let mut output = Cursor::new(Vec::new());
        self.decode(Cursor::new(input), &mut output)?;
        Ok(output.into_inner())
    }
}

pub fn decode_file<P, Q>(input_path: P, output_path: Q) -> Result<DecodeSummary>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    Decoder::default().decode_file(input_path, output_path)
}

pub fn decode_bytes(input: &[u8]) -> Result<Vec<u8>> {
    Decoder::default().decode_bytes(input)
}

fn summary_from_stream_info(stream_info: StreamInfo, frame_count: usize) -> DecodeSummary {
    DecodeSummary {
        frame_count,
        total_samples: stream_info.total_samples,
        block_size: stream_info.max_block_size,
        min_frame_size: stream_info.min_frame_size,
        max_frame_size: stream_info.max_frame_size,
        min_block_size: stream_info.min_block_size,
        max_block_size: stream_info.max_block_size,
        sample_rate: stream_info.sample_rate,
        channels: stream_info.channels,
        bits_per_sample: stream_info.bits_per_sample,
    }
}
