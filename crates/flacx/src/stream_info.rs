pub(crate) const MAX_STREAMINFO_SAMPLE_RATE: u32 = 0x0f_ffff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfo {
    pub min_block_size: u16,
    pub max_block_size: u16,
    pub min_frame_size: u32,
    pub max_frame_size: u32,
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
    pub total_samples: u64,
    pub md5: [u8; 16],
}

impl StreamInfo {
    #[inline]
    pub fn new(
        sample_rate: u32,
        channels: u8,
        bits_per_sample: u8,
        total_samples: u64,
        md5: [u8; 16],
    ) -> Self {
        assert!((1..=8).contains(&channels));
        assert!((4..=32).contains(&bits_per_sample));
        assert!(sample_rate <= MAX_STREAMINFO_SAMPLE_RATE);
        assert!(total_samples <= 0x0f_ff_ff_ff_ff);

        Self {
            min_block_size: 0,
            max_block_size: 0,
            min_frame_size: 0,
            max_frame_size: 0,
            sample_rate,
            channels,
            bits_per_sample,
            total_samples,
            md5,
        }
    }

    pub fn update_block_size(&mut self, block_size: u16) {
        if self.min_block_size == 0 {
            self.min_block_size = block_size;
        } else {
            self.min_block_size = self.min_block_size.min(block_size);
        }

        if self.max_block_size == 0 {
            self.max_block_size = block_size;
        } else {
            self.max_block_size = self.max_block_size.max(block_size);
        }
    }

    #[inline]
    pub fn update_frame_size(&mut self, frame_size: u32) {
        if self.min_frame_size == 0 {
            self.min_frame_size = frame_size;
        } else {
            self.min_frame_size = self.min_frame_size.min(frame_size);
        }

        if self.max_frame_size == 0 {
            self.max_frame_size = frame_size;
        } else {
            self.max_frame_size = self.max_frame_size.max(frame_size);
        }
    }

    #[allow(clippy::wrong_self_convention)]
    #[inline]
    pub fn to_bytes(&self) -> [u8; 34] {
        let mut bytes = [0u8; 34];
        bytes[..2].copy_from_slice(&self.min_block_size.to_be_bytes());
        bytes[2..4].copy_from_slice(&self.max_block_size.to_be_bytes());
        bytes[4..7].copy_from_slice(&Self::u24_be_bytes(self.min_frame_size));
        bytes[7..10].copy_from_slice(&Self::u24_be_bytes(self.max_frame_size));
        bytes[10..18].copy_from_slice(
            &(((self.sample_rate as u64) << 44)
                | (((self.channels - 1) as u64) << 41)
                | (((self.bits_per_sample - 1) as u64) << 36)
                | self.total_samples)
                .to_be_bytes(),
        );
        bytes[18..].copy_from_slice(&self.md5);
        bytes
    }

    #[inline]
    pub fn from_bytes(bytes: [u8; 34]) -> Self {
        let mut packed_bytes = [0u8; 8];
        packed_bytes.copy_from_slice(&bytes[10..18]);
        let packed = u64::from_be_bytes(packed_bytes);

        Self {
            min_block_size: u16::from_be_bytes([bytes[0], bytes[1]]),
            max_block_size: u16::from_be_bytes([bytes[2], bytes[3]]),
            min_frame_size: u32::from_be_bytes([0, bytes[4], bytes[5], bytes[6]]),
            max_frame_size: u32::from_be_bytes([0, bytes[7], bytes[8], bytes[9]]),
            sample_rate: ((packed >> 44) & 0x000f_ffff) as u32,
            channels: (((packed >> 41) & 0x7) as u8) + 1,
            bits_per_sample: (((packed >> 36) & 0x1f) as u8) + 1,
            total_samples: packed & ((1u64 << 36) - 1),
            md5: bytes[18..34]
                .try_into()
                .expect("fixed STREAMINFO md5 slice"),
        }
    }

