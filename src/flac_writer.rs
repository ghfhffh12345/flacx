use std::io::{Seek, Write};

use crate::metadata::StreamInfo;

pub struct FlacWriter<W: Seek + Write> {
    writer: W,
    stream_info: StreamInfo,
}
