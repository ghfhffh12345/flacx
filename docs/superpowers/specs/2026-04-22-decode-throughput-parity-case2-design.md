# Streaming-Only Decode Throughput Parity Across `case2`

## Summary

Restore decode throughput so that aggregate decode performance is at least on par with aggregate encode performance across the full `case2` dataset.

All codec operations in `flacx` must execute using streaming. Eager whole-input materialization is not allowed as an operational path.

The public API must remain unchanged.

## Problem

There is a clear throughput gap between encoding and decoding on the `case2` corpus, including the known `case2/test03` workload. That gap is unacceptable because decode should not be slower than encode for the default CLI workflow.

The current tree also still contains eager or materialized execution branches. That conflicts with the new architectural requirement that all codec operations in `flacx` must be streaming-only.

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
- Remove eager operational paths so encode, decode, and recompress execute through streaming-only flows.
- Keep the public API and CLI contract unchanged while changing internal execution policy.

## Non-Goals

- No public API changes in `flacx`
- No CLI behavior or flag changes
- No eager whole-input codec path retained as a normal operation mode
- No benchmark-only or CLI-only fast path that bypasses the normal library pipeline
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

### Option 1: Make the Existing Codec Pipelines Fully Streaming

Adjust the internal scheduler and buffering logic so encode, decode, and recompress all execute through streaming-only paths, while restoring decode throughput under the default decode path.

Why this is the chosen direction:

- It directly addresses the reproduced regression.
- It preserves the current API and user-visible behavior.
- It aligns the implementation with the explicit streaming-only requirement.
- It avoids trading throughput for higher memory use.
- It keeps the solution aligned with the library design instead of adding a benchmark-only shortcut.

### Option 2: Keep a Mixed Streaming and Eager Execution Model

Retain or expand eager materialization in places where it seems faster, while only tuning the streaming path for the specific decode regression.

Why this was rejected:

- It violates the explicit streaming-only requirement.
- It preserves the architectural inconsistency that caused the current tension between throughput work and execution model.
- It risks improving one benchmark while leaving the crate with forbidden eager paths.

### Option 3: Add a CLI-Only or Benchmark-Only Fast Path

Tune only the CLI or benchmark harness to make numbers look better.

Why this was rejected:

- It would hide an internal decode regression instead of fixing it.
- The target is default decode behavior, not a special measurement path.

## Chosen Design

The fix must eliminate eager operational branches and make codec execution uniformly streaming.

The main invariant is:

`active decode work = ready packets + draining packet + in-flight packets`

The existing pipeline already has the right architectural pieces for a streaming-only design:

- Streaming decode packets
- Background worker coordination
- Ordered output draining
- Existing progress and metadata handling

The chosen design has two parts:

1. Remove eager operational branches so the crate always executes codec work through streaming paths.
2. Restore correct producer-side window accounting so packet submission remains bounded by real outstanding work. The scheduler must continue to observe in-flight work after a streaming session has started, rather than undercounting active decode work and distorting the intended decode window.

This keeps the following unchanged:

- `DecodeConfig`
- `Decoder`
- Reader-driven decode flow
- CLI command contract
- Progress callback semantics
- Metadata alignment and output container behavior

This changes the following internal policy:

- `flacx` no longer treats eager whole-input materialization as a normal operational branch for codec execution.

## Affected Areas

- `crates/flacx/src/read.rs`
  - Internal decode window accounting
  - Scheduler invariants
  - Focused regression coverage
- `crates/flacx/src/decode_output.rs`
  - Removal of eager decode execution branches
- `crates/flacx/src/recompress/`
  - Removal of eager verification or materialization branches that conflict with streaming-only execution
- Any encode-side helper paths that currently materialize whole-input codec work
- Existing tests and benches
  - Reuse the current large-streaming decode coverage
  - Reuse the current matched throughput bench
  - Add or update coverage so streaming-only execution is enforced by tests

No new public modules or public types are required.

## Testing Plan

### Functional Regression Coverage

- Add a focused regression test in `read.rs` that fails if active decode work undercounts in-flight packets after the session starts.
- Add or update tests so eager codec execution is no longer allowed as a normal operational branch.

### Decode Safety Coverage

- Run existing decode tests that validate:
  - Background-session behavior
  - Streaming branch selection for large inputs
  - Matching output across thread counts

### Streaming-Only Safety Coverage

- Run or add tests for encode, decode, and recompress paths to ensure operations remain correct when eager materialization is removed.
- Verify small-input behavior still produces correct outputs without falling back to eager whole-input execution.

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
- Removing eager paths widens the internal change surface beyond decode alone and may expose hidden coupling in recompress or small-input helpers.
- Any remaining eager operational path after the change would violate the architectural requirement.

## Implementation Boundary

This spec covers the internal streaming-only transition required to restore decode parity across `case2` under the default CLI workflow.

It does not authorize:

- API redesign
- New CLI controls
- Broader benchmark matrices
- Unrelated refactors