    #[inline]
    const fn u24_be_bytes(value: u32) -> [u8; 3] {
        let [_, b1, b2, b3] = value.to_be_bytes();
        [b1, b2, b3]
    }
}

#[cfg(test)]
mod tests {
    use super::StreamInfo;

    #[derive(Clone, Copy)]
    struct Case {
        min_block_size: u16,
        max_block_size: u16,
        min_frame_size: u32,
        max_frame_size: u32,
        sample_rate: u32,
        channels: u8,
        bits_per_sample: u8,
        total_samples: u64,
        md5: [u8; 16],
    }

    fn assert_streaminfo_layout(streaminfo: &StreamInfo, expected: Case) {
        let bytes = streaminfo.to_bytes();
        let min_frame_size = expected.min_frame_size.to_be_bytes();
        let max_frame_size = expected.max_frame_size.to_be_bytes();
        let mut packed_bytes = [0u8; 8];
        packed_bytes.copy_from_slice(&bytes[10..18]);
        let packed = u64::from_be_bytes(packed_bytes);

        assert_eq!(&bytes[0..2], &expected.min_block_size.to_be_bytes());
        assert_eq!(&bytes[2..4], &expected.max_block_size.to_be_bytes());
        assert_eq!(&bytes[4..7], &min_frame_size[1..]);
        assert_eq!(&bytes[7..10], &max_frame_size[1..]);
        assert_eq!(packed >> 44, expected.sample_rate as u64);
        assert_eq!((packed >> 41) & 0x7, (expected.channels - 1) as u64);
        assert_eq!((packed >> 36) & 0x1f, (expected.bits_per_sample - 1) as u64);
        assert_eq!(packed & ((1u64 << 36) - 1), expected.total_samples);
        assert_eq!(&bytes[18..34], &expected.md5);
    }

    #[test]
    fn new_packs_all_fields() {
        let cases = [
            Case {
                min_block_size: 0,
                max_block_size: 0,
                min_frame_size: 0,
                max_frame_size: 0,
                sample_rate: 44_100,
                channels: 2,
                bits_per_sample: 16,
                total_samples: 1,
                md5: [0xAB; 16],
            },
            Case {
                min_block_size: 0,
                max_block_size: 0,
                min_frame_size: 0,
                max_frame_size: 0,
                sample_rate: 0x0f_ff_ff,
                channels: 8,
                bits_per_sample: 32,
                total_samples: (1u64 << 36) - 1,
                md5: [0x55; 16],
            },
        ];

        for case in cases {
            let streaminfo = StreamInfo::new(
                case.sample_rate,
                case.channels,
                case.bits_per_sample,
                case.total_samples,
                case.md5,
            );
            assert_streaminfo_layout(&streaminfo, case);
            assert_eq!(StreamInfo::from_bytes(streaminfo.to_bytes()), streaminfo);
        }
    }

    #[test]
    fn update_block_and_frame_sizes_expand_ranges() {
        let mut streaminfo = StreamInfo::new(44_100, 2, 16, 1, [0x11; 16]);

        streaminfo.update_block_size(43);
        streaminfo.update_block_size(32);
        streaminfo.update_block_size(256);
        streaminfo.update_block_size(124);

        streaminfo.update_frame_size(1000);
        streaminfo.update_frame_size(900);
        streaminfo.update_frame_size(2500);
        streaminfo.update_frame_size(2000);

        assert_eq!(streaminfo.min_block_size, 32);
        assert_eq!(streaminfo.max_block_size, 256);
        assert_eq!(streaminfo.min_frame_size, 900);
        assert_eq!(streaminfo.max_frame_size, 2500);
    }

    #[test]
    fn new_initializes_ranges_to_zero() {
        let streaminfo = StreamInfo::new(44_100, 2, 16, 1, [0xAA; 16]);

        assert_eq!(streaminfo.min_block_size, 0);
        assert_eq!(streaminfo.max_block_size, 0);
        assert_eq!(streaminfo.min_frame_size, 0);
        assert_eq!(streaminfo.max_frame_size, 0);
    }
}
