use std::io::{self, Seek, Write};

use crate::metadata::StreamInfo;

pub struct FlacWriter<W: Seek + Write> {
    writer: W,
    stream_info: StreamInfo,
}

impl<W: Seek + Write> FlacWriter<W> {
    pub fn new(mut writer: W, stream_info: StreamInfo) -> io::Result<Self> {
        writer.write_all(b"fLaC")?;
        writer.write_all(&stream_info.to_bytes())?;

        Ok(Self {
            writer,
            stream_info,
        })
    }
}
