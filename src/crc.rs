use std::io::{self, Write};

pub(crate) struct Crc8Writer<W: Write> {
    writer: W,
    crc: u8,
}

impl<W: Write> Crc8Writer<W> {
    #[inline]
    pub(crate) fn new(writer: W) -> Self {
        Self { writer, crc: 0 }
    }

    #[inline]
    pub(crate) fn with_crc(crc: u8, writer: W) -> Self {
        Self { writer, crc }
    }

    #[inline]
    fn update_crc(&mut self, bytes: &[u8]) {
        let mut crc = self.crc;

        for &byte in bytes {
            crc ^= byte;
            for _ in 0..8 {
                crc = if crc & 0x80 != 0 {
                    (crc << 1) ^ 0x07
                } else {
                    crc << 1
                };
            }
        }

        self.crc = crc;
    }
}

impl<W: Write> Write for Crc8Writer<W> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.writer.write(buf)?;
        self.update_crc(&buf[..written]);
        Ok(written)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        self.writer.write_all(&[self.crc])?;
        self.writer.flush()
    }
}

pub(crate) struct Crc16Writer<W: Write> {
    writer: W,
    crc: u16,
}

impl<W: Write> Crc16Writer<W> {
    #[inline]
    pub(crate) fn new(writer: W) -> Self {
        Self { writer, crc: 0 }
    }

    #[inline]
    pub(crate) fn with_crc(crc: u16, writer: W) -> Self {
        Self { writer, crc }
    }

    #[inline]
    fn update_crc(&mut self, bytes: &[u8]) {
        let mut crc = self.crc;

        for &byte in bytes {
            crc ^= (byte as u16) << 8;
            for _ in 0..8 {
                crc = if crc & 0x8000 != 0 {
                    (crc << 1) ^ 0x8005
                } else {
                    crc << 1
                };
            }
        }

        self.crc = crc;
    }
}

