# flacx Public API Hard Reset Design

Date: 2026-04-22

## Goal

Reset the `flacx` crate's public API while it is still in `0.x` so the
intentional surface is smaller, more coherent, and less misleading for
downstream users.

The target outcome is not compatibility. The target outcome is a cleaner API
that reflects how the crate actually works today.

## Problems To Fix

1. The public error taxonomy is WAV-specific even when the failure came from a
   different PCM container family.
2. `EncoderConfig` and `DecodeConfig` expose public fields even though their
   method-based APIs rely on invariants and coupled policy.
3. Session-level builder entry points such as `Encoder::builder()` and
   `Decoder::builder()` are publicly exposed but ergonomically weak because
   their generic writer type is not inferable at the normal call site.
4. Parse-time policy is split between reader options and later session config,
   which makes it easy for callers to assume a session config can affect parse
   behavior that already happened.
5. The public inspection naming still exposes legacy WAV-specific names for
   behavior that now covers multiple PCM container families.

## Non-Goals

1. Preserve source compatibility for existing downstream users.
2. Add deprecation shims or compatibility aliases unless they are needed
   internally during the refactor.
3. Change codec behavior or container support beyond what is necessary to clean
   up the public API.
4. Redesign the explicit pipeline model. The reader -> source -> session model
   remains the core abstraction.

## Approved Direction

### Public API Shape

1. Remove weak session-level builder entry points from the public surface,
   specifically `Encoder::builder()` and `Decoder::builder()`.
2. Make `EncoderConfig` and `DecodeConfig` fields private and expose accessor
   methods, matching the `RecompressConfig` style.
3. Rename the non-FLAC container error variants away from `InvalidWav` and
   `UnsupportedWav` to container-generic names.
4. Replace WAV-specific inspection naming with PCM-family naming as the primary
   public API.

### Structure And Ownership Of Policy

1. Reader parse policy remains owned by reader constructors and reader option
   types.
2. Session config remains owned by session behavior:
   compression tuning, decode output policy, and recompress policy.
3. Convenience helpers continue exposing short file/byte entry points, but
   internally they convert explicit policy into reader options at the parse
   boundary.
4. Top-level re-exports and the `core` module will expose only the intentional
   surface.

### Testing And Documentation

1. Update public API tests first so the new surface is specified before
   implementation changes.
2. Rewrite docs and crate-level examples to use only the reset API.
3. Remove tests that lock in legacy names or field access and replace them with
   tests for the new surface.
4. Verify with targeted API tests, then full crate tests, then doctests.

## Detailed Changes

### 1. Error Taxonomy

Replace the WAV-specific PCM container error variants with generic names:

1. `InvalidWav` -> `InvalidPcmContainer`
2. `UnsupportedWav` -> `UnsupportedPcmContainer`

`InvalidFlac` and `UnsupportedFlac` remain unchanged.

The display text should also become container-generic for the renamed variants.
Feature-gate failures for AIFF and CAF must no longer surface as "unsupported
wav".

### 2. Config Types

`EncoderConfig` and `DecodeConfig` become private-field structs with public
accessors.

Required accessors:

1. `EncoderConfig::level()`
2. `EncoderConfig::threads()`
3. `EncoderConfig::block_size()`
4. `EncoderConfig::block_schedule()`
5. `EncoderConfig::capture_fxmd()`
6. `EncoderConfig::strict_fxmd_validation()`
7. `DecodeConfig::threads()`
8. `DecodeConfig::emit_fxmd()`
9. `DecodeConfig::output_container()`
10. `DecodeConfig::strict_channel_mask_provenance()`
11. `DecodeConfig::strict_seektable_validation()`

The existing `with_*` methods and builders remain the construction path.

### 3. Session Builder Cleanup

Remove public session-level builder helpers whose generic type parameter makes
them a poor public entry point:

1. `Encoder::builder()`
2. `Decoder::builder()`

The public builder story becomes:

1. `EncoderConfig::builder()`
2. `DecodeConfig::builder()`
3. `RecompressConfig::builder()`

`Recompressor` already does not expose the same weak pattern and needs no
equivalent change.

### 4. Parse Policy Cleanup

The API needs a clearer distinction between parse-time and session-time
behavior.

Encode-side:

1. `WavReaderOptions` continues to own RIFF-family parse policy.
2. `PcmReader::with_reader_options` remains the explicit parse-policy entry
   point for dynamic PCM-family input.
3. `EncoderConfig` should not be presented as the parse-policy authority in the
   explicit pipeline.

Decode-side:

1. `FlacReaderOptions` continues to own FLAC parse and validation policy.
2. `DecodeConfig` remains responsible for decode output behavior and strict
   semantic alignment during output materialization.

Convenience helpers may still derive reader options from config values
internally, but documentation must describe that as convenience-layer wiring,
not as the general ownership model for parse policy.

### 5. Inspection Surface

Make PCM-family naming primary:

1. `inspect_pcm_total_samples` is the main public inspection helper for
   supported PCM containers.
2. Remove `inspect_wav_total_samples` from the top-level public surface and from
   public docs/examples.
3. Keep the FLAC counterpart as `inspect_flac_total_samples`.

The `builtin` module should also reflect the PCM-family naming consistently.

### 6. Documentation And Re-exports

Update the crate docs, README snippets, and re-export tables to align with the
reset surface.

This includes:

1. Removing documentation that highlights removed builders.
2. Describing `inspect_pcm_total_samples` as the supported PCM-container
   inspection helper.
3. Ensuring the `core` module re-exports only the reset names.
4. Ensuring examples favor `Config::builder()` or `Config::default().with_*()`
   over any removed session-level builder helpers.

## Test Plan

1. Update or add API tests for renamed error variants and messages.
2. Update or add API tests proving config fields are private and accessors are
   the supported path.
3. Update or add API tests proving the removed session-level builders are no
   longer part of the public API.
4. Update or add API tests proving `inspect_pcm_total_samples` is the supported
   public helper.
5. Run targeted public API tests.
6. Run the full `flacx` test suite.
7. Run `cargo test -p flacx --doc`.

## Risks

1. Existing workspace tests may encode assumptions about removed names and field
   access, so the first red phase will be broad.
2. Renaming public error variants may require more internal churn than the other
   changes because parse and validation code use them pervasively.
3. The convenience layer and rustdoc examples must be updated together or the
   crate will compile while still advertising stale APIs.

## Success Criteria

1. The public surface no longer exposes misleading session builders.
2. Public config invariants are enforced through private fields plus accessors.
3. PCM-container failures no longer report WAV-specific error variants.
4. The inspection API names match the actual supported input families.
5. All `flacx` tests and doctests pass against the reset surface.
