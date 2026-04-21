# Streaming Decode Architecture Design

**Date:** 2026-04-21

**Goal:** Redesign the internal multithreaded decode architecture so streaming FLAC decode throughput improves to encode-class performance on the existing level-8, 8-thread benchmark without changing the public decode-facing API and without ever loading the full FLAC input into memory.

## Constraints

- Preserve the public decode-facing API in the `flacx` crate.
- Keep the decode path streaming-based end to end.
- Never read or buffer the entire FLAC payload at once.
- Maintain output parity across thread counts.
- Keep memory bounded through explicit queue sizing and packet limits.

## Current State

The current decode path already packetizes frame work and sends it to worker threads, but the scheduling model is still consumer-driven. `FlacPcmStream::read_chunk` in `crates/flacx/src/read.rs` is responsible for:

- reading and staging more FLAC bytes
- scanning frame boundaries
- building decode packets
- submitting packets to the worker pool
- polling and collecting completed packets
- reordering completed packets
- partially draining ordered PCM back to the caller

This means the caller thread is still the decode-session scheduler. The worker pool in `crates/flacx/src/read/frame.rs` only handles frame reconstruction after jobs have already been staged. As a result, the decode side does not overlap packetization, worker decode, and output draining as effectively as the encode pipeline does in `crates/flacx/src/encode_pipeline.rs`.

The bottleneck is not that decode lacks multithreading entirely. The bottleneck is that the current architecture centralizes too much session control in `read_chunk`, causing worker starvation and serialized handoff around the ordered drain.

## Recommended Approach

Adopt a bounded background streaming decode session that separates:

1. FLAC byte ingestion and frame packetization
2. worker-thread frame reconstruction
3. ordered PCM draining into the container writer

The public boundary remains pull-based because `DecodePcmStream` still exposes `read_chunk`, but internally the decode session becomes push-driven in the background so the caller thread can mostly drain already-prepared work.

## Alternatives Considered

### 1. Background streaming decode session

This is the recommended design. It matches the encode pipeline's strengths while respecting the streaming-only constraint. It removes the main scheduler bottleneck from `read_chunk`, keeps memory bounded, and preserves the API.

### 2. Optimize the current pull-driven loop in place

This is lower risk but not sufficient. The caller thread would still remain responsible for scheduling, polling, and ordering. It may recover some throughput but is unlikely to close the decode/encode gap reliably.

### 3. Move container-ready output generation into workers

This is not recommended. It would complicate ordering, duplicate container logic across worker outputs, and push against the current ownership model where the writer remains on the caller thread. It introduces more architectural complexity than necessary.

## Proposed Architecture

### Overview

Introduce a new internal `StreamingDecodeSession` that owns the lifetime of the decode pipeline. `FlacPcmStream` becomes a thin façade over that session for the streaming path.

The pipeline has three bounded stages:

- `PacketizerThread`
- `DecodeWorkerPool`
- `OrderedDrain`

Only the caller thread writes PCM containers and reports progress. All expensive scan-and-decode work is moved ahead of the caller into a bounded background session.

### Stage A: PacketizerThread

The packetizer thread owns the streaming FLAC reader state that is currently mixed into `FlacPcmStream`:

- pending compressed bytes
- current buffer cursor
- discovered frame count
- discovered sample number
- EOF detection

Its responsibilities are:

- read compressed bytes from the FLAC source incrementally
- scan frame boundaries
- accumulate frames into bounded decode packets
- hand those packets to the worker pool through a bounded job queue
- stop producing when the queue window is full
- propagate producer-side errors into shared session state

The packetizer must not read ahead without bound. The queue depth and packet sizing become the hard memory cap for compressed data residency.

### Stage B: DecodeWorkerPool

The worker pool remains responsible for frame reconstruction, but it becomes part of a fuller session contract rather than an isolated compute helper.

Each worker:

- receives a packet containing compressed frame bytes plus frame descriptors
- decodes those frames into interleaved PCM
- publishes a completed packet keyed by `start_frame_index`
- returns failures through the shared result path

The worker output type should support buffer reuse so the decode path does not allocate a fresh `Vec<i32>` for every packet under steady-state load.

### Stage C: OrderedDrain

Ordered draining remains on the caller thread because the public API still owns the output writer locally and the writer type is not required to move to another thread.

The ordered drain is responsible for:

- receiving completed packets
- storing out-of-order packets in a bounded ordered map keyed by `start_frame_index`
- exposing only the next in-order packet
- partially draining packet PCM into the caller-provided `output` buffer when `read_chunk` asks for fewer frames than a packet contains
- updating `completed_input_frames` and drained PCM counters
- recycling fully drained packet buffers

This preserves deterministic frame ordering while allowing decode workers to complete freely out of order.

## Component Boundaries

### `FlacPcmStream`

`FlacPcmStream` should still implement `DecodePcmStream` and `EncodePcmStream`, but its role changes:

