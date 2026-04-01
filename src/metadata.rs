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
        min_block_size: u16,
        max_block_size: u16,
        min_frame_size: u32,
        max_frame_size: u32,
        sample_rate: u32,
        channels: u8,
        bits_per_sample: u8,
        total_samples: u64,
        md5: [u8; 16],
    ) -> Self {
        assert!(channels >= 1 && channels <= 8);
        assert!(bits_per_sample >= 4 && bits_per_sample <= 32);
        assert!(sample_rate <= 0x0f_ffff);
        assert!(min_frame_size <= 0x00ff_ffff);
        assert!(max_frame_size <= 0x00ff_ffff);
        assert!(total_samples <= 0x0f_ff_ff_ff_ff);

        Self {
            min_block_size,
            max_block_size,
            min_frame_size,
            max_frame_size,
            sample_rate,
            channels,
            bits_per_sample,
            total_samples,
            md5,
        }
    }

    pub fn update_block_size(&mut self, block_size: u16) {
        self.min_block_size = self.min_block_size.min(block_size);
        self.max_block_size = self.max_block_size.max(block_size);
    }

    #[inline]
    pub fn update_frame_size(&mut self, frame_size: u32) {
        self.min_frame_size = self.min_frame_size.min(frame_size);
        self.max_frame_size = self.max_frame_size.max(frame_size);
    }

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
    pub fn as_bytes(&self) -> [u8; 34] {
        self.to_bytes()
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
                min_block_size: 16,
                max_block_size: 16,
                min_frame_size: 0,
                max_frame_size: 0,
                sample_rate: 44_100,
                channels: 2,
                bits_per_sample: 16,
                total_samples: 1,
                md5: [0xAB; 16],
            },
            Case {
                min_block_size: 0x1234,
                max_block_size: 0xabcd,
                min_frame_size: 0x00_12_34,
                max_frame_size: 0x00_fe_dc,
                sample_rate: 0x0f_ff_ff,
                channels: 8,
                bits_per_sample: 32,
                total_samples: (1u64 << 36) - 1,
                md5: [0x55; 16],
            },
        ];

        for case in cases {
            let streaminfo = StreamInfo::new(
                case.min_block_size,
                case.max_block_size,
                case.min_frame_size,
                case.max_frame_size,
                case.sample_rate,
                case.channels,
                case.bits_per_sample,
                case.total_samples,
                case.md5,
            );
            assert_streaminfo_layout(&streaminfo, case);
        }
    }

    #[test]
    fn update_block_and_frame_sizes_expand_ranges() {
        let mut streaminfo = StreamInfo::new(64, 128, 1000, 2000, 44_100, 2, 16, 1, [0x11; 16]);

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
}
