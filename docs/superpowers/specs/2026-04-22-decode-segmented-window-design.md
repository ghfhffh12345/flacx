# Rolling-Window Segmented Decode Design

**Date:** 2026-04-22

**Goal:** Redesign the internal decode architecture so release-mode decode throughput can improve toward `~60 MiB/s` without requiring upfront indexing work that grows with file size, while preserving the current public API unless later-approved changes become necessary.

## Constraints

- Maximum decode throughput is the top priority.
- Upfront indexing cost must stay low regardless of total file size.
- The public decode-facing API should remain unchanged in the initial redesign.
- Any future public API change requires explicit approval before implementation.
- Output bytes, error behavior, and container-writing semantics must remain deterministic across thread counts.

## Current State

The current decoder already has a multithreaded execution path, but the hot path is still organized around:

1. staging a relatively small decode packet
2. submitting that packet to a worker
3. collecting packet completions
4. restoring packet order
5. draining packet PCM through a single writer path

The recent streaming-only work removed some avoidable handoff overhead, but the architecture still recentralizes too often around packet scheduling and ordered draining. That is why increasing the configured thread count from `2` to `8` yields little meaningful throughput gain.

The encode side in `crates/flacx/src/encode_pipeline.rs` performs better because its scheduler works on larger, more deliberate units of work and keeps a broader productive window in flight. The decode side in `crates/flacx/src/read.rs`, `crates/flacx/src/read/session.rs`, and `crates/flacx/src/read/frame.rs` still spends too much time on packet-level coordination and consumer-driven control.

## Problem

Two constraints must both hold:

- Decode throughput must materially improve toward `~60 MiB/s`.
- Indexing overhead must not become a file-size-scaled startup tax.

A full-file preindex would improve scheduling quality, but it violates the second constraint because startup latency and memory would grow with the size of the FLAC input. Small packet streaming avoids that startup tax, but it leaves too much coordination overhead in the steady-state decode loop.

The redesign therefore needs to keep the scheduler working on larger throughput-oriented units without requiring a whole-file planning pass.

## Options Considered

### Option 1: Keep the Existing Packet Pipeline and Tune Constants

This means increasing packet sizes, reducing queue churn, and trying to remove more buffer handoffs while keeping the current packet/session model intact.

Why this is rejected as the main solution:

- It does not remove the fundamental packet-level reconvergence point.
- It is unlikely to move decode far enough to the `~60 MiB/s` target by itself.
- It keeps the caller thread too involved in session control.

### Option 2: Full-File Preindex Plus Slab Decode

This means scanning the full FLAC stream into a complete frame schedule first, then decoding large ordered slabs in parallel.

Why this is rejected:

- It makes startup indexing cost scale with total file length.
- It grows planning memory with the whole file.
- It conflicts directly with the requirement that indexing overhead stay low regardless of file size.

### Option 3: Rolling-Window Segmented Decode

This is the chosen design.

It keeps the high-throughput benefits of slab-sized work units, but limits planning to a bounded lookahead window. The decoder only indexes far enough ahead to keep workers and the writer busy.

Why this is the recommended design:

- It preserves low startup and low bounded planning memory.
- It gives workers larger ownership units than the current packet model.
- It removes most packet-level scheduling churn from the hot path.
- It preserves the existing public decode API in the initial implementation.

## Chosen Design

Replace the packet-driven streaming decoder with a `rolling-window segmented pipeline`.

The pipeline still remains logically streaming and bounded, but the internal scheduling unit changes from a small packet to a larger `decode slab`.

The decoder runs as a bounded rolling pipeline:

1. `IndexCoordinator` scans ahead just enough to maintain a small decode window.
2. It converts discovered frames into `FrameDescriptor`s and seals them into larger `DecodeSlabPlan`s.
3. `SlabScheduler` dispatches those slab plans to workers while keeping the number of indexed-but-not-retired slabs bounded.
4. `DecodeWorkers` decode entire slabs into reusable PCM buffers.
5. `OrderedSlabWriter` emits completed slabs strictly in slab order, updates MD5, and writes the output container sequentially.
6. Retired slab resources return to a shared buffer pool so the next window can reuse them.

The important architectural decision is that the planning horizon is bounded, but the work unit is large.

## Components

### `IndexCoordinator`

This component owns FLAC frame discovery. It is the only part of the pipeline allowed to:

- read additional compressed bytes from the source
- scan frame boundaries
- validate frame continuity during discovery
- assign monotonic slab sequence numbers

Its output is a sequence of `DecodeSlabPlan`s, each containing enough frame and byte information for a worker to decode a slab independently.

The coordinator must stop indexing when the bounded lookahead window is full.

### `SlabScheduler`

This component owns slab lifecycle tracking:

- pending
- in flight
- completed
- retired

