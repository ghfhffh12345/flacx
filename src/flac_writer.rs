use std::io::{Seek, Write};

use crate::metadata::StreamInfo;

pub struct FlacWriter<W: Seek + Write> {
    writer: W,
    stream_info: StreamInfo,
}

impl<W: Seek + Write> FlacWriter<W> {
    pub fn new(writer: W, stream_info: StreamInfo) -> Self {
        Self {
            writer,
            stream_info,
        }
    }
}
