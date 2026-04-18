# flacx family parity audit

This document records the **current family-parity review** for the same-crate
architecture rebuild. The goal is to keep WAV, AIFF, and CAF visible as
first-class peers while the encode/decode spine stays central.

It is a review artifact, not a user tutorial.

## Current family map

| Family | Read-side modules | Write-side modules | Feature gate | Public exports |
| --- | --- | --- | --- | --- |
| WAV / RF64 / Wave64 | `crates/flacx/src/wav_input.rs` | `crates/flacx/src/wav_output.rs` | `wav` | `WavReader`, `WavPcmStream`, `WavReaderOptions`, `inspect_wav_total_samples` |
| AIFF / AIFC | `crates/flacx/src/aiff.rs` | `crates/flacx/src/aiff_output.rs` | `aiff` | `AiffReader`, `AiffPcmStream` |
| CAF | `crates/flacx/src/caf.rs` | `crates/flacx/src/caf_output.rs` | `caf` | `CafReader`, `CafPcmStream` |

Shared entry points that must stay family-neutral where possible:

- `PcmReader::new(...)` when format choice is truly dynamic
- explicit family readers plus `into_source()` conversions
- `write_pcm_stream(...)`
- `PcmContainer`
- `DecodeConfig::with_output_container(...)`

## Evidence that parity exists today

### 1. Family-specific modules are real peers in the crate tree

The current source tree includes separate reader/writer files for all three
families:

- WAV: `wav_input.rs`, `wav_output.rs`
- AIFF: `aiff.rs`, `aiff_output.rs`
- CAF: `caf.rs`, `caf_output.rs`

That is the right same-crate shape: family boundaries are explicit without
forcing a multi-crate split.

### 2. Feature gates are coarse and symmetric

`crates/flacx/Cargo.toml` exposes:

- `wav`
- `aiff`
- `caf`
- `progress`

The default library surface enables all three family gates together. That keeps
family support legible in docs, tests, and review.

### 3. The public docs already describe the three families together

Current public docs list:

- WAV, RF64, and Wave64
- AIFF and AIFC
- CAF

That avoids a public story where AIFF/CAF appear as postscript formats.

## Current parity caveats

Parity is materially better than the old WAV-heavy shape, but a few naming
artifacts still deserve review attention.

### Legacy public naming caveat

`inspect_wav_total_samples` remains the stable root inspection helper, even
though the same contract now spans WAV, AIFF, and CAF and `inspect_pcm_total_samples`
is also exposed.

This is acceptable for API stability, but docs should avoid presenting that
WAV-shaped name as the conceptual center of the typed substrate.

### Internal helper naming caveat

`write_pcm_stream(...)` currently routes through
`wav_output::write_wav_with_metadata_and_md5_with_options(...)` while selecting
the actual family via `PcmContainer`.

That does **not** prove a WAV-first architecture by itself, but it is exactly
the kind of private naming that can leak outward if follow-up cleanup is not
intentional.

## Review rules for future changes

Treat these as parity gates during follow-up work:

1. A shared substrate type should be explainable without WAV-specific naming.
2. A new family feature should not have to pass through another family's
   intermediate model just to participate.
3. Public docs should mention all enabled families together when the API is
   actually shared.
4. Tests and verification commands should keep family gates visible instead of
   assuming the default feature set is enough evidence.

## Suggested verification cues

Use these checks when parity-related docs or code change:

```bash
cargo check -p flacx
cargo test -p flacx --test api --test decode --test encode --test feature_gates
cargo test -p flacx --no-default-features --features progress,wav
cargo test -p flacx --no-default-features --features progress,wav,aiff,caf
rg -n "inspect_wav_total_samples|inspect_pcm_total_samples|AiffReader|CafReader|WavReader" \
  crates/flacx/src/lib.rs crates/flacx/README.md docs/*.md
```

## Bottom line

The current same-crate rebuild already has real family peers in code structure
and feature gating. The remaining review risk is mostly **naming drift**, not
missing AIFF/CAF architecture.
