use std::io::{Read, Seek};

use crate::{
    error::Result,
    input::{EncodePcmStream, WavSpec},
    md5::{StreaminfoMd5, verify_streaminfo_digest},
    read::FlacPcmStream,
};

pub(super) struct VerifyingPcmStream<R> {
    inner: FlacPcmStream<R>,
    expected_md5: [u8; 16],
    md5: Option<StreaminfoMd5>,
    verified: bool,
}

impl<R> VerifyingPcmStream<R> {
    pub(super) fn new(inner: FlacPcmStream<R>, expected_md5: [u8; 16]) -> Self {
        Self {
            md5: Some(StreaminfoMd5::new(inner.spec())),
            expected_md5,
            inner,
            verified: false,
        }
    }

    pub(super) fn spec(&self) -> WavSpec {
        self.inner.spec()
    }

    pub(super) fn set_threads(&mut self, threads: usize) {
        self.inner.set_threads(threads);
    }
}

impl<R: Read + Seek> EncodePcmStream for VerifyingPcmStream<R> {
    fn spec(&self) -> WavSpec {
        self.spec()
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize> {
        let output_start = output.len();
        let frames = self.inner.read_chunk(max_frames, output)?;
        if frames == 0 {
            if !self.verified {
                verify_streaminfo_digest(
                    self.md5.take().expect("md5 state present").finalize()?,
                    self.expected_md5,
                )?;
                self.verified = true;
            }
            return Ok(0);
        }
        self.md5
            .as_mut()
            .expect("md5 state present")
            .update_samples(&output[output_start..])?;
        Ok(frames)
    }
}