It replaces the current packet-count-based coordination with slab-oriented accounting. Its job is to keep enough decode work in flight for throughput without allowing the index window, compressed staging bytes, or decoded PCM residency to grow without bound.

### `DecodeWorkers`

Each worker receives one `DecodeSlabPlan` plus the staged source bytes for that slab. The worker fully decodes the slab into an interleaved PCM buffer and returns one `DecodedSlab`.

Workers do not participate in frame discovery, output ordering, or container writing. They only perform frame decode and slab-local accounting.

### `OrderedSlabWriter`

This component restores strict output order by slab sequence number. It is responsible for:

- accepting completed slabs from workers
- buffering out-of-order completions only until earlier slabs arrive
- writing PCM sequentially into the output container
- updating ordered progress counters
- updating streaminfo MD5 on the ordered output stream
- retiring slab resources back into the buffer pool

Container writing remains single-threaded by design.

### `SlabBufferPool`

This component reuses:

- staged compressed byte buffers
- decoded PCM sample buffers

The goal is to prevent the new architecture from replacing coordination overhead with allocator churn.

## Data Flow

1. The decode entry point initializes the segmented session lazily on first decode demand.
2. `IndexCoordinator` reads FLAC bytes and discovers frames until it can seal one slab.
3. The slab is submitted to `SlabScheduler`.
4. The scheduler dispatches it to a decode worker if capacity is available.
5. The worker decodes the slab and returns a `DecodedSlab`.
6. `OrderedSlabWriter` either writes that slab immediately or stores it temporarily if earlier slabs have not completed yet.
7. Once written, the slab is retired and its buffers return to the pool.
8. Retirement frees window capacity, allowing `IndexCoordinator` to advance and prepare the next slab.

This preserves a streaming model while changing the coordination shape from many small packet handoffs to fewer, larger slab handoffs.

## Memory Model

Memory usage is bounded by window geometry, not file duration.

At any point, residency is limited to:

- the active frame-descriptor lookahead window
- staged compressed bytes for in-flight slabs
- decoded PCM buffers for in-flight or completed-but-not-yet-written slabs
- a small ordered completion backlog

The design should define internal slab/window limits in terms of throughput-oriented quantities such as:

- target PCM frames per slab
- maximum compressed bytes per slab
- maximum frames per slab
- maximum slabs ahead of the writer

These are internal tuning knobs, not public API. The key invariant is:

`resident decode state <= bounded slab window`

That invariant keeps upfront indexing cost low and prevents memory growth with total file size.

## Compatibility

The initial redesign does not require a public API change.

The following should remain unchanged:

- `DecodeConfig`
- `Decoder`
- `DecodePcmStream`
- `FlacReader::into_decode_source`
- reader-driven decode usage
- progress callback shape
- output container policy

If later implementation work uncovers a real public API limitation, that change must be proposed separately and approved before implementation.

## Failure Handling

The first terminal failure stops the pipeline.

Rules:

- `IndexCoordinator` errors stop new slab creation immediately.
- `SlabScheduler` cancels outstanding work after the first terminal failure.
- `OrderedSlabWriter` does not report decode success until all prior slabs are written and final MD5 verification succeeds.
- Dropping the session early must still cancel worker activity and shut down without deadlock.
- Temporary output commit behavior remains unchanged, so failures do not publish partial outputs.

This preserves deterministic behavior while keeping shutdown explicit.

## Determinism

The redesign must preserve:

- bit-exact output across thread counts
- stable ordering regardless of worker completion order
- stable error behavior where current tests already require it

Ordering is keyed by slab sequence number, never by completion order.

## Verification Plan

### Correctness

Keep and extend the existing decode parity coverage so the new architecture proves:

- slab out-of-order completion still produces ordered output
- rolling window scheduling is bit-exact across thread counts
- cancellation on index, decode, or write error does not deadlock
- buffer reuse does not leak stale data between slabs

### Boundedness

Add focused tests for:

- maximum lookahead slabs
- maximum in-flight decoded PCM residency
- bounded staged compressed-byte residency
- deterministic retirement of completed slabs

### Performance

Extend the existing Criterion decode throughput coverage to compare:

- current baseline versus segmented decode path
- `1`, `2`, `4`, and `8` decode threads
- large streaming decode throughput
- matched encode/decode throughput on the existing large workload

Benchmark reporting should make the effective MiB/s visible so the `~60 MiB/s` target is easy to evaluate.

## Rollout

The redesign should land as an internal replacement behind the current decode API.

Rollout order:

1. introduce the rolling slab session internals
2. preserve all existing public decode entry points
3. prove parity, boundedness, and throughput improvements
4. remove superseded packet-oriented internals once the new path is authoritative

This spec does not authorize any public API change.
