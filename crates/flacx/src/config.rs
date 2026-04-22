//! Shared configuration types for the `flacx` encoder and decoder.
//!
//! The crate exposes two small configuration values:
//! [`EncoderConfig`] for PCM-container-to-FLAC conversion and [`DecodeConfig`]
//! for FLAC-to-PCM-container conversion. Both are cheap to clone and can be
//! constructed directly or through their builders.
//!
//! Use [`EncoderConfig::builder`] / [`DecodeConfig::builder`] when you want a
//! fluent configuration flow, and use the `with_*` methods when you want to
//! start from [`Default::default`].

use crate::{PcmContainer, level::Level};

/// User-facing encoder configuration for PCM-container-to-FLAC conversion.
///
/// `EncoderConfig` backs both [`EncoderBuilder`] and [`crate::Encoder`]. The default
/// encoder configuration uses the highest preset (`Level::Level8`), the host's
/// available parallelism when it can be detected, and the block size suggested
/// by the selected level profile.
///
/// `with_level` updates the preset and refreshes the block size to match that
/// level's default. `with_block_size` replaces the fixed block size and clears
/// any existing variable block schedule. `with_block_schedule` enables a custom
/// block schedule instead of a single fixed block size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderConfig {
    /// Compression level preset to use for encoding.
    level: Level,
    /// Number of worker threads the encoder may use.
    threads: usize,
    /// Fixed block size in samples when no custom block schedule is supplied.
    block_size: u16,
    /// Optional sequence of block sizes to use instead of a single block size.
    block_schedule: Option<Vec<u16>>,
    /// Whether to import the private `fxmd` WAV chunk during encode-side metadata capture.
    capture_fxmd: bool,
    /// Whether invalid or duplicate `fxmd` chunks should fail encode-side metadata capture.
    strict_fxmd_validation: bool,
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
            capture_fxmd: true,
            strict_fxmd_validation: true,
        }
    }
}

impl EncoderConfig {
    /// Create a fluent builder for [`EncoderConfig`].
    ///
    /// # Example
    ///
    /// ```no_run
    /// use flacx::{EncoderConfig, level::Level};
    ///
    /// let config = EncoderConfig::builder()
    ///     .level(Level::Level8)
    ///     .threads(4)
    ///     .build();
    ///
    /// assert_eq!(config.level(), Level::Level8);
    /// assert_eq!(config.threads(), 4);
    /// ```
    #[must_use]
    pub fn builder() -> EncoderBuilder {
        EncoderBuilder::default()
    }

    /// Return the compression level preset used for encoding.
    #[must_use]
    pub fn level(&self) -> Level {
        self.level
    }

    /// Return the worker thread count.
    #[must_use]
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// Return the fixed block size in samples when no custom block schedule is supplied.
    #[must_use]
    pub fn block_size(&self) -> u16 {
        self.block_size
    }

    /// Return the optional custom block schedule.
    #[must_use]
    pub fn block_schedule(&self) -> Option<&[u16]> {
        self.block_schedule.as_deref()
    }

    /// Return whether encode-side metadata capture imports the private `fxmd` chunk.
    #[must_use]
    pub fn capture_fxmd(&self) -> bool {
        self.capture_fxmd
    }

    /// Return whether invalid or duplicate `fxmd` chunks fail metadata capture.
    #[must_use]
    pub fn strict_fxmd_validation(&self) -> bool {
        self.strict_fxmd_validation
    }

    /// Set the compression level preset.
    ///
    /// This updates [`EncoderConfig::level`] and refreshes
    /// [`EncoderConfig::block_size`] to the new level's default block size.
    /// Any existing `block_schedule` is left unchanged.
    #[must_use]
    pub fn with_level(mut self, level: Level) -> Self {
        let profile = level.profile();
        self.level = level;
        self.block_size = profile.block_size;
        self
    }

    /// Set the worker thread count.
    ///
    /// Values are clamped to at least `1` so the encoder always has a usable
    /// thread count.
    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads.max(1);
        self
    }

    /// Set a fixed block size and clear any custom block schedule.
    #[must_use]
    pub fn with_block_size(mut self, block_size: u16) -> Self {
        self.block_size = block_size;
        self.block_schedule = None;
        self
    }

    /// Enable a custom block schedule.
    ///
    /// The schedule is stored verbatim and will be used by the encoder's plan
    /// stage instead of a single fixed block size.
    #[must_use]
    pub fn with_block_schedule(mut self, block_schedule: Vec<u16>) -> Self {
        self.block_schedule = Some(block_schedule);
        self
    }

    /// Enable or disable `fxmd` import during encode-side WAV metadata capture.
    #[must_use]
    pub fn with_capture_fxmd(mut self, capture: bool) -> Self {
        self.capture_fxmd = capture;
        self
    }

    /// Enable or disable strict `fxmd` validation during encode-side WAV metadata capture.
    #[must_use]
    pub fn with_strict_fxmd_validation(mut self, strict: bool) -> Self {
        self.strict_fxmd_validation = strict;
        self
    }
}

