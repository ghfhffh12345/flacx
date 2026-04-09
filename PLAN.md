# Zero-Loss PCM Container Plan

Source of truth:
- `.omx/specs/deep-interview-every-legal-audio-envelope.md`
- `.omx/plans/prd-zero-loss-pcm-container-rollout.md`
- `.omx/plans/test-spec-zero-loss-pcm-container-rollout.md`

## Long-term target

`flacx` should eventually support **every PCM container/envelope that can be converted to FLAC with zero information loss**.

That means:
- broaden beyond the current WAV-only container surface,
- stay **FLAC-centric** rather than adding alternate codecs,
- accept only PCM shapes that FLAC can represent exactly,
- reject any lossy or non-FLAC-representable structure explicitly.

## Hard boundary

Qualifying inputs/outputs must stay inside FLAC’s exact representable envelope:
- integer PCM only
- FLAC-native precision / layout constraints
- no silent approximation

Explicitly rejected:
- float PCM
- >32-bit integer PCM
- non-PCM codecs/subtypes
- channel/topology structures that FLAC cannot represent exactly
- any fallback that would introduce loss

## Brownfield facts driving the plan

- `crates/flacx/README.md:334-359` says the crate is intentionally narrow and currently scoped to WAV ↔ FLAC.
- `crates/flacx/src/input.rs:134-135` rejects RF64 today.
- `crates/flacx/src/input.rs:337-390` still validates only WAV PCM / WAVEFORMATEXTENSIBLE PCM within the current byte-aligned integer envelope.
- `crates/flacx/src/read.rs:103-114` already defines the FLAC-side exactness ceiling as `1..8` channels and `4..32` bits/sample.
- `crates/flacx/src/wav_output.rs:61-143` still writes RIFF/WAVE-only output.
- `crates/flacx/tests/format_envelope.rs:53-167` already locks useful envelope behavior that can anchor broader work.

## Staged rollout order

### Stage 0 — Minimal shared envelope core
- extract a minimal container-neutral PCM envelope contract from the current WAV-specific path
- define exact accept/reject rules for “FLAC-representable” vs “must fail”
- separate shared PCM semantics from container adapters

### Stage 1 — RIFF-family expansion
- add RF64 input/output
- add Wave64 / W64 input/output
- keep WAV parity and explicit error behavior for still-unsupported cases

Why first: current `input.rs` and `wav_output.rs` are already RIFF/WAVE-shaped, so RF64/W64 are the best proof slice for the minimal shared core before more structurally different families land.

### Stage 2 — AIFF / AIFC PCM expansion
- add AIFF PCM support
- add AIFC only for exact integer-PCM forms that map losslessly to FLAC
- reject compressed, float, or otherwise lossy AIFC variants

### Stage 3 — CAF and raw PCM descriptor-based support
- add CAF integer-PCM support where the PCM description is exactly FLAC-representable
- add raw PCM only when the caller provides an explicit descriptor/sidecar/API envelope
- reject ambiguous raw payloads without enough metadata to prove zero-loss conversion

### Stage 4 — Symmetric export, metadata policy, and matrix completion
- finish decode/output parity across supported families
- align CLI/library UX and documentation with the broadened matrix
- complete cross-container round-trip and rejection-path verification

### Stage 5 — Remainder pass
- create a concrete remainder register for all post-Stage-4 candidate gaps
- seed the register with grounded asymmetry questions such as raw-output symmetry and AIFC `sowt` output symmetry
- classify every candidate as `supported`, `close-now`, `defer`, or `reject`
- implement only grounded `close-now` items that preserve the exact FLAC boundary
- close the stage when the register is fully classified and no unclassified candidates remain

## Definition of done for planning

Planning is complete when:
- the full zero-loss target is explicit,
- staged rollout order is explicit,
- the reject boundary is explicit,
- canonical PRD and test-spec artifacts exist for downstream execution.
