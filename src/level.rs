#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Level0,
    Level1,
    Level2,
    Level3,
    Level4,
    Level5,
    Level6,
    Level7,
    Level8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelProfile {
    pub block_size: u16,
    pub max_fixed_order: u8,
    pub max_lpc_order: u8,
    pub max_residual_partition_order: u8,
    pub use_mid_side_stereo: bool,
    pub exhaustive_model_search: bool,
}

impl Level {
    #[inline]
    pub const fn profile(self) -> LevelProfile {
        match self {
            Level::Level0 => LevelProfile {
                block_size: 576,
                max_fixed_order: 4,
                max_lpc_order: 0,
                max_residual_partition_order: 0,
                use_mid_side_stereo: false,
                exhaustive_model_search: false,
            },
            Level::Level1 => LevelProfile {
                block_size: 576,
                max_fixed_order: 4,
                max_lpc_order: 0,
                max_residual_partition_order: 1,
                use_mid_side_stereo: true,
                exhaustive_model_search: false,
            },
            Level::Level2 => LevelProfile {
                block_size: 1152,
                max_fixed_order: 4,
                max_lpc_order: 8,
                max_residual_partition_order: 2,
                use_mid_side_stereo: true,
                exhaustive_model_search: false,
            },
            Level::Level3 => LevelProfile {
                block_size: 1152,
                max_fixed_order: 4,
                max_lpc_order: 8,
                max_residual_partition_order: 2,
                use_mid_side_stereo: true,
                exhaustive_model_search: false,
            },
            Level::Level4 => LevelProfile {
                block_size: 2304,
                max_fixed_order: 4,
                max_lpc_order: 12,
                max_residual_partition_order: 3,
                use_mid_side_stereo: true,
                exhaustive_model_search: false,
            },
            Level::Level5 => LevelProfile {
                block_size: 2304,
                max_fixed_order: 4,
                max_lpc_order: 12,
                max_residual_partition_order: 4,
                use_mid_side_stereo: true,
                exhaustive_model_search: false,
            },
            Level::Level6 => LevelProfile {
                block_size: 4096,
                max_fixed_order: 4,
                max_lpc_order: 16,
                max_residual_partition_order: 5,
                use_mid_side_stereo: true,
                exhaustive_model_search: false,
            },
            Level::Level7 => LevelProfile {
                block_size: 4096,
                max_fixed_order: 4,
                max_lpc_order: 32,
                max_residual_partition_order: 6,
                use_mid_side_stereo: true,
                exhaustive_model_search: true,
            },
            Level::Level8 => LevelProfile {
                block_size: 4096,
                max_fixed_order: 4,
                max_lpc_order: 32,
                max_residual_partition_order: 6,
                use_mid_side_stereo: true,
                exhaustive_model_search: true,
            },
        }
    }
}

impl From<Level> for u8 {
    #[inline]
    fn from(level: Level) -> Self {
        match level {
            Level::Level0 => 0,
            Level::Level1 => 1,
            Level::Level2 => 2,
            Level::Level3 => 3,
            Level::Level4 => 4,
            Level::Level5 => 5,
            Level::Level6 => 6,
            Level::Level7 => 7,
            Level::Level8 => 8,
        }
    }
}

impl core::convert::TryFrom<u8> for Level {
    type Error = u8;

    #[inline]
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Level::Level0),
            1 => Ok(Level::Level1),
            2 => Ok(Level::Level2),
            3 => Ok(Level::Level3),
            4 => Ok(Level::Level4),
            5 => Ok(Level::Level5),
            6 => Ok(Level::Level6),
            7 => Ok(Level::Level7),
            8 => Ok(Level::Level8),
            _ => Err(value),
        }
    }
}

impl LevelProfile {
    #[inline]
    pub const fn new(
        block_size: u16,
        max_fixed_order: u8,
        max_lpc_order: u8,
        max_residual_partition_order: u8,
        use_mid_side_stereo: bool,
        exhaustive_model_search: bool,
    ) -> Self {
        Self {
            block_size,
            max_fixed_order,
            max_lpc_order,
            max_residual_partition_order,
            use_mid_side_stereo,
            exhaustive_model_search,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Level;

    #[test]
    fn converts_from_numeric_levels() {
        for (raw, expected) in [
            (0, Level::Level0),
            (1, Level::Level1),
            (2, Level::Level2),
            (3, Level::Level3),
            (4, Level::Level4),
            (5, Level::Level5),
            (6, Level::Level6),
            (7, Level::Level7),
            (8, Level::Level8),
        ] {
            assert_eq!(Level::try_from(raw), Ok(expected));
            assert_eq!(u8::from(expected), raw);
        }
    }

    #[test]
    fn rejects_out_of_range_levels() {
        assert_eq!(Level::try_from(9), Err(9));
    }
}