/// Fluent builder for [`EncoderConfig`].
///
/// The builder starts from [`EncoderConfig::default`] and mirrors the same
/// `with_*` customization surface.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EncoderBuilder {
    config: EncoderConfig,
}

impl EncoderBuilder {
    /// Create a new builder starting from [`EncoderConfig::default`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the compression level preset used by the encoder.
    #[must_use]
    pub fn level(mut self, level: Level) -> Self {
        self.config = self.config.with_level(level);
        self
    }

    /// Set the worker thread count.
    #[must_use]
    pub fn threads(mut self, threads: usize) -> Self {
        self.config = self.config.with_threads(threads);
        self
    }

    /// Set a fixed block size.
    #[must_use]
    pub fn block_size(mut self, block_size: u16) -> Self {
        self.config = self.config.with_block_size(block_size);
        self
    }

    /// Set a custom block schedule.
    #[must_use]
    pub fn block_schedule(mut self, block_schedule: Vec<u16>) -> Self {
        self.config = self.config.with_block_schedule(block_schedule);
        self
    }

    /// Enable or disable `fxmd` import during encode-side WAV metadata capture.
    #[must_use]
    pub fn capture_fxmd(mut self, capture: bool) -> Self {
        self.config = self.config.with_capture_fxmd(capture);
        self
    }

    /// Enable or disable strict `fxmd` validation during encode-side WAV metadata capture.
    #[must_use]
    pub fn strict_fxmd_validation(mut self, strict: bool) -> Self {
        self.config = self.config.with_strict_fxmd_validation(strict);
        self
    }

    /// Finish building the configuration.
    #[must_use]
    pub fn build(self) -> EncoderConfig {
        self.config
    }
}

/// User-facing decode configuration for FLAC-to-PCM-container conversion.
///
/// `DecodeConfig` backs both [`DecodeBuilder`] and [`crate::Decoder`]. The default
/// decode configuration uses the host's available parallelism when it can be
/// detected and leaves channel-mask provenance and seektable checks disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeConfig {
    /// Number of worker threads the decoder may use.
    threads: usize,
    /// Whether to emit the private `fxmd` chunk when RIFF-family metadata is preserved during decode.
    emit_fxmd: bool,
    /// Which PCM container family to emit for decoded output.
    output_container: PcmContainer,
    /// Require channel-layout provenance metadata before restoring a non-ordinary mask.
    strict_channel_mask_provenance: bool,
    /// Require RFC 9639 seektable validation instead of tolerating malformed tables.
    strict_seektable_validation: bool,
}

impl Default for DecodeConfig {
    fn default() -> Self {
        Self {
            threads: std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1),
            emit_fxmd: true,
            output_container: PcmContainer::Auto,
            strict_channel_mask_provenance: false,
            strict_seektable_validation: false,
        }
    }
}

impl DecodeConfig {
    /// Create a fluent builder for [`DecodeConfig`].
    ///
    /// # Example
    ///
    /// ```no_run
    /// use flacx::DecodeConfig;
    ///
    /// let config = DecodeConfig::builder()
    ///     .threads(4)
    ///     .strict_channel_mask_provenance(true)
    ///     .strict_seektable_validation(true)
    ///     .build();
    ///
    /// assert_eq!(config.threads(), 4);
    /// assert!(config.strict_channel_mask_provenance());
    /// assert!(config.strict_seektable_validation());
    /// ```
    #[must_use]
    pub fn builder() -> DecodeBuilder {
        DecodeBuilder::default()
    }

    /// Return the worker thread count.
    #[must_use]
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// Return whether decode-side RIFF-family output emits the private `fxmd` chunk.
    #[must_use]
    pub fn emit_fxmd(&self) -> bool {
        self.emit_fxmd
    }

    /// Return the selected output PCM container family.
    #[must_use]
    pub fn output_container(&self) -> PcmContainer {
        self.output_container
    }

    /// Return whether strict channel-mask provenance checks are enabled.
    #[must_use]
    pub fn strict_channel_mask_provenance(&self) -> bool {
        self.strict_channel_mask_provenance
    }

    /// Return whether strict seektable validation is enabled.
    #[must_use]
    pub fn strict_seektable_validation(&self) -> bool {
        self.strict_seektable_validation
    }

