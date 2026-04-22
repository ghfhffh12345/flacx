# Background Producer Decode Design

**Date:** 2026-04-23

**Goal:** Reduce decode wall-clock time on `decode_large_streaming_path` enough to improve compressed-byte throughput toward `~60 MiB/s` without increasing peak memory beyond the current segmented decoder budget.

## Constraints

- Peak memory must not increase beyond the current segmented decode level.
- Lower memory usage is preferred when possible.
- The public decode API must remain unchanged.
- The current slab/window memory budget remains the hard ceiling for any new concurrency.
- The change should improve real wall-clock time, not just benchmark normalization or reporting.

## Current State

The current decoder already has:

- a rolling slab index window in `crates/flacx/src/read/index.rs`
- slab-native worker/session coordination in `crates/flacx/src/read/frame.rs` and `crates/flacx/src/read/session.rs`
- ordered slab draining and writer-side release accounting in `crates/flacx/src/read/slab.rs`, `crates/flacx/src/read.rs`, and `crates/flacx/src/decode_output.rs`

However, `FlacPcmStream::read_chunk` in `crates/flacx/src/read.rs` still drives these serial steps on the caller thread:

1. read more FLAC bytes
2. scan the next frame boundary
3. feed the rolling index window
4. seal slab plans
5. submit slab plans into the decode session

Workers and ordered writing overlap, but slab production itself does not. That leaves a large serial slice on the wall-clock path.

## Why Another Design Step Is Needed

Fresh measurement on the current branch shows:

- `decode_large_streaming_path_threads_8` is still about `26.9 MiB/s` on compressed FLAC bytes
- `matched_large_streaming_decode_threads_8` is about `155 MiB/s` on WAV-byte normalization
- `matched_large_streaming_encode` is about `123 MiB/s`

This means decode is no longer slower than encode on the matched benchmark. The remaining shortfall is specifically that the standalone compressed-byte benchmark still spends too much wall-clock time in foreground serial work.

The next step therefore needs to reduce foreground serial work without buying speed by holding more slabs or more staged bytes in memory.

## Options Considered

### Option 1: Background Producer Thread

Move FLAC byte ingestion, frame scanning, and slab-plan creation into a dedicated producer thread that operates under the same slab/window backpressure limit as today.

Why this is the recommended design:

- It attacks the remaining serial wall-clock slice directly.
- It preserves the current memory ceiling.
- It does not require public API changes.
- It layers naturally on top of the current segmented decoder.

### Option 2: Narrower Transient PCM Buffers

Reduce slab PCM memory by decoding into narrower temporary sample buffers when the output path allows it.

Why this is not the first step:

- It is attractive for memory reduction, but it is a broader data-path change.
- It touches worker decode, ordered draining, and writer boundaries simultaneously.
- It should be considered only after removing the obvious serial producer bottleneck.

### Option 3: More Slab/Window Geometry Tuning

Retune slab size and queue geometry under the current execution model.

Why this is not enough:

- The current measurements suggest the dominant issue is not just slab geometry.
- Further tuning still leaves slab production on the caller thread.
- It is more likely to shuffle time between stages than to cut the serial slice materially.

## Chosen Design

Add a `background producer` that owns frame scanning and slab-plan production while respecting the same slab/window budget as the current implementation.

The producer thread becomes responsible for:

- reading FLAC bytes from the source reader
- maintaining `pending_bytes`
- scanning frame boundaries
- driving the rolling index window
- sealing `DecodeSlabPlan`s
- submitting those plans into the slab decode session

The caller thread becomes responsible for:

- collecting completed slabs
- draining ordered PCM
- writing the output container
- releasing ordered slab residency after writes commit

The key rule is that the producer may only read and stage work while the active slab window has capacity. If the window is full, the producer blocks rather than increasing memory usage.

## Components

### `ProducerThread`

Owns:

- the FLAC reader
- `pending_bytes`
- frame scanning state
- rolling index window state

It is the only component allowed to discover new frame boundaries and create new slab plans.

### `StreamingDecodeSession`

Extends its current role to manage producer lifecycle in addition to worker lifecycle. It becomes the bounded runtime hub coordinating:

- producer wake/sleep behavior
- worker submission
- ordered ready slabs
- terminal errors
- shutdown and cancellation

### `FrameDecodeWorkerPool`

Unchanged in responsibility. It still decodes slab plans into decoded slabs.

### `OrderedSlabDrain`

Unchanged in responsibility. It still restores slab order and exposes PCM to the writer loop.

### `WriterLoop`

Still lives in `decode_stream_to_container` plus `FlacPcmStream::read_chunk`. It writes ordered PCM, updates MD5/progress, and acknowledges slab release after ordered writes complete.

## Data Flow

1. `FlacPcmStream` lazily starts the producer-backed session on first decode demand.
2. The producer thread reads FLAC bytes and scans frames.
3. The rolling index window seals a slab plan when it reaches the configured slab boundary.
4. The producer submits the slab plan to the decode session.
5. If the active slab window is full, the producer blocks until ordered completion retires one or more slabs.
6. Workers decode slabs in parallel.
7. The foreground path drains ready slabs in order and writes them.
8. Ordered slab completion releases residency, which reopens capacity for the producer.

The important change is overlap: slab production is no longer on the same thread as ordered writing.

## Memory Model

This design must remain within the current peak memory envelope.

Current rough peak for the large benchmark fixture at `8` threads is approximately:

- `~128 MiB` decoded slab PCM residency
- `~25 MiB` staged compressed slab bytes
- `~16 MiB` writer chunk buffer
- small metadata overhead

Total rough current peak:

- `~170-180 MiB`

The new design must keep that as the hard ceiling.

Required invariants:

- no increase to slab PCM target
- no increase to max slabs ahead
- no increase to per-slab compressed byte cap
- producer-owned `pending_bytes` remains bounded inside the same slab/window accounting

In other words:

`producer bytes + in-flight slab bytes + resident slab PCM + writer chunk <= current budget`

## Failure Handling

- Producer-side FLAC read or frame-scan errors become terminal session errors.
- Worker-side decode errors stop further producer submission.
- Writer/progress callback errors stop the producer and workers promptly.
- Early drop must join the producer thread cleanly without deadlock.
- Ordered output semantics remain unchanged.

## Testing Plan

Add coverage that proves:

- producer-side slab generation continues while the foreground path is busy draining/writing
- active slab window count never exceeds the current configured limit
- producer-side errors cancel cleanly
- progress/writer errors cancel the producer cleanly
- single-thread and multi-thread outputs remain bit-exact

Keep the current decode throughput and parity benches, and use them to validate whether the producer thread materially reduces wall-clock time for `decode_large_streaming_path`.

## Non-Goals

- No public decode API change
- No increase in memory ceiling
- No transient benchmark-only fast path
- No broader sample-format redesign in this step
- No unrelated cleanup of remaining packet-era compatibility shims
