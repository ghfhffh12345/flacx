use std::io::{self, Write};

pub(crate) struct Crc8Writer<W: Write> {
    writer: W,
    crc: u8,
}

impl<W: Write> Crc8Writer<W> {
    #[inline]
    pub(crate) fn new(writer: W) -> Self {
        Self {
            writer,
            crc: 0,
        }
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

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use super::Crc8Writer;

    #[test]
    fn write_accumulates_crc_and_flush_appends_current_state_each_time() {
        let mut writer = Crc8Writer::new(Cursor::new(Vec::new()));

        writer.write_all(b"1234").unwrap();
        writer.write_all(b"56789").unwrap();
        writer.flush().unwrap();
        writer.flush().unwrap();

        assert_eq!(writer.writer.into_inner(), b"123456789\xF4\xF4".to_vec());
    }
}
