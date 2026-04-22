# Decode Throughput Parity Across `case2`

## Summary

Restore decode throughput so that aggregate decode performance is at least on par with aggregate encode performance across the full `case2` dataset.

The public API must remain unchanged.

## Problem

There is a clear throughput gap between encoding and decoding on the `case2` corpus, including the known `case2/test03` workload. That gap is unacceptable because decode should not be slower than encode for the default CLI workflow.

The current work must address the regression without changing:

- The public library API
- CLI surface area or flags
- Default metadata behavior
- Progress behavior and output semantics

## Goals

- Make aggregate release-mode CLI decode time across `case2` less than or equal to aggregate release-mode CLI encode time across the same dataset.
- Keep the final gate on the default CLI workflow:
  - `flacx encode --threads 8 --mode default --level 8`
  - `flacx decode --threads 8 --mode default`
- Use both the library throughput bench and CLI timing as verification inputs.
- Keep the fix inside the existing decode implementation.

## Non-Goals

- No public API changes in `flacx`
- No CLI behavior or flag changes
- No widening of the eager decode policy purely to win benchmarks
- No decode-only special-case fast path that bypasses the normal library pipeline
- No expansion of the target matrix beyond default mode and `.wav` decode output

## Success Criteria

### Hard Gate

Aggregate release-mode CLI decode time across all `case2` FLAC inputs, writing `.wav`, must be less than or equal to aggregate release-mode CLI encode time across the matched `case2` WAV inputs.

The authoritative commands are:

```bash
flacx encode --threads 8 --mode default --level 8
flacx decode --threads 8 --mode default
```

### Supporting Signals

- The matched library throughput bench must show decode no worse than encode on the large streaming workload after the fix.
- Per-file CLI timings across `case2` must be collected and reported for diagnosis, but they are not individual hard failures unless they cause aggregate decode to lose the final gate.

## Options Considered

### Option 1: Fix the Existing Streaming Decode Pipeline

Adjust the internal decode scheduler and buffering logic so the current streaming pipeline maintains full throughput under the default decode path.

Why this is the chosen direction:

- It directly addresses the reproduced regression.
- It preserves the current API and user-visible behavior.
- It avoids trading throughput for higher memory use.
- It keeps the solution aligned with the library design instead of adding a benchmark-only shortcut.

### Option 2: Push More Inputs Through Eager Materialization

Decode whole inputs into memory more often before writing container output.

Why this was rejected:

- It changes the memory posture of large decodes.
- It weakens the intended streaming design.
- It risks improving benchmark totals while regressing realistic large-input behavior.

### Option 3: Add a CLI-Only or Benchmark-Only Fast Path

Tune only the CLI or benchmark harness to make numbers look better.

Why this was rejected:

- It would hide an internal decode regression instead of fixing it.
- The target is default decode behavior, not a special measurement path.

## Chosen Design

The fix stays inside the existing decode scheduler in `crates/flacx/src/read.rs`.

The main invariant is:

`active decode work = ready packets + draining packet + in-flight packets`

The decode pipeline already has the correct architectural pieces:

- Streaming decode packets
- Background worker coordination
- Ordered output draining
- Existing progress and metadata handling

The chosen design is to restore correct producer-side window accounting so packet submission remains bounded by real outstanding work. The scheduler must continue to observe in-flight work after a streaming session has started, rather than undercounting active decode work and distorting the intended decode window.

This keeps the following unchanged:

- `DecodeConfig`
- `Decoder`
- Reader-driven decode flow
- CLI command contract
- Progress callback semantics
- Metadata alignment and output container behavior
- Eager-versus-streaming split in `decode_output.rs`

## Affected Areas

- `crates/flacx/src/read.rs`
  - Internal decode window accounting
  - Scheduler invariants
  - Focused regression coverage
- Existing tests and benches
  - Reuse the current large-streaming decode coverage
  - Reuse the current matched throughput bench

No new public modules or public types are required.

## Testing Plan

### Functional Regression Coverage

- Add a focused regression test in `read.rs` that fails if active decode work undercounts in-flight packets after the session starts.

### Decode Safety Coverage

- Run existing decode tests that validate:
  - Background-session behavior
  - Streaming branch selection for large inputs
  - Matching output across thread counts

### Performance Verification

- Run the existing matched library throughput bench and compare encode versus decode.
- Run release-mode CLI timings across the full `case2` dataset.
- Record aggregate encode and decode totals.
- Record per-file timings for diagnosis.

## Failure Policy

- Any functional regression blocks the change.
- If the library bench still shows decode slower than encode, continue tuning before completion.
- If the aggregate CLI gate fails, the task is not complete.
- Per-file outliers are diagnostic only unless they cause aggregate decode to miss parity.

## Risks

- The decode scheduler is concurrency-sensitive, so a small accounting bug can cause large throughput swings.
- A throughput-only fix must not break ordered draining or progress accounting.
- A superficially faster decode path that changes memory behavior would violate the intended scope.

## Implementation Boundary

This spec covers only the internal throughput recovery needed to restore decode parity across `case2` under the default CLI workflow.

It does not authorize:

- API redesign
- New CLI controls
- Broader benchmark matrices
- Unrelated refactors
