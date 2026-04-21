# CLI Byte Throughput Reporting Design

Date: 2026-04-21

## Goal

Replace the current sample-rate-based CLI `Rate` display with true byte throughput reporting for encode, decode, and recompress.

The CLI must show all three byte-rate views:

- `In`
- `Out`
- `Total`

The displayed rates must reflect actual bytes processed, not bytes inferred from audio format metadata.

## Non-Goals

- Redesigning the existing progress layout beyond the rate section
- Removing sample/frame progress, percentages, or ETA
- Changing codec semantics or threading behavior
- Loading entire files into memory for measurement

## Current Problem

The progress UI currently computes:

- `processed_samples / elapsed`

and formats that number as `Rate`.

This is misleading because:

- it is not a byte throughput metric
- it does not distinguish source bytes from destination bytes
- it does not map cleanly to encode, decode, and recompress in the same way
- it diverges from the throughput users measure externally with file sizes and wall time

## Recommended Approach

Extend library progress payloads so byte counters are produced at the actual read/write boundaries, then let `flacx-cli` render `In`, `Out`, and `Total` rates from those counters.

This is preferred over CLI-only derivation because:

- it avoids guessing bytes from sample counts
- it stays correct across WAV, RF64, Wave64, AIFF, AIFC, CAF, FLAC, and recompress phases
- it gives the CLI exact accounting for all commands through one consistent path

## Alternatives Considered

### 1. CLI-only byte estimation

The CLI already knows input file sizes and some output totals, so it could estimate byte throughput from existing metadata.

Rejected because:

- it would still be approximate during streaming
- it would be awkward for recompress phase transitions
- it would duplicate format knowledge in the CLI
- it would drift from actual committed I/O

### 2. Trace-only byte throughput

The CLI could keep the current visible UI and add true byte rates only to progress traces.

Rejected because it does not solve the misleading user-facing display.

## Architecture

### Library Progress Model

`crates/flacx/src/progress.rs`

Extend `ProgressSnapshot` with byte counters:

- `input_bytes_processed: u64`
- `output_bytes_processed: u64`

These fields are monotonic and represent actual bytes processed so far for the current operation.

### Recompress Progress Model

`crates/flacx/src/recompress/progress.rs`

Extend `RecompressProgress` with explicit byte counters for both phase-local and overall reporting:

- `phase_input_bytes_processed: u64`
- `phase_output_bytes_processed: u64`
- `overall_input_bytes_processed: u64`
- `overall_output_bytes_processed: u64`

This mirrors the existing sample-based phase and overall split.

### Producer Responsibilities

Encode:

- `input_bytes_processed` is PCM-container or raw PCM bytes actually read
- `output_bytes_processed` is FLAC bytes actually written

Decode:

- `input_bytes_processed` is FLAC bytes actually read
- `output_bytes_processed` is PCM-container bytes actually written

Recompress:

- decode phase reports FLAC-in and intermediate PCM-out
- encode phase reports intermediate PCM-in and FLAC-out
- overall counters accumulate the full recompress operation across both phases

## Data Flow

1. Codec paths update sample/frame progress as they do now.
2. The same progress emission sites also update exact byte counters at real read/write boundaries.
3. Progress callbacks emit samples, frames, and bytes together.
4. `flacx-cli` stores the latest byte counters per active file.
5. The renderer computes byte rates from byte counters and elapsed time.
6. The CLI displays `In`, `Out`, and `Total` in binary units.

## CLI Rendering Rules

Replace the single `Rate` field with:

- `In <rate>`
- `Out <rate>`
- `Total <rate>`

Examples:

- `In 24.8 MiB/s`
- `Out 103.1 MiB/s`
- `Total 127.9 MiB/s`

Formatting rules:

- use `KiB/s`, `MiB/s`, `GiB/s`
- use binary units, not decimal MB
- keep `Elapsed` and `ETA`
- keep sample-based percent complete

For encode and decode:

- `Total = In + Out`

For recompress:

- file line shows phase-local `In`, `Out`, and `Total`
- batch-overall line shows overall `In`, `Out`, and `Total`

## Error Handling And Correctness Rules

- Byte counters must remain monotonic.
- Progress must not report bytes beyond successfully consumed or committed I/O.
- Recompress phase transitions may reset phase-local byte counters but must not reset overall byte counters.
- Zero-byte and tiny-file cases must format cleanly without divide-by-zero behavior.
- Existing progress consumers must continue compiling after the API update with straightforward field additions, and release notes should call out the public progress-struct expansion.

## Testing

### Library Tests

- verify encode progress byte counters reach expected input and output totals
- verify decode progress byte counters reach expected input and output totals
- verify recompress progress byte counters are correct for phase-local and overall accounting
- verify counters are monotonic

### CLI Tests

- update interactive progress tests to assert `In`, `Out`, and `Total`
- replace old `Rate`-only expectations with explicit byte-rate text
- add at least one deterministic formatting test for `KiB/s` and `MiB/s`
- verify recompress batch lines show phase-local file rates and overall batch rates

### Trace Tests

If trace output keeps first-progress events sample-based, no trace schema change is required for this task. If trace output is expanded later to include bytes, that should be a follow-up change.

## Migration Notes

- `ProgressSnapshot` is a public type, so the new fields are an API expansion.
- `RecompressProgress` is also public and must be updated consistently.
- The CLI should not attempt to infer byte rates once exact library counters exist.

## Acceptance Criteria

- encode, decode, and recompress progress lines show `In`, `Out`, and `Total`
- displayed rates are byte-based and match real throughput measurements
- recompress correctly distinguishes phase-local and overall rates
- no whole-file buffering is introduced
- existing tests pass and new byte-progress tests cover the added accounting
