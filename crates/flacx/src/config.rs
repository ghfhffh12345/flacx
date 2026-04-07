use crate::level::Level;

/// User-facing encoder configuration for WAV-to-FLAC conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderConfig {
    pub level: Level,
    pub threads: usize,
    pub block_size: u16,
    pub block_schedule: Option<Vec<u16>>,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        let level = Level::Level8;
        let profile = level.profile();
        Self {
            level,
            threads: std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1),
            block_size: profile.block_size,
            block_schedule: None,
        }
    }
}

impl EncoderConfig {
    #[must_use]
    pub fn builder() -> EncoderBuilder {
        EncoderBuilder::default()
    }

    #[must_use]
    pub fn with_level(mut self, level: Level) -> Self {
        let profile = level.profile();
        self.level = level;
        self.block_size = profile.block_size;
        self
    }

    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads.max(1);
        self
    }

    #[must_use]
    pub fn with_block_size(mut self, block_size: u16) -> Self {
        self.block_size = block_size;
        self.block_schedule = None;
        self
    }

    #[must_use]
    pub fn with_block_schedule(mut self, block_schedule: Vec<u16>) -> Self {
        self.block_schedule = Some(block_schedule);
        self
    }
}

/// Fluent builder for [`EncoderConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EncoderBuilder {
    config: EncoderConfig,
}

impl EncoderBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn level(mut self, level: Level) -> Self {
        self.config = self.config.with_level(level);
        self
    }

    #[must_use]
    pub fn threads(mut self, threads: usize) -> Self {
        self.config = self.config.with_threads(threads);
        self
    }

    #[must_use]
    pub fn block_size(mut self, block_size: u16) -> Self {
        self.config = self.config.with_block_size(block_size);
        self
    }

    #[must_use]
    pub fn block_schedule(mut self, block_schedule: Vec<u16>) -> Self {
        self.config = self.config.with_block_schedule(block_schedule);
        self
    }

    #[must_use]
    pub fn build(self) -> EncoderConfig {
        self.config
    }
}

/// User-facing decode configuration for FLAC-to-WAV conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeConfig {
    pub threads: usize,
    pub strict_channel_mask_provenance: bool,
}

impl Default for DecodeConfig {
    fn default() -> Self {
        Self {
            threads: std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1),
            strict_channel_mask_provenance: false,
        }
    }
}

impl DecodeConfig {
    #[must_use]
    pub fn builder() -> DecodeBuilder {
        DecodeBuilder::default()
    }

    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads.max(1);
        self
    }

    #[must_use]
    pub fn with_strict_channel_mask_provenance(mut self, strict: bool) -> Self {
        self.strict_channel_mask_provenance = strict;
        self
    }
}

/// Fluent builder for [`DecodeConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecodeBuilder {
    config: DecodeConfig,
}

impl DecodeBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn threads(mut self, threads: usize) -> Self {
        self.config = self.config.with_threads(threads);
        self
    }

    #[must_use]
    pub fn strict_channel_mask_provenance(mut self, strict: bool) -> Self {
        self.config = self.config.with_strict_channel_mask_provenance(strict);
        self
    }

    #[must_use]
    pub fn build(self) -> DecodeConfig {
        self.config
    }
}

#[cfg(test)]
mod tests {
    use super::{DecodeConfig, EncoderConfig};
    use crate::level::Level;

    #[test]
    fn with_threads_clamps_to_one() {
        assert_eq!(EncoderConfig::default().with_threads(0).threads, 1);
    }

    #[test]
    fn with_level_resets_block_size_to_level_default() {
        let config = EncoderConfig::default()
            .with_block_size(576)
            .with_level(Level::Level6);
        assert_eq!(config.block_size, Level::Level6.profile().block_size);
    }

    #[test]
    fn builder_matches_fluent_config() {
        let built = EncoderConfig::builder()
            .level(Level::Level4)
            .threads(2)
            .block_size(1024)
            .build();

        assert_eq!(
            built,
            EncoderConfig::default()
                .with_level(Level::Level4)
                .with_threads(2)
                .with_block_size(1024)
        );
    }

    #[test]
    fn with_block_schedule_enables_variable_mode() {
        let schedule = vec![576, 1152, 576];
        let config = EncoderConfig::default().with_block_schedule(schedule.clone());

        assert_eq!(config.block_schedule, Some(schedule));
    }

    #[test]
    fn with_block_size_clears_block_schedule() {
        let config = EncoderConfig::default()
            .with_block_schedule(vec![576, 1152])
            .with_block_size(1024);

        assert_eq!(config.block_schedule, None);
        assert_eq!(config.block_size, 1024);
    }

    #[test]
    fn builder_supports_block_schedule() {
        let schedule = vec![576, 1152, 576];
        let built = EncoderConfig::builder()
            .level(Level::Level4)
            .threads(2)
            .block_schedule(schedule.clone())
            .build();

        assert_eq!(
            built,
            EncoderConfig::default()
                .with_level(Level::Level4)
                .with_threads(2)
                .with_block_schedule(schedule)
        );
    }

    #[test]
    fn decode_with_threads_clamps_to_one() {
        assert_eq!(DecodeConfig::default().with_threads(0).threads, 1);
    }

    #[test]
    fn decode_builder_matches_fluent_config() {
        let built = DecodeConfig::builder()
            .threads(4)
            .strict_channel_mask_provenance(true)
            .build();

        assert_eq!(
            built,
            DecodeConfig::default()
                .with_threads(4)
                .with_strict_channel_mask_provenance(true)
        );
    }
}