impl<W: Write> Write for Crc16Writer<W> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.writer.write(buf)?;
        self.update_crc(&buf[..written]);
        Ok(written)
    }

    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        let crc = self.crc.to_be_bytes();
        self.writer.write_all(&crc)?;
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::{Crc8Writer, Crc16Writer};
    use std::io::{self, Write};

    struct ShortWriter {
        max_write: usize,
        bytes: Vec<u8>,
    }

    impl ShortWriter {
        fn new(max_write: usize) -> Self {
            Self {
                max_write,
                bytes: Vec::new(),
            }
        }
    }

    impl Write for ShortWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let written = buf.len().min(self.max_write);
            self.bytes.extend_from_slice(&buf[..written]);
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn assert_crc_output(input: &[u8], expected_crc: u8, chunk_sizes: &[usize]) {
        let mut sink = Vec::new();

        {
            let mut writer = Crc8Writer::new(&mut sink);
            let mut offset = 0;

            for &chunk_size in chunk_sizes {
                let end = offset + chunk_size;
                writer.write_all(&input[offset..end]).unwrap();
                offset = end;
            }

            assert_eq!(offset, input.len());
            writer.flush().unwrap();
        }

        let mut expected = input.to_vec();
        expected.push(expected_crc);
        assert_eq!(sink, expected);
    }

    fn crc8_reference(mut crc: u8, bytes: &[u8]) -> u8 {
        for &byte in bytes {
            crc ^= byte;
            for _ in 0..8 {
                crc = if crc & 0x80 != 0 {
                    (crc << 1) ^ 0x07
                } else {
                    crc << 1
                };
            }
        }

        crc
    }

    fn assert_crc16_output(input: &[u8], expected_crc: u16, chunk_sizes: &[usize]) {
        let mut sink = Vec::new();

        {
            let mut writer = Crc16Writer::new(&mut sink);
            let mut offset = 0;

            for &chunk_size in chunk_sizes {
                let end = offset + chunk_size;
                writer.write_all(&input[offset..end]).unwrap();
                offset = end;
            }

            assert_eq!(offset, input.len());
            writer.flush().unwrap();
        }

        let mut expected = input.to_vec();
        expected.extend_from_slice(&expected_crc.to_be_bytes());
        assert_eq!(sink, expected);
    }

    fn crc16_reference(mut crc: u16, bytes: &[u8]) -> u16 {
        for &byte in bytes {
            crc ^= (byte as u16) << 8;
            for _ in 0..8 {
                crc = if crc & 0x8000 != 0 {
                    (crc << 1) ^ 0x8005
                } else {
                    crc << 1
                };
            }
        }

        crc
    }

    #[test]
    fn crc_matches_rfc9639_frame_header_examples() {
        assert_crc_output(b"", 0x00, &[]);

        let example_2_first_frame = b"\xff\xf8\x69\x98\x00\x0f";
        assert_crc_output(example_2_first_frame, 0x99, &[6]);
        assert_crc_output(example_2_first_frame, 0x99, &[2, 4]);
        assert_crc_output(example_2_first_frame, 0x99, &[1, 1, 1, 1, 1, 1]);

        let example_2_second_frame = b"\xff\xf8\x69\x18\x01\x02";
        assert_crc_output(example_2_second_frame, 0xa4, &[6]);
        assert_crc_output(example_2_second_frame, 0xa4, &[3, 3]);
        assert_crc_output(example_2_second_frame, 0xa4, &[1, 1, 1, 1, 1, 1]);

        let example_3_frame = b"\xff\xf8\x68\x02\x00\x17";
        assert_crc_output(example_3_frame, 0xe9, &[6]);
        assert_crc_output(example_3_frame, 0xe9, &[2, 2, 2]);
        assert_crc_output(example_3_frame, 0xe9, &[1, 1, 1, 1, 1, 1]);
    }

    #[test]
    fn write_handles_short_writes_and_accumulates_only_written_bytes() {
        let input = b"\xff\xf8\x68\x02\x00\x17";
        let mut sink = ShortWriter::new(2);

        {
            let mut writer = Crc8Writer::new(&mut sink);

            let written = writer.write(input).unwrap();
            assert_eq!(written, 2);

            writer.write_all(&input[written..]).unwrap();
            writer.flush().unwrap();
        }

        let mut expected = input.to_vec();
        expected.push(0xe9);
        assert_eq!(sink.bytes, expected);
    }

    #[test]
    fn with_crc_resumes_from_previous_state() {
        let prefix = b"\xff\xf8\x68";
        let suffix = b"\x02\x00\x17";

        let mut prefix_sink = Vec::new();
        let seed = {
            let mut writer = Crc8Writer::new(&mut prefix_sink);
            writer.write_all(prefix).unwrap();
            writer.crc
        };

        let mut sink = Vec::new();
        {
            let mut writer = Crc8Writer::with_crc(seed, &mut sink);
            writer.write_all(suffix).unwrap();
            writer.flush().unwrap();
        }

        assert_eq!(prefix_sink, prefix.to_vec());

        let mut expected = suffix.to_vec();
        expected.push(crc8_reference(seed, suffix));
        assert_eq!(sink, expected);
    }

    #[test]
    fn crc16_matches_rfc9639_frame_footer_examples() {
        assert_crc16_output(b"", 0x0000, &[]);

        let example_1_frame = b"\xff\xf8\x69\x18\x00\x00\xbf\x03\x58\xfd\x03\x12\x8b";
        assert_crc16_output(example_1_frame, 0xaa9a, &[13]);
        assert_crc16_output(example_1_frame, 0xaa9a, &[2, 11]);
        assert_crc16_output(
            example_1_frame,
            0xaa9a,
            &[1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
        );

        let example_3_frame =
            b"\xff\xf8\x68\x02\x00\x17\xe9\x44\x00\x4f\x6f\x31\x3d\x10\x47\xd2\x27\xcb\x6d\x09\x08\x31\x45\x2b\xdc\x28\x22\x22\x80";
        assert_crc16_output(example_3_frame, 0x57a3, &[29]);
        assert_crc16_output(example_3_frame, 0x57a3, &[4, 8, 17]);
        assert_crc16_output(
            example_3_frame,
            0x57a3,
            &[
                1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
                1,
            ],
        );
    }

    #[test]
    fn crc16_write_handles_short_writes_and_accumulates_only_written_bytes() {
        let input = b"\xff\xf8\x68\x02\x00\x17";
        let mut sink = ShortWriter::new(2);

        {
            let mut writer = Crc16Writer::new(&mut sink);

            let written = writer.write(input).unwrap();
            assert_eq!(written, 2);

            writer.write_all(&input[written..]).unwrap();
            writer.flush().unwrap();
        }

        let mut expected = input.to_vec();
        expected.extend_from_slice(&crc16_reference(0, input).to_be_bytes());
        assert_eq!(sink.bytes, expected);
    }

    #[test]
    fn crc16_with_crc_resumes_from_previous_state() {
        let prefix = b"\xff\xf8\x69\x18\x00\x00";
        let suffix = b"\xbf\x03\x58\xfd\x03\x12\x8b";

        let mut prefix_sink = Vec::new();
        let seed = {
            let mut writer = Crc16Writer::new(&mut prefix_sink);
            writer.write_all(prefix).unwrap();
            writer.crc
        };

        let mut sink = Vec::new();
        {
            let mut writer = Crc16Writer::with_crc(seed, &mut sink);
            writer.write_all(suffix).unwrap();
            writer.flush().unwrap();
        }

        assert_eq!(prefix_sink, prefix.to_vec());

        let mut expected = suffix.to_vec();
        expected.extend_from_slice(&crc16_reference(seed, suffix).to_be_bytes());
        assert_eq!(sink, expected);
    }
}
