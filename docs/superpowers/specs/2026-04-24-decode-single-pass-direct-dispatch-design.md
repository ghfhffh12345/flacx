# Decode Single-Pass Direct-Dispatch Design

**Date:** 2026-04-24

**Goal:** Increase decode throughput substantially by removing duplicated parse work, simplifying scheduling to match the encode path more closely, and preserving mandatory malformed-input checks without increasing peak memory.

## Constraints

- Maximum decode throughput is the top priority.
- Peak memory must not increase beyond the current decode implementation.
- Lower peak memory is preferred when possible.
- Public API behavior should remain unchanged unless a specific API change is approved first.
- Core malformed-input validation must remain mandatory.
- Output ordering, progress semantics, and container-writer behavior must remain unchanged.

## Current State

The current decode pipeline is simpler than the old producer-thread design, but it is still heavier than the encode path:

1. `crates/flacx/src/read.rs` runs a dispatcher loop inside `FlacPcmStream::read_chunk`.
2. `crates/flacx/src/read/chunk.rs` scans buffered compressed input to form bounded decode chunks.
3. `crates/flacx/src/read/session.rs` routes submitted chunks through a coordinator thread and worker pool.
4. `crates/flacx/src/read/frame.rs` reparses each chunk, validates frames, and reconstructs PCM.
5. `crates/flacx/src/read/slab.rs` restores order and drains PCM to `crates/flacx/src/decode_output.rs`.
6. `crates/flacx/src/wav_output.rs` or the equivalent container writer packs and writes decoded samples.

The encode path is structurally leaner:

1. read one planned PCM chunk
2. submit directly to workers
3. drain ordered results directly to the writer

Decode still carries extra layers and duplicate compressed-stream work that encode does not.

## Root Cause

The primary decode slowdown is duplicated parsing and validation work on the compressed stream.

### Foreground Duplicate Work

`crates/flacx/src/read/chunk.rs` currently performs an expensive boundary-discovery pass:

- it repeatedly calls `parse_frame_header`
- it searches for the next frame start by advancing byte-by-byte
- it computes frame lengths before workers begin real decode

This keeps too much bitstream work on the serial path.

### Worker Duplicate Work

Workers in `crates/flacx/src/read/frame.rs` still traverse each frame payload twice:

1. `scan_frame` parses the header again, skips subframes, and validates CRC
2. `decode_frame_samples_into` reads the same frame payload again to reconstruct samples

This means the architecture still pays for:

- front-end parse/boundary work
- worker parse/validation work
- worker decode work

### Allocation And Coordination Overhead

There are additional secondary costs:

- chunk sealing copies compressed bytes into new owned allocations
- worker decode allocates per-channel sample vectors and then interleaves them into another buffer
- the decode session uses an extra coordinator thread between submission and worker execution
- ordered ready-state uses a `BTreeMap` even though the decode window is bounded and strictly sequential

These are not the main bottleneck, but they remain meaningful costs after the main serial parse problem is removed.

## Validation Principle

This redesign changes where validation happens, not whether it happens.

### Mandatory Checks

The new design must continue to reject malformed core FLAC structure, including:

- invalid frame sync or reserved bits
- wrong frame-number or sample-number progression
- impossible block size, sample rate, or bits-per-sample structure
- invalid subframe layout
- header CRC failure
- footer CRC failure
- truncated frame payloads

These checks remain mandatory and are not performance options.

### Optional Policy Checks

Existing optional policy behavior may remain optional, including:

- strict seektable validation
- strict channel-mask provenance behavior

These are metadata policy concerns, not decode-core correctness.

### Explicit Non-Goal

The redesign must not introduce a fast mode that skips malformed-input checks.

## Options Considered

### Option 1: Single-Pass Worker Decode With Direct Dispatch

- dispatcher performs only bounded chunk formation
- dispatcher submits chunks directly to worker queues
- workers parse, validate, reconstruct, and materialize interleaved PCM in one pass
- main thread receives results directly, restores order, and writes output

Why this is recommended:

- removes the dominant duplicated parse work
- aligns decode with the encode pipeline structure
- removes the coordinator-thread hop
- keeps validation mandatory
- can remain memory-neutral by reusing worker buffers and preserving the current bounded window

### Option 2: Keep The Current Session Shape, Fix Only The Scanner And Worker Double-Pass

- keep the coordinator thread and most current chunk/session plumbing
- fix the obvious scanner and worker duplication issues first

Why this is not preferred:

- lower risk but preserves unnecessary coordination overhead
- leaves decode farther from the simpler encode model
- likely yields less long-term throughput headroom

### Option 3: Decode Directly Into Output-Container Byte Buffers

- workers would emit final packed output bytes rather than interleaved PCM

Why this is rejected:

- couples FLAC decode tightly to output-container packing
- makes ordering and container semantics harder to preserve cleanly
- enlarges change surface and risk too much for the current problem

## Chosen Design

Adopt Option 1: single-pass worker decode with direct dispatch and bounded ordered drain.

The core rule is:

> The dispatcher forms bounded compressed chunks, but only workers fully validate and decode chunk contents, and each worker traverses each frame payload once.

## Architecture

### Dispatcher

The foreground decode loop becomes the only scheduler, matching encode more closely.

Responsibilities:

- read compressed bytes from the source
- detect plausible frame starts using cheap sync-driven scanning plus minimal header confirmation
- seal bounded chunks only on confirmed frame boundaries
- submit chunks directly to worker queues
- collect worker results directly
- release window capacity only after ordered writeout retires a chunk

