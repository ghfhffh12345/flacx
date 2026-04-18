use crate::{
    error::Error,
    error::Result,
    input::{EncodePcmStream, PcmStream, WavSpec},
    md5::{StreaminfoMd5, verify_streaminfo_digest},
    read::DecodePcmStream,
};

pub(super) struct VerifyingPcmStream<S> {
    inner: S,
    expected_md5: [u8; 16],
    md5: Option<StreaminfoMd5>,
    verified: bool,
}

impl<S> VerifyingPcmStream<S>
where
    S: DecodePcmStream,
{
    pub(super) fn new(inner: S, expected_md5: [u8; 16]) -> Self {
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

impl<S> VerifyingPcmStream<S>
where
    S: DecodePcmStream,
{
    pub(super) fn into_verified_pcm_stream(mut self) -> Result<(PcmStream, [u8; 16])> {
        let spec = self.spec();
        let (samples, _frame_count) = self
            .inner
            .take_decoded_samples()?
            .ok_or(Error::InvalidFlac("recompress source already consumed"))?;
        self.md5
            .as_mut()
            .expect("md5 state present")
            .update_samples(&samples)?;
        let actual_md5 = self.md5.take().expect("md5 state present").finalize()?;
        verify_streaminfo_digest(actual_md5, self.expected_md5)?;
        self.verified = true;
        Ok((PcmStream { spec, samples }, self.expected_md5))
    }
}

impl<S> EncodePcmStream for VerifyingPcmStream<S>
where
    S: DecodePcmStream,
{
    fn spec(&self) -> WavSpec {
        self.spec()
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize> {
        let output_start = output.len();
        let total_samples = usize::try_from(self.spec().total_samples).unwrap_or(usize::MAX);
        if !self.verified
            && max_frames >= total_samples
            && let Some((samples, _frame_count)) = self.inner.take_decoded_samples()?
        {
            let frames = samples.len() / usize::from(self.spec().channels);
            if frames == 0 {
                verify_streaminfo_digest(
                    self.md5.take().expect("md5 state present").finalize()?,
                    self.expected_md5,
                )?;
                self.verified = true;
                return Ok(0);
            }
            self.md5
                .as_mut()
                .expect("md5 state present")
                .update_samples(&samples)?;
            verify_streaminfo_digest(
                self.md5.take().expect("md5 state present").finalize()?,
                self.expected_md5,
            )?;
            self.verified = true;
            output.extend_from_slice(&samples);
            return Ok(frames);
        }

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
