#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct StreanInfo([u8; 34]);

impl StreanInfo {
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

        let mut bytes = [0u8; 34];
        bytes[..2].copy_from_slice(&min_block_size.to_be_bytes());
        bytes[2..4].copy_from_slice(&max_block_size.to_be_bytes());
        bytes[4..7].copy_from_slice(&Self::u24_be_bytes(min_frame_size));
        bytes[7..10].copy_from_slice(&Self::u24_be_bytes(max_frame_size));
        bytes[10..18].copy_from_slice(&(((sample_rate as u64) << 44)
            | (((channels - 1) as u64) << 41)
            | (((bits_per_sample - 1) as u64) << 36)
            | total_samples)
            .to_be_bytes());
        bytes[18..].copy_from_slice(&md5);
        Self(bytes)
    }

    #[inline]
    pub fn set_min_block_size(&mut self, min_block_size: u16) {
        self.0[..2].copy_from_slice(&min_block_size.to_be_bytes());
    }

    #[inline]
    pub fn set_max_block_size(&mut self, max_block_size: u16) {
        self.0[2..4].copy_from_slice(&max_block_size.to_be_bytes());
    }

    #[inline]
    pub fn set_min_frame_size(&mut self, min_frame_size: u32) {
        self.0[4..7].copy_from_slice(&Self::u24_be_bytes(min_frame_size));
    }

    #[inline]
    pub fn set_max_frame_size(&mut self, max_frame_size: u32) {
        self.0[7..10].copy_from_slice(&Self::u24_be_bytes(max_frame_size));
    }

    #[inline]
    pub fn as_bytes(&self) -> &[u8; 34] {
        &self.0
    }

    #[inline]
    const fn u24_be_bytes(value: u32) -> [u8; 3] {
        let [_, b1, b2, b3] = value.to_be_bytes();
        [b1, b2, b3]
    }
}

#[cfg(test)]
mod tests {
    use super::StreanInfo;

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

    fn assert_streaminfo_layout(streaminfo: &StreanInfo, expected: Case) {
        let bytes = streaminfo.as_bytes();
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
            let streaminfo = StreanInfo::new(
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
    fn setters_touch_only_their_ranges() {
        let original = StreanInfo::new(16, 16, 0x00_12_34, 0x00_ab_cd, 44_100, 2, 16, 1, [0xAA; 16]);
        let mut streaminfo = original;
        streaminfo.set_min_block_size(4096);
        streaminfo.set_max_block_size(8192);
        streaminfo.set_min_frame_size(0x00_fe_dc);
        streaminfo.set_max_frame_size(0x00_65_43);

        let min_frame_size = 0x00_fe_dc_u32.to_be_bytes();
        let max_frame_size = 0x00_65_43_u32.to_be_bytes();

        assert_eq!(&streaminfo.as_bytes()[0..2], &4096u16.to_be_bytes());
        assert_eq!(&streaminfo.as_bytes()[2..4], &8192u16.to_be_bytes());
        assert_eq!(&streaminfo.as_bytes()[4..7], &min_frame_size[1..]);
        assert_eq!(&streaminfo.as_bytes()[7..10], &max_frame_size[1..]);
        assert_eq!(&streaminfo.as_bytes()[10..34], &original.as_bytes()[10..34]);
    }
}
