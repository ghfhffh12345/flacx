use crate::level::Level;

/// User-facing encoder configuration for WAV-to-FLAC conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    pub level: Level,
    pub threads: usize,
    pub block_size: u16,
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
        self
    }
}

/// Fluent builder for [`EncoderConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
    pub fn build(self) -> EncoderConfig {
        self.config
    }
}

#[cfg(test)]
mod tests {
    use super::EncoderConfig;
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
}