The dispatcher must not:

- skip subframes for validation
- validate footer CRC
- maintain a second long-lived parse/index representation of the same chunk

### Chunk Formation

Chunk formation should be deliberately lightweight.

Allowed work:

- sync-code search
- enough header confirmation to reject obvious false positives
- enough state to know frame ordering and chunk boundaries
- bounded chunk sizing by compressed bytes and expected PCM frames

Forbidden work:

- full subframe traversal
- footer CRC validation
- any second semantic pass over frame payload bytes

### Worker Decode

Each worker owns one chunk and one reusable decode context.

Per chunk, a worker must:

- parse each frame header once
- validate header and footer CRC during the same decode pass
- validate frame-number or sample-number progression
- decode subframes
- reconstruct channels
- append interleaved PCM directly into the worker output buffer
- return one decoded chunk or one terminal error

The worker should not:

- call one function to scan the frame and another to decode it
- allocate fresh per-channel vectors for every frame unless absolutely required

### Ordered Results

Ordered completion remains mandatory, but the implementation should be cheaper.

Replace the current tree-based ready-state with a bounded sequence-indexed structure:

- sequence numbers are assigned at chunk formation time
- completed chunks occupy bounded slots
- ordered drain advances monotonically
- a chunk retires only after its PCM has been handed to the output writer

### Output Writer

The output container path remains unchanged in contract:

- ordered PCM chunks are written in order
- MD5/progress accounting remains in ordered output order
- no public output behavior changes

This work does not redesign container writers. Output packing is a follow-up optimization only if decode-core improvements make the writer the new throughput ceiling.

## Data Flow

1. `FlacPcmStream::read_chunk` resumes the foreground dispatcher.
2. The dispatcher reads compressed bytes and seals a bounded chunk on confirmed frame boundaries.
3. The dispatcher submits the chunk directly to a worker queue if window capacity is available.
4. The worker performs single-pass frame parse, validation, reconstruction, and interleaved PCM materialization.
5. The worker returns either:
   - one decoded chunk tagged with its sequence, or
   - one terminal decode error
6. The main thread stores completed chunks in bounded ordered-ready state.
7. Ordered drain hands PCM to the output writer.
8. When ordered writeout retires the chunk, both chunk memory and window capacity are released.

## Memory Model

This redesign must remain memory-neutral or better relative to the current implementation.

Required invariants:

- no increase in decode window depth
- no increase in target in-flight PCM residency
- no increase in staged compressed-input residency
- no duplicate long-lived retention of dispatcher parse state plus worker decode state for the same chunk

Memory should improve through:

- reusable worker-local scratch and PCM buffers
- elimination of per-frame per-channel temporary vectors where possible
- elimination of compressed-chunk copy-on-seal behavior where feasible
- removal of unused compatibility state and extra coordination layers

The design target is to keep peak memory at or below the current decode path while materially reducing allocation churn.

## Error Handling

### Malformed Input

Malformed compressed audio remains a hard failure.

Failure discovery moves to the worker, except for obvious dispatcher-level truncation before a plausible frame header can be formed.

When one worker reports a terminal decode error:

- further submission stops
- ordered output does not advance beyond already-committed earlier chunks
- the decode operation returns an error deterministically

### Output And Callback Errors

Writer failures or progress-callback failures must:

- stop further chunk submission
- stop ordered draining as soon as practical
- allow worker shutdown or natural drain without deadlock

## Implementation Order

### Step 1: Lightweight Chunk Formation

- replace byte-by-byte `find_next_frame_start` behavior with cheap sync-driven boundary detection
- keep current worker/session behavior temporarily
- verify chunk parity and malformed-input behavior

### Step 2: Single-Pass Worker Decode

- remove the `scan_frame` then `decode_frame_samples_into` split
- decode, validate, and reconstruct in one pass
- introduce reusable worker scratch/output buffers

### Step 3: Remove Compressed-Chunk Copying

- stop rebuilding owned compressed payload storage unnecessarily
- preserve bounded ownership and simple lifetime rules

### Step 4: Direct Worker Dispatch

- remove the coordinator thread in `read/session.rs`
- submit directly to worker queues from the dispatcher
- receive worker results directly on the foreground side

### Step 5: Cheaper Ordered Ready State

- replace `BTreeMap`-based ordered completion with a bounded sequence-indexed structure
- preserve ordered output semantics exactly

### Step 6: Throughput Re-Measurement

- re-run the large streaming decode bench
- compare against prior compressed-byte decode throughput
- stop if target progress is sufficient
- only then consider output-packing optimization

## Testing Plan

The redesign must preserve and extend tests for:

- exact PCM parity against the current decode output
- bad CRC behavior
- wrong frame-number or sample-number progression
- truncation and EOF behavior
- ordered output under out-of-order completion
- bounded compressed and PCM residency
- parity across thread counts
- large-streaming throughput measurement

The authoritative performance verification remains the existing throughput bench on the large streaming fixture.

## Risks

- single-pass worker decode touches correctness-critical FLAC bitstream logic
- moving validation deeper into workers must not change deterministic failure behavior
- direct dispatch must not introduce deadlock or unbounded backpressure
- if decode-core improvements are large, output packing may become the next limiting stage

## Non-Goals

- no public API changes
- no optional unsafe validation-skipping mode
- no redesign of decode output containers as part of this change
- no memory-growth trade for speed
