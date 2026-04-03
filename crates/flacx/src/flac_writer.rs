use std::io::{self, Seek, SeekFrom, Write};

use crate::metadata::StreamInfo;

const STREAMINFO_LENGTH: [u8; 3] = [0x00, 0x00, 34];

pub(crate) struct FlacWriter<W: Seek + Write> {
    writer: W,
    stream_info: StreamInfo,
    streaminfo_offset: u64,
}

impl<W: Seek + Write> FlacWriter<W> {
    pub(crate) fn new(mut writer: W, stream_info: StreamInfo) -> io::Result<Self> {
        writer.write_all(b"fLaC")?;
        writer.write_all(&[
            0x80,
            STREAMINFO_LENGTH[0],
            STREAMINFO_LENGTH[1],
            STREAMINFO_LENGTH[2],
        ])?;
        let streaminfo_offset = writer.stream_position()?;
        writer.write_all(&stream_info.to_bytes())?;

        Ok(Self {
            writer,
            stream_info,
            streaminfo_offset,
        })
    }

    pub(crate) fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.stream_info.update_frame_size(frame.len() as u32);
        self.writer.write_all(frame)
    }

    pub(crate) fn finalize(mut self) -> io::Result<(W, StreamInfo)> {
        let end_position = self.writer.stream_position()?;
        self.writer.seek(SeekFrom::Start(self.streaminfo_offset))?;
        self.writer.write_all(&self.stream_info.to_bytes())?;
        self.writer.seek(SeekFrom::Start(end_position))?;
        self.writer.flush()?;
        Ok((self.writer, self.stream_info))
    }
}
