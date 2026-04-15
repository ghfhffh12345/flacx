# flacx CLI encoding performance review and documentation guide

This maintainer document translates
`.omx/plans/prd-flacx-encoding-performance.md`
and
`.omx/plans/test-spec-flacx-encoding-performance.md`
into a repo-local review companion for the active CLI directory-encode
performance lane.

It exists so code review, implementation, and verification can share one durable
in-repo description of the current bottleneck, the thread-budget contract, the
progress/trace constraints, and the documentation obligations for this work.

## Non-negotiable guardrails

- keep the external CLI and library API unchanged
- preserve compression-level behavior, especially level 8
- preserve logical output equivalence and existing per-file write correctness
- treat `--threads` for directory encode as a bounded **process-wide** budget,
  not as blind per-file multiplication
- do not trade the target directory win for worse single-file performance, worse
  small-directory behavior, obvious write-I/O collapse, or meaningful peak-memory
  growth
- preserve per-file progress ordering as `begin -> progress -> finish` even if
  different files eventually interleave globally
- keep performance claims tied to measured evidence, not scheduler intuition

## Grounded repo findings

### 1. Directory encode is still serialized in the CLI orchestration layer

Repo evidence from `crates/flacx-cli/src/lib.rs`:

- `encode_command(...)` still iterates `for item in planned.items` and fully
  finishes one file before beginning the next
- the same sequential pattern also exists today in `decode_command(...)` and
  `recompress_command(...)`
- the current target command therefore keeps the library's per-file frame
  parallelism, but not any directory-level overlap

Review implication:

- the PRD is correctly aimed at a visible CLI scheduling bottleneck first
- scheduler work should stay narrowly focused on directory orchestration and not
  expand into a broad CLI rewrite unless fresh evidence requires it

### 2. `--threads` currently flows straight through to every file config clone

Repo evidence:

- `encode_command(...)` clones `command.config` for each work item before calling
  `into_encoder(...)`
- `EncoderConfig::default()` still defaults to the machine parallelism path,
  which the workspace docs summarize as `8` for the CLI default
- `crates/flacx-cli/benches/directory_parity.rs` also passes the same thread
  count straight into each benchmarked directory run

Review implication:

- naive multi-file concurrency would multiply encoder worker demand and violate
  the new contract immediately
- the first implementation review question is not “does it run in parallel?” but
  “where is the total budget enforced?”

### 3. Batch progress and trace code still model one active file at a time

Repo evidence from `crates/flacx-cli/src/lib.rs`:

- `BatchProgressCoordinator` stores a single `current_file`
- `observe(...)`, `observe_recompress(...)`, `heartbeat(...)`, and
  `finish_current_file(...)` all derive state from that one active file record
- trace output is currently organized around ordered `file_begin`,
  `first_progress`, and `file_finish` events for the active file

Review implication:

- concurrent directory encode cannot be treated as a pure scheduler change
- either event production must stay serialized at the coordinator boundary, or
  the coordinator/trace model must become explicitly multi-file while still
  preserving per-file ordering
- reviewers should reject any optimization that improves wall time by making
  progress attribution ambiguous

### 4. Exact batch totals and deterministic planning already exist and should stay authoritative

Repo evidence:

- `plan_directory_worklist(...)` sorts work items by `display_name`
- directory planning precomputes total samples and rejects overflow before work
  dispatch
- CLI tests already guard exact batch-total behavior for encode and decode

Review implication:

- progress math should continue to use planned exact sample totals rather than
  heuristic file counts or approximate work units
- if concurrent execution changes global event order, maintainers should still be
  able to reason from one deterministic planned worklist

### 5. Reuse the existing verification lane

Repo evidence:

- `scripts/cli_perf_compare.py` already captures repeated baseline/head runs,
  writes `FLACX_PROGRESS_TRACE` files, and separates wall time from summed
  `file_finish` timing
