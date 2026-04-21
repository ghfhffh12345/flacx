use crate::{
    error::Result,
    input::{EncodePcmStream, PcmSpec, PcmStream},
    md5::{StreaminfoMd5, verify_streaminfo_digest},
    read::DecodePcmStream,
};

pub(super) struct VerifyingPcmStream<S> {
    inner: S,
    expected_md5: [u8; 16],
    md5: Option<StreaminfoMd5>,
    pending_samples: Vec<i32>,
    pending_cursor: usize,
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
            pending_samples: Vec::new(),
            pending_cursor: 0,
            verified: false,
        }
    }

    pub(super) fn spec(&self) -> PcmSpec {
        self.inner.spec()
    }

    pub(super) fn set_threads(&mut self, threads: usize) {
        self.inner.set_threads(threads);
    }

    pub(super) fn finish_verification(&mut self) -> Result<()> {
        if self.verified {
            return Ok(());
        }

        verify_streaminfo_digest(
            self.md5.take().expect("md5 state present").finalize()?,
            self.expected_md5,
        )?;
        self.verified = true;
        Ok(())
    }

    #[cfg(feature = "progress")]
    pub(super) fn into_verified_pcm_stream(mut self) -> Result<(PcmStream, [u8; 16], u64)> {
        let spec = self.spec();
        let (samples, _frame_count) = self
            .inner
            .take_decoded_samples()?
            .expect("small-input recompress path requires materialized samples");
        self.md5
            .as_mut()
            .expect("md5 state present")
            .update_samples(&samples)?;
        self.finish_verification()?;
        let input_bytes_read = DecodePcmStream::input_bytes_processed(&self.inner);
        Ok((
            PcmStream { spec, samples },
            self.expected_md5,
            input_bytes_read,
        ))
    }

    #[cfg(not(feature = "progress"))]
    pub(super) fn into_verified_pcm_stream(mut self) -> Result<(PcmStream, [u8; 16])> {
        let spec = self.spec();
        let (samples, _frame_count) = self
            .inner
            .take_decoded_samples()?
            .expect("small-input recompress path requires materialized samples");
        self.md5
            .as_mut()
            .expect("md5 state present")
            .update_samples(&samples)?;
        self.finish_verification()?;
        Ok((PcmStream { spec, samples }, self.expected_md5))
    }
}

impl<S> EncodePcmStream for VerifyingPcmStream<S>
where
    S: DecodePcmStream,
{
    fn spec(&self) -> PcmSpec {
        self.spec()
    }

    #[cfg(feature = "progress")]
    fn input_bytes_processed(&self) -> u64 {
        DecodePcmStream::input_bytes_processed(&self.inner)
    }

    fn read_chunk(&mut self, max_frames: usize, output: &mut Vec<i32>) -> Result<usize> {
        if max_frames == 0 {
            return Ok(0);
        }

        let output_start = output.len();
        let channels = usize::from(self.spec().channels);
        let mut produced_frames = 0usize;

        while produced_frames < max_frames {
            produced_frames +=
                self.drain_pending_into(max_frames - produced_frames, channels, output);
            if produced_frames == max_frames {
                break;
            }

            let mut decoded = Vec::new();
            let frames = self
                .inner
                .read_chunk(max_frames - produced_frames, &mut decoded)?;
            if frames == 0 {
                self.finish_verification()?;
                break;
            }
            self.md5
                .as_mut()
                .expect("md5 state present")
                .update_samples(&decoded)?;
            produced_frames += self.push_decoded_samples(
                decoded,
                frames,
                max_frames - produced_frames,
                channels,
                output,
            );
        }

        if produced_frames == 0 {
            return Ok(0);
        }

        debug_assert_eq!(
            output.len() - output_start,
            produced_frames * channels,
            "recompress verifier emitted a mismatched sample/frame count",
        );
        Ok(produced_frames)
    }

    fn update_streaminfo_md5(&mut self, _md5: &mut StreaminfoMd5, _samples: &[i32]) -> Result<()> {
        Ok(())
    }

    fn finish_streaminfo_md5(&mut self, _md5: StreaminfoMd5) -> Result<[u8; 16]> {
        self.finish_verification()?;
        Ok(self.expected_md5)
    }

    fn preferred_encode_chunk_max_frames(&self) -> Option<usize> {
        Some(1_024)
    }

    fn preferred_encode_chunk_target_pcm_frames(&self) -> Option<usize> {
        Some(4 << 20)
    }
}

impl<S> VerifyingPcmStream<S>
where
    S: DecodePcmStream,
{
    fn drain_pending_into(
        &mut self,
        max_frames: usize,
        channels: usize,
        output: &mut Vec<i32>,
    ) -> usize {
        if self.pending_cursor >= self.pending_samples.len() {
            self.pending_samples.clear();
            self.pending_cursor = 0;
            return 0;
        }

        let pending_frames = (self.pending_samples.len() - self.pending_cursor) / channels;
        let frames = pending_frames.min(max_frames);
        if frames == 0 {
            return 0;
        }
        let sample_count = frames * channels;
        let next = self.pending_cursor + sample_count;
        output.extend_from_slice(&self.pending_samples[self.pending_cursor..next]);
        self.pending_cursor = next;
        if self.pending_cursor == self.pending_samples.len() {
            self.pending_samples.clear();
            self.pending_cursor = 0;
        }
        frames
    }

    fn push_decoded_samples(
        &mut self,
        decoded: Vec<i32>,
        decoded_frames: usize,
        max_frames: usize,
        channels: usize,
        output: &mut Vec<i32>,
    ) -> usize {
        let frames = decoded_frames.min(max_frames);
        let sample_count = frames * channels;
        output.extend_from_slice(&decoded[..sample_count]);
        if frames < decoded_frames {
            self.pending_samples = decoded;
            self.pending_cursor = sample_count;
        } else {
            self.pending_samples.clear();
            self.pending_cursor = 0;
        }
        frames
    }
}