- start the `StreamingDecodeSession` lazily on the first streaming `read_chunk`
- delegate chunk reads to the session
- report stream metadata and completion counters through the same trait methods
- remain compatible with the existing `Decoder` and recompress flows

The public type and trait contract remain unchanged.

### `StreamingDecodeSession`

Add a new internal session object in the decode path that owns:

- packetizer thread lifecycle
- worker pool lifecycle
- bounded queues and reuse pools
- ordered ready state
- terminal error state
- completion and shutdown coordination

This is the decode-side equivalent of the encode streaming session in spirit, but adapted for a source that must first discover packet boundaries on the fly.

### Packet and Buffer Types

Use explicit internal packet types with clear ownership:

- compressed work packet: frame descriptors plus a compressed byte slab
- decoded work packet: frame block sizes plus a reusable PCM sample buffer
- draining packet: decoded packet plus a sample cursor for partial exposure through `read_chunk`

These types should keep ownership transitions obvious and allow buffers to be recycled once fully drained.

## Data Flow

1. `decode_stream_to_container` repeatedly calls `read_chunk` on the stream, as it does today.
2. On first demand, `FlacPcmStream` initializes the `StreamingDecodeSession`.
3. The packetizer reads and scans FLAC frames continuously until the bounded window is full.
4. Workers decode packets in parallel and emit completed packets.
5. The caller thread drains the next in-order packet, writes samples to `StreamingPcmWriter`, updates MD5 and progress, and returns buffers to the reuse pool.
6. At EOF, the session drains the remaining completed packets, joins background threads cleanly, and subsequent `read_chunk` calls return `0`.

The key difference from today is overlap. Packetization, worker decode, and container writes proceed concurrently instead of serializing around each `read_chunk` control loop.

## Memory Model

The streaming requirement makes memory discipline part of the architecture, not just a tuning detail.

The session must bound:

- in-flight compressed packet bytes
- in-flight decoded PCM frames
- out-of-order completion backlog
- number of reusable decoded buffers

The initial tuning knobs can reuse the existing concepts:

- `DECODE_PACKET_MAX_INPUT_FRAMES`
- `DECODE_PACKET_TARGET_PCM_FRAMES`
- `DECODE_SESSION_QUEUE_DEPTH_MULTIPLIER`
- `DECODE_SESSION_RESULT_BACKLOG_PER_WORKER`

These constants should remain configurable internally through code changes, but the architecture must not depend on a specific numeric choice. The important property is boundedness.

## API Preservation

No public decode-facing API changes are required.

The following should remain stable:

- `Decoder`
- `DecodeConfig`
- `DecodePcmStream`
- `FlacReader::into_decode_source`
- decode progress behavior at the public boundary

All changes are internal to the streaming decode implementation and its supporting internal types.

## Error Handling

The session should treat packetizer errors, worker errors, and coordination failures as terminal.

Rules:

- the first terminal error is stored in the session
- once a terminal error exists, no new work is accepted
- subsequent `read_chunk` calls return the stored error promptly
- dropping the stream before completion must still shut down threads and queues without deadlock
- out-of-order completion is allowed internally, but out-of-order exposure is not

This keeps behavior deterministic while avoiding hangs on partial consumption or failure.

## Progress And MD5

Progress reporting and streaminfo MD5 verification should remain on the caller thread alongside ordered draining and container writing.

Reasons:

- the container writer already lives there
- progress snapshots are defined by ordered stream advancement
- MD5 should reflect the exact ordered PCM that is written out

This keeps the correctness boundary narrow while moving only the expensive scan/decode work to background threads.

## Testing Strategy

The redesign needs stronger decode-side guardrails similar to the encode session tests.

Add tests for:

- bounded session residency under multithreaded decode
- respect for packet sizing and queue depth limits
- deterministic handling of out-of-order worker completions
- source error cancellation without deadlock
- progress callback failure cancellation without deadlock
- output parity between `threads=1` and multithreaded runs
- continued use of the streaming path for the large fixture above the eager-materialization threshold

Keep existing throughput-oriented benchmarks and add decode-session instrumentation so performance regressions are visible in profiling output, not just benchmark totals.

## Success Criteria

The redesign is successful when all of the following are true:

- public decode APIs remain unchanged
- large-stream decode stays streaming-based
- no full-file FLAC buffering is introduced
- decode throughput on the existing level-8, 8-thread benchmark improves enough to remove the current architecture bottleneck
- memory remains bounded and attributable to queue sizing and packet sizes
- all decode parity and cancellation tests pass

## Implementation Notes

The most likely files involved are:

- `crates/flacx/src/read.rs`
- `crates/flacx/src/read/frame.rs`
- `crates/flacx/src/decode_output.rs`
- `crates/flacx/tests/decode.rs`

The implementation should follow the codebase's existing pattern of explicit session-state structs, bounded queues, and deterministic ordered draining rather than introducing a radically different concurrency model.