    /// Set the worker thread count.
    ///
    /// Values are clamped to at least `1` so the decoder always has a usable
    /// thread count.
    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = threads.max(1);
        self
    }

    /// Enable or disable `fxmd` emission in decoded RIFF-family output.
    #[must_use]
    pub fn with_emit_fxmd(mut self, emit: bool) -> Self {
        self.emit_fxmd = emit;
        self
    }

    /// Select the PCM container family used for decoded output.
    #[must_use]
    pub fn with_output_container(mut self, output_container: PcmContainer) -> Self {
        self.output_container = output_container;
        self
    }

    /// Enable or disable strict channel-mask provenance checks.
    ///
    /// When this is enabled, decoding fails unless the FLAC stream carries the
    /// crate's channel-layout provenance marker for non-ordinary masks.
    #[must_use]
    pub fn with_strict_channel_mask_provenance(mut self, strict: bool) -> Self {
        self.strict_channel_mask_provenance = strict;
        self
    }

    /// Enable or disable strict seektable validation.
    ///
    /// When this is enabled, malformed SEEKTABLE metadata causes decode to fail
    /// instead of being ignored after validation.
    #[must_use]
    pub fn with_strict_seektable_validation(mut self, strict: bool) -> Self {
        self.strict_seektable_validation = strict;
        self
    }
}

/// Fluent builder for [`DecodeConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecodeBuilder {
    config: DecodeConfig,
}

impl DecodeBuilder {
    /// Create a new builder starting from [`DecodeConfig::default`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the worker thread count used by the decoder.
    #[must_use]
    pub fn threads(mut self, threads: usize) -> Self {
        self.config = self.config.with_threads(threads);
        self
    }

    /// Enable or disable `fxmd` emission in decoded RIFF-family output.
    #[must_use]
    pub fn emit_fxmd(mut self, emit: bool) -> Self {
        self.config = self.config.with_emit_fxmd(emit);
        self
    }

    /// Select the PCM container family used for decoded output.
    #[must_use]
    pub fn output_container(mut self, output_container: PcmContainer) -> Self {
        self.config = self.config.with_output_container(output_container);
        self
    }

    /// Enable or disable strict channel-mask provenance checks.
    #[must_use]
    pub fn strict_channel_mask_provenance(mut self, strict: bool) -> Self {
        self.config = self.config.with_strict_channel_mask_provenance(strict);
        self
    }

    /// Enable or disable strict seektable validation.
    #[must_use]
    pub fn strict_seektable_validation(mut self, strict: bool) -> Self {
        self.config = self.config.with_strict_seektable_validation(strict);
        self
    }

    /// Finish building the configuration.
    #[must_use]
    pub fn build(self) -> DecodeConfig {
        self.config
    }
}

#[cfg(test)]
mod tests {
    use super::{DecodeConfig, EncoderConfig};
    use crate::{PcmContainer, level::Level};

    #[test]
    fn with_threads_clamps_to_one() {
        assert_eq!(EncoderConfig::default().with_threads(0).threads(), 1);
    }

    #[test]
    fn with_level_resets_block_size_to_level_default() {
        let config = EncoderConfig::default()
            .with_block_size(576)
            .with_level(Level::Level6);
        assert_eq!(config.block_size(), Level::Level6.profile().block_size);
    }

    #[test]
    fn builder_matches_fluent_config() {
        let built = EncoderConfig::builder()
            .level(Level::Level4)
            .threads(2)
            .block_size(1024)
            .capture_fxmd(false)
            .strict_fxmd_validation(false)
            .build();

        assert_eq!(
            built,
            EncoderConfig::default()
                .with_level(Level::Level4)
                .with_threads(2)
                .with_block_size(1024)
                .with_capture_fxmd(false)
                .with_strict_fxmd_validation(false)
        );
    }

    #[test]
    fn with_block_schedule_enables_variable_mode() {
        let schedule = vec![576, 1152, 576];
        let config = EncoderConfig::default().with_block_schedule(schedule.clone());

        assert_eq!(config.block_schedule(), Some(schedule.as_slice()));
    }

    #[test]
    fn with_block_size_clears_block_schedule() {
        let config = EncoderConfig::default()
            .with_block_schedule(vec![576, 1152])
            .with_block_size(1024);

        assert_eq!(config.block_schedule(), None);
        assert_eq!(config.block_size(), 1024);
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
        assert_eq!(DecodeConfig::default().with_threads(0).threads(), 1);
    }

    #[test]
    fn decode_builder_matches_fluent_config() {
        let built = DecodeConfig::builder()
            .threads(4)
            .emit_fxmd(false)
            .output_container(PcmContainer::Wave64)
            .strict_channel_mask_provenance(true)
            .strict_seektable_validation(true)
            .build();

        assert_eq!(
            built,
            DecodeConfig::default()
                .with_threads(4)
                .with_emit_fxmd(false)
                .with_output_container(PcmContainer::Wave64)
                .with_strict_channel_mask_provenance(true)
                .with_strict_seektable_validation(true)
        );
    }

    #[test]
    fn encoder_default_preserves_fxmd_with_strict_validation() {
        let config = EncoderConfig::default();
        assert!(config.capture_fxmd());
        assert!(config.strict_fxmd_validation());
    }

    #[test]
    fn decode_default_emits_fxmd_without_extra_validation() {
        let config = DecodeConfig::default();
        assert!(config.emit_fxmd());
        assert_eq!(config.output_container(), PcmContainer::Auto);
        assert!(!config.strict_channel_mask_provenance());
        assert!(!config.strict_seektable_validation());
    }
}
