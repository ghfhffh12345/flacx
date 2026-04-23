# Encode-Shaped Decode Simplification Design

**Date:** 2026-04-23

**Goal:** Reshape the FLAC decode path to match the encoder's simpler scheduling model so compressed-byte decode throughput improves toward `~60 MiB/s` without increasing peak memory and without weakening malformed-input failure semantics.

## Constraints

- Maximum decode throughput is the top priority.
- Peak memory must not increase beyond the current decode implementation.
- Lower peak memory is preferred when possible.
- Public API behavior should remain unchanged unless a specific API change is approved first.
- Core malformed-input validation must remain mandatory.
- Optional policy-style validation may remain configurable, but the simplification must not depend on making core checks optional.

## Current State

The current decode architecture is more layered than the encode path:

- `crates/flacx/src/read.rs` owns a background producer that reads FLAC bytes, tracks pending bytes, scans frame boundaries, and seals `DecodeSlabPlan`s.
- `crates/flacx/src/read/session.rs` owns producer coordination, worker coordination, ordered draining, and bounded residency.
- `crates/flacx/src/read/frame.rs` decodes submitted slabs into PCM.
- `crates/flacx/src/read/slab.rs` restores order and drains decoded PCM to the writer.

This architecture already improved throughput by overlapping slab production with worker decode, but the pipeline still carries structural overhead that the encode side does not:

1. foreground or producer-side frame scanning and slab formation
2. worker-side frame parsing and decode
3. ordered drain and container writing

The encode path is structurally simpler:

1. foreground chunk formation from already-valid PCM input
2. worker-side full frame encode
3. ordered writeout

The decode path therefore still spends wall-clock time on a dedicated pre-decode stage that the encode path does not have.

## Root Cause

Decode is still slower than encode primarily because it does more serialized and duplicated compressed-stream work, not because channels are copying giant PCM buffers across threads.

Specifically:

- decode performs a separate frame-boundary discovery step before worker decode
- workers then parse frame bytes again to perform actual decode
- decode must materialize expanded PCM and write it out after that
- ordered slab coordination is carrying state for both prevalidated compressed work and decoded PCM

The current producer-backed design improves overlap, but it does not eliminate the extra architectural stage.

## Validation Principle

The encode path distinguishes between:

- always-on structural validity checks
- optional policy checks

That same principle should govern the decode redesign.

### Always Mandatory

The simplified decode path must always reject malformed core FLAC structure, including:

- invalid FLAC frame headers
- impossible block size / sample rate / bit depth transitions
- invalid channel assignment or subframe structure
- bad header or footer CRC
- wrong frame-number or sample-number progression
- truncated or otherwise incomplete compressed frame payloads

These checks are part of core decode correctness and are not a tuning knob.

### Still Optional

Policy checks that are already conceptually optional may remain optional, such as:

- strict seektable validation
- strict channel-mask provenance handling

These affect metadata acceptance policy, not whether the compressed audio stream itself is valid enough to decode.

### Explicit Non-Goal

This redesign must not introduce a new "unsafe fast path" that skips core malformed-input checks in exchange for speed.

## Options Considered

### Option 1: Encode-Shaped Decode Pipeline

Make decode structurally mirror encode:

- the foreground runtime reads bounded compressed chunks
- the foreground runtime performs only enough boundary discovery to cut work chunks
- workers perform the full per-chunk parse, CRC validation, subframe decode, reconstruction, and PCM materialization once
- the foreground runtime only reorders completed chunks and writes PCM output

Why this is the recommended option:

- it removes the dedicated prevalidation stage as an architectural layer
- it aligns decode with the simpler encode scheduling model
- it keeps mandatory malformed-input failure semantics intact
- it can preserve the existing memory ceiling by keeping the same bounded window discipline

### Option 2: Keep Serial Parse Authority, Parallelize Only Reconstruction

Retain the current authoritative front-end parser and move only later decode work to workers.

Why this is not recommended:

- it keeps the main serial bottleneck
- it preserves most of the current architectural complexity
- it is unlikely to recover enough throughput to justify the churn

### Option 3: Fully Speculative Multi-Reader Decode

Push raw compressed bytes aggressively to workers and let them discover boundaries independently.

Why this is not recommended:

- hard to keep deterministic and memory-bounded
- hard to preserve strict ordered failure behavior
- much larger design and testing surface than needed

## Chosen Design

Adopt Option 1: an encode-shaped decode pipeline with bounded compressed chunks and single-pass worker decode.

The key architectural rule is:

> The dispatcher may discover chunk boundaries, but workers are the only stage that fully validate and decode chunk contents.

This removes the current "validate once in the producer, decode again in workers" shape without weakening correctness.

## Architecture

### Dispatcher

Replace the current producer-thread-centric model with a simpler dispatcher that plays the same structural role as the encode-side foreground loop.

Responsibilities:

- read compressed bytes from the input stream
- accumulate a bounded chunk of complete FLAC frames
- keep chunk formation within the current memory window
- submit chunks to workers
- collect completed results
- release completed chunk residency after ordered writeout

The dispatcher may run on the calling thread or an equally simple runtime-owned loop, but it should no longer be a separate architectural stage with its own long-lived producer/session protocol layered on top of worker scheduling.

### Chunk Formation

Chunk formation should do only enough work to produce exact work boundaries:

- discover where each frame starts and ends
- accumulate complete frames into one chunk
- stop when the chunk reaches the target PCM or compressed-byte budget
- stop early if the in-flight window is full

Chunk formation must not:

- perform full subframe skipping for validation
- verify frame CRC as a separate pass
- retain a second copy of per-frame parse state that exists only to be re-parsed by workers

The chunk boundary scan is allowed to parse the minimum frame header information needed to find the next full frame boundary and to maintain deterministic chunk sequencing.

### Worker Decode

Each worker owns one compressed chunk and performs all expensive correctness work exactly once:

- parse frame headers
- validate frame numbering progression within the chunk
- validate CRC
- decode subframes
- reconstruct channels
- interleave PCM
- produce one ordered decoded chunk

If any frame is malformed, the worker returns a terminal decode error and the session stops.

### Ordered Drain And Writer

Ordered output semantics remain unchanged:

- completed decoded chunks are held until all prior chunks are ready
- the writer consumes ordered PCM only
- progress and MD5 updates remain in ordered output order
- chunk residency is released only after ordered writeout commits

The existing ordered drain concept is still useful, but it should operate on the simplified chunk model rather than carrying compatibility state for both old producer plans and decoded slabs.

## Data Flow

1. `FlacPcmStream::read_chunk` starts or resumes the decode dispatcher.
2. The dispatcher reads compressed bytes and seals a bounded chunk on frame boundaries.
3. The dispatcher submits that chunk to a worker if the in-flight window has capacity.
4. The worker fully parses and decodes the chunk.
5. The worker returns either:
   - one decoded chunk with ordered sequence information, or
   - one terminal decode error
6. The dispatcher accepts ready decoded chunks and places them into ordered drain state.
7. The foreground decode loop drains ordered PCM into the output container writer.
8. After ordered writeout completes, the dispatcher releases that chunk's residency and reopens window capacity.

## Memory Model

This simplification must remain memory-neutral or better relative to the current implementation.

The current large-fixture decode peak is roughly:

- `~128 MiB` resident decoded PCM slabs
- `~25 MiB` staged compressed slab bytes
- `~16 MiB` writer chunk output
- small coordination and metadata overhead

Total rough peak:

- `~170-180 MiB`

The new design must not exceed that level.

Required invariants:

- no increase to decode window depth
- no increase to target per-chunk PCM residency
- no increase to bounded compressed input staged ahead of ordered completion
- no duplicate long-lived residency for both producer-owned parse state and worker-owned decode state for the same chunk

The redesign should ideally reduce memory by deleting retained prevalidation/index state that no longer serves a unique purpose.

## Error Handling

### Malformed FLAC

Malformed compressed audio remains a hard failure:

- bad CRC must still fail
- wrong frame numbering must still fail
- truncated data must still fail
- impossible mid-stream structural changes must still fail

The difference is only where the failure is discovered: during worker decode rather than in a separate prevalidation stage.

### Metadata Policy Errors

Optional decode metadata policy failures remain where they conceptually belong:

- seektable validation still occurs during reader/metadata parsing
- channel-mask provenance validation still occurs during metadata restoration / reader setup

These are not part of the chunked worker decode simplification.

### Cancellation

Writer failures, progress callback failures, or foreground early drop must:

- stop further chunk submission
- allow in-flight workers to terminate cleanly
- avoid deadlock during shutdown
- avoid leaking chunk residency

## Public API Impact

No public API change is planned in this redesign.

The intended external behavior remains:

- same decode entry points
- same configuration surface
- same result shape
- same strict-vs-policy validation meaning

If implementation work reveals that a public API change is truly necessary, that change must be proposed separately and approved before it is made.

## Testing Plan

The implementation must prove all of the following:

- decode output remains bit-exact across thread counts
- malformed FLAC still fails deterministically
- bad CRC still fails
- wrong frame-number/sample-number progression still fails
- truncation still fails
- ordered output remains stable under out-of-order worker completion
- peak resident compressed + PCM state stays within the current bound
- single-thread performance does not regress materially
- multi-thread compressed-byte throughput improves relative to the current producer-backed baseline

Benchmark validation should continue to use the large streaming fixture measured on compressed FLAC bytes, because that is the target metric the redesign is supposed to improve.

## Non-Goals

- No public API expansion for unsafe decode modes
- No weakening of core malformed-input checks
- No memory-for-speed trade that raises peak memory above the current level
- No broader output-format redesign in this step
- No unrelated metadata or convenience API cleanup
- No speculative multi-reader architecture