- `crates/flacx-cli/benches/directory_parity.rs` already provides a smaller
  criterion lane for directory encode/decode
- `crates/flacx-cli/tests/cli.rs` already covers directory output layout,
  progress rendering, trace startup events, and exact batch totals

Review implication:

- the approved plan should reuse these lanes for scheduler evidence whenever
  possible
- new verification should extend current encode-directory and trace assertions to
  cover bounded concurrency and per-file ordering, not replace them with an
  unrelated benchmark story

### 6. `crates/flacx-cli/src/lib.rs` is the main merge and review hotspot

Repo evidence:

- `crates/flacx-cli/src/lib.rs` is already ~2.6k lines and currently owns
  planning, command orchestration, trace emission, live rendering, and many
  inline tests
- the lane's scheduler, progress, and trace changes are concentrated in that one
  file unless work is intentionally factored

Review implication:

- keep diffs narrow and attributable
- prefer extracting small helpers only when they reduce coupling around the new
  scheduler/resource contract; do not turn the performance lane into a stylistic
  refactor

## Documentation contract for the implementation lane

When this work lands, keep the documentation story in this order:

1. **Resource contract first**
   - state that directory-mode `--threads` is a bounded global budget
   - explain how the CLI divides that budget between concurrent files and
     per-file encoder work
2. **Progress and trace invariants second**
   - state that per-file ordering remains `begin -> progress -> finish`
   - note that different files may interleave globally once directory concurrency
     exists
   - keep `FLACX_PROGRESS_TRACE` usable as an attribution aid, not just a debug
     leftover
3. **Performance evidence third**
   - show the target command, repetition count, and aggregation method
   - report target-directory, single-file, and small-directory before/after
     results together
   - include memory/write-I/O notes when scheduler concurrency changes
4. **Correctness proof fourth**
   - link equivalence results, level-8 regression coverage, and CLI progress
     regression tests
   - keep public-surface preservation tied to existing API/CLI tests, not prose
     alone

Do not document this lane as “CLI got faster” without also documenting the
thread-budget and progress-ordering contract that makes the speedup safe.

## Review checklist for implementation PRs

- Is directory concurrency explicitly bounded by the requested `--threads`
  budget?
- Does the implementation avoid stacking full per-file thread counts across all
  in-flight files?
- Does each file still emit attributable `begin -> progress -> finish` behavior?
- Do progress totals still use exact planned sample counts?
- Are target-directory, single-file, and small-directory measurements reported
  together?
- Are memory or write-I/O risks discussed instead of assumed away?
- Did the change stay out of codec-internal micro-optimization territory unless
  post-scheduler evidence justified it?

## Verification lanes this document expects

- `cargo test -p flacx-cli --test cli`
- `cargo check --workspace`
- `cargo fmt --all --check`
- `cargo bench -p flacx-cli --bench directory_parity`
- `python3 scripts/cli_perf_compare.py --baseline-worktree <baseline> \
  --corpus <corpus> --out-dir <artifact-dir>`

The benchmark and compare commands are the evidence lane for scheduler claims.
The cargo test/check/fmt commands are the correctness and integration floor.

## Review companions

Keep this note paired with:

- `.omx/plans/prd-flacx-encoding-performance.md`
- `.omx/plans/test-spec-flacx-encoding-performance.md`
- `crates/flacx-cli/src/lib.rs`
- `crates/flacx-cli/tests/cli.rs`
- `crates/flacx-cli/benches/directory_parity.rs`
- `scripts/cli_perf_compare.py`
- `README.md`
- `crates/flacx-cli/README.md`

## Exit criteria for this note

This note can shrink or disappear once all of the following are true:

1. the bounded scheduler contract has landed and is documented in the user-facing
   CLI docs
2. benchmark evidence shows a real target-directory improvement with no material
   single-file or small-directory regression
3. progress/trace ordering guarantees are explicitly tested and no longer rely on
   reviewer inference
