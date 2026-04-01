use std::io::{self, Seek, Write};

use crate::{level::LevelProfile, metadata::StreamInfo};

pub struct FlacWriter<W: Seek + Write> {
    writer: W,
    stream_info: StreamInfo,
    level_profile: LevelProfile,
}

impl<W: Seek + Write> FlacWriter<W> {
    pub fn new(
        mut writer: W,
        stream_info: StreamInfo,
        level_profile: LevelProfile,
    ) -> io::Result<Self> {
        writer.write_all(b"fLaC")?;
        writer.write_all(&stream_info.to_bytes())?;

        Ok(Self {
            writer,
            stream_info,
            level_profile,
        })
    }
}
