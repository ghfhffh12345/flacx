# Progress Read/Write Throughput Alignment

## Goal

Align the `flacx` progress API and the `flacx-cli` throughput display so `Input` always means read throughput and `Output` always means write throughput. Remove the current ambiguity around generic "processed bytes" naming, and ensure that progress-related byte accounting code in `flacx` only exists when the `progress` feature is enabled.

## Scope

- Rename public byte-counter fields in `flacx` progress types from `*_processed` to `*_read` / `*_written`
- Propagate the renamed fields through encode, decode, and recompress progress producers
- Update `flacx-cli` progress tracking and rendering to use the renamed read/write counters
- Ensure progress-only byte-accounting code in `flacx` is compiled only with `feature = "progress"`
- Preserve existing CLI labels `In`, `Out`, and `Total`, but make their underlying semantics strictly read/write based

## Non-Goals

- No change to non-progress encode or decode behavior
- No throughput unit changes beyond the existing byte-rate display
- No public API preservation for the renamed progress fields; this is an intentional breaking change in the progress API

## Approaches Considered

### 1. Rename the progress API fields and propagate the semantic change

This renames the public `flacx` progress fields to `input_bytes_read` and `output_bytes_written`, and applies the same language to recompress phase and overall counters. The CLI then consumes the renamed fields directly.

Pros:

- Makes the public API match the CLI display
- Removes semantic ambiguity instead of documenting around it
- Keeps a single source of truth for read/write throughput

Cons:

- Breaking change for external progress consumers

### 2. Add new read/write fields and keep processed-byte aliases temporarily

Pros:

- Lower migration cost for callers

Cons:

- Preserves duplicate semantics and code paths
- Keeps the API and implementation more confusing than necessary

### 3. Change only the CLI interpretation

Pros:

- Lowest code churn

Cons:

- Fails to align the crate API with the CLI
- Leaves the semantic mismatch in place

## Recommended Approach

Use approach 1. The progress API should state the same read/write contract that the CLI reports.

## Design

### Public API Changes

In `crates/flacx/src/progress.rs`:

- `ProgressSnapshot.input_bytes_processed` -> `input_bytes_read`
- `ProgressSnapshot.output_bytes_processed` -> `output_bytes_written`

In `crates/flacx/src/recompress/progress.rs`:

- `phase_input_bytes_processed` -> `phase_input_bytes_read`
- `phase_output_bytes_processed` -> `phase_output_bytes_written`
- `overall_input_bytes_processed` -> `overall_input_bytes_read`
- `overall_output_bytes_processed` -> `overall_output_bytes_written`

Internal helper names should follow the same rename so the codebase does not retain the old terminology in intermediate structs or variables.

### Semantic Contract

The renamed counters have strict meaning:

- `input_bytes_read`: bytes successfully read from the operation's input side
- `output_bytes_written`: bytes successfully written to the operation's output side

For recompress:

- Decode phase:
  - input = FLAC bytes read
  - output = decoded PCM/container bytes written to the intermediate handoff model
- Encode phase:
  - input = PCM bytes read from the verified decode side
  - output = FLAC bytes written to the final sink
- Overall counters:
  - overall input = sum of decode-phase input bytes read and encode-phase input bytes read
  - overall output = sum of decode-phase output bytes written and encode-phase output bytes written

The CLI keeps:

- `In` = read throughput
- `Out` = write throughput
- `Total` = `In + Out`

### Producer Sites

Encode path:

- Read counters come from the counted PCM input stream once bytes have actually been read
- Write counters come from the FLAC writer once bytes have actually been committed

Decode path:

- Read counters come from the FLAC source as bytes are actually consumed
- Write counters come from the output container writer once bytes have actually been emitted
- Final decode progress must still include finish-time header/padding writes where applicable

Recompress path:

- Decode and encode phase snapshots each use the phase-local read/write counters
- Overall counters are derived from exact phase totals, not inferred estimates

### Feature Gating

All progress-specific byte-accounting code in `flacx` should exist only when `feature = "progress"` is enabled.

This includes:

- progress snapshot structs and aliases
- recompress progress structs and helpers
- callback progress adapters
- progress-only counting wrappers or fields
- progress-only thread-local state
- progress-only helper functions used solely to report byte counters

Implementation guidance:

- Prefer small `#[cfg(feature = "progress")]` helpers over broad duplicated control flow
- If a counter or helper exists only to feed progress snapshots, remove it entirely from non-progress builds
- Do not keep progress-only struct fields alive behind unused assignments in non-progress builds

Writer and stream methods that have independent non-progress value may remain available, but calls that exist purely to populate progress data should disappear from non-progress compilation paths.

### CLI Changes

In `flacx-cli`:

- Rename internal byte-tracking fields from `*_processed` to `*_read` / `*_written`
- Continue to use sample counts for percent and ETA
- Use byte read/write counters exclusively for throughput calculation
- Keep the existing `In`, `Out`, and `Total` labels

The CLI progress renderer should not reinterpret the counters. It should directly present:

- bytes read per second
- bytes written per second
- their sum

### Error Handling

- Byte read/write counters must remain monotonic
- Partial failures must not over-report bytes beyond what was actually read or written
- Recompress phase transitions must reset phase-local counters while preserving overall totals
- Final progress snapshots must reflect finish-time writes accurately

### Testing

`flacx` tests:

- Update progress API unit tests for the renamed fields
- Verify encode progress reports exact bytes read and bytes written
- Verify decode progress reports exact bytes read and bytes written, including final container padding/header updates
- Verify recompress progress reports exact phase and overall read/write byte totals
- Add or update non-progress compilation coverage so progress-only byte-accounting code does not linger when the feature is disabled

`flacx-cli` tests:

- Update renderer tests to use the renamed read/write fields
- Verify `In`, `Out`, and `Total` still render correctly
- Verify recompress progress uses phase-local read/write rates and batch lines use overall read/write rates

### Risks

- This is a breaking API change for external `progress` consumers
- Feature-gating cleanup can accidentally remove accounting needed by progress-enabled builds if `cfg` boundaries are placed too aggressively
- Recompress overall counters need careful review to preserve exact phase aggregation semantics during eager and streaming handoffs

### Success Criteria

- `flacx` progress types expose read/write byte field names instead of processed-byte names
- `flacx-cli` throughput labels and calculations are backed by the same read/write semantics
- Progress-enabled tests pass with the renamed API
- Non-progress builds compile without retaining progress-only byte-accounting scaffolding
